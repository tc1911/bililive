//! bililive：哔哩哔哩直播弹幕 TUI。
//!
//! 分工：`api/` 只跟 B 站说话，`ui/` 只负责画和按键，`config` 管那一个文件。

mod api;
mod cli;
mod config;
mod timefmt;
mod ui;

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;

use api::client::{BiliClient, LIVE_BASE};
use api::danmaku::DanmuMsg;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::Cli::parse(std::env::args().skip(1))?;
    if cli.help {
        println!("{}", cli::Cli::USAGE);
        return Ok(());
    }

    let mut cfg = config::Config::load_or_create(cli.config.as_deref())?;
    if let Some(room) = cli.room {
        // 只改这一次运行的房间号。写回配置的话，「我就瞄一眼别的房间」
        // 会变成「我的默认房间被改了」，太意外。
        cfg.room_id = room;
    }

    let client = Arc::new(BiliClient::new(&cfg.cookie)?);

    // 两条链路各一个 channel，跟 Go 版一样：弹幕（含系统提示）和房间信息。
    // 弹幕用有界 channel，界面卡住时最多堆这么多条，不会把内存吃光。
    let (danmu_tx, danmu_rx) = mpsc::channel::<DanmuMsg>(1024);
    let (room_tx, room_rx) = mpsc::channel(16);
    // 容量 1 的手动刷新信号：连按几下也只补一次，而且发送方永不阻塞。
    let (refresh_tx, refresh_rx) = mpsc::channel::<()>(1);
    // 界面 -> 发送端。有界：界面卡住时最多堆这么多条，不会把内存吃光；
    // 界面那侧用 try_send，永远不等发送端（发送端在切段之间要 sleep 1 秒）。
    let (send_tx, send_rx) = mpsc::channel::<String>(32);

    if cfg.room_id > 0 {
        tokio::spawn(api::danmaku::supervisor(
            cfg.room_id,
            client.clone(),
            danmu_tx.clone(),
        ));
        // 发弹幕跟收弹幕共用一个 client（同一份 cookie 和请求头）。
        tokio::spawn(api::send::send_loop(
            client.clone(),
            LIVE_BASE.to_string(),
            cfg.room_id,
            send_rx,
            danmu_tx.clone(),
        ));
        tokio::spawn(api::room::sync_loop(
            cfg.room_id,
            client,
            LIVE_BASE.to_string(),
            refresh_rx,
            room_tx,
        ));
    } else {
        // 房间号没设就别去戳接口了，直接在弹幕框里说清楚该改哪儿 ——
        // 否则用户看到的是一屏「弹幕连接断开」，不知道是自己没配置。
        let hint = format!(
            "还没设置直播间号：把 {} 里的 room_id 改成你要看的房间",
            config::Config::path()?.display()
        );
        let _ = danmu_tx.send(DanmuMsg::system(hint)).await;
    }

    ui::run(cfg, danmu_rx, room_rx, refresh_tx, send_tx).await
}
