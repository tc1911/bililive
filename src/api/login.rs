//! 扫码登录：`passport.bilibili.com` 的 generate / poll 两个接口。
//!
//! 这一层只跟 B 站说话。登录成功后它把**拼好的 Cookie 串**交给 main 的会话任务
//! （`creds` 通道），落盘、换 client 手里那套凭据、重启弹幕链路都在那边 ——
//! `api/` 不碰配置文件，`ui/` 不碰网络，这条线别绕。
//!
//! 全流程只读：generate 申请一张码、poll 问状态，不改账号上任何东西。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::sleep;

use crate::api::client::{BiliClient, Nav, cookie_value, int_of, str_of};

pub const PASSPORT_BASE: &str = "https://passport.bilibili.com";

const GENERATE_PATH: &str = "/x/passport-login/web/qrcode/generate";
const POLL_PATH: &str = "/x/passport-login/web/qrcode/poll";

/// 会被写进 config.toml 的字段，顺序也按它来。
///
/// 只留这五个：`buvid3` 之类的旁路 cookie 服务端每次都重发一份，
/// 存下来既没用，又会让用户以为配置里那串是什么要紧的机密。
pub const COOKIE_NAMES: [&str; 5] = [
    "SESSDATA",
    "bili_jct",
    "DedeUserID",
    "DedeUserID__ckMd5",
    "sid",
];

/// `data.code` 的取值。注意不是外壳那个 `code` —— 外壳恒为 0，
/// 照着外壳判断的话二维码过期了程序还在那儿傻等。
pub const QR_SUCCESS: i64 = 0;
pub const QR_EXPIRED: i64 = 86038;
pub const QR_WAITING_SCAN: i64 = 86101;
pub const QR_WAITING_CONFIRM: i64 = 86090;

/// 轮询间隔与上限：2 秒一次、90 次正好 3 分钟，跟二维码自己的有效期对齐。
pub const POLL_GAP: Duration = Duration::from_secs(2);
pub const POLL_ATTEMPTS: usize = 90;

/// 一次 poll 的结果 → 下一步干什么。纯函数，界面和测试都只看它。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrStep {
    /// 拿到凭据了
    Done,
    /// 超时了，得换一张
    Expired,
    /// 还没人扫
    WaitingScan,
    /// 扫了，等手机上点确认
    WaitingConfirm,
    /// 没见过的 code（`-400` 参数坏了之类），原样报给用户
    Unknown(i64),
}

pub fn next_step(code: i64) -> QrStep {
    match code {
        QR_SUCCESS => QrStep::Done,
        QR_EXPIRED => QrStep::Expired,
        QR_WAITING_SCAN => QrStep::WaitingScan,
        QR_WAITING_CONFIRM => QrStep::WaitingConfirm,
        other => QrStep::Unknown(other),
    }
}

/// poll 的 `data` 那一层。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrPoll {
    pub code: i64,
    pub message: String,
    /// 成功时这里是那条**带着凭据的跳转地址**，兜底就靠它。
    pub url: String,
}

/// 申请一张登录二维码，返回（二维码内容，`qrcode_key`）。
pub async fn generate(client: &BiliClient, base: &str) -> Result<(String, String)> {
    let v = client.get_api(&format!("{base}{GENERATE_PATH}")).await?;
    let url = str_of(&v["url"]);
    let key = str_of(&v["qrcode_key"]);
    if url.is_empty() || key.is_empty() {
        // 少了任何一个后面都走不动，在这儿说清楚，别揣着空串去轮询换一堆 86101。
        bail!("二维码接口没给 url / qrcode_key");
    }
    Ok((url, key))
}

/// 查一次扫码状态，顺带把响应里的 `Set-Cookie` 原样带回来。
pub async fn poll(client: &BiliClient, base: &str, key: &str) -> Result<(QrPoll, Vec<String>)> {
    let url = format!("{base}{POLL_PATH}?qrcode_key={key}");
    let (v, cookies) = client.get_json_and_cookies(&url).await?;
    let d = &v["data"];
    Ok((
        QrPoll {
            code: int_of(&d["code"]),
            message: str_of(&d["message"]),
            url: str_of(&d["url"]),
        },
        cookies,
    ))
}

/// 从 `Set-Cookie` 里抠出要持久化的字段。
///
/// 一条长这样：`SESSDATA=xxx; Path=/; Domain=.bilibili.com; HttpOnly`。
/// 只取第一个 `=` 之前那一段，后面的属性全不要 —— 把 `Path=/` 当成值存进配置，
/// 下次带上去服务端只当你没登录。
///
/// 值是 `deleted` 的跳过：那是服务端在**清**这个 cookie（退出登录会用到），
/// 照收下来配置里就多一串 `SESSDATA=deleted`，比没有还糟。
pub fn cookies_from_set_cookie(headers: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for h in headers {
        let Some((name, rest)) = h.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if !COOKIE_NAMES.contains(&name) {
            continue;
        }
        let value = rest.split(';').next().unwrap_or("").trim();
        if value.is_empty() || value.eq_ignore_ascii_case("deleted") {
            continue;
        }
        out.push((name.to_string(), value.to_string()));
    }
    out
}

/// 从成功跳转 URL 的 query 里抠同样那几个字段 —— 这是**兜底**那条路。
///
/// `Set-Cookie` 有可能被中间那几跳吃掉（真遇到过），那样凭据就只剩 URL 上这一份。
/// query 是百分号编码的，得解一次码：SESSDATA 里的逗号在 URL 上是 `%2C`，
/// 不解码直接存进配置，下次带上去服务端认不出来，表现的却只是「登录态时好时坏」。
pub fn cookies_from_redirect(url: &str) -> Vec<(String, String)> {
    let Some((_, rest)) = url.split_once('?') else {
        return Vec::new();
    };
    let query = rest.split('#').next().unwrap_or(rest);
    url::form_urlencoded::parse(query.as_bytes())
        .filter(|(k, _)| COOKIE_NAMES.contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .filter(|(_, v)| !v.is_empty())
        .collect()
}

/// 把新拿到的字段和原来那串拼成一份完整的 Cookie。
///
/// `extra` 里**前面的优先**（调用方按 Set-Cookie、跳转 URL 的顺序拼），
/// 缺的再从 `base` 里补；输出顺序固定按 `COOKIE_NAMES` 走。
/// 顺序固定是为了 config.toml 每次保存都长得一样 —— 一堆 cookie 每次换位置，
/// git diff 里根本看不出到底改了哪一条。
pub fn merge_cookie(base: &str, extra: &[(String, String)]) -> String {
    let mut have: Vec<(&'static str, String)> = Vec::new();
    for (k, v) in extra {
        let Some(name) = COOKIE_NAMES.iter().find(|n| *n == k).copied() else {
            continue;
        };
        if v.is_empty() || have.iter().any(|(n, _)| *n == name) {
            continue;
        }
        have.push((name, v.clone()));
    }
    for name in COOKIE_NAMES {
        if have.iter().any(|(n, _)| *n == name) {
            continue;
        }
        if let Some(v) = cookie_value(base, name) {
            have.push((name, v));
        }
    }
    COOKIE_NAMES
        .iter()
        .filter_map(|name| {
            have.iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| format!("{name}={v}"))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// 界面要看的那些事。网络这半边只往通道里塞这个，别的什么都不干。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginEvent {
    /// 账号那行要写的字，形如「名字 (uid 7)」
    LoggedIn(String),
    /// 没登录 / 登录态失效，带一句说明（界面上就是「未登录（回车扫码）」这种）
    LoggedOut(String),
    /// 二维码内容（就是那个 URL），界面自己编成二维码画出来
    Qr(String),
    /// 顶栏第二行那句「最近一条消息」
    Hint(String),
    /// 出错了（网络抖、接口报错、码过期），照原文显示，绝不 panic
    Failed(String),
}

/// 扫码任务的运行参数。
///
/// 间隔和次数做成字段只为了测试：单测里真睡 2 秒 ×90 会把 CI 拖死，
/// 而「整条流程打一遍」又必须走真正的状态机，不能只测零件。
pub struct LoginCtx {
    pub client: Arc<BiliClient>,
    /// 二维码接口的 base，测试时指向假服务器
    pub base: String,
    pub gap: Duration,
    pub attempts: usize,
    /// 登录成功后把 Cookie 串交给谁（main 的会话任务）
    pub creds: Sender<String>,
}

/// 扫码登录任务：开局查一次登录态，之后每收到一个信号就开一张新码。
///
/// 永不返回错误、永不 panic —— 界面上多一行说明是小事，整屏消失是大事。
pub async fn login_loop(ctx: LoginCtx, mut start: Receiver<()>, evt: Sender<LoginEvent>) {
    // 开局看一眼配置里那套凭据还灵不灵：灵就显示「名字 (uid N)」，
    // 不然账号栏会一直挂着「未登录」，用户以为得重新扫一遍。
    if ctx.client.logged_in().await {
        match ctx.client.nav().await {
            Ok(nav) if nav.is_login => {
                if evt
                    .send(LoginEvent::LoggedIn(account_line(&nav)))
                    .await
                    .is_err()
                {
                    return;
                }
                // 说清楚这是「配置里那套还能用」，不是「刚刚扫码成功」——
                // 两句话长得像，但用户看到「登录成功」会以为要重连一次。
                let _ = evt
                    .send(LoginEvent::Hint("配置里的凭据还有效，不用扫码".into()))
                    .await;
            }
            // 凭据在，但服务端不认（多半是 SESSDATA 过期了）
            Ok(_) => {
                let _ = evt
                    .send(LoginEvent::LoggedOut("登录态失效（回车扫码）".into()))
                    .await;
            }
            Err(e) => {
                let _ = evt
                    .send(LoginEvent::LoggedOut("登录态失效（回车扫码）".into()))
                    .await;
                let _ = evt
                    .send(LoginEvent::Hint(format!("登录态查询失败：{e}")))
                    .await;
            }
        }
    }

    while start.recv().await.is_some() {
        run_once(&ctx, &evt).await;
    }
}

pub fn account_line(nav: &Nav) -> String {
    format!("{} (uid {})", nav.uname, nav.mid)
}

/// 生成一张二维码并轮询到出结果（或者过期）。
async fn run_once(ctx: &LoginCtx, evt: &Sender<LoginEvent>) {
    // 已经登录了的就别再生成一张：屏幕上两张二维码，用户扫了哪张都说不清
    // （Go 版的原话）。回车在这儿是「重新扫码」，不是「重新登录」。
    if ctx.client.logged_in().await
        && let Ok(nav) = ctx.client.nav().await
        && nav.is_login
    {
        let _ = evt.send(LoginEvent::LoggedIn(account_line(&nav))).await;
        let _ = evt
            .send(LoginEvent::Hint("已登录，无需重复扫码".into()))
            .await;
        return;
    }

    let (content, key) = match generate(&ctx.client, &ctx.base).await {
        Ok(v) => v,
        Err(e) => {
            let _ = evt
                .send(LoginEvent::Failed(format!("二维码获取失败：{e}")))
                .await;
            return;
        }
    };
    if evt.send(LoginEvent::Qr(content)).await.is_err() {
        return; // 界面没了
    }
    if evt
        .send(LoginEvent::Hint("等待扫码…二维码 3 分钟内有效".into()))
        .await
        .is_err()
    {
        return;
    }

    for _ in 0..ctx.attempts {
        sleep(ctx.gap).await;

        let (status, set_cookie) = match poll(&ctx.client, &ctx.base, &key).await {
            Ok(v) => v,
            // 单次轮询失败不当成登录失败：网络抖一下很正常，2 秒后再问。
            // 真一直不通，最后那条「过期」的提示也够用户明白该重来。
            Err(_) => continue,
        };

        match next_step(status.code) {
            QrStep::Done => {
                let extra: Vec<(String, String)> = cookies_from_set_cookie(&set_cookie)
                    .into_iter()
                    .chain(cookies_from_redirect(&status.url))
                    .collect();
                let cookie = merge_cookie("", &extra);
                // SESSDATA 是身份、bili_jct 是防 CSRF 的令牌，缺一个都不算登录成功：
                // 少了后者所有写操作都会被服务端用「csrf 校验失败」拒掉，
                // 而用户以为自己已经登录了，只会在那儿反复试。
                if cookie_value(&cookie, "SESSDATA").is_none()
                    || cookie_value(&cookie, "bili_jct").is_none()
                {
                    let _ = evt
                        .send(LoginEvent::Failed(
                            "扫码成功了，但没拿到完整凭据（SESSDATA / bili_jct 缺一个），回车再扫一次"
                                .into(),
                        ))
                        .await;
                    return;
                }
                // 交接给会话任务，后面的落盘 / 换凭据 / 重启链路都归它。
                let _ = ctx.creds.send(cookie).await;
                return;
            }
            QrStep::Expired => {
                let _ = evt
                    .send(LoginEvent::Failed(
                        "二维码已过期，在「账号」栏按回车换一张".into(),
                    ))
                    .await;
                return;
            }
            QrStep::WaitingConfirm => {
                if evt
                    .send(LoginEvent::Hint("已扫码，请在手机上点确认".into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            QrStep::WaitingScan => {
                if evt.send(LoginEvent::Hint("等待扫码…".into())).await.is_err() {
                    return;
                }
            }
            QrStep::Unknown(code) => {
                let note = format!("扫码状态异常（code {code}）：{}", status.message);
                if evt.send(LoginEvent::Hint(note)).await.is_err() {
                    return;
                }
            }
        }
    }

    let _ = evt
        .send(LoginEvent::Failed(
            "二维码已过期（等了 3 分钟），在「账号」栏按回车换一张".into(),
        ))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_http;

    /// 四种 code 各是什么下一步 —— 全流程的分支就挂在这一张表上，
    /// 哪天真多一个状态码，这里先红。
    #[test]
    fn every_poll_code_maps_to_a_step() {
        assert_eq!(next_step(QR_SUCCESS), QrStep::Done);
        assert_eq!(next_step(QR_EXPIRED), QrStep::Expired);
        assert_eq!(next_step(QR_WAITING_SCAN), QrStep::WaitingScan);
        assert_eq!(next_step(QR_WAITING_CONFIRM), QrStep::WaitingConfirm);
        // 没见过的别硬猜成成功或者过期，照原样报给用户看
        assert_eq!(next_step(-400), QrStep::Unknown(-400));
        assert_eq!(next_step(86039), QrStep::Unknown(86039));
    }

    #[test]
    fn set_cookie_keeps_only_the_five_fields() {
        // 真实形状：值后面跟着一堆属性，属性里的等号不能被当成值的一部分
        let headers = vec![
            "SESSDATA=abc%2Cdef; Path=/; Domain=.bilibili.com; Expires=Wed, 01 Jan 2027 00:00:00 GMT; HttpOnly; Secure"
                .to_string(),
            "bili_jct=tok; Path=/".to_string(),
            "buvid3=xxx; Path=/".to_string(), // 旁路 cookie，不存
            "DedeUserID=7; Path=/".to_string(),
            "notacookie".to_string(), // 连等号都没有，不能崩
        ];
        assert_eq!(
            cookies_from_set_cookie(&headers),
            vec![
                ("SESSDATA".to_string(), "abc%2Cdef".to_string()),
                ("bili_jct".to_string(), "tok".to_string()),
                ("DedeUserID".to_string(), "7".to_string()),
            ]
        );
    }

    /// 服务端用 `deleted` 表示「把这个 cookie 清掉」（退出登录会发）。
    /// 照收下来配置里就多一串 `SESSDATA=deleted`，比没有还糟。
    #[test]
    fn deleted_and_empty_set_cookie_values_are_dropped() {
        let headers = vec![
            "SESSDATA=deleted; Path=/".to_string(),
            "bili_jct=DELETED; Path=/".to_string(),
            "sid=; Path=/".to_string(),
        ];
        assert!(cookies_from_set_cookie(&headers).is_empty());
    }

    #[test]
    fn redirect_query_is_percent_decoded() {
        // 兜底那条路：Set-Cookie 被跳转吃掉时，凭据只剩 URL 上这一份。
        // `%2C` 是 SESSDATA 里的逗号 —— 不解码就存，下次带上去服务端不认。
        let url = "https://passport.biligame.com/crossDomain?DedeUserID=7&bili_jct=tok%2C1&c=b\
                   &SESSDATA=abc%2Cdef&gourl=https%3A%2F%2Flive.bilibili.com#frag";
        assert_eq!(
            cookies_from_redirect(url),
            vec![
                ("DedeUserID".to_string(), "7".to_string()),
                ("bili_jct".to_string(), "tok,1".to_string()),
                ("SESSDATA".to_string(), "abc,def".to_string()),
            ]
        );
        // 没有 query / 不是 URL 的一律给空表，不能 panic
        assert!(cookies_from_redirect("").is_empty());
        assert!(cookies_from_redirect("https://live.bilibili.com/").is_empty());
    }

    /// 输入的字段顺序乱、只有一部分，输出都要按固定顺序、缺的从原来那串补。
    #[test]
    fn merge_is_ordered_and_fills_the_gaps() {
        let extra = vec![
            ("sid".to_string(), "s1".to_string()),
            ("SESSDATA".to_string(), "new".to_string()),
        ];
        let merged = merge_cookie("SESSDATA=old; bili_jct=keepme; DedeUserID=7", &extra);
        assert_eq!(
            merged, "SESSDATA=new; bili_jct=keepme; DedeUserID=7; sid=s1",
            "顺序要固定，而且新的盖旧的"
        );

        // 两边都有同一个字段时，前面的（Set-Cookie）赢
        let extra = vec![
            ("SESSDATA".to_string(), "from-header".to_string()),
            ("SESSDATA".to_string(), "from-url".to_string()),
        ];
        assert_eq!(merge_cookie("", &extra), "SESSDATA=from-header");

        // 库里没有的字段（buvid3…）不该被带出来
        assert_eq!(merge_cookie("buvid3=x; SESSDATA=a", &[]), "SESSDATA=a");
    }

    /// 整条流程：generate -> poll 三次（未扫 / 待确认 / 成功）-> 凭据交出去。
    #[tokio::test]
    async fn whole_qr_flow_against_a_fake_server() {
        use crate::api::client::BiliClient;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));
        let polls = counter.clone();
        let srv = test_http::start(move |r| {
            if r.path.ends_with("/qrcode/generate") {
                return (
                    200,
                    r#"{"code":0,"message":"0","data":{"url":"https://www.bilibili.com/h5/login?k=1","qrcode_key":"KEY123"}}"#
                        .to_string(),
                );
            }
            // 三次轮询各给一个不同的 code；成功后带上那条「带着凭据的跳转地址」
            let n = polls.fetch_add(1, Ordering::SeqCst);
            let body = match n {
                0 => r#"{"code":0,"message":"0","data":{"code":86101,"message":"未扫码","url":""}}"#
                    .to_string(),
                1 => r#"{"code":0,"message":"0","data":{"code":86090,"message":"已扫码","url":""}}"#
                    .to_string(),
                _ => r#"{"code":0,"message":"0","data":{"code":0,"message":"","url":"https://passport.biligame.com/crossDomain?DedeUserID=7&bili_jct=tok%2C1&SESSDATA=abc%2Cdef&sid=s1"}}"#
                    .to_string(),
            };
            (200, body)
        })
        .await;

        let client = Arc::new(BiliClient::new("").unwrap());
        let (creds_tx, mut creds_rx) = tokio::sync::mpsc::channel(1);
        let ctx = LoginCtx {
            client,
            base: srv.base.clone(),
            gap: Duration::ZERO, // 单测里别真睡 2 秒
            attempts: 10,
            creds: creds_tx,
        };

        let (start_tx, start_rx) = tokio::sync::mpsc::channel(1);
        let (evt_tx, mut evt_rx) = tokio::sync::mpsc::channel(16);
        let task = tokio::spawn(login_loop(ctx, start_rx, evt_tx));
        start_tx.send(()).await.unwrap();

        // 交接出来的就是那五个字段拼成的 Cookie 串，顺序固定、值已解码
        let cookie = tokio::time::timeout(Duration::from_secs(5), creds_rx.recv())
            .await
            .expect("整条流程 5 秒内该走完")
            .expect("该交出凭据");
        assert_eq!(cookie, "SESSDATA=abc,def; bili_jct=tok,1; DedeUserID=7; sid=s1");

        let mut seen = Vec::new();
        while let Ok(ev) = evt_rx.try_recv() {
            seen.push(ev);
        }
        assert!(
            seen.iter()
                .any(|e| matches!(e, LoginEvent::Qr(u) if u.contains("h5/login"))),
            "要先把二维码内容交给界面：{seen:?}"
        );
        assert!(
            seen.iter()
                .any(|e| matches!(e, LoginEvent::Hint(h) if h.contains("已扫码"))),
            "86090 要提示「请在手机上点确认」：{seen:?}"
        );

        // 请求形状：路径要对，轮询必须把 qrcode_key 带上
        let hits = srv.hits();
        assert_eq!(
            hits[0].path, "/x/passport-login/web/qrcode/generate",
            "请求顺序：先 generate"
        );
        assert_eq!(
            hits.iter()
                .filter(|h| h.path.ends_with("/qrcode/poll"))
                .count(),
            3,
            "三次不同 code 各问一次"
        );
        for h in hits.iter().filter(|h| h.path.ends_with("/qrcode/poll")) {
            assert_eq!(h.query_param("qrcode_key"), Some("KEY123"), "{}", h.query);
            // 没登录时不该凭空带一个 Cookie 头出去
            assert!(h.header("cookie").is_none(), "{:?}", h.headers);
        }
        for h in &hits {
            assert_eq!(h.method, "GET");
        }

        task.abort();
    }

    /// generate 拿到的东西不全时要在**发轮询之前**停住，
    /// 不然空 key 会一直换来 86101，用户看到的是「永远等待扫码」。
    #[tokio::test]
    async fn generate_without_a_key_stops_early() {
        let srv = test_http::start(|_| {
            (
                200,
                r#"{"code":0,"message":"0","data":{"url":"x","qrcode_key":""}}"#.to_string(),
            )
        })
        .await;
        let client = BiliClient::new("").unwrap();
        let err = generate(&client, &srv.base).await.unwrap_err().to_string();
        assert!(err.contains("qrcode_key"), "{err}");
    }

    /// 轮询全程失败（网络断了）也不能 panic，最多是多问几次然后说过期。
    #[tokio::test]
    async fn poll_failures_never_panic() {
        let srv = test_http::start(|_| (500, "not json".to_string())).await;
        let client = BiliClient::new("").unwrap();
        assert!(poll(&client, &srv.base, "k").await.is_err());
    }

    /// 已经登录了再按回车，不该又生成一张码 —— 屏幕上两张二维码，
    /// 用户扫了哪张都说不清（Go 版的原话）。
    #[tokio::test]
    async fn already_logged_in_does_not_ask_for_another_code() {
        let srv = test_http::start(|r| {
            if r.path.ends_with("web-interface/nav") {
                return (
                    200,
                    r#"{"code":0,"message":"0","data":{"mid":7,"isLogin":true,"uname":"小明",
                        "wbi_img":{"img_url":"https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png",
                                   "sub_url":"https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"}}}"#
                        .to_string(),
                );
            }
            // 真去申请二维码才会走到这儿
            (200, r#"{"code":0,"message":"0","data":{"url":"x","qrcode_key":"y"}}"#.to_string())
        })
        .await;

        let client = Arc::new(
            BiliClient::new("SESSDATA=s; bili_jct=t")
                .unwrap()
                .with_main_base(&srv.base),
        );
        let (creds_tx, _creds_rx) = tokio::sync::mpsc::channel(1);
        let ctx = LoginCtx {
            client,
            base: srv.base.clone(),
            gap: Duration::ZERO,
            attempts: 3,
            creds: creds_tx,
        };
        let (start_tx, start_rx) = tokio::sync::mpsc::channel(1);
        let (evt_tx, mut evt_rx) = tokio::sync::mpsc::channel(16);
        let task = tokio::spawn(login_loop(ctx, start_rx, evt_tx));
        start_tx.send(()).await.unwrap();

        // 开局那次 nav 会说「已登录」，回车那一次再说一遍「无需重复扫码」
        let hints = drain(&mut evt_rx).await;
        assert!(
            hints
                .iter()
                .any(|e| matches!(e, LoginEvent::LoggedIn(l) if l == "小明 (uid 7)")),
            "{hints:?}"
        );
        assert!(
            hints
                .iter()
                .any(|e| matches!(e, LoginEvent::Hint(h) if h.contains("无需重复扫码"))),
            "{hints:?}"
        );
        assert!(
            !srv.hits()
                .iter()
                .any(|h| h.path.ends_with("/qrcode/generate")),
            "已经登录了还去申请二维码"
        );
        task.abort();
    }

    /// 一直没人扫（一直是 86101）：问满次数就停，别无限轮询，
    /// 而且要留一句「换一张」让用户知道下一步按什么。
    #[tokio::test]
    async fn a_code_nobody_scans_times_out() {
        let srv = test_http::start(|r| {
            if r.path.ends_with("/qrcode/generate") {
                return (
                    200,
                    r#"{"code":0,"message":"0","data":{"url":"https://x/y","qrcode_key":"k"}}"#
                        .to_string(),
                );
            }
            (
                200,
                r#"{"code":0,"message":"0","data":{"code":86101,"message":"未扫码","url":""}}"#
                    .to_string(),
            )
        })
        .await;

        let (creds_tx, _creds_rx) = tokio::sync::mpsc::channel(1);
        let ctx = LoginCtx {
            client: Arc::new(BiliClient::new("").unwrap()),
            base: srv.base.clone(),
            gap: Duration::ZERO,
            attempts: 3,
            creds: creds_tx,
        };
        let (start_tx, start_rx) = tokio::sync::mpsc::channel(1);
        let (evt_tx, mut evt_rx) = tokio::sync::mpsc::channel(16);
        let task = tokio::spawn(login_loop(ctx, start_rx, evt_tx));
        start_tx.send(()).await.unwrap();

        let last = drain(&mut evt_rx).await.pop();
        assert!(
            matches!(&last, Some(LoginEvent::Failed(f)) if f.contains("回车换一张")),
            "最后该说清楚去哪换一张：{last:?}"
        );
        assert_eq!(
            srv.hits()
                .iter()
                .filter(|h| h.path.ends_with("/qrcode/poll"))
                .count(),
            3,
            "问满 attempts 就停"
        );
        task.abort();
    }

    /// 把事件收干净，直到通道安静下来（任务还活着，通道不会关，
    /// 所以只能按「没消息了」判断，不能等 `recv()` 返回 `None`）。
    async fn drain(rx: &mut tokio::sync::mpsc::Receiver<LoginEvent>) -> Vec<LoginEvent> {
        let mut out = Vec::new();
        while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await
        {
            out.push(ev);
        }
        out
    }
}
