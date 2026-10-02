//! OBS 联动：开播拿到推流凭据后，把服务器 + 密钥填进 OBS 的「设置 → 推流」。
//!
//! 协议是 [obs-websocket](https://github.com/obsproject/obs-websocket) 5.x 的 JSON 版
//! （OBS 28 起内置），一条连接走完：
//! `Hello(op 0)` → `Identify(op 1)` → `Identified(op 2)` → `Request(op 6)` → `Response(op 7)`。
//! 要鉴权时 `Hello.d.authentication{challenge,salt}`，`Identify.d.authentication` 填应答。
//!
//! **这里只管填，不管推**：填完绝不代按「开始推流」（开播那下得用户自己在 OBS 里按），
//! 也绝不让「填不进去」影响开播本身 —— 所有失败都以 `Err(一句人话)` 出来，
//! 调用方只管把它变成界面上一行字。

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// 协议里的操作码（obs-websocket 5.x）。
const OP_HELLO: i64 = 0;
const OP_IDENTIFY: i64 = 1;
const OP_IDENTIFIED: i64 = 2;
/// 事件。默认订阅会推事件，等 `Response` 的时候可能先冒一条出来 ——
/// 跳过它接着等，别把「OBS 冒了句别的话」当成请求失败。
const OP_EVENT: i64 = 5;
const OP_REQUEST: i64 = 6;
const OP_RESPONSE: i64 = 7;

/// 配置文件里没写端口时的默认值（OBS 自己的配置文件里写的也是这个）。
const DEFAULT_PORT: u16 = 4455;

/// 连接 / 每一步读写最多等多久。
///
/// 「连不上」得有个头，「连上了但半天不吭声」更得有：后者不设超时的话那条后台
/// 任务会一直挂着，用户永远等不到那一行字（界面不会卡 —— 但等于什么都没发生）。
/// 几秒足够本地回环握一次手；超过就是对面不对劲，不是慢。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// 两种底层流都走这一个类型：本地 OBS 用不上 TLS，但 `connect_async` 统一给这个壳。
type Sock = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// `config.toml` 里 OBS 那四个字段，原样搬过来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// 开播成功后要不要填
    pub fill: bool,
    /// 空 = 127.0.0.1
    pub host: String,
    /// 0 = 去读 OBS 自己的配置
    pub port: u16,
    /// 空 = 去读 OBS 自己的配置
    pub password: String,
}

impl Default for Config {
    /// 默认**不填**（`fill: false`）：单测里那些跟 OBS 无关的用例拿它当「什么都没配」。
    fn default() -> Self {
        Self {
            fill: false,
            host: String::new(),
            port: 0,
            password: String::new(),
        }
    }
}

/// 补齐之后真正拿去连的那三项。跟 `Config` 分开：`Config` 是用户写的，
/// 这个是「用户写的 + 从 OBS 配置里读来的」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub password: String,
}

/// obs-websocket 的鉴权算法：
///
/// ```text
/// secret = base64(sha256(password + salt))
/// auth   = base64(sha256(secret + challenge))
/// ```
///
/// 黄金值在测试里钉着（python 独立算的那一串），改算法它就会红。
pub fn auth_string(password: &str, salt: &str, challenge: &str) -> String {
    let secret = b64_sha256(&[password, salt]);
    b64_sha256(&[secret.as_str(), challenge])
}

/// 把几段拼起来 sha256 再 base64。分两步是协议定的：中间那串 base64 是下一步的输入。
fn b64_sha256(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(h.finalize())
}

/// `SetStreamServiceSettings` 那条 Request 的 `d`（纯函数，方便单测）。
///
/// 服务类型钉死 `rtmp_custom`：B 站给的密钥自带 `?streamname=…`，**原样**塞进 `key`
/// 让 OBS 自己去拼。自己往 key 里补地址或者转义一下，那串参数就对不上了
/// （现象是 OBS 里「开始推流」之后立刻断开，而界面上什么都看不出来）。
pub fn set_stream_request(server: &str, key: &str) -> Value {
    json!({
        "requestType": "SetStreamServiceSettings",
        "requestId": "bililive-fill",
        "requestData": {
            "streamServiceType": "rtmp_custom",
            "streamServiceSettings": {
                "server": server,
                "key": key,
                "use_auth": false,
            },
        },
    })
}

/// 补齐没写的项：端口 / 密码缺哪个就去读 OBS 自己的 websocket 配置
/// （`$XDG_CONFIG_HOME/obs-studio/plugin_config/obs-websocket/config.json`，
/// 没设 `XDG_CONFIG_HOME` 就退回 `~/.config/…`）。
///
/// `Err` 里永远是**一句可以直接摆到界面上的话**。
pub fn resolve(host: &str, port: u16, password: &str) -> Result<Settings, String> {
    resolve_at(&config_path(), host, port, password)
}

/// `resolve` 的本体，路径从外面给（单测直接拿临时文件跑，不碰进程环境）。
fn resolve_at(path: &Path, host: &str, port: u16, password: &str) -> Result<Settings, String> {
    let mut set = Settings {
        host: if host.is_empty() {
            "127.0.0.1".to_string()
        } else {
            host.to_string()
        },
        port,
        password: password.to_string(),
    };
    // 端口和密码都写死了就别去开文件：用户既然写死，说明他清楚自己连的是哪台 OBS。
    // 读文件只是替「不方便手抄密码」的人补默认值。
    if set.port != 0 && !set.password.is_empty() {
        return Ok(set);
    }
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        // 没这个文件通常是「从来没在 OBS 里开过 WebSocket 服务器」——
        // 光报一句「路径不存在」，用户不知道该去哪儿点。
        Err(_) => {
            return Err(format!(
                "没找到 OBS 的 WebSocket 配置（{}），先去 OBS 里 工具 → WebSocket 服务器设置 打开它",
                path.display()
            ));
        }
    };
    let cfg: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("OBS 的 WebSocket 配置读不动（{}）：{e}", path.display()))?;
    // 只有**明确写了 false** 才算「没开」：字段缺失（老版本配置）不能冤成没开，
    // 否则用户明明开着也只会收到一句「去打开它」，然后开始怀疑端口和防火墙。
    if cfg.get("server_enabled").and_then(Value::as_bool) == Some(false) {
        return Err(
            "OBS 里的 WebSocket 服务器没开：工具 → WebSocket 服务器设置 → 勾上「启用 WebSocket 服务器」"
                .to_string(),
        );
    }
    if set.port == 0 {
        set.port = cfg
            .get("server_port")
            .and_then(Value::as_u64)
            // 写歪的超大端口不 panic，也不会截成 0 之后又被当成「没写」
            .map_or(0, |p| p.clamp(1, u16::MAX as u64) as u16);
    }
    if set.password.is_empty() {
        set.password = cfg
            .get("server_password")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
    }
    if set.port == 0 {
        set.port = DEFAULT_PORT;
    }
    Ok(set)
}

/// OBS 那份 websocket 配置的路径。认 `XDG_CONFIG_HOME`（跟 `config.rs` 一个口径）。
fn config_path() -> PathBuf {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(".config"),
            // 拿不到 HOME 也照样拼一个相对路径出来：读不到就报「没找到」，别 panic
            None => PathBuf::from(".config"),
        },
    };
    base.join("obs-studio")
        .join("plugin_config")
        .join("obs-websocket")
        .join("config.json")
}

/// 把服务器 + 密钥填进 OBS：`resolve` → 连上 → 握手 → 发一条 `SetStreamServiceSettings`。
///
/// 返回的 `Err` 是给人看的一句话（连不上 / 没开 / 密码不对 / OBS 拒绝），
/// 调用方只管把它变成一行字，**绝不能拿它去改开播的成败**。
pub async fn fill(cfg: &Config, server: &str, key: &str) -> Result<(), String> {
    let set = resolve(&cfg.host, cfg.port, &cfg.password)?;
    fill_resolved(&set, server, key, CONNECT_TIMEOUT, STEP_TIMEOUT).await
}

/// `fill` 的本体，超时从外面给（单测拿小超时跑「连上但不吭声」那条路）。
async fn fill_resolved(
    set: &Settings,
    server: &str,
    key: &str,
    connect_timeout: Duration,
    step: Duration,
) -> Result<(), String> {
    let url = format!("ws://{}:{}/", set.host, set.port);
    let (mut ws, _) = tokio::time::timeout(connect_timeout, tokio_tungstenite::connect_async(&url))
        .await
        .map_err(|_| format!("连 OBS 的 WebSocket 超时（{url}）"))?
        .map_err(|e| format!("连不上 OBS 的 WebSocket（{url}）：{e}"))?;

    let hello = read_frame(&mut ws, step).await?;
    if hello.op != OP_HELLO {
        return Err(format!("OBS 第一句应该是 Hello，收到 op={}", hello.op));
    }
    let mut identify = json!({ "rpcVersion": 1 });
    if let Some(a) = hello.d.get("authentication") {
        // 缺 salt / challenge 就别硬算了 —— 算出来的串一定对不上，
        // 而 OBS 只会回一句「密码不对」，把人往错的方向指。
        let salt = a.get("salt").and_then(Value::as_str).unwrap_or_default();
        let challenge = a.get("challenge").and_then(Value::as_str).unwrap_or_default();
        if set.password.is_empty() {
            return Err(
                "OBS 要密码，但没拿到：把密码填进 config.toml 的 obs_password，或在 OBS 里关掉鉴权"
                    .to_string(),
            );
        }
        identify["authentication"] = json!(auth_string(&set.password, salt, challenge));
    }
    send_frame(&mut ws, OP_IDENTIFY, identify, step).await?;

    let identified = read_frame(&mut ws, step).await?;
    if identified.op != OP_IDENTIFIED {
        return Err(format!(
            "OBS 没确认这次连接（op={}），密码对不上？",
            identified.op
        ));
    }

    send_frame(&mut ws, OP_REQUEST, set_stream_request(server, key), step).await?;
    loop {
        let f = read_frame(&mut ws, step).await?;
        if f.op == OP_EVENT {
            continue; // 事件不是回话，接着等
        }
        if f.op != OP_RESPONSE {
            return Err(format!("OBS 回了条没见过的消息（op={}）", f.op));
        }
        let status = f.d.get("requestStatus");
        let ok = status
            .and_then(|s| s.get("result"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !ok {
            let code = status
                .and_then(|s| s.get("code"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let comment = status
                .and_then(|s| s.get("comment"))
                .and_then(Value::as_str)
                .unwrap_or("");
            // result=false 也算失败：这里要是「收着回话就当成功」，
            // 用户会在 OBS 里对着一份没换过的密钥纳闷。
            return Err(format!("OBS 拒绝了这次填写（{code}）：{comment}"));
        }
        return Ok(());
    }
}

/// OBS 发来的一帧：操作码 + 里面的 `d`。
struct Frame {
    op: i64,
    d: Value,
}

/// 等一帧。每一步都套超时 —— 没有它，「连上了但不吭声」会把这条任务永远挂住。
async fn read_frame(ws: &mut Sock, step: Duration) -> Result<Frame, String> {
    let msg = tokio::time::timeout(step, ws.next())
        .await
        .map_err(|_| "OBS 半天没回话（连上了但不响应）".to_string())?
        .ok_or_else(|| "OBS 把连接关了".to_string())?
        .map_err(|e| format!("跟 OBS 的连接断了：{e}"))?;
    let text = match msg {
        Message::Text(t) => t,
        Message::Close(_) => return Err("OBS 把连接关了".to_string()),
        other => return Err(format!("OBS 发了条看不懂的消息：{other:?}")),
    };
    let v: Value =
        serde_json::from_str(text.as_str()).map_err(|e| format!("OBS 的消息不是 JSON：{e}"))?;
    Ok(Frame {
        op: v.get("op").and_then(Value::as_i64).unwrap_or(-1),
        d: v.get("d").cloned().unwrap_or(Value::Null),
    })
}

async fn send_frame(ws: &mut Sock, op: i64, d: Value, step: Duration) -> Result<(), String> {
    let text = json!({ "op": op, "d": d }).to_string();
    tokio::time::timeout(step, ws.send(Message::text(text)))
        .await
        .map_err(|_| "给 OBS 发消息超时".to_string())?
        .map_err(|e| format!("给 OBS 发消息失败：{e}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    /// 黄金值：拿 python 独立算的（`base64(sha256("secret" + "salt123"))`，
    /// 再跟 "chal456" 哈希一遍）。**别**拿 `auth_string` 自己的输出当期望值 ——
    /// 那是循环论证，算法写错的时候测试会跟着一起错。
    const GOLDEN_SALT: &str = "salt123";
    const GOLDEN_CHALLENGE: &str = "chal456";
    const GOLDEN_PASSWORD: &str = "secret";
    const GOLDEN_AUTH: &str = "yo3DuCXyQQheiGKNZpyXB//3OodP2GoXULXeX19lE4M=";

    #[test]
    fn auth_string_matches_the_python_golden_value() {
        assert_eq!(
            auth_string(GOLDEN_PASSWORD, GOLDEN_SALT, GOLDEN_CHALLENGE),
            GOLDEN_AUTH
        );
    }

    /// 请求体是个纯函数拼出来的：密钥原样进 `key`，服务类型钉死 `rtmp_custom`。
    #[test]
    fn the_request_body_keeps_the_key_verbatim() {
        let key = "?streamname=live_1_2&key=abc";
        let v = set_stream_request("rtmp://live-push.bilivideo.com/live-bvc", key);
        assert_eq!(v["requestType"], "SetStreamServiceSettings");
        // OBS 用 requestId 对号，空的话它不知道是谁问的
        assert!(v["requestId"].as_str().is_some_and(|s| !s.is_empty()), "{v}");
        let rd = &v["requestData"];
        assert_eq!(rd["streamServiceType"], "rtmp_custom");
        assert_eq!(
            rd["streamServiceSettings"]["server"],
            "rtmp://live-push.bilivideo.com/live-bvc"
        );
        assert_eq!(rd["streamServiceSettings"]["key"], key, "密钥不许被改");
        assert_eq!(rd["streamServiceSettings"]["use_auth"], false);
    }

    /// 一个假 OBS：接一条连接、开口先 `Hello`，收到 `Identify` 回 `Identified`，
    /// 收到 `Request` 按 `answer` 决定回不回 `Response`；收到的每条都塞进 `frames`。
    pub(crate) struct FakeObs {
        pub(crate) port: u16,
        /// 收到的每条消息（op, d）
        pub(crate) frames: mpsc::UnboundedReceiver<(i64, Value)>,
        pub(crate) task: tokio::task::JoinHandle<()>,
    }

    /// `authentication` 给了就写进 Hello 的 `authentication`（走鉴权那条路）。
    /// `answer` 为 false = 连上、握手也走完，就是不回 `Request` 的回话（测超时用）。
    pub(crate) async fn fake_obs(
        authentication: Option<(&str, &str)>,
        response: Value,
        answer: bool,
    ) -> FakeObs {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("占一个本地端口");
        let port = listener.local_addr().expect("拿到端口").port();
        let (tx, frames) = mpsc::unbounded_channel();
        // 服务端任务要 'static，借来的 &str 得先变成自己的
        let auth =
            authentication.map(|(salt, challenge)| (salt.to_string(), challenge.to_string()));
        let task = tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                return;
            };
            let mut hello = json!({ "obsWebSocketVersion": "5.5.2", "rpcVersion": 1 });
            if let Some((salt, challenge)) = auth {
                hello["authentication"] = json!({ "salt": salt, "challenge": challenge });
            }
            if send_json(&mut ws, json!({ "op": OP_HELLO, "d": hello }))
                .await
                .is_err()
            {
                return;
            }
            while let Some(Ok(msg)) = ws.next().await {
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else {
                    continue;
                };
                let op = v.get("op").and_then(Value::as_i64).unwrap_or(-1);
                // 先记下来再回话：不然客户端可能已经拿到 Response 返回了，
                // 测试这边还在跟这条记录赛跑。
                let _ = tx.send((op, v["d"].clone()));
                let identified = json!({ "op": OP_IDENTIFIED, "d": { "negotiatedRpcVersion": 1 } });
                if op == OP_IDENTIFY && send_json(&mut ws, identified).await.is_err() {
                    return;
                }
                if op == OP_REQUEST && answer {
                    let mut d = response.clone();
                    d["requestId"] = v["d"]["requestId"].clone();
                    if send_json(&mut ws, json!({ "op": OP_RESPONSE, "d": d }))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        });
        FakeObs { port, frames, task }
    }

    /// 服务端这半边是**裸 TCP**（本地假 OBS 不走 TLS），跟客户端的 `Sock` 不是同一个壳。
    async fn send_json(
        ws: &mut WebSocketStream<TcpStream>,
        v: Value,
    ) -> Result<(), tokio_tungstenite::tungstenite::Error> {
        ws.send(Message::text(v.to_string())).await
    }

    fn ok_response() -> Value {
        json!({
            "requestType": "SetStreamServiceSettings",
            "requestId": "bililive-fill",
            "requestStatus": { "result": true, "code": 100 }
        })
    }

    fn settings(port: u16, password: &str) -> Settings {
        Settings {
            host: "127.0.0.1".to_string(),
            port,
            password: password.to_string(),
        }
    }

    async fn next_frame(rx: &mut mpsc::UnboundedReceiver<(i64, Value)>) -> (i64, Value) {
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("假 OBS 该收到消息")
            .expect("通道还在")
    }

    /// 跟一个假 OBS 走完整全程：Hello → Identify（带鉴权）→ Identified →
    /// Request → Response，顺带验请求体的形状 —— 字段写错 OBS 那头直接不认。
    #[tokio::test]
    async fn a_round_trip_fills_the_stream_settings() {
        let server = "rtmp://live-push.bilivideo.com/live-bvc/";
        let key = "?streamname=live_1_2&key=abc";
        let mut obs = fake_obs(Some((GOLDEN_SALT, GOLDEN_CHALLENGE)), ok_response(), true).await;

        fill_resolved(
            &settings(obs.port, GOLDEN_PASSWORD),
            server,
            key,
            CONNECT_TIMEOUT,
            STEP_TIMEOUT,
        )
        .await
        .expect("这一趟该成功");

        let (op, d) = next_frame(&mut obs.frames).await;
        assert_eq!(op, OP_IDENTIFY, "第二条该是 Identify");
        assert_eq!(d["rpcVersion"], 1);
        assert_eq!(d["authentication"], GOLDEN_AUTH, "鉴权串必须是黄金值");

        let (op, d) = next_frame(&mut obs.frames).await;
        assert_eq!(op, OP_REQUEST);
        assert_eq!(d["requestType"], "SetStreamServiceSettings");
        let rd = &d["requestData"];
        assert_eq!(rd["streamServiceType"], "rtmp_custom");
        assert_eq!(rd["streamServiceSettings"]["server"], server);
        assert_eq!(rd["streamServiceSettings"]["key"], key, "密钥要原样塞进去");
        assert_eq!(rd["streamServiceSettings"]["use_auth"], false);
        obs.task.abort();
    }

    /// Hello 里没有 `authentication`（OBS 里没设密码）：Identify 就**不许**带这个字段 ——
    /// 带上一个凭空的串，OBS 会当成鉴权失败直接拒绝。
    #[tokio::test]
    async fn a_hello_without_authentication_means_identify_without_it() {
        let mut obs = fake_obs(None, ok_response(), true).await;
        fill_resolved(
            &settings(obs.port, ""),
            "rtmp://a/live",
            "?s=1",
            CONNECT_TIMEOUT,
            STEP_TIMEOUT,
        )
        .await
        .expect("不要鉴权时该成功");

        let (op, d) = next_frame(&mut obs.frames).await;
        assert_eq!(op, OP_IDENTIFY);
        assert_eq!(d["rpcVersion"], 1);
        assert!(
            d.get("authentication").is_none(),
            "Hello 里没有 authentication 就别塞这个字段：{d}"
        );
        obs.task.abort();
    }

    /// OBS 要密码而配置里没有：得说清楚填哪儿，别只报一句「连不上」。
    #[tokio::test]
    async fn a_required_password_that_is_missing_is_explained() {
        let obs = fake_obs(Some((GOLDEN_SALT, GOLDEN_CHALLENGE)), ok_response(), true).await;
        let err = fill_resolved(
            &settings(obs.port, ""),
            "rtmp://a/live",
            "?s=1",
            CONNECT_TIMEOUT,
            STEP_TIMEOUT,
        )
        .await
        .expect_err("没密码不该当成填好了");
        assert!(err.contains("密码"), "{err}");
        assert!(err.contains("obs_password"), "{err}");
        obs.task.abort();
    }

    /// `requestStatus.result = false`：必须变成错误，不能当成功 ——
    /// 当成功的话用户在 OBS 里看到的还是旧密钥，界面上却说「填好了」。
    #[tokio::test]
    async fn a_rejected_request_is_an_error_instead_of_a_success() {
        let rejected = json!({
            "requestType": "SetStreamServiceSettings",
            "requestStatus": { "result": false, "code": 401, "comment": "密码不对" }
        });
        let obs = fake_obs(None, rejected, true).await;
        let err = fill_resolved(
            &settings(obs.port, ""),
            "rtmp://a/live",
            "?s=1",
            CONNECT_TIMEOUT,
            STEP_TIMEOUT,
        )
        .await
        .expect_err("OBS 说没成就不能说成了");
        assert!(err.contains("拒绝"), "{err}");
        assert!(err.contains("401"), "{err}");
        assert!(err.contains("密码不对"), "{err}");
        obs.task.abort();
    }

    /// 连上了、握手也走完，就是不回 `Request` 的回话：几秒内必须放弃，
    /// 不能把这条任务永远挂在那儿（界面上永远等不到那一行字）。
    #[tokio::test]
    async fn an_obs_that_never_answers_gives_up_instead_of_hanging() {
        let obs = fake_obs(None, ok_response(), false).await;
        let step = Duration::from_millis(500);
        let started = std::time::Instant::now();
        let err = fill_resolved(&settings(obs.port, ""), "rtmp://a/live", "?s=1", step, step)
            .await
            .expect_err("一直不吭声该超时");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "超时要真的生效，别等下去"
        );
        assert!(err.contains("半天") || err.contains("超时"), "{err}");
        obs.task.abort();
    }

    /// 端口上没人听（OBS 没开 / 端口写错）：一句人话，不是 panic。
    #[tokio::test]
    async fn a_closed_port_is_reported_as_cannot_connect() {
        // 借一个刚放掉的端口：连过去只会被拒
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").await.expect("占端口");
            l.local_addr().expect("端口").port()
        };
        let err = fill_resolved(
            &settings(port, ""),
            "rtmp://a/live",
            "?s=1",
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await
        .expect_err("没人在听就该报连不上");
        assert!(err.contains("连不上"), "{err}");
    }

    /// 端口 / 密码留空就去读 OBS 自己那份 `config.json`（认 `XDG_CONFIG_HOME`）；
    /// `server_enabled: false` 和「没这个文件」各给一句能照着做的话。
    #[test]
    fn resolve_reads_the_obs_config_under_xdg_config_home() {
        let dir = std::env::temp_dir().join(format!("bililive-obs-test-{}", std::process::id()));
        let path = dir
            .join("obs-studio")
            .join("plugin_config")
            .join("obs-websocket")
            .join("config.json");
        std::fs::create_dir_all(path.parent().expect("有父目录")).expect("建临时目录");

        // `std::env::set_var` 是**进程级**的，edition 2024 里还是 unsafe。全仓只有这一个
        // 测试碰它（别处读 XDG_CONFIG_HOME 的只有 `config.rs`，那些单测都显式传路径），
        // 所以改之前先存下旧值、测完放回去。
        let old = std::env::var_os("XDG_CONFIG_HOME");
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };

        let write = |body: &str| std::fs::write(&path, body).expect("写假配置");

        // 1) 配置为空：端口 / 密码都从文件读，host 空 = 127.0.0.1
        write(r#"{"server_enabled":true,"server_port":4455,"server_password":"pw"}"#);
        let set = resolve("", 0, "").expect("开着就该读出来");
        assert_eq!(set.host, "127.0.0.1");
        assert_eq!(set.port, 4455);
        assert_eq!(set.password, "pw");

        // 2) 只写了端口：端口以配置为准，没写的那个才去读文件
        let set = resolve("192.168.1.5", 4456, "").expect("该合并两处");
        assert_eq!(set.host, "192.168.1.5");
        assert_eq!(set.port, 4456, "配置里写了的优先");
        assert_eq!(set.password, "pw", "只有没写的那个才去读文件");

        // 3) 端口和密码都写了：一个文件都不读（文件这时候是坏的也不该影响）
        write("{ 这不是 JSON");
        let set = resolve("10.0.0.9", 4457, "mine").expect("写死了就别读文件");
        assert_eq!(set.host, "10.0.0.9");
        assert_eq!(set.port, 4457);
        assert_eq!(set.password, "mine");

        // 4) 服务器没开：说清楚去哪儿开 —— 只说「连不上」的话用户会去折腾端口和防火墙
        write(r#"{"server_enabled":false,"server_port":4455,"server_password":"pw"}"#);
        let err = resolve("", 0, "").expect_err("没开就别往下走");
        assert!(err.contains("没开"), "{err}");
        assert!(err.contains("启用 WebSocket 服务器"), "{err}");

        // 5) 没有这个文件：说清楚它去哪儿开
        std::fs::remove_file(&path).expect("删掉假配置");
        let err = resolve("", 0, "").expect_err("没文件该报没找到");
        assert!(err.contains("没找到"), "{err}");
        assert!(err.contains("obs-websocket"), "{err}");

        match old {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 配置文件里没有 `server_enabled` 这一项（老版本）**不能**当成「没开」，
    /// 端口也缺的时候才退回 4455。
    #[test]
    fn a_missing_server_enabled_is_not_a_disabled_server() {
        let dir = std::env::temp_dir().join(format!("bililive-obs-shape-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"server_password":"pw"}"#).expect("写假配置");
        let set = resolve_at(&path, "", 0, "").expect("缺字段不等于没开");
        assert_eq!(set.port, DEFAULT_PORT);
        assert_eq!(set.password, "pw");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 配置文件是坏 JSON：给一句话，别 panic。
    #[test]
    fn an_unreadable_config_becomes_a_sentence_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("bililive-obs-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join("config.json");
        std::fs::write(&path, "{ 半截").expect("写假配置");
        let err = resolve_at(&path, "", 0, "").expect_err("坏 JSON 该报错");
        assert!(err.contains("读不动"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
