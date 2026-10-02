//! 开播 / 下播 / 推流码。
//!
//! 三个接口两套规矩，别串：
//!   - `xlive/app-blink/v1/liveVersionInfo/getHomePageLiveVersion` 和 `room/v1/Room/startLive`
//!     走 **直播姬那套 app 签名**（`api::appsign`，写死的 appkey/appsec）；
//!   - `room/v1/Room/stopLive` **不签名**，只有 csrf / csrf_token
//!     （Go 版就是不带签名的，顺手补上一个 appkey 反而可能被拒）。
//!
//! 这一层只碰接口：不读配置、不画界面、不判「有没有选分区」（界面先拦一道，
//! 这里只做兜底）。开播是**账号上真正会变**的操作（直播间立刻对外可见、给粉丝推推送），
//! 所以这一层的测试一律假服务器 —— 绝不许真开一次播。

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::api::appsign;
use crate::api::client::{BiliClient, int_of, str_of};

pub const LIVE_VERSION_PATH: &str = "/xlive/app-blink/v1/liveVersionInfo/getHomePageLiveVersion";
pub const START_LIVE_PATH: &str = "/room/v1/Room/startLive";
pub const STOP_LIVE_PATH: &str = "/room/v1/Room/stopLive";

/// 服务端在开播被验证挡住时给的 code。
///
/// `60024` = 需要扫码验证（验证地址在 `data.qr` 里）；
/// `60043` = 需要人脸认证（响应里**没有**二维码，地址得拿 nav 的 mid 自己拼）。
pub const CODE_VERIFY_QR: i64 = 60024;
pub const CODE_FACE_AUTH: i64 = 60043;

/// 一路推流凭据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    /// `rtmp-1` / `rtmp-2` / `srt-1` 这种编号（同协议多路就往后排）
    pub kind: String,
    pub protocol: String,
    pub address: String,
    pub key: String,
    /// 地址 + 密钥拼好的完整串。拼的时候有个怪情况见 `push_stream`。
    pub full_url: String,
}

/// 开播需要哪种验证。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyKind {
    /// 60024：扫码
    Qr,
    /// 60043：人脸认证
    FaceAuth,
}

/// 一次开播请求的结果。
///
/// 「要验证」**不是**普通失败，所以不走 `Err`：它得让界面把地址画成二维码让用户扫，
/// 而 `Err` 那一路的归宿是顶栏一句红字。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    /// 真开播了，这是各路推流凭据
    Started(Vec<Stream>),
    /// 服务端说这次得先验证。`url` 是空串表示没拿到验证地址
    /// （60024 没给 qr、或者 60043 拼不出来），界面得说清楚「再按 F4 试一次」。
    Verify {
        kind: VerifyKind,
        url: String,
        message: String,
    },
}

/// 界面 -> 开播任务。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveRequest {
    /// 进「推流码」栏时先看一眼开播状态（走只读的 `get_info`）
    LoadStatus,
    /// 开播（确认层已经点过「开播」了）。`area_v2` 是配置里那个开播分区。
    Start { area_v2: i64 },
    /// 下播，不用确认
    Stop,
}

/// 是哪一步失败了，只用来决定顶栏那句话的前缀。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveAction {
    Status,
    Start,
    Stop,
}

/// 开播任务 -> 界面。跟别的几条链一样，只动显示状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveEvent {
    /// 状态查回来了：`data.live_status`（0 未开播 / 1 直播中 / 2 轮播）
    Status { live_status: i64 },
    /// 开播成功，附各路推流凭据
    Started(Vec<Stream>),
    /// 开播被验证挡住（60024 / 60043）：界面得画二维码 + 说「扫完再按 F4」
    Verify {
        kind: VerifyKind,
        url: String,
        message: String,
    },
    /// 下播成功
    Stopped,
    /// 哪一步失败了，`message` 是服务端 / 网络的原话
    Failed { action: LiveAction, message: String },
}

/// 毫秒时间戳。这三个接口都拿它当防重放，Go 版也是现取。
fn ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `csrf` / `csrf_token` 都取 cookie 里的 `bili_jct`。
/// 取不到就**在发请求之前**说清楚缺什么（服务端只会回一句含糊的 `-111`）。
async fn csrf(client: &BiliClient) -> Result<String> {
    client.csrf().await.context(
        "Cookie 里没有 bili_jct（开播 / 下播拿它当 csrf），把登录后的完整 Cookie 补进配置",
    )
}

/// 服务端那句话。`message` 和 `msg` 两个字段历史接口都出现过。
fn server_message(v: &Value) -> String {
    let m = str_of(&v["message"]);
    if !m.is_empty() {
        return m;
    }
    let m = str_of(&v["msg"]);
    if !m.is_empty() { m } else { "没有消息".to_string() }
}

/// `{code,message,data}` 外壳；`post_body` 不判 code，写操作得自己看一眼。
fn unwrap_live(v: &Value, path: &str) -> Result<()> {
    let code = int_of(&v["code"]);
    if code != 0 {
        bail!("{path} 返回 {code}: {}", server_message(v));
    }
    Ok(())
}

/// 拉一次开播版本号，返回（`curr_version`，`build`）。
///
/// 这两个值是 `startLive` 的必填字段，缺了服务端会回「版本不对」之类的话。
pub async fn live_version(client: &BiliClient, base: &str) -> Result<(String, i64)> {
    let params = [
        ("system_version", "2".to_string()),
        ("ts", ms().to_string()),
    ];
    let url = format!(
        "{base}{LIVE_VERSION_PATH}?{}",
        appsign::encode_params(&params, true)
    );
    let d = client.get_api(&url).await?;
    let version = str_of(&d["curr_version"]);
    if version.is_empty() {
        // 空版本号发过去只会换一句看不懂的错，不如在这儿说清楚。
        bail!("版本接口没给 curr_version，开不了播");
    }
    Ok((version, int_of(&d["build"])))
}

/// 开播。**这一步会让直播间立刻对外可见、给粉丝推开播推送**。
///
/// `room_id` 要用服务端回的**规范房间号**（`get_info` 的 `data.room_id`），
/// 不是配置里那个短号 —— 上一轮已经证实这俩不是一个数。
pub async fn start_live(
    client: &BiliClient,
    base: &str,
    room_id: i64,
    area_v2: i64,
) -> Result<StartOutcome> {
    if area_v2 <= 0 {
        bail!("还没选开播分区（area_v2 是 0）");
    }
    let csrf = csrf(client).await?;
    let (version, build) = live_version(client, base).await?;

    let params = [
        ("room_id", room_id.to_string()),
        ("platform", "pc_link".to_string()),
        ("backup_stream", "0".to_string()),
        ("csrf", csrf.clone()),
        ("csrf_token", csrf),
        ("area_v2", area_v2.to_string()),
        ("version", version),
        ("build", build.to_string()),
        ("ts", ms().to_string()),
    ];
    let body = appsign::encode_params(&params, true);
    let v = client
        .post_body(&format!("{base}{START_LIVE_PATH}"), &body)
        .await?;

    let code = int_of(&v["code"]);
    if code != 0 {
        return verify_or_error(client, code, &v).await;
    }
    Ok(StartOutcome::Started(assemble_streams(&v["data"])?))
}

/// 开播被拒时的分诊：两种验证各自对应什么，别的 code 就是普通失败。
async fn verify_or_error(client: &BiliClient, code: i64, v: &Value) -> Result<StartOutcome> {
    let kind = match code {
        CODE_VERIFY_QR => VerifyKind::Qr,
        CODE_FACE_AUTH => VerifyKind::FaceAuth,
        other => {
            bail!("{START_LIVE_PATH} 返回 {other}: {}", server_message(v));
        }
    };

    let mut url = str_of(&v["data"]["qr"]);
    if kind == VerifyKind::FaceAuth && url.is_empty() {
        // 人脸认证那一次服务端**不返二维码**，只能拿 nav 里的 mid 自己拼地址。
        // nav 失败不算开播失败（那会让用户以为是开播挂了），所以只当「没拿到地址」。
        if let Ok(nav) = client.nav().await
            && nav.mid > 0
        {
            url = face_auth_url(nav.mid);
        }
    }

    let what = match kind {
        VerifyKind::Qr => "本次开播需要扫码验证",
        VerifyKind::FaceAuth => "本次开播需要人脸认证",
    };
    Ok(StartOutcome::Verify {
        kind,
        url,
        message: format!("{what}：{}", server_message(v)),
    })
}

/// 人脸认证页的地址。服务端不返这个地址，只能自己拼（`source_event=400` 是人脸认证那一种）。
pub fn face_auth_url(mid: i64) -> String {
    format!(
        "https://www.bilibili.com/blackboard/live/face-auth-middle.html?source_event=400&mid={mid}"
    )
}

/// 下播。这个接口**不带 app 签名**（Go 版就是不带签名的）。
pub async fn stop_live(client: &BiliClient, base: &str, room_id: i64) -> Result<()> {
    let csrf = csrf(client).await?;
    let params = [
        ("room_id", room_id.to_string()),
        ("platform", "pc_link".to_string()),
        ("csrf", csrf.clone()),
        ("csrf_token", csrf),
    ];
    let body = appsign::encode_params(&params, false);
    let v = client
        .post_body(&format!("{base}{STOP_LIVE_PATH}"), &body)
        .await?;
    unwrap_live(&v, STOP_LIVE_PATH)
}

/// 从 `startLive` 的 `data` 里拼出各路推流凭据：`rtmp` 那一组 + `protocols` 数组。
pub fn assemble_streams(data: &Value) -> Result<Vec<Stream>> {
    let mut streams = Vec::new();
    push_stream(
        &mut streams,
        "rtmp",
        &str_of(&data["rtmp"]["addr"]),
        &str_of(&data["rtmp"]["code"]),
    );
    if let Some(list) = data["protocols"].as_array() {
        for p in list {
            push_stream(
                &mut streams,
                &str_of(&p["protocol"]),
                &str_of(&p["addr"]),
                &str_of(&p["code"]),
            );
        }
    }
    if streams.is_empty() {
        // 开播成了（服务端说 code 0）却没给地址，等于用户拿不到推流码 —— 必须报错。
        bail!("开播成功但接口没返回推流地址");
    }
    Ok(streams)
}

/// 收一路凭据。缺地址 / 缺密钥的那一路直接不给（Go 版同样跳过）。
fn push_stream(streams: &mut Vec<Stream>, protocol: &str, addr: &str, key: &str) {
    if protocol.is_empty() || addr.is_empty() || key.is_empty() {
        return;
    }
    // 同协议多路就往后编号：rtmp-1、rtmp-2、srt-1。
    let n = streams.iter().filter(|s| s.protocol == protocol).count() + 1;
    // 密钥有时以 `?` 开头（`?streamname=...`），地址有时以 `/` 结尾 ——
    // 这两种再补一个 `/` 就多出一个斜杠，OBS 里表现成「连不上服务器」。
    let full_url = if key.starts_with('?') || addr.ends_with('/') {
        format!("{addr}{key}")
    } else {
        format!("{addr}/{key}")
    };
    streams.push(Stream {
        kind: format!("{protocol}-{n}"),
        protocol: protocol.to_string(),
        address: addr.to_string(),
        key: key.to_string(),
        full_url,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_http::{self, Request};
    use md5::{Digest, Md5};

    const VERSION_BODY: &str =
        r#"{"code":0,"message":"0","data":{"curr_version":"9.9.9","build":12345}}"#;

    fn client() -> BiliClient {
        BiliClient::new("SESSDATA=abc; bili_jct=tok123").unwrap()
    }

    fn form_of(body: &str) -> std::collections::HashMap<String, String> {
        url::form_urlencoded::parse(body.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    /// 照真接口形状编的：rtmp 那组密钥以 `?` 开头，protocols 里第二路地址以 `/` 结尾。
    fn streams_body() -> String {
        r#"{"code":0,"message":"0","data":{
            "rtmp":{"addr":"rtmp://live-push.bilivideo.com/live-bvc","code":"?streamname=abc&key=xyz"},
            "protocols":[
                {"protocol":"rtmp","addr":"rtmp://live-push.bililive.com/live-bvc/","code":"?streamname=def"},
                {"protocol":"srt","addr":"srt://live-push.bilivideo.com:1935","code":"?streamname=ghi"}]}}"#
            .to_string()
    }

    /// 版本号那一次：GET、app 签名（appkey + 32 位 sign）、`system_version=2` + 毫秒时间戳。
    #[tokio::test]
    async fn version_request_is_app_signed() {
        let srv = test_http::start(|_| (200, VERSION_BODY.to_string())).await;
        let (version, build) = live_version(&client(), &srv.base).await.unwrap();
        assert_eq!(version, "9.9.9");
        assert_eq!(build, 12345);

        let r = &srv.hits()[0];
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, LIVE_VERSION_PATH);
        assert_eq!(r.query_param("system_version"), Some("2"));
        assert_eq!(
            r.query_param("appkey"),
            Some(appsign::APP_KEY),
            "这是直播姬那套签名，appkey 必须对上"
        );
        let ts = r.query_param("ts").unwrap();
        assert_eq!(ts.len(), 13, "ts 是毫秒：{ts}");
        assert!(ts.chars().all(|c| c.is_ascii_digit()), "{ts}");
        assert_eq!(r.query_param("sign").map(str::len), Some(32));
    }

    /// 开播那一次：九个字段全在 + appkey + sign，而且 sign 就是对着**发出去的那一串**算的。
    /// 这一条同时钉住「签名和发送用同一个串」—— 编码口径一改（比如换成 url crate 那套）
    /// 这里立刻红。
    #[tokio::test]
    async fn start_live_sends_the_signed_form_and_returns_streams() {
        let srv = test_http::start(|r: &Request| match r.path.as_str() {
            LIVE_VERSION_PATH => (200, VERSION_BODY.to_string()),
            START_LIVE_PATH => (200, streams_body()),
            other => (
                200,
                format!(r#"{{"code":-1,"message":"不认识的路径 {other}"}}"#),
            ),
        })
        .await;

        let out = start_live(&client(), &srv.base, 7734200, 371).await.unwrap();
        let StartOutcome::Started(streams) = out else {
            panic!("这条该真开播：{out:?}");
        };
        assert_eq!(streams.len(), 3);
        assert_eq!(streams[0].kind, "rtmp-1");
        assert_eq!(streams[1].kind, "rtmp-2", "同协议第二路往后编号");
        assert_eq!(streams[2].kind, "srt-1");
        assert_eq!(
            streams[0].full_url,
            "rtmp://live-push.bilivideo.com/live-bvc?streamname=abc&key=xyz"
        );
        assert_eq!(
            streams[1].full_url,
            "rtmp://live-push.bililive.com/live-bvc/?streamname=def",
            "地址以 / 结尾时不能再补一个斜杠"
        );
        assert_eq!(
            streams[2].full_url,
            "srt://live-push.bilivideo.com:1935?streamname=ghi",
            "密钥以 ? 开头时直接接上，不补斜杠"
        );

        let hits = srv.hits();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![LIVE_VERSION_PATH, START_LIVE_PATH],
            "先版本号再开播"
        );

        let r = &hits[1];
        assert_eq!(r.method, "POST");
        assert!(
            r.header("content-type").unwrap().contains("x-www-form-urlencoded"),
            "{:?}",
            r.header("content-type")
        );
        assert!(r.header("cookie").unwrap().contains("SESSDATA=abc"));

        let f = form_of(&r.body);
        assert_eq!(f.get("room_id").map(String::as_str), Some("7734200"));
        assert_eq!(f.get("platform").map(String::as_str), Some("pc_link"));
        assert_eq!(f.get("backup_stream").map(String::as_str), Some("0"));
        assert_eq!(f.get("csrf").map(String::as_str), Some("tok123"));
        assert_eq!(
            f.get("csrf_token").map(String::as_str),
            Some("tok123"),
            "csrf 和 csrf_token 是同一个值"
        );
        assert_eq!(f.get("area_v2").map(String::as_str), Some("371"));
        assert_eq!(f.get("version").map(String::as_str), Some("9.9.9"));
        assert_eq!(f.get("build").map(String::as_str), Some("12345"));
        assert_eq!(f.get("appkey").map(String::as_str), Some(appsign::APP_KEY));
        assert_eq!(f.len(), 11, "九个业务字段 + appkey + sign：{f:?}");

        // sign 必须是 md5(去掉 &sign= 的整串 + appsec)：算法本身由 appsign 的黄金值钉着，
        // 这里验的是「签名算的那一串 == 真发出去的那一串」。
        let (prefix, sig) = r.body.split_once("&sign=").expect("要带签名");
        let mut hasher = Md5::new();
        hasher.update(prefix.as_bytes());
        hasher.update(appsign::APP_SECRET.as_bytes());
        assert_eq!(sig, hex::encode(hasher.finalize()), "{}", r.body);
    }

    /// 下播：四个字段、**没有 appkey / sign**（Go 版就是不带签名的）。
    /// 路径也钉死：别被「顺手统一成直播姬那套」改走。
    #[tokio::test]
    async fn stop_live_sends_no_app_signature() {
        let srv = test_http::start(|_| {
            (
                200,
                r#"{"code":0,"message":"0","data":{}}"#.to_string(),
            )
        })
        .await;
        stop_live(&client(), &srv.base, 7734200).await.unwrap();

        assert_eq!(STOP_LIVE_PATH, "/room/v1/Room/stopLive");
        let r = &srv.hits()[0];
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, STOP_LIVE_PATH);
        let f = form_of(&r.body);
        assert_eq!(f.get("room_id").map(String::as_str), Some("7734200"));
        assert_eq!(f.get("platform").map(String::as_str), Some("pc_link"));
        assert_eq!(f.get("csrf").map(String::as_str), Some("tok123"));
        assert_eq!(f.get("csrf_token").map(String::as_str), Some("tok123"));
        assert_eq!(f.len(), 4, "别顺手多塞字段：{f:?}");
        assert!(!r.body.contains("appkey"), "{}", r.body);
        assert!(!r.body.contains("sign="), "{}", r.body);
    }

    /// 60024：要扫码，二维码地址在 `data.qr` 里。**不能报成普通失败**。
    #[tokio::test]
    async fn verify_60024_carries_the_scan_address() {
        let srv = test_http::start(|r: &Request| match r.path.as_str() {
            LIVE_VERSION_PATH => (200, VERSION_BODY.to_string()),
            START_LIVE_PATH => (
                200,
                r#"{"code":60024,"message":"请扫码验证","data":{"qr":"https://www.bilibili.com/h5/verify?token=abc"}}"#
                    .to_string(),
            ),
            other => (
                200,
                format!(r#"{{"code":-1,"message":"不认识的路径 {other}"}}"#),
            ),
        })
        .await;

        let out = start_live(&client(), &srv.base, 7734200, 371).await.unwrap();
        let StartOutcome::Verify { kind, url, message } = out else {
            panic!("这条该是「要扫码验证」：{out:?}");
        };
        assert_eq!(kind, VerifyKind::Qr);
        assert_eq!(url, "https://www.bilibili.com/h5/verify?token=abc");
        assert!(message.contains("扫码验证"), "{message}");
        assert!(message.contains("请扫码验证"), "服务端原话要带出来：{message}");
    }

    /// 60043：人脸认证。响应里没有二维码，得拿 nav 的 mid 自己拼地址 ——
    /// 拼错了用户会被送到一个认不出自己的页面，所以 mid 必须对。
    #[tokio::test]
    async fn verify_60043_builds_the_face_auth_url_from_the_nav_mid() {
        let srv = test_http::start(|r: &Request| match r.path.as_str() {
            "/x/web-interface/nav" => (
                200,
                // nav 得带两个够长的 wbi 种子，不然 `nav()` 会先报「签不了名」
                // （它不做签名之外的事，但这条链上确实会走到 nav）
                r#"{"code":0,"message":"0","data":{"mid":42,"uname":"小明","isLogin":true,
                    "wbi_img":{
                      "img_url":"https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png",
                      "sub_url":"https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"}}}"#
                    .to_string(),
            ),
            LIVE_VERSION_PATH => (200, VERSION_BODY.to_string()),
            START_LIVE_PATH => (
                200,
                r#"{"code":60043,"message":"请先完成人脸认证","data":{}}"#.to_string(),
            ),
            other => (
                200,
                format!(r#"{{"code":-1,"message":"不认识的路径 {other}"}}"#),
            ),
        })
        .await;

        // nav 在主站上：测试里把 main_base 也顶到假服务器（不然这一条会去打真网络）
        let c = BiliClient::new("SESSDATA=abc; bili_jct=tok123")
            .unwrap()
            .with_main_base(&srv.base);
        let out = start_live(&c, &srv.base, 7734200, 371).await.unwrap();
        let StartOutcome::Verify { kind, url, message } = out else {
            panic!("这条该是「要人脸认证」：{out:?}");
        };
        assert_eq!(kind, VerifyKind::FaceAuth);
        assert_eq!(
            url,
            "https://www.bilibili.com/blackboard/live/face-auth-middle.html?source_event=400&mid=42",
            "mid 是 nav 给的 42，不能是别的"
        );
        assert!(message.contains("人脸认证"), "{message}");
        assert!(message.contains("请先完成人脸认证"), "{message}");
    }

    /// 60024 但服务端没给 qr：仍然是「要验证」这件事（不是普通失败），只是没地址可画，
    /// 界面得说「再按 F4 试一次」而不是把 60024 当成一句看不懂的错。
    #[tokio::test]
    async fn verify_without_an_address_is_still_a_verification() {
        let srv = test_http::start(|r: &Request| match r.path.as_str() {
            LIVE_VERSION_PATH => (200, VERSION_BODY.to_string()),
            START_LIVE_PATH => (
                200,
                r#"{"code":60024,"message":"请扫码验证","data":{}}"#.to_string(),
            ),
            other => (
                200,
                format!(r#"{{"code":-1,"message":"不认识的路径 {other}"}}"#),
            ),
        })
        .await;

        let out = start_live(&client(), &srv.base, 7734200, 371).await.unwrap();
        let StartOutcome::Verify { kind, url, .. } = out else {
            panic!("这条该还是「要验证」：{out:?}");
        };
        assert_eq!(kind, VerifyKind::Qr);
        assert!(url.is_empty());
    }

    /// 别的 code（比如「已经在直播中」）就是普通失败，带上服务端的原话。
    #[tokio::test]
    async fn other_error_codes_are_plain_errors() {
        let srv = test_http::start(|r: &Request| match r.path.as_str() {
            LIVE_VERSION_PATH => (200, VERSION_BODY.to_string()),
            START_LIVE_PATH => (
                200,
                r#"{"code":-400,"message":"已经在直播中"}"#.to_string(),
            ),
            other => (
                200,
                format!(r#"{{"code":-1,"message":"不认识的路径 {other}"}}"#),
            ),
        })
        .await;

        let err = start_live(&client(), &srv.base, 7734200, 371)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("-400"), "{err}");
        assert!(err.contains("已经在直播中"), "{err}");
    }

    /// 没选分区 / 没 csrf：一个请求都不发。
    #[tokio::test]
    async fn missing_area_or_csrf_sends_nothing() {
        let srv = test_http::start(|_| (200, VERSION_BODY.to_string())).await;

        let err = start_live(&client(), &srv.base, 7734200, 0)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("分区"), "{err}");

        let no_csrf = BiliClient::new("SESSDATA=abc").unwrap();
        let err = start_live(&no_csrf, &srv.base, 7734200, 371)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("bili_jct"), "{err}");
        let err = stop_live(&no_csrf, &srv.base, 7734200)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("bili_jct"), "{err}");

        assert!(srv.hits().is_empty(), "一个请求都不该发出去");
    }

    /// 凭据组装：`rtmp` + `protocols` 多路，密钥以 `?` 开头、地址以 `/` 结尾，
    /// 缺地址 / 缺密钥的那一路丢掉，全空要报错（不能让用户对着空框以为开播失败了）。
    #[test]
    fn assembling_streams_covers_the_edge_shapes() {
        let data = serde_json::json!({
            "rtmp": {"addr": "rtmp://a/live", "code": "?k=1"},
            "protocols": [
                {"protocol": "rtmp", "addr": "rtmp://b/live/", "code": "key2"},
                {"protocol": "srt", "addr": "srt://c:1935", "code": "k3"},
                {"protocol": "srt", "addr": "", "code": "k4"},
                {"protocol": "srt", "addr": "srt://d:1935", "code": ""}
            ]
        });
        let streams = assemble_streams(&data).unwrap();
        let kinds: Vec<&str> = streams.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, vec!["rtmp-1", "rtmp-2", "srt-1"], "缺地址/密钥的跳过");
        assert_eq!(streams[0].full_url, "rtmp://a/live?k=1", "密钥以 ? 开头");
        assert_eq!(streams[1].full_url, "rtmp://b/live/key2", "地址以 / 结尾");
        assert_eq!(streams[2].full_url, "srt://c:1935/k3", "普通情况补一个 /");
        assert_eq!(streams[0].protocol, "rtmp");
        assert_eq!(streams[1].key, "key2");

        // 一路都没有 = 服务端没给地址，必须报错
        for empty in [
            serde_json::json!({}),
            serde_json::json!({"rtmp": {"addr": "rtmp://a", "code": ""}, "protocols": []}),
        ] {
            let err = assemble_streams(&empty).unwrap_err().to_string();
            assert!(err.contains("没返回推流地址"), "{err}");
        }
    }

    /// 人脸认证地址是写死的模板，B 站改一次就静默失效，钉住它（Go 版也有这一条）。
    #[test]
    fn face_auth_url_is_pinned() {
        assert_eq!(
            face_auth_url(123456),
            "https://www.bilibili.com/blackboard/live/face-auth-middle.html?source_event=400&mid=123456"
        );
    }
}
