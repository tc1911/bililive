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
    pub mixin_key: String,
    at: Instant,
}

pub struct BiliClient {
    http: reqwest::Client,
    /// 预先校验过的 Cookie 头值。
    ///
    /// 配置里那串是用户自己抄的，可能带换行或者根本不是 ASCII —— 老 Go 版生成的默认值
    /// 就是一句中文。直接把这种东西交给 `reqwest` 的 `.header()` 会 **panic**，
    /// 而 TUI 一 panic 就是整屏消失、用户什么都看不到。所以这里先过滤 + 校验，
    /// 不合法就干脆不带 Cookie（退化成未登录，报错也只是几行系统弹幕）。
    cookie: Option<HeaderValue>,
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
            cookie: sanitize_cookie(cookie),
            nav: Mutex::new(None),
        })
    }


    /// 发一次 GET，返回**整个**响应体（不判 code）。
    ///
    /// 有的接口 `code != 0` 也照样给数据 —— nav 没登录时返 `-101`，但 `wbi_img` 是齐的，
    /// 拿它当失败就永远签不了名。
    pub async fn get_value(&self, url: &str) -> Result<Value> {
        let mut req = self.http.get(url);
        if let Some(cookie) = &self.cookie {
            req = req.header(reqwest::header::COOKIE, cookie.clone());
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
        serde_json::from_str(&text).map_err(|_| {
            anyhow!(
                "{} 返回的不是 JSON (HTTP {status}): {}",
                short(url),
                truncate(&text, 200)
            )
        })
    }

    /// 发一次 GET 并拆掉 `{code,message,data}` 外壳，`code != 0` 一律转成错误。
    pub async fn get_api(&self, url: &str) -> Result<Value> {
        let v = self.get_value(url).await?;
        unwrap(url, v)
    }

    /// POST 表单。本轮全是「读」，没有任何地方调用它 —— 留着给下一轮的
    /// 改标题 / 发弹幕用，接口形状和 Go 版一致（含 app 签名的 `sign` 字段）。
    ///
    /// 表单自己拼而不走 reqwest 的 `.form()`：那个方法要额外开 `form` feature，
    /// 而这里只有几个字段。`byte_serialize` 按 x-www-form-urlencoded 规则来
    /// （空格变 `+`、其余百分号编码），跟 Go 的 `url.Values.Encode()` 一致。
    #[allow(dead_code)]
    pub async fn post_form(&self, url: &str, form: &[(&str, String)]) -> Result<Value> {
        let body: String = form
            .iter()
            .map(|(k, v)| {
                let mut ser = url::form_urlencoded::Serializer::new(String::new());
                ser.append_pair(k, v);
                ser.finish()
            })
            .collect::<Vec<_>>()
            .join("&");
        let mut req = self
            .http
            .post(url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded; charset=UTF-8",
            )
            .body(body);
        if let Some(cookie) = &self.cookie {
            req = req.header(reqwest::header::COOKIE, cookie.clone());
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

    /// nav：一次拿 uid 和 WBI 种子。缓存见 `NAV_TTL`。
    pub async fn nav(&self) -> Result<Nav> {
        if let Some(n) = self.nav.lock().await.as_ref()
            && n.at.elapsed() < NAV_TTL
        {
            return Ok(n.clone());
        }

        let body = self
            .get_value(&format!("{MAIN_BASE}/x/web-interface/nav"))
            .await?;
        let img = body["data"]["wbi_img"]["img_url"].as_str().unwrap_or("");
        let sub = body["data"]["wbi_img"]["sub_url"].as_str().unwrap_or("");
        if img.len() < 32 || sub.len() < 32 {
            bail!("nav 没返回可用的 wbi_img，签不了名");
        }

        let nav = Nav {
            mid: int_of(&body["data"]["mid"]),
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
