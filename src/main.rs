//! bililive：哔哩哔哩直播弹幕 TUI。
//!
//! 分工：`api/` 只跟 B 站说话，`ui/` 只负责画和按键，`config` 管那一个文件。

mod api;
mod cli;
mod config;
mod timefmt;
mod ui;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{mpsc, watch};

use api::client::{BiliClient, LIVE_BASE};
use api::danmaku::DanmuMsg;
use api::login::{LoginCtx, LoginEvent, PASSPORT_BASE, POLL_ATTEMPTS, POLL_GAP};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::Cli::parse(std::env::args().skip(1))?;
    if cli.help {
        println!("{}", cli::Cli::USAGE);
        return Ok(());
    }

    // 路径先算出来：扫码登录成功后要把 cookie 写回**这一份**配置。
    // 不先把路径定下来的话，写盘只能写默认路径，`-c` 指到别处时等于白登一次。
    let cfg_path = config::Config::resolve_path(cli.config.as_deref())?;
    let mut cfg = config::Config::load_or_create(Some(&cfg_path))?;
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
    // 扫码登录那条链：界面要一张码 -> 登录任务；登录任务的进展 -> 界面。
    let (login_start_tx, login_start_rx) = mpsc::channel::<()>(1);
    let (login_evt_tx, login_evt_rx) = mpsc::channel::<LoginEvent>(8);
    // 登录任务 -> 会话任务：拼好的 Cookie 串。
    let (creds_tx, creds_rx) = mpsc::channel::<String>(1);
    // 「凭据换过了」的信号。watch 里那个数本身没用，变一下就是信号 ——
    // 弹幕那条链路看到它就整条重来（见 `supervise_danmaku`）。
    let (auth_tx, auth_rx) = watch::channel(0u64);

    tokio::spawn(api::login::login_loop(
        LoginCtx {
            client: client.clone(),
            base: PASSPORT_BASE.to_string(),
            gap: POLL_GAP,
            attempts: POLL_ATTEMPTS,
            creds: creds_tx,
        },
        login_start_rx,
        login_evt_tx.clone(),
    ));
    tokio::spawn(apply_login(
        client.clone(),
        cfg_path,
        creds_rx,
        auth_tx,
        refresh_tx.clone(),
        login_evt_tx,
    ));

    if cfg.room_id > 0 {
        tokio::spawn(supervise_danmaku(
            cfg.room_id,
            client.clone(),
            danmu_tx.clone(),
            auth_rx,
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

    ui::run(
        cfg,
        ui::Wiring {
            danmaku: danmu_rx,
            room: room_rx,
            refresh: refresh_tx,
            send: send_tx,
            login_start: login_start_tx,
            login_events: login_evt_rx,
        },
    )
    .await
}

/// 看着弹幕那条链路，凭据一换就整条重来。
///
/// 不能只把新 cookie 塞给正在跑的连接：wss 的认证包（op=7）在握手时就发完了，
/// 里面带着 uid，之后没有「换个身份」这个操作。所以直接把连接连同它的重连循环
/// 一起丢掉，重新 getDanmuInfo + 重新认证
/// （Go 版 `onLogin` 里重启 getter 是同一个意思）。
async fn supervise_danmaku(
    room_id: i64,
    client: Arc<BiliClient>,
    tx: mpsc::Sender<DanmuMsg>,
    mut auth: watch::Receiver<u64>,
) {
    loop {
        let mut running =
            tokio::spawn(api::danmaku::supervisor(room_id, client.clone(), tx.clone()));
        tokio::select! {
            // 弹幕任务自己结束 = 界面没了，收工
            _ = &mut running => return,
            changed = auth.changed() => {
                if changed.is_err() {
                    return; // 发送端没了，不会再有新凭据
                }
            }
        }
        // 走到这儿 `running` 被 drop —— JoinHandle 一 drop 就是 abort。
    }
}

/// 扫码成功后把新凭据落到三个地方：内存里的 client、磁盘上的 config.toml、
/// 以及两条正在跑的网络链路。
///
/// 落盘失败**不算登录失败**：凭据已经在内存里能用了，说一声「下次启动还得重扫」
/// 就够了，不能反过来告诉用户「登录失败」让他白扫一次。
async fn apply_login(
    client: Arc<BiliClient>,
    cfg_path: PathBuf,
    mut creds: mpsc::Receiver<String>,
    auth: watch::Sender<u64>,
    refresh: mpsc::Sender<()>,
    evt: mpsc::Sender<LoginEvent>,
) {
    while let Some(cookie) = creds.recv().await {
        client.set_cookie(&cookie).await;

        // 重新读一遍再写：只覆盖 cookie 那一行，别把用户（或者在别处）改过的
        // 房间号、单行显示这些冲掉。
        let saved = config::Config::load_or_create(Some(&cfg_path)).and_then(|mut cfg| {
            cfg.cookie = cookie.clone();
            cfg.save_to(&cfg_path)
        });
        let note = match saved {
            Ok(()) => String::new(),
            Err(e) => format!("（Cookie 没能写进配置：{e}）"),
        };

        // 房间信息那条是「每轮现读 cookie」的循环，不用重启，但得补一次手刷，
        // 不然要等满 30 秒才轮到新凭据。
        api::room::refresh(&refresh);
        // 弹幕那条得整条重来（见 `supervise_danmaku`）。
        auth.send_replace(auth.borrow().wrapping_add(1));

        // nav 的缓存被 `set_cookie` 清过了，这里会拿新凭据重新问一次。
        let line = match client.nav().await {
            Ok(n) if n.is_login => format!("{} (uid {})", n.uname, n.mid),
            Ok(_) => "已登录（账号信息还没出来，回账号栏再按一次回车）".to_string(),
            Err(e) => format!("已登录，但取账号信息失败：{e}"),
        };
        if evt.send(LoginEvent::LoggedIn(line)).await.is_err() {
            return; // 界面没了
        }
        let _ = evt
            .send(LoginEvent::Hint(format!(
                "登录成功，弹幕已用新凭据重连{note}"
            )))
            .await;
    }
}
