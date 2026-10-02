//! 发弹幕：`POST /msg/send`（URL 带 WBI 签名）+ 长文本切段。
//!
//! 这是这个项目第一条**写**链路。规矩跟读链路一样 —— 任何失败都只变成一条系统弹幕：
//! 发不出去是小事，整屏消失才是大事。

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::sleep;

use crate::api::client::BiliClient;
use crate::api::danmaku::DanmuMsg;
use crate::api::wbi;

/// 单条弹幕的字符数上限：B 站这边是 20 个字，超了整条会被拒（不是自动截断）。
pub const MAX_DANMAKU_CHARS: usize = 20;

/// 切段之间的间隔。Go 版 `sender/sender.go` 就是 1 秒 —— 连着发会被判成刷屏，
/// 接口直接回「发送过于频繁」，后半截全白扔。
pub const SEGMENT_GAP: Duration = Duration::from_secs(1);

/// `/msg/send` 在 `api.live.bilibili.com` 下。
pub const SEND_PATH: &str = "/msg/send";

/// 白色。网页端默认就是它（16777215 = 0xFFFFFF）；别的颜色要粉丝牌等级，先不碰。
const COLOR_WHITE: &str = "16777215";
const FONTSIZE: &str = "25";

/// 网页端发弹幕时挂在 URL 上的定位参数。签名只签这一个参数 ——
/// 服务端是拿 URL 上这批参数重算 w_rid 的，多塞一个就会对不上。
const WEB_LOCATION: &str = "444.8";

/// 把长文本切成不超过 `limit` 个**字符**的段。
///
/// 按字符切，不按字节切：中文一个字 3 字节，按字节切会把「你」劈成半个，
/// 发出去是乱码，风控还会记一笔。空串切出来是空的（没有要发的），
/// `limit == 0` 也不能 panic —— `chunks(0)` 自己会 panic，TUI 就没了。
pub fn split_segments(text: &str, limit: usize) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    text.chars()
        .collect::<Vec<char>>()
        .chunks(limit)
        .map(|c| c.iter().collect())
        .collect()
}

/// `rnd` 用毫秒时间戳。它就是给服务端做去重的随机数，每次请求都不一样就行。
fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 发一条（已经切好的）弹幕。
///
/// `mixin_key` / `wts` / `rnd` 由调用方给：测试才能把签名和时间戳钉死，
/// 线上那条路（`send_danmaku`）自己从 nav 现取。
pub async fn send_one(
    client: &BiliClient,
    base: &str,
    room_id: i64,
    text: &str,
    mixin_key: &str,
    wts: i64,
    rnd: i64,
) -> Result<()> {
    // 没登录就没 csrf，接口只会回一个含糊的 -101/-111；在这儿就把话说白，
    // 用户至少知道该去配置里补什么。
    let csrf = client
        .csrf()
        .await
        .context(
            "Cookie 里没有 bili_jct（发弹幕拿它当 csrf），把登录后的完整 Cookie 补进 config.toml",
        )?;

    let query = wbi::sign(&[("web_location", WEB_LOCATION.to_string())], mixin_key, wts);
    let url = format!("{base}{SEND_PATH}?{query}");
    let form = [
        ("msg", text.to_string()),
        ("color", COLOR_WHITE.to_string()),
        ("fontsize", FONTSIZE.to_string()),
        ("rnd", rnd.to_string()),
        ("roomid", room_id.to_string()),
        // 同一个值要发两遍：老接口读 csrf、新接口读 csrf_token，只给一个总有半边不认。
        ("csrf", csrf.clone()),
        ("csrf_token", csrf),
    ];

    let v = client.post_form_raw(&url, &form).await?;
    let code = v["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        // 这里的 message（「发送过于频繁」「被禁言」…）是唯一能解释原因的东西，
        // 别的字段都可能没有，所以原样带出来，只把 code 补在后面。
        let msg = v["message"]
            .as_str()
            .or_else(|| v["msg"].as_str())
            .unwrap_or("接口没给原因");
        bail!("发送失败：{msg}（code {code}）");
    }
    Ok(())
}

/// 发一条弹幕：先从 nav 拿 WBI 种子（有缓存），再走 `send_one`。
pub async fn send_danmaku(client: &BiliClient, base: &str, room_id: i64, text: &str) -> Result<()> {
    let nav = client.nav().await.context("取 nav（WBI 种子）失败")?;
    send_one(
        client,
        base,
        room_id,
        text,
        &nav.mixin_key,
        crate::timefmt::now_epoch(),
        now_millis(),
    )
    .await
}

/// 界面 -> 发送端：收到一条就发一条，超长的切段连发，段间留 1 秒。
///
/// 发送是**另一个任务**，不是界面的：段间的 1 秒 sleep 放在事件循环里就是把 TUI 冻住，
/// 而这里 sleep 的时候界面照常收弹幕、照常按键。
pub async fn send_loop(
    client: Arc<BiliClient>,
    base: String,
    room_id: i64,
    rx: Receiver<String>,
    tx: Sender<DanmuMsg>,
) {
    pump(client, base, room_id, rx, tx, SEGMENT_GAP).await;
}

/// 段间间隔做成参数只为了测试能传 0：跑一次单测不该真的等一秒。
async fn pump(
    client: Arc<BiliClient>,
    base: String,
    room_id: i64,
    mut rx: Receiver<String>,
    tx: Sender<DanmuMsg>,
    gap: Duration,
) {
    while let Some(text) = rx.recv().await {
        let segments = split_segments(&text, MAX_DANMAKU_CHARS);
        let last = segments.len().saturating_sub(1);
        for (i, seg) in segments.iter().enumerate() {
            // 一段失败不影响后面的段：Go 版也是发完剩下的再报。
            if let Err(e) = send_danmaku(&client, &base, room_id, seg).await
                && tx.send(DanmuMsg::system(e.to_string())).await.is_err()
            {
                return; // 界面没了
            }
            if i < last {
                sleep(gap).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_http;
    use std::collections::HashMap;

    const MIXIN: &str = "ea1db124af3c7062474693fa704f4ff8";
    const WTS: i64 = 1_700_000_000;
    const RND: i64 = 1_700_000_000_123;

    fn form_of(body: &str) -> HashMap<String, String> {
        url::form_urlencoded::parse(body.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    #[test]
    fn split_segments_handles_boundaries() {
        // 空串：没有要发的（界面那边也会挡住，这里是第二道保险）
        assert!(split_segments("", 20).is_empty());
        // 正好 20 个字：不能切成两段（一段是空的那种切法会让接口收到一条空弹幕）
        assert_eq!(split_segments(&"字".repeat(20), 20), vec!["字".repeat(20)]);
        // 21 个字：20 + 1，尾巴那段不能丢
        assert_eq!(split_segments(&"字".repeat(21), 20), vec!["字".repeat(20), "字".to_string()]);
        // 纯中文按字数切：一段最多 20 个字符，不是 20 字节
        let segs = split_segments(&"好".repeat(41), 20);
        assert_eq!(segs.len(), 3);
        assert!(segs.iter().all(|s| s.chars().count() <= 20));
        assert_eq!(segs.iter().map(String::len).sum::<usize>(), "好".repeat(41).len());
        // 中英混排也别按字节劈（劈开会变成无效 UTF-8，压根构造不出 String）
        assert_eq!(split_segments("abc中文def", 4), vec!["abc中", "文def"]);
        // limit 是 0 时不能 panic（chunks(0) 会）
        assert!(split_segments("随便", 0).is_empty());
    }

    /// 请求形状：路径、WBI 签名的参数、表单字段、cookie 头，一样都不能少。
    #[tokio::test]
    async fn send_request_shape_is_exactly_what_bilibili_expects() {
        let srv = test_http::start(|_| {
            (200, r#"{"code":0,"message":"0","data":{}}"#.to_string())
        })
        .await;
        let client = BiliClient::new("SESSDATA=abc; bili_jct=tok123").unwrap();
        send_one(&client, &srv.base, 9527, "你好世界", MIXIN, WTS, RND)
            .await
            .unwrap();

        let hits = srv.hits();
        assert_eq!(hits.len(), 1, "一条弹幕只发一个请求");
        let r = &hits[0];
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/msg/send");

        // URL 上的签名：签的就只有 web_location 这一个参数，多一个少一个都算错。
        // 直接跟 wbi::sign 的结果逐字节比，顺手把 wts / w_rid 的格式也钉住了。
        let expect_query = wbi::sign(&[("web_location", "444.8".to_string())], MIXIN, WTS);
        assert_eq!(r.query, expect_query, "URL 上的签名参数变了");
        assert_eq!(r.query_param("web_location"), Some("444.8"));
        assert_eq!(r.query_param("wts"), Some(&WTS.to_string() as &str));
        assert_eq!(r.query_param("w_rid").unwrap().len(), 32, "w_rid 是 md5 十六进制");
        assert_eq!(r.query.split('&').count(), 3, "只许有 web_location / wts / w_rid");

        let form = form_of(&r.body);
        assert_eq!(form.get("msg").map(String::as_str), Some("你好世界"));
        assert_eq!(form.get("roomid").map(String::as_str), Some("9527"));
        assert_eq!(form.get("color").map(String::as_str), Some("16777215"));
        assert_eq!(form.get("fontsize").map(String::as_str), Some("25"));
        assert_eq!(form.get("rnd").map(String::as_str), Some(&RND.to_string() as &str));
        // 两个字段同一个值：只填一个的写法在老接口上会失败
        assert_eq!(form.get("csrf").map(String::as_str), Some("tok123"));
        assert_eq!(form.get("csrf_token").map(String::as_str), Some("tok123"));

        assert!(r.header("content-type").unwrap().contains("x-www-form-urlencoded"));
        assert!(r.header("cookie").unwrap().contains("bili_jct=tok123"));
        // 风控认这套头，缺一个就可能返 -352
        assert!(r.header("user-agent").unwrap().contains("Mozilla"));
        assert_eq!(r.header("origin"), Some("https://live.bilibili.com"));
    }

    /// `code != 0` 一律当失败，而且要把接口那句 message 原样带给界面
    /// （用户看到的应该是「发送过于频繁」，不是「返回 10030」）。
    #[tokio::test]
    async fn api_error_carries_the_server_message() {
        let srv = test_http::start(|_| {
            (200, r#"{"code":10030,"message":"发送过于频繁"}"#.to_string())
        })
        .await;
        let client = BiliClient::new("bili_jct=tok").unwrap();
        let err = send_one(&client, &srv.base, 1, "在", MIXIN, WTS, RND)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("发送过于频繁"), "{err}");
        assert!(err.contains("10030"), "{err}");
    }

    /// 没登录时必须**在发请求之前**就报出来：请求已经发出去的话，
    /// 服务端回的那句错既不准确（可能是风控）也没意义，还给账号白记一笔。
    #[tokio::test]
    async fn missing_bili_jct_fails_before_the_request() {
        let srv = test_http::start(|_| (200, r#"{"code":0,"message":"0"}"#.to_string())).await;
        let client = BiliClient::new("SESSDATA=abc").unwrap();
        let err = send_one(&client, &srv.base, 1, "在", MIXIN, WTS, RND)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("bili_jct"), "{err}");
        assert!(srv.hits().is_empty(), "缺 csrf 时一个请求都不该发出去");
    }

    /// 长文本在发送端切段连发：21 个字要出两个请求，先前 20 个字、再剩下的那一个，
    /// 段间还得真的停一下（不留间隔就是刷屏，接口会把后半截全判失败）。
    #[tokio::test]
    async fn long_text_is_sent_in_segments_with_a_gap() {
        let srv = test_http::start(|r| {
            if r.path.ends_with("web-interface/nav") {
                return (200, nav_body());
            }
            (200, r#"{"code":0,"message":"0","data":{}}"#.to_string())
        })
        .await;
        // nav 也指到假服务器：不然这里一按回车就是一次真网络请求，
        // 单测必须纯离线（断网也要能跑）。
        let client = Arc::new(
            BiliClient::new("SESSDATA=abc; bili_jct=tok123")
                .unwrap()
                .with_main_base(&srv.base),
        );
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let (danmu_tx, mut danmu_rx) = tokio::sync::mpsc::channel(4);
        let gap = Duration::from_millis(50);
        let task = tokio::spawn({
            let client = client.clone();
            let base = srv.base.clone();
            async move { pump(client, base, 9527, rx, danmu_tx, gap).await }
        });

        let started = std::time::Instant::now();
        tx.send("字".repeat(21)).await.unwrap();

        // 发成功时发送端什么都不塞（那一份会从弹幕 websocket 回显回来），
        // 所以这里只能等请求本身到齐。
        let mut sent: Vec<String> = Vec::new();
        while sent.len() < 2 {
            sent = srv
                .hits()
                .iter()
                .filter(|h| h.path == SEND_PATH)
                .map(|h| form_of(&h.body).remove("msg").unwrap())
                .collect();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            sent,
            vec!["字".repeat(20), "字".to_string()],
            "先 20 个字，再剩下的 1 个字"
        );
        assert!(started.elapsed() >= gap, "段间要真的留出间隔");
        assert!(danmu_rx.try_recv().is_err(), "发成功时不该自己塞系统弹幕");

        task.abort();
    }

    /// 一段失败（被禁言、发送过于频繁）不能 panic、不能吞掉后面的段，
    /// 只能变成一条系统弹幕挂到弹幕框里。
    #[tokio::test]
    async fn a_failed_segment_becomes_a_system_danmaku() {
        let srv = test_http::start(|r| {
            if r.path.ends_with("web-interface/nav") {
                return (200, nav_body());
            }
            (200, r#"{"code":10030,"message":"发送过于频繁"}"#.to_string())
        })
        .await;
        let client = Arc::new(
            BiliClient::new("bili_jct=tok")
                .unwrap()
                .with_main_base(&srv.base),
        );
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let (danmu_tx, mut danmu_rx) = tokio::sync::mpsc::channel(4);
        let task = tokio::spawn({
            let client = client.clone();
            let base = srv.base.clone();
            async move { pump(client, base, 9527, rx, danmu_tx, Duration::ZERO).await }
        });

        tx.send("在".to_string()).await.unwrap();
        let m = danmu_rx.recv().await.expect("失败必须变成一条系统弹幕");
        assert!(m.is_system());
        assert!(m.content.contains("发送过于频繁"), "{}", m.content);
        assert!(srv.hits().iter().any(|h| h.path == SEND_PATH), "请求该发出去过");

        task.abort();
    }

    /// nav 的响应。两个 key 是 nav 公开下发的种子（每天轮换），不是凭据，
    /// 只要够长就能签发 —— 这里用的是 `wbi.rs` 里那对黄金值，签名可以逐字节对。
    fn nav_body() -> String {
        r#"{"code":0,"message":"0","data":{"mid":7,"wbi_img":{
            "img_url":"https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png",
            "sub_url":"https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"}}}"#
            .to_string()
    }
}
