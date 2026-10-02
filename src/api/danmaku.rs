//! 弹幕长连接：getDanmuInfo 拿 token/host_list -> wss 认证 -> 收包拆包 -> 丢进 channel。
//!
//! 两段截然不同，分开放：
//! - 上半是**纯函数**（拆包、解压、解析 JSON），能拿一段自造的字节直接断言；
//! - 下半才是网络。任何一步失败都只返回 `Err`，由 `supervisor` 变成一条系统弹幕再重连，
//!   绝不允许 panic —— TUI 一 panic 整屏都没了，用户连报错都看不见。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::mpsc::Sender;
use tokio::time::{MissedTickBehavior, sleep};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::HeaderValue;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::api::client::{BiliClient, BROWSER_HEADERS, LIVE_BASE, int_of, str_of};
use crate::api::wbi;
use crate::timefmt;

// ---------------------------------------------------------------- 协议（纯函数）

/// 包头固定 16 字节：总长(4) 魔数(2) 版本(2) 操作码(4) 序列(4)，全部大端。
pub const HEADER_LEN: usize = 16;

/// 认证包（op=7）。
pub const OP_AUTH: u32 = 7;
/// 心跳包（op=2）。
pub const OP_HEARTBEAT: u32 = 2;
/// 正常消息包（op=5），弹幕/礼物都在这里。
#[allow(dead_code)] // 协议常量：测试和下一轮的发送端都要用
pub const OP_MESSAGE: u32 = 5;

/// 心跳内容就是这 15 个字节，Go 版写的是它的十六进制形式，别改成 `{}`。
pub const HEARTBEAT_BODY: &[u8] = b"[object Object]";

/// 一次解压最多吃这么多。服务端坏了或者有人塞压缩炸弹时，
/// `read_to_end` 会一直吃内存直到 OOM —— 又是整屏消失的那种崩法。
const MAX_UNCOMPRESSED: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub ver: u16,
    pub op: u32,
    pub body: Vec<u8>,
}

/// 按 16 字节包头把一坨字节拆成包。
pub fn split_packets(raw: &[u8]) -> Vec<Packet> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + HEADER_LEN <= raw.len() {
        let len = u32::from_be_bytes([raw[pos], raw[pos + 1], raw[pos + 2], raw[pos + 3]]) as usize;
        // 长度字段坏了就停：宁可少收几条，也不能越界 panic。
        if len < HEADER_LEN || pos + len > raw.len() {
            break;
        }
        let ver = u16::from_be_bytes([raw[pos + 6], raw[pos + 7]]);
        let op = u32::from_be_bytes([raw[pos + 8], raw[pos + 9], raw[pos + 10], raw[pos + 11]]);
        out.push(Packet {
            ver,
            op,
            body: raw[pos + HEADER_LEN..pos + len].to_vec(),
        });
        pos += len;
    }
    out
}

/// 收包总入口：protover 2 的包体是 zlib 压过的「一坨包」，先解压再拆第二层。
pub fn unpack(raw: &[u8]) -> Vec<Packet> {
    let mut out = Vec::new();
    for p in split_packets(raw) {
        if p.ver == 2 {
            match zlib_decompress(&p.body) {
                Ok(plain) => out.extend(split_packets(&plain)),
                // 解不开就当这条消息没来过：报错只会变成刷屏的系统弹幕，不值得。
                Err(_) => continue,
            }
        } else {
            out.push(p);
        }
    }
    out
}

pub fn zlib_decompress(src: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut d = flate2::read::ZlibDecoder::new(src).take(MAX_UNCOMPRESSED as u64);
    let mut out = Vec::new();
    d.read_to_end(&mut out)?;
    Ok(out)
}

/// 组一个包。`magic` 恒为 16；B 站不校验序列号，固定 1 就行。
pub fn build_packet(ver: u16, op: u32, body: &[u8]) -> Vec<u8> {
    let len = (HEADER_LEN + body.len()) as u32;
    let mut out = Vec::with_capacity(len as usize);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&16u16.to_be_bytes());
    out.extend_from_slice(&ver.to_be_bytes());
    out.extend_from_slice(&op.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(body);
    out
}

pub fn heartbeat_packet() -> Vec<u8> {
    build_packet(1, OP_HEARTBEAT, HEARTBEAT_BODY)
}

/// 认证包：protover 固定 2，包体就是那串 JSON。
pub fn auth_packet(uid: i64, room_id: i64, token: &str) -> Vec<u8> {
    let body = serde_json::json!({
        "uid": uid,
        "roomid": room_id,
        "protover": 2,
        "buvid": "",
        "platform": "web",
        "type": 2,
        "key": token,
    });
    build_packet(1, OP_AUTH, body.to_string().as_bytes())
}

// ---------------------------------------------------------------- 消息

/// 系统消息的 `kind`。界面靠它把提示和真弹幕分开着色。
pub const SYSTEM_KIND: &str = "SYSTEM";

#[derive(Debug, Clone, PartialEq)]
pub struct DanmuMsg {
    pub author: String,
    pub content: String,
    /// 原始 cmd，多行模式下同一个人连着说话时用来判断要不要重打名字
    pub kind: String,
    pub time: SystemTime,
}

impl DanmuMsg {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            author: "system".to_string(),
            content: text.into(),
            kind: SYSTEM_KIND.to_string(),
            time: SystemTime::now(),
        }
    }

    pub fn is_system(&self) -> bool {
        self.kind == SYSTEM_KIND
    }
}

/// 一条消息 JSON -> 界面要的几个字段。不关心的 cmd 返回 `None`（静默丢掉）。
pub fn parse_message(body: &[u8], now: SystemTime) -> Option<DanmuMsg> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let raw_cmd = v.get("cmd")?.as_str()?;
    // DANMU_MSG 有时带后缀（`DANMU_MSG:4:0:2:2:2:0`），不剥掉整条弹幕会被当成未知 cmd 丢掉。
    let cmd = raw_cmd.split(':').next().unwrap_or(raw_cmd);

    let (author, content) = match cmd {
        "DANMU_MSG" => {
            // info[1] 是正文，info[2][1] 是昵称。Go 版直接类型断言，字段一缺就 panic；
            // 这里退一步用 uid 当名字，至少弹幕不丢。
            let mut who = str_of(&v["info"][2][1]);
            if who.is_empty() {
                who = str_of(&v["info"][2][0]);
            }
            (who, str_of(&v["info"][1]))
        }
        "SEND_GIFT" => (
            str_of(&v["data"]["uname"]),
            format!(
                "投喂了 {} 个 {}",
                int_of(&v["data"]["num"]),
                str_of(&v["data"]["giftName"])
            ),
        ),
        "COMBO_SEND" => (
            str_of(&v["data"]["uname"]),
            format!(
                "送给 {} {} 个 {}",
                str_of(&v["data"]["r_uname"]),
                int_of(&v["data"]["combo_num"]),
                str_of(&v["data"]["gift_name"])
            ),
        ),
        "GUARD_BUY" => (
            str_of(&v["data"]["username"]),
            format!("购买了 {}", str_of(&v["data"]["giftName"])),
        ),
        "INTERACT_WORD" => (str_of(&v["data"]["uname"]), "进入了房间".to_string()),
        "USER_TOAST_MSG" => ("system".to_string(), str_of(&v["data"]["toast_msg"])),
        // 通知类消息的两个文案字段名就是 msg_self / msg_common，网页端也这么取。
        "NOTICE_MSG" => {
            let text = str_of(&v["msg_self"]);
            if text.is_empty() {
                ("system".to_string(), str_of(&v["msg_common"]))
            } else {
                ("system".to_string(), text)
            }
        }
        // 其余（LIVE / ONLINE_RANK_* / PANEL / WIDGET_BANNER…）一律不看。
        _ => return None,
    };

    Some(DanmuMsg {
        author,
        content,
        kind: cmd.to_string(),
        time: now,
    })
}

// ---------------------------------------------------------------- 网络

/// 重连退避：从 1 秒开始翻倍，最多 1 分钟。Go 版写死 30 秒，
/// 被风控时每 30 秒撞一次墙；指数退避在普通掉线时也能两秒内回来。
const BASE_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// 连接活过这么久才算「稳了」，这时才把退避归位。
const HEALTHY: Duration = Duration::from_secs(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone)]
pub struct DanmuInfo {
    pub token: String,
    pub hosts: Vec<String>,
}

/// `getDanmuInfo`：必须带 WBI 签名 + 浏览器头，缺一个就返 `-352`、`host_list` 为空。
pub async fn fetch_danmu_info(
    client: &BiliClient,
    base: &str,
    room_id: i64,
    mixin_key: &str,
) -> Result<DanmuInfo> {
    let query = wbi::sign(
        &[
            ("id", room_id.to_string()),
            ("type", "0".to_string()),
            ("web_location", "444.8".to_string()),
        ],
        mixin_key,
        timefmt::now_epoch(),
    );
    let url = format!("{base}/xlive/web-room/v1/index/getDanmuInfo?{query}");
    let d = client
        .get_api(&url)
        .await
        .context("getDanmuInfo 失败（签名或浏览器头不对时返 -352）")?;

    let token = str_of(&d["token"]);
    let hosts: Vec<String> = d["host_list"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|h| str_of(&h["host"]))
                .filter(|h| !h.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if token.is_empty() || hosts.is_empty() {
        bail!("getDanmuInfo 没给出 token 或 host_list");
    }
    Ok(DanmuInfo { token, hosts })
}

async fn connect_ws(host: &str) -> Result<Ws> {
    let url = format!("wss://{host}:443/sub");
    let mut req = url.as_str().into_client_request()?;
    {
        let h = req.headers_mut();
        for (k, v) in BROWSER_HEADERS {
            h.insert(k, HeaderValue::from_static(v));
        }
        h.insert("accept-encoding", HeaderValue::from_static("gzip, deflate, br"));
    }
    let (ws, _resp) = tokio_tungstenite::connect_async(req)
        .await
        .with_context(|| format!("连 {url} 失败"))?;
    Ok(ws)
}

/// 一趟完整的「连上并收到断线」。返回 `Ok` 只表示界面没了（channel 已关）。
async fn connect_and_pump(room_id: i64, client: &BiliClient, tx: &Sender<DanmuMsg>) -> Result<()> {
    let nav = client.nav().await.context("取 nav（uid + WBI 种子）失败")?;
    let info = fetch_danmu_info(client, LIVE_BASE, room_id, &nav.mixin_key).await?;

    // host_list 挨个试。全试完还连不上要报「试了几个」，
    // 否则用户只看到一句 dial failed，不知道是网络还是列表空了。
    let mut last_err: Option<anyhow::Error> = None;
    let mut ws = None;
    for host in &info.hosts {
        match connect_ws(host).await {
            Ok(w) => {
                ws = Some(w);
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let Some(mut ws) = ws else {
        bail!(
            "没有可用的弹幕服务器（host_list 共 {} 个）{}",
            info.hosts.len(),
            last_err.map(|e| format!("：{e:#}")).unwrap_or_default()
        );
    };

    ws.send(Message::Binary(auth_packet(nav.mid, room_id, &info.token).into()))
        .await
        .context("发认证包失败")?;

    pump(ws, tx).await
}

async fn pump(mut ws: Ws, tx: &Sender<DanmuMsg>) -> Result<()> {
    let mut hb = tokio::time::interval(HEARTBEAT_INTERVAL);
    hb.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // interval 的第一个 tick 是立刻就到的，先吃掉，否则刚连上就白发一个心跳。
    hb.tick().await;

    loop {
        tokio::select! {
            _ = hb.tick() => {
                // 心跳发不出去说明连接已经废了，别干等读超时，直接走重连。
                ws.send(Message::Binary(heartbeat_packet().into()))
                    .await
                    .context("发心跳失败")?;
            }
            msg = ws.next() => {
                let Some(msg) = msg else {
                    bail!("弹幕服务器关闭了连接");
                };
                match msg? {
                    Message::Binary(data) => {
                        for p in unpack(&data) {
                            match parse_message(&p.body, SystemTime::now()) {
                                // 空白正文（有人发空格刷屏）别往界面上灌。
                                Some(m) if !m.content.trim().is_empty() => {
                                    tx.send(m).await.map_err(|_| anyhow!("界面已关闭"))?;
                                }
                                _ => {}
                            }
                        }
                    }
                    // 文本帧只在协议没协商好时出现，能解析就收。
                    Message::Text(t) => {
                        if let Some(m) = parse_message(t.as_bytes(), SystemTime::now())
                            && !m.content.trim().is_empty()
                        {
                            tx.send(m).await.map_err(|_| anyhow!("界面已关闭"))?;
                        }
                    }
                    // tungstenite 自己也会回 pong，这里再明确回一次，双份无害。
                    Message::Ping(p) => ws.send(Message::Pong(p)).await?,
                    Message::Close(_) => bail!("弹幕服务器要求关闭连接"),
                    _ => {}
                }
            }
        }
    }
}

/// 入口。永不返回错误，也永不 panic：挂了就写一条系统弹幕，然后退避重连。
pub async fn supervisor(room_id: i64, client: Arc<BiliClient>, tx: Sender<DanmuMsg>) {
    {
        let client = client.clone();
        let tx = tx.clone();
        tokio::spawn(async move { fetch_history(&client, LIVE_BASE, room_id, &tx).await });
    }

    let mut backoff = BASE_BACKOFF;
    loop {
        let started = Instant::now();
        match connect_and_pump(room_id, &client, &tx).await {
            // 只有「界面关了」才会走到这儿。
            Ok(()) => return,
            Err(e) => {
                if tx.is_closed() {
                    return;
                }
                if started.elapsed() >= HEALTHY {
                    backoff = BASE_BACKOFF;
                }
                let msg = format!("弹幕连接断开（{} 秒后重连）: {e:#}", backoff.as_secs());
                if tx.send(DanmuMsg::system(msg)).await.is_err() {
                    return;
                }
            }
        }
        sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// 历史弹幕是**进程级**只拉一次：重连再拉会把同一批内容重放一遍，
/// 屏幕上看就是「怎么同样的弹幕又来了」。
static HISTORY_SHOWN: AtomicBool = AtomicBool::new(false);

async fn fetch_history(client: &BiliClient, base: &str, room_id: i64, tx: &Sender<DanmuMsg>) {
    if HISTORY_SHOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    let url = format!("{base}/xlive/web-room/v1/dM/gethistory?roomid={room_id}");
    match client.get_api(&url).await {
        Ok(d) => {
            for h in d["room"].as_array().into_iter().flatten() {
                let content = str_of(&h["text"]);
                if content.trim().is_empty() {
                    continue;
                }
                // timeline 是北京时间的墙上时间，当成 UTC 收下会差 8 小时。
                let time = timefmt::beijing_epoch(&str_of(&h["timeline"]))
                    .and_then(|s| u64::try_from(s).ok())
                    .and_then(|s| UNIX_EPOCH.checked_add(Duration::from_secs(s)))
                    .unwrap_or_else(SystemTime::now);
                let msg = DanmuMsg {
                    author: str_of(&h["nickname"]),
                    content,
                    kind: "DANMU_MSG".to_string(),
                    time,
                };
                if tx.send(msg).await.is_err() {
                    return;
                }
            }
        }
        // 没拉到就把旗子放回去，让下次重连再试一遍。
        Err(_) => HISTORY_SHOWN.store(false, Ordering::SeqCst),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const DANMU: &str = r#"{"cmd":"DANMU_MSG","info":[[0,1,25,16777215,1700000000000,0,0,"",0,0,0],"你好啊",[12345,"小明",0,0,0,10000,1,""]]}"#;
    const GIFT: &str = r#"{"cmd":"SEND_GIFT","data":{"uname":"小红","num":3,"giftName":"辣条"}}"#;

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn json_packet(payload: &str) -> Vec<u8> {
        build_packet(1, OP_MESSAGE, payload.as_bytes())
    }

    #[test]
    fn packet_round_trip() {
        let raw = build_packet(1, OP_MESSAGE, b"hello");
        let packets = split_packets(&raw);
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].ver, 1);
        assert_eq!(packets[0].op, OP_MESSAGE);
        assert_eq!(packets[0].body, b"hello");
        // 总长写在大端前 4 字节里
        assert_eq!(&raw[..4], &((HEADER_LEN + 5) as u32).to_be_bytes());
    }

    /// protover 2 是「外层包 + zlib + 内层若干个包」，这条链路是收弹幕最容易写错的一段。
    #[test]
    fn unpack_inflates_protover2_batch() {
        let mut inner = Vec::new();
        inner.extend_from_slice(&json_packet(DANMU));
        inner.extend_from_slice(&json_packet(GIFT));
        let outer = build_packet(2, OP_MESSAGE, &zlib(&inner));

        let packets = unpack(&outer);
        assert_eq!(packets.len(), 2, "两个内层包都要拆出来");

        let a = parse_message(&packets[0].body, UNIX_EPOCH).expect("拆出来的第一包该是弹幕");
        assert_eq!(a.author, "小明");
        assert_eq!(a.content, "你好啊");
        assert_eq!(a.kind, "DANMU_MSG");

        let b = parse_message(&packets[1].body, UNIX_EPOCH).unwrap();
        assert_eq!(b.author, "小红");
        assert_eq!(b.content, "投喂了 3 个 辣条");
        assert_eq!(b.kind, "SEND_GIFT");
    }

    /// 未压缩的单包（protover 1）也要能收。
    #[test]
    fn unpack_passes_through_plain_packet() {
        let packets = unpack(&json_packet(DANMU));
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].ver, 1);
    }

    /// 心跳回复这类没有 cmd 的包必须安静丢掉，不能 panic 也不能变成空弹幕。
    #[test]
    fn non_message_packets_are_ignored() {
        let mut raw = build_packet(1, 3, br#"{"code":0}"#);
        raw.extend_from_slice(&json_packet(r#"{"cmd":"ONLINE_RANK_COUNT","data":{}}"#));
        raw.extend_from_slice(&build_packet(2, OP_MESSAGE, &zlib(&json_packet(DANMU))));
        let msgs: Vec<_> = unpack(&raw)
            .iter()
            .filter_map(|p| parse_message(&p.body, UNIX_EPOCH))
            .collect();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "你好啊");
    }

    #[test]
    fn danmu_cmd_suffix_is_stripped() {
        let payload = r#"{"cmd":"DANMU_MSG:4:0:2:2:2:0","info":[[],"后缀",[1,"阿强"]]}"#;
        let m = parse_message(payload.as_bytes(), UNIX_EPOCH).unwrap();
        assert_eq!(m.author, "阿强");
        assert_eq!(m.content, "后缀");
    }

    /// 正文缺字段时用 uid 顶着，不能因为类型不符就崩（Go 版是直接类型断言）。
    #[test]
    fn danmu_without_nickname_falls_back_to_uid() {
        let payload = r#"{"cmd":"DANMU_MSG","info":[[],"没有昵称",[998877]]}"#;
        let m = parse_message(payload.as_bytes(), UNIX_EPOCH).unwrap();
        assert_eq!(m.author, "998877");
    }

    #[test]
    fn truncated_and_garbage_input_does_not_panic() {
        // 长度字段比实际字节多：多出来的部分必须被丢掉，而不是越界读
        let mut bad = build_packet(1, OP_MESSAGE, b"abc");
        bad[..4].copy_from_slice(&9999u32.to_be_bytes());
        assert!(split_packets(&bad).is_empty());
        // 长度字段是 0 / 小于包头
        let mut zero = build_packet(1, OP_MESSAGE, b"abc");
        zero[..4].copy_from_slice(&0u32.to_be_bytes());
        assert!(split_packets(&zero).is_empty());
        // 头都没凑齐
        assert!(split_packets(&[0, 1, 2]).is_empty());
        assert!(unpack(b"\x00\x01\x02").is_empty());
        // 声明是 zlib 其实不是
        assert!(unpack(&build_packet(2, OP_MESSAGE, b"not zlib at all")).is_empty());
        assert!(parse_message(b"{not json", UNIX_EPOCH).is_none());
    }

    /// 压缩炸弹不能把内存吃干：解压出来的字节数要有上限。
    #[test]
    fn decompression_is_capped() {
        let bomb = zlib(&vec![0u8; MAX_UNCOMPRESSED + 1024]);
        let out = zlib_decompress(&bomb).unwrap();
        assert_eq!(out.len(), MAX_UNCOMPRESSED);
    }

    #[test]
    fn auth_packet_shape() {
        let raw = auth_packet(7, 9527, "tok");
        let p = &split_packets(&raw)[0];
        assert_eq!(p.ver, 1);
        assert_eq!(p.op, OP_AUTH);
        let v: Value = serde_json::from_slice(&p.body).unwrap();
        assert_eq!(v["uid"], 7);
        assert_eq!(v["roomid"], 9527);
        assert_eq!(v["protover"], 2);
        assert_eq!(v["key"], "tok");
    }

    #[test]
    fn heartbeat_body_is_the_go_one() {
        let raw = heartbeat_packet();
        let p = &split_packets(&raw)[0];
        assert_eq!(p.op, OP_HEARTBEAT);
        assert_eq!(p.body, b"[object Object]");
    }
}
