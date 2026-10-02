//! HTTP 客户端：cookie 和浏览器请求头只在这里出现，别在别处另起炉灶建 `reqwest::Client`。
//!
//! B 站 2025 年之后的风控会连着看 UA / Origin / Referer / Pragma，
//! 光有 WBI 签名、头缺一个，`getDanmuInfo` 依旧返 `-352`。

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::api::wbi;

pub const LIVE_BASE: &str = "https://api.live.bilibili.com";
pub const MAIN_BASE: &str = "https://api.bilibili.com";

/// 跟 Go 版一致的那份 UA。换新版本号没意义 —— 风控认的是「像浏览器」，不是版本号。
pub const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/96.0.4664.110 Safari/537.36";

/// 全部小写，`HeaderName::from_static` 才收。
pub const BROWSER_HEADERS: [(&str, &str); 7] = [
    ("user-agent", BROWSER_UA),
    ("accept", "*/*"),
    (
        "accept-language",
        "zh-CN,zh;q=0.8,zh-TW;q=0.7,zh-HK;q=0.5,en-US;q=0.3,en;q=0.2",
    ),
    ("origin", "https://live.bilibili.com"),
    ("referer", "https://live.bilibili.com/"),
    ("pragma", "no-cache"),
    ("cache-control", "no-cache"),
];

pub fn browser_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in BROWSER_HEADERS {
        h.insert(k, HeaderValue::from_static(v));
    }
    h
}

/// WBI 的 img_key/sub_key 和 uid 是同一次 nav 拿的，缓存 30 分钟 —— key 每天轮换，
/// 但也别每个请求都去问一遍 nav（那本身也是流量，还更容易被判成脚本）。
const NAV_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone)]
pub struct Nav {
    /// 登录 uid；没登录时是 0，发弹幕认证包时照发 0。
    pub mid: i64,
    /// 昵称。账号栏那行「名字 (uid N)」要它，不登录时是空串。
    pub uname: String,
    /// 服务端说这次是不是登录着的（`data.isLogin`）。
    ///
    /// 光看 mid 不够：有的接口没登录也回一个非 0 的 mid（设备 id 之类）。
    pub is_login: bool,
    pub mixin_key: String,
    at: Instant,
}

/// 手上这套凭据。登录成功后要能把新的换进来 —— 客户端是 `Arc` 共享给
/// 好几条链路的（弹幕 / 房间 / 发送），整只换掉做不到，只能让里面这块可变。
#[derive(Debug, Clone, Default)]
struct Auth {
    /// 预先校验过的 Cookie 头值，见 `sanitize_cookie`
    header: Option<HeaderValue>,
    /// 原样的 Cookie 串。判断 `SESSDATA` 在不在只看它，别去解 header。
    raw: String,
    /// Cookie 里的 `bili_jct`
    csrf: Option<String>,
}

impl Auth {
    fn from_cookie(raw: &str) -> Self {
        Self {
            header: sanitize_cookie(raw),
            raw: raw.to_string(),
            csrf: cookie_value(raw, "bili_jct"),
        }
    }
}

pub struct BiliClient {
    http: reqwest::Client,
    /// 预先校验过的 Cookie 头值。
    ///
    /// 配置里那串是用户自己抄的，可能带换行或者根本不是 ASCII —— 老 Go 版生成的默认值
    /// 就是一句中文。直接把这种东西交给 `reqwest` 的 `.header()` 会 **panic**，
    /// 而 TUI 一 panic 就是整屏消失、用户什么都看不到。所以这里先过滤 + 校验，
    /// 不合法就干脆不带 Cookie（退化成未登录，报错也只是几行系统弹幕）。
    auth: tokio::sync::Mutex<Auth>,
    /// 主站（`api.bilibili.com`）的 base，nav 走它。
    ///
    /// 做成字段纯粹是为了测试：假服务器只能顶掉一个 base，
    /// 而发送链路要先打 nav 拿 WBI 种子 —— 写死常量的话单测就会打真网络。
    main_base: String,
    nav: Mutex<Option<Nav>>,
}

impl BiliClient {
    pub fn new(cookie: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .default_headers(browser_headers())
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            http,
            auth: Mutex::new(Auth::from_cookie(cookie)),
            main_base: MAIN_BASE.to_string(),
            nav: Mutex::new(None),
        })
    }

    /// 让 nav 也走别的 base（只给测试用）。
    #[cfg(test)]
    pub fn with_main_base(mut self, base: &str) -> Self {
        self.main_base = base.to_string();
        self
    }

    /// 主站 base。图床上传（`api.bilibili.com/x/upload/web/image`）跟 nav 同一个域，
    /// 走的是同一个字段 —— 测试里 `with_main_base` 把它顶成假服务器，那时上传也得跟着走假的，
    /// 所以别在图床那儿写死 `MAIN_BASE`。
    pub fn main_base(&self) -> &str {
        &self.main_base
    }

    /// 登录令牌。没登录（或配置里那串 Cookie 不全）时是 `None`，
    /// 发送端据此在**发请求之前**就说清楚缺什么，而不是等一个含糊的 `-101`。
    ///
    /// 取出来的是拷贝而不是引用：登录成功后 cookie 会换，引用会指到旧的。
    pub async fn csrf(&self) -> Option<String> {
        self.auth.lock().await.csrf.clone()
    }

    /// 配置里那套凭据**看起来**齐不齐（`SESSDATA` + `bili_jct` 都在）。
    /// 真灵不灵验交给 `nav` 判断 —— 跟 Go 版一样分两步，不然每次启动都要多打一次接口。
    pub async fn logged_in(&self) -> bool {
        let auth = self.auth.lock().await;
        cookie_value(&auth.raw, "SESSDATA").is_some() && auth.csrf.is_some()
    }

    /// 换上一套新凭据（扫码登录成功之后）。
    ///
    /// 顺手把 nav 缓存清掉：那份缓存里带着上一个身份的 uid 和 WBI 种子，
    /// 不清的话下一次 nav 会直接命中缓存，界面上还是旧账号，
    /// 弹幕认证包里那个 uid 也一直是旧的。
    pub async fn set_cookie(&self, raw: &str) {
        *self.auth.lock().await = Auth::from_cookie(raw);
        *self.nav.lock().await = None;
    }

    /// 发一次 GET，返回**整个**响应体（不判 code）。
    ///
    /// 有的接口 `code != 0` 也照样给数据 —— nav 没登录时返 `-101`，但 `wbi_img` 是齐的，
    /// 拿它当失败就永远签不了名。
    pub async fn get_value(&self, url: &str) -> Result<Value> {
        Ok(self.get_json_and_cookies(url).await?.0)
    }

    /// 发一次 GET，把 JSON 和响应里的 `Set-Cookie` **一起**带回来。
    ///
    /// 扫码登录的凭据就藏在 `Set-Cookie` 里，`get_value` 只看 body 会把它整个丢掉。
    /// 也别指望 reqwest 的 cookie_store：那跟我们自己管的那条 Cookie 头是两套账，
    /// 两边都会往请求里塞，风控看到两份 `SESSDATA` 只会更可疑。
    pub async fn get_json_and_cookies(&self, url: &str) -> Result<(Value, Vec<String>)> {
        let mut req = self.http.get(url);
        if let Some(cookie) = self.auth.lock().await.header.clone() {
            req = req.header(reqwest::header::COOKIE, cookie);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| anyhow!("请求 {} 失败: {e}", short(url)))?;
        let status = resp.status();
        // 头得在 `text()` 之前收走 —— 那个方法会把整个响应吃掉。
        let cookies: Vec<String> = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(str::to_string)
            .collect();
        let text = resp
            .text()
            .await
            .map_err(|e| anyhow!("读 {} 的响应失败: {e}", short(url)))?;
        let v = serde_json::from_str(&text).map_err(|_| {
            anyhow!(
                "{} 返回的不是 JSON (HTTP {status}): {}",
                short(url),
                truncate(&text, 200)
            )
        })?;
        Ok((v, cookies))
    }

    /// 发一次 GET 并拆掉 `{code,message,data}` 外壳，`code != 0` 一律转成错误。
    pub async fn get_api(&self, url: &str) -> Result<Value> {
        let v = self.get_value(url).await?;
        unwrap(url, v)
    }

    /// POST 表单，返回**整个**响应体（不判 code）。
    ///
    /// 写操作要自己看 `code`：发弹幕失败时接口给的那句 `message`
    /// （「发送过于频繁」之类）是唯一能告诉用户「为什么没发出去」的东西，
    /// 先被 `unwrap` 折成一句「返回 10030」就白丢了。
    ///
    /// 表单自己拼而不走 reqwest 的 `.form()`：那个方法要额外开 `form` feature，
    /// 而这里只有几个字段。`byte_serialize` 按 x-www-form-urlencoded 规则来
    /// （空格变 `+`、其余百分号编码），跟 Go 的 `url.Values.Encode()` 一致。
    pub async fn post_form_raw(&self, url: &str, form: &[(&str, String)]) -> Result<Value> {
        let body: String = form
            .iter()
            .map(|(k, v)| {
                let mut ser = url::form_urlencoded::Serializer::new(String::new());
                ser.append_pair(k, v);
                ser.finish()
            })
            .collect::<Vec<_>>()
            .join("&");
        self.post_body(url, &body).await
    }

    /// POST 一段**已经拼好**的表单体，返回整个响应体（不判 code）。
    ///
    /// 开播那两个接口的签名是对着拼好的那一串算的（`api::appsign` 用 Go 的 Encode 口径：
    /// `~` 不转义、`*` 要转义），交给 `post_form_raw` 再编码一遍签名就对不上了 ——
    /// 它走的是 `url` crate 的 WHATWG 口径，正好在那两个字符上相反。
    /// 所以签名那条链必须把最终的串原样送出去。
    pub async fn post_body(&self, url: &str, body: &str) -> Result<Value> {
        let mut req = self
            .http
            .post(url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded; charset=UTF-8",
            )
            .body(body.to_string());
        if let Some(cookie) = self.auth.lock().await.header.clone() {
            req = req.header(reqwest::header::COOKIE, cookie);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| anyhow!("请求 {} 失败: {e}", short(url)))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| anyhow!("读 {} 的响应失败: {e}", short(url)))?;
        let v: Value = serde_json::from_str(&text).map_err(|_| {
            anyhow!(
                "{} 返回的不是 JSON (HTTP {status}): {}",
                short(url),
                truncate(&text, 200)
            )
        })?;
        Ok(v)
    }

    /// POST 表单并拆掉 `{code,message,data}` 外壳，`code != 0` 一律转成错误。
    /// 改标题 / 换封面走这条（那两句 message 本来就是给用户看的，折进错误里正合适）；
    /// 发弹幕要看服务端原话，所以那条走 `post_form_raw`。
    pub async fn post_form(&self, url: &str, form: &[(&str, String)]) -> Result<Value> {
        let v = self.post_form_raw(url, form).await?;
        unwrap(url, v)
    }

    /// POST 一个 multipart 表单（传图床用），拆掉 `{code,message,data}` 外壳。
    ///
    /// 边界和 content-type 交给 `reqwest::multipart`：手拼 boundary 迟早会撞上
    /// 「文件内容里正好出现那一行」这种鬼事。cookie 还是自己挂（跟别处一个口径）。
    pub async fn post_multipart(
        &self,
        url: &str,
        form: reqwest::multipart::Form,
    ) -> Result<Value> {
        let mut req = self.http.post(url).multipart(form);
        if let Some(cookie) = self.auth.lock().await.header.clone() {
            req = req.header(reqwest::header::COOKIE, cookie);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| anyhow!("请求 {} 失败: {e}", short(url)))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| anyhow!("读 {} 的响应失败: {e}", short(url)))?;
        let v: Value = serde_json::from_str(&text).map_err(|_| {
            anyhow!(
                "{} 返回的不是 JSON (HTTP {status}): {}",
                short(url),
                truncate(&text, 200)
            )
        })?;
        unwrap(url, v)
    }

    /// 拉一段二进制（封面预览那张图）。**不带 cookie**：图在 `hdslb.com` 上，
    /// 那是另一个域，登录凭据不该跟着跑到图床的日志里去。
    ///
    /// 大小有上限：封面图再大也不该有 8M，没上限的话一张「8K 原图」能把内存吃掉一半。
    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        /// 跟 Go 版 `cover.fetchLimit` 一致。
        const MAX_BYTES: usize = 8 << 20;
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| anyhow!("请求 {} 失败: {e}", short(url)))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("{} 返回 HTTP {status}", short(url));
        }
        if let Some(len) = resp.content_length()
            && len > MAX_BYTES as u64
        {
            bail!("{} 说有 {len} 字节，太大了不抓", short(url));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| anyhow!("读 {} 的响应失败: {e}", short(url)))?;
        if bytes.len() > MAX_BYTES {
            bail!("{} 超过了 8M，不抓", short(url));
        }
        Ok(bytes.to_vec())
    }

    /// nav：一次拿 uid 和 WBI 种子。缓存见 `NAV_TTL`。
    pub async fn nav(&self) -> Result<Nav> {
        if let Some(n) = self.nav.lock().await.as_ref()
            && n.at.elapsed() < NAV_TTL
        {
            return Ok(n.clone());
        }

        let body = self
            .get_value(&format!("{}/x/web-interface/nav", self.main_base))
            .await?;
        let img = body["data"]["wbi_img"]["img_url"].as_str().unwrap_or("");
        let sub = body["data"]["wbi_img"]["sub_url"].as_str().unwrap_or("");
        if img.len() < 32 || sub.len() < 32 {
            bail!("nav 没返回可用的 wbi_img，签不了名");
        }

        let nav = Nav {
            mid: int_of(&body["data"]["mid"]),
            uname: str_of(&body["data"]["uname"]),
            is_login: body["data"]["isLogin"].as_bool().unwrap_or(false),
            mixin_key: wbi::mixin_key(&wbi::key_from_url(img), &wbi::key_from_url(sub)),
            at: Instant::now(),
        };
        *self.nav.lock().await = Some(nav.clone());
        Ok(nav)
    }
}

/// 拆 `{code,message,data}` 外壳。
fn unwrap(url: &str, v: Value) -> Result<Value> {
    let code = v["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        let msg = v["message"]
            .as_str()
            .or_else(|| v["msg"].as_str())
            .unwrap_or("没有消息");
        bail!("{} 返回 {code}: {msg}", short(url));
    }
    Ok(v["data"].clone())
}

/// 把配置里那串 Cookie 变成能安全塞进请求头的值。
///
/// 先掐掉控制字符（尤其是 `\r\n` —— 留着就是请求头注入，而且 reqwest 自己会先 panic），
/// 再交给 `HeaderValue` 判定：非 ASCII 一律不合格，直接放弃整条 Cookie。
fn sanitize_cookie(raw: &str) -> Option<HeaderValue> {
    let cleaned: String = raw.trim().chars().filter(|c| !c.is_control()).collect();
    if cleaned.is_empty() {
        return None;
    }
    HeaderValue::from_str(&cleaned).ok()
}

/// 从 Cookie 串里抠一个键的值（发弹幕要 `bili_jct`）。
///
/// 先过一遍控制字符过滤：用户从浏览器 DevTools 里抄 Cookie 时经常连换行一起复制进来，
/// 这个值会被塞进**请求体**（`csrf=`），带着 `\r` 发出去服务端会认不出来，
/// 报的还是一句含糊的「csrf 校验失败」。
pub fn cookie_value(cookie: &str, name: &str) -> Option<String> {
    let cleaned: String = cookie.chars().filter(|c| !c.is_control()).collect();
    cleaned
        .split(';')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| k.trim() == name)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 接口有时把数字给成字符串（老接口尤其爱这么干），两种都收。
pub fn int_of(v: &Value) -> i64 {
    if let Some(i) = v.as_i64() {
        return i;
    }
    v.as_str().unwrap_or("").trim().parse().unwrap_or(0)
}

pub fn str_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn truncate(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if t.len() < s.len() { format!("{t}…") } else { t }
}

/// 报错里只留路径：带 query 的完整 URL 又长又会把 w_rid 之类的东西糊到屏幕上。
fn short(url: &str) -> String {
    let no_query = url.split('?').next().unwrap_or(url);
    match no_query.find("://") {
        Some(i) => no_query[i + 3..].split_once('/').map_or_else(
            || no_query.to_string(),
            |(_, path)| format!("/{path}"),
        ),
        None => no_query.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_http;

    #[test]
    fn int_of_accepts_both_shapes() {
        assert_eq!(int_of(&serde_json::json!(42)), 42);
        assert_eq!(int_of(&serde_json::json!("42")), 42);
        assert_eq!(int_of(&serde_json::json!("")), 0);
        assert_eq!(int_of(&serde_json::json!(null)), 0);
    }

    #[test]
    fn empty_cookie_is_dropped() {
        assert!(sanitize_cookie("").is_none());
        assert!(sanitize_cookie("   ").is_none());
        assert!(sanitize_cookie("SESSDATA=a; bili_jct=b").is_some());
    }

    /// 发弹幕的 csrf 只能来自 cookie 里的 bili_jct：取不到就得当场说「没登录」，
    /// 不能拿空串去发（服务端只会回一句含糊的 -111）。
    #[tokio::test]
    async fn csrf_comes_from_bili_jct() {
        let c = BiliClient::new("SESSDATA=abc; bili_jct=tok123; DedeUserID=7").unwrap();
        assert_eq!(c.csrf().await.as_deref(), Some("tok123"));

        // 名字对不上 / 没有这个键 / 值是空的，一律算没登录
        assert!(BiliClient::new("SESSDATA=abc").unwrap().csrf().await.is_none());
        assert!(BiliClient::new("bili_jct2=nope").unwrap().csrf().await.is_none());
        assert!(BiliClient::new("bili_jct=").unwrap().csrf().await.is_none());
        assert!(BiliClient::new("").unwrap().csrf().await.is_none());
    }

    /// 登录成功后要能换掉手上的凭据：client 被好几条链路 Arc 共享着，整只换不了。
    /// 换完 nav 缓存也得跟着失效，不然下次 nav 直接命中旧账号的缓存。
    #[tokio::test]
    async fn set_cookie_swaps_credentials_and_drops_the_nav_cache() {
        // 第一次问（还没登录）回 isLogin:false，之后回真账号 ——
        // 这样「缓存被清掉、真的重新问了一次」才验得出来。
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let srv = test_http::start(move |_| {
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = format!(
                r#"{{"code":0,"message":"0","data":{{"mid":{},"isLogin":{},"uname":"{}",
                    "wbi_img":{{"img_url":"https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png",
                               "sub_url":"https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"}}}}}}"#,
                if n == 0 { 0 } else { 7 },
                n != 0,
                if n == 0 { "" } else { "小明" },
            );
            (200, body)
        })
        .await;

        let c = BiliClient::new("").unwrap().with_main_base(&srv.base);
        assert!(!c.logged_in().await, "空 Cookie 不算登录");
        assert!(c.csrf().await.is_none());

        // 先让 nav 缓存里存一份「没登录」的结果
        assert!(!c.nav().await.unwrap().is_login);

        c.set_cookie("SESSDATA=s; bili_jct=tok; DedeUserID=7").await;
        assert!(c.logged_in().await);
        assert_eq!(c.csrf().await.as_deref(), Some("tok"));

        // 缓存没被清掉的话这里会拿到上面那份「没登录」，界面上就还是旧账号
        let nav = c.nav().await.unwrap();
        assert!(nav.is_login);
        assert_eq!(nav.uname, "小明");
        assert_eq!(nav.mid, 7);
        assert_eq!(srv.hits().len(), 2, "换凭据之后 nav 该重新问一次");
    }

    /// 值里混进换行时要清掉：它会被拼进请求体，带 `\r` 发出去服务端认不出来。
    #[test]
    fn cookie_value_strips_control_chars() {
        assert_eq!(cookie_value("a=1; bili_jct=x\r\ny; c=2", "bili_jct").as_deref(), Some("xy"));
        assert_eq!(cookie_value("  bili_jct = spaced  ", "bili_jct").as_deref(), Some("spaced"));
    }

    /// 配置里的 Cookie 脏了只能退化成「没登录」，绝不能让整个 TUI 崩掉：
    /// 带换行的头值会让 `reqwest` 的 `.header()` 直接 panic（它内部是 expect），
    /// 而老 Go 版自动生成的默认值干脆是一句中文。
    #[tokio::test]
    async fn weird_cookie_does_not_take_the_process_down() {
        let srv = test_http::start(|_| {
            (
                200,
                r#"{"code":0,"message":"0","data":{}}"#.to_string(),
            )
        })
        .await;

        let messy = BiliClient::new("从你BILIBILI的请求里抓一个Cookie").unwrap();
        messy
            .get_api(&format!("{}/x", srv.base))
            .await
            .expect("非 ASCII 的 Cookie 也不能让请求炸掉");

        let injected = BiliClient::new("SESSDATA=a\r\nX-Evil: 1").unwrap();
        injected
            .get_api(&format!("{}/x", srv.base))
            .await
            .expect("带换行的 Cookie 也要能发出去");

        let hits = srv.hits();
        assert!(
            !hits[1].header("cookie").unwrap().contains('\n'),
            "换行必须被掐掉，否则就是请求头注入"
        );
    }

    #[test]
    fn short_strips_query_and_host() {
        assert_eq!(
            short("https://api.live.bilibili.com/room/v1/Room/get_info?room_id=1"),
            "/room/v1/Room/get_info"
        );
    }

    /// 本轮没有写操作，但下一轮的改标题 / 发弹幕全靠这个封装，
    /// 顺手用假服务器把「请求方法 + Content-Type + 表单编码」钉住。
    #[tokio::test]
    async fn post_form_sends_urlencoded_body() {
        let srv = test_http::start(|_| {
            (
                200,
                r#"{"code":0,"message":"0","data":{"ok":1}}"#.to_string(),
            )
        })
        .await;
        let client = BiliClient::new("SESSDATA=abc").unwrap();
        let out = client
            .post_form(
                &format!("{}/x/test", srv.base),
                &[
                    ("title", "你好 世界".to_string()),
                    ("csrf", "tok".to_string()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(out["ok"], 1);

        let r = &srv.hits()[0];
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/x/test");
        assert!(r.header("content-type").unwrap().contains("x-www-form-urlencoded"));
        assert!(r.header("cookie").unwrap().contains("SESSDATA=abc"));
        // x-www-form-urlencoded：空格是 +，中文是 UTF-8 百分号编码
        assert!(
            r.body.contains("title=%E4%BD%A0%E5%A5%BD+%E4%B8%96%E7%95%8C"),
            "{}",
            r.body
        );
    }
}
