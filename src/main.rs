//! bililive：哔哩哔哩直播弹幕 TUI。
//!
//! 分工：`api/` 只跟 B 站说话，`ui/` 只负责画和按键，`config` 管那一个文件。

mod api;
mod cli;
mod config;
mod obs;
mod timefmt;
mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{mpsc, watch};

use api::area::{AreaEvent, AreaRequest};
use api::client::{BiliClient, LIVE_BASE};
use api::danmaku::DanmuMsg;
use api::info::{InfoEvent, InfoRequest};
use api::live::{LiveAction, LiveEvent, LiveRequest};
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
    // 分区那条链：界面要表 / 要选定 -> 分区任务；分区任务 -> 界面。
    // 容量 4 是故意的：拉表还在飞的时候用户又按了回车选定，那一下不能被丢掉。
    let (area_tx, area_rx) = mpsc::channel::<AreaRequest>(4);
    let (area_evt_tx, area_evt_rx) = mpsc::channel::<AreaEvent>(8);
    // 直播间信息那条链：拉当前标题 / 改标题 / 换封面 -> 信息任务；信息任务 -> 界面。
    // 事件容量开 4：里面可能带着一整张封面图（几 MB），别让队列把内存吃掉一半。
    let (info_tx, info_rx) = mpsc::channel::<InfoRequest>(4);
    let (info_evt_tx, info_evt_rx) = mpsc::channel::<InfoEvent>(4);
    // 开播那条链：界面 -> 开播任务（查状态 / 开播 / 下播），开播任务 -> 界面。
    // 容量 4：开播和下播只有确认那一下会发，正常按不出「忙」；
    // 事件容量给小一点 —— 里面装着推流凭据（几百字节），用不着排队。
    let (live_tx, live_rx) = mpsc::channel::<LiveRequest>(4);
    let (live_evt_tx, live_evt_rx) = mpsc::channel::<LiveEvent>(8);
    // OBS 联动那条：开播任务 -> 界面，只有一行字（填好了 / 没填上的原话）。
    // 容量 4 够用：开播那一下最多来一条，它还比 `LiveEvent::Started` 晚几秒。
    let (obs_note_tx, obs_note_rx) = mpsc::channel::<String>(4);
    // 登录任务 -> 会话任务：拼好的 Cookie 串。
    let (creds_tx, creds_rx) = mpsc::channel::<String>(1);
    // 界面 -> 会话任务：退出登录。容量 1，跟手动刷新一个口径 ——
    // 连按几下也只排一次，而且界面那侧永不阻塞（它跑在事件循环里）。
    let (logout_tx, logout_rx) = mpsc::channel::<()>(1);
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
    tokio::spawn(session_task(
        client.clone(),
        cfg_path.clone(),
        CredentialPaths {
            creds: creds_rx,
            logout: logout_rx,
            // 退出登录之后顺手替用户要一张新码，别让他再去找键。
            // 这个发送端界面那边也留着一份（`Wiring::login_start`），克隆过去就行。
            login_start: login_start_tx.clone(),
            auth: auth_tx,
            refresh: refresh_tx.clone(),
        },
        login_evt_tx,
    ));
    tokio::spawn(area_task(
        client.clone(),
        LIVE_BASE.to_string(),
        cfg_path,
        area_rx,
        area_evt_tx,
    ));
    tokio::spawn(info_task(
        client.clone(),
        LIVE_BASE.to_string(),
        cfg.room_id,
        refresh_tx.clone(),
        info_rx,
        info_evt_tx,
    ));
    tokio::spawn(live_task(
        client.clone(),
        LIVE_BASE.to_string(),
        cfg.room_id,
        refresh_tx.clone(),
        ObsFill {
            cfg: obs::Config {
                fill: cfg.obs_fill,
                host: cfg.obs_host.clone(),
                port: cfg.obs_port,
                password: cfg.obs_password.clone(),
            },
            notes: obs_note_tx,
        },
        live_rx,
        live_evt_tx,
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
            logout: logout_tx,
            area: area_tx,
            area_events: area_evt_rx,
            info: info_tx,
            info_events: info_evt_rx,
            live: live_tx,
            live_events: live_evt_rx,
            obs_notes: obs_note_rx,
        },
    )
    .await
}

/// 分区那条链路：拉表（网络）和选定（落盘）都从这一条通道进来。
///
/// 两种请求合在一个任务里跑是省事，也是**分工**：界面两件事都不许自己干
/// （不碰网络、不碰磁盘），所以都在这一层落地。拉一次不到一秒，写盘是一次小文件读写，
/// 谁也不等谁；拆成两个任务只会多两处 join。
async fn area_task(
    client: Arc<BiliClient>,
    base: String,
    cfg_path: PathBuf,
    mut req: mpsc::Receiver<AreaRequest>,
    evt: mpsc::Sender<AreaEvent>,
) {
    while let Some(r) = req.recv().await {
        let out = match r {
            AreaRequest::Load => match api::area::fetch_areas(&client, &base).await {
                Ok(areas) => AreaEvent::Loaded(areas),
                Err(e) => AreaEvent::Failed(e.to_string()),
            },
            // 只覆盖 area_id / area_name 两个字段（`save_area` 内部**先读一遍再写**，
            // 别在这儿改成拿启动时那份配置整个覆盖 —— 会把刚扫的登录冲掉）。
            AreaRequest::Pick { id, name } => {
                match config::Config::save_area(&cfg_path, id, &name) {
                    Ok(()) => AreaEvent::Saved { name, error: None },
                    Err(e) => AreaEvent::Saved {
                        name,
                        error: Some(e.to_string()),
                    },
                }
            }
        };
        if evt.send(out).await.is_err() {
            return; // 界面没了
        }
    }
}

/// 直播间信息那条链：拉当前标题 / 封面（只读）、改标题、换封面（写）。
///
/// 三件事都从一条 `mpsc<InfoRequest>` 进来，理由跟 `area_task` 一样：
/// 界面不许自己干这些（不碰网络、不碰磁盘），所以「谁先谁后」只能落在这儿。
///
/// **换封面是两步，顺序不能反**：先把本地图传到 B 站图床拿到 `.hdslb.com` 地址，
/// 再拿那个地址去 `UpdatePreLiveInfo`。别处的链接服务端一律回 `100402`，
/// 所以「直接填一个链接」那条路也只能是 hdslb 的。
async fn info_task(
    client: Arc<BiliClient>,
    live_base: String,
    room_id: i64,
    refresh: mpsc::Sender<()>,
    mut req: mpsc::Receiver<InfoRequest>,
    evt: mpsc::Sender<InfoEvent>,
) {
    // 已经抓过的封面地址。同一张图不重复抓：来回切栏、终端拉伸都不该重新下一次图片。
    let mut fetched_cover = String::new();
    // 改标题要用的房间号。配置里那个可能是短号（比如 `6`），`get_info` 才告诉我们
    // 真正的房间号（短号也查得到，但写操作拿规范号更稳）—— 拉到过就用拉到的那个。
    let mut live_room_id = room_id;

    while let Some(r) = req.recv().await {
        let events: Vec<InfoEvent> = match r {
            InfoRequest::LoadMeta => {
                // 房间号是 0 的时候 `get_info` 只会换回一个没用的错，直接在界面上说清楚
                if room_id <= 0 {
                    vec![InfoEvent::MetaFailed(
                        "还没设置直播间号（config.toml 里的 room_id）".to_string(),
                    )]
                } else {
                    match api::room::fetch_room_info(&client, &live_base, room_id).await {
                        Ok(info) => {
                            if info.room_id > 0 {
                                live_room_id = info.room_id;
                            }
                            // 只补协议相对那一种（`//i0.hdslb.com/...`）；`http://` 不动，
                            // 抓图本来就走 http 也能成（Go 版 `ui/cover` 的 normalize 也只做这个）
                            let cover = api::info::absolute_image_url(&info.cover);
                            let mut out = vec![InfoEvent::Meta {
                                title: info.title,
                                cover: cover.clone(),
                            }];
                            out.extend(cover_event(&client, &cover, &mut fetched_cover).await);
                            out
                        }
                        // 读不到不等于改不了：界面拿这句话提示一下，输入框照用
                        Err(e) => vec![InfoEvent::MetaFailed(e.to_string())],
                    }
                }
            }

            InfoRequest::SetTitle(title) => {
                let error = api::info::update_title(&client, &live_base, live_room_id, &title)
                    .await
                    .err()
                    .map(|e| e.to_string());
                if error.is_none() {
                    // 刚改完就让主页那条链路重拉一次，别让人对着旧标题等满 30 秒
                    //（服务端也可能要几秒才生效，那就等下一轮）
                    api::room::refresh(&refresh);
                }
                vec![InfoEvent::Title { title, error }]
            }

            InfoRequest::SetCover(src) => {
                let mut resolved = api::info::normalize_image_url(&src);
                let mut error = None;
                // 填的是链接就直接用；否则当本地路径先传图床。
                // 图床在主站（`api.bilibili.com`），不在直播那台机器上。
                if !api::info::is_link(&src) {
                    match api::info::upload_image(&client, client.main_base(), &src).await {
                        Ok(url) => resolved = url,
                        Err(e) => error = Some(format!("传图床失败：{e}")),
                    }
                }
                // 第一步没过就别去写第二步：写上去的那个地址根本不是图床的，只会换个 100402 回来
                if error.is_none() {
                    error = api::info::update_cover(&client, &live_base, &resolved)
                        .await
                        .err()
                        .map(|e| e.to_string());
                }
                let mut out = vec![InfoEvent::Cover {
                    cover: resolved.clone(),
                    error: error.clone(),
                }];
                if error.is_none() {
                    // 同上：封面换完顺手让房间信息那一栏也重拉一次
                    api::room::refresh(&refresh);
                    out.extend(cover_event(&client, &resolved, &mut fetched_cover).await);
                }
                out
            }
        };

        for e in events {
            if evt.send(e).await.is_err() {
                return; // 界面没了
            }
        }
    }
}

/// 开播那条链：查开播状态（只读）、开播、下播。
///
/// 三件事都从一条 `mpsc<LiveRequest>` 进来，理由跟 `area_task` / `info_task` 一样：
/// 界面不许自己发请求。不同的是**开播会改账号状态**（直播间立刻对外可见、给粉丝推推送），
/// 所以那一下只能由用户按 F4、走完确认层之后才发得出来。
///
/// 写操作（开播 / 下播）用的房间号必须是 `get_info` 回的**规范号**：配置里那个可能是短号，
/// 短号也查得到，但写操作拿规范号更稳（上一轮已经证实这俩不是一个数）。
///
/// 开播成功后还会顺手做一件跟 B 站无关的事：把推流凭据填进 OBS（`obs_fill` 那一包），
/// 见 `spawn_obs_fill`。
async fn live_task(
    client: Arc<BiliClient>,
    base: String,
    room_id: i64,
    refresh: mpsc::Sender<()>,
    obs_fill: ObsFill,
    mut req: mpsc::Receiver<LiveRequest>,
    evt: mpsc::Sender<LiveEvent>,
) {
    // 一旦 `get_info` 回过规范号就用它，别再用配置里那个短号。
    let mut canonical = room_id;

    while let Some(r) = req.recv().await {
        let out = match r {
            LiveRequest::LoadStatus => match canonical_room(&client, &base, canonical).await {
                Ok((id, info)) => {
                    canonical = id;
                    LiveEvent::Status {
                        live_status: info.live_status,
                    }
                }
                Err(e) => LiveEvent::Failed {
                    action: LiveAction::Status,
                    message: e.to_string(),
                },
            },
            LiveRequest::Start { area_v2 } => {
                // 开播之前先把规范房间号拿到手：`startLive` 是写操作，拿短号只会白挨一次错。
                match canonical_room(&client, &base, canonical).await {
                    Ok((id, _)) => canonical = id,
                    Err(e) => {
                        let out = LiveEvent::Failed {
                            action: LiveAction::Start,
                            message: e.to_string(),
                        };
                        if evt.send(out).await.is_err() {
                            return;
                        }
                        continue;
                    }
                }
                match api::live::start_live(&client, &base, canonical, area_v2).await {
                    Ok(api::live::StartOutcome::Started(streams)) => {
                        // 主页那格「obs 推流状态」也走这条只读链，顺手让它重拉一次，
                        // 别让人对着一句「未开播」等满 30 秒。
                        api::room::refresh(&refresh);
                        // OBS 那件事**甩到另一条任务上**（见 `spawn_obs_fill`）：
                        // 它要连网络、还可能等到超时，挡在这儿就等于「OBS 没开机时
                        // 开播成功也要多等几秒才显示出来」。
                        spawn_obs_fill(&obs_fill.cfg, &streams, &obs_fill.notes);
                        LiveEvent::Started(streams)
                    }
                    Ok(api::live::StartOutcome::Verify { kind, url, message }) => {
                        LiveEvent::Verify { kind, url, message }
                    }
                    Err(e) => LiveEvent::Failed {
                        action: LiveAction::Start,
                        message: e.to_string(),
                    },
                }
            }
            LiveRequest::Stop => {
                // 下播也是写操作，同样得用规范号。**不能**拿配置里那个短号发出去
                // ——哪怕用户一进来就直接按 F5（那时手上还没有 get_info 的结果）。
                match canonical_room(&client, &base, canonical).await {
                    Ok((id, _)) => canonical = id,
                    Err(e) => {
                        let out = LiveEvent::Failed {
                            action: LiveAction::Stop,
                            message: e.to_string(),
                        };
                        if evt.send(out).await.is_err() {
                            return;
                        }
                        continue;
                    }
                }
                match api::live::stop_live(&client, &base, canonical).await {
                    Ok(()) => {
                        api::room::refresh(&refresh);
                        LiveEvent::Stopped
                    }
                    Err(e) => LiveEvent::Failed {
                        action: LiveAction::Stop,
                        message: e.to_string(),
                    },
                }
            }
        };
        if evt.send(out).await.is_err() {
            return; // 界面没了
        }
    }
}

/// 开播成功后顺手要做的 OBS 那一件事：开关 + 连哪儿 + 把结果写成一行字的那条通道。
///
/// 打成一个包只是因为 `live_task` 的参数已经够多了（一个个摆出来还得两头对顺序）。
struct ObsFill {
    cfg: obs::Config,
    notes: mpsc::Sender<String>,
}

/// 开播成功后把推流凭据填进 OBS 的「设置 → 推流」。**只管填，不管推**：
/// 填完绝不代按「开始推流」，那一下得用户自己在 OBS 里按。
///
/// 另起一条任务：连 OBS 可能要等到超时，挡在开播那条链上就等于「OBS 没开机时，
/// 开播成功也要多等几秒才显示出来」。结果只变成 `notes` 里的一行字
///（界面把它摆在推流码栏末尾），**绝不回头改这次开播的成败**。
fn spawn_obs_fill(
    obs_cfg: &obs::Config,
    streams: &[api::live::Stream],
    notes: &mpsc::Sender<String>,
) {
    if !obs_cfg.fill {
        return;
    }
    // **第一路 rtmp**：OBS 的「设置 → 推流」只吃 rtmp，srt 那几路填进去它也不认
    //（B 站给的主推流就是 `rtmp` 那一组，协议字段在 `api::live::Stream::protocol`）。
    let Some(s) = streams.iter().find(|s| s.protocol == "rtmp") else {
        let _ = notes.try_send("OBS 没填上：这次开播没给 rtmp 推流地址".to_string());
        return;
    };
    let cfg = obs_cfg.clone();
    // 服务器和密钥原样搬过去：`key` 里自带 `?streamname=…`，谁都不许在这儿拼一遍
    //（拼错的表现是 OBS 里一按「开始推流」就断，而界面上看不出任何异常）。
    let (server, key) = (s.address.clone(), s.key.clone());
    let notes = notes.clone();
    tokio::spawn(async move {
        let text = match obs::fill(&cfg, &server, &key).await {
            Ok(()) => "OBS：已把推流地址与密钥填进「设置 → 推流」，开始推流还是你自己按".to_string(),
            Err(e) => format!("OBS 没填上：{e}"),
        };
        let _ = notes.send(text).await;
    });
}

/// 拿写操作要用的**规范房间号**，顺手把 `get_info` 一起查了。
///
/// 房间号是启动时读进内存的，写操作要用服务端自己认的那一个（配置里可能是短号）。
/// 房间号是 0 时一个请求都不发 —— 直接说清楚该改哪儿。
async fn canonical_room(
    client: &BiliClient,
    base: &str,
    room_id: i64,
) -> anyhow::Result<(i64, api::room::RoomInfo)> {
    if room_id <= 0 {
        anyhow::bail!("还没设置直播间号（config.toml 里的 room_id）");
    }
    let info = api::room::fetch_room_info(client, base, room_id).await?;
    let id = if info.room_id > 0 { info.room_id } else { room_id };
    Ok((id, info))
}

/// 「凭据变了」—— 弹幕那条链路看到这个信号就整条重来（见 `supervise_danmaku`）。
///
/// 单独开一个函数只因为这儿埋着一个**自锁**：写成
/// `auth.send_replace(auth.borrow().wrapping_add(1))` 时，`borrow()` 的读借用
/// 会活到整条语句结束，而 `send_replace` 要拿同一把锁的写权 ——
/// 同一个线程就把自己锁死了（parking_lot 的读写锁不认重入）。
/// 现象是**扫码登录 / 退出登录成功那一刻进程一声不响地卡住**，界面上什么都不再动。
/// 原来 `apply_login` 里就是这么写的，这一轮加退出登录时测试才把它逼出来。
fn signal_credential_change(auth: &watch::Sender<u64>) {
    auth.send_modify(|v| *v = v.wrapping_add(1));
}

/// 顺手把封面图抓回来给预览用。
///
/// 抓失败**不算改封面失败**（那一步早就成功了），只让预览那一格写一句话，
/// 所以这里返回的是一个事件而不是 `Result`。空地址、抓过的地址都不抓。
async fn cover_event(
    client: &BiliClient,
    url: &str,
    fetched: &mut String,
) -> Option<InfoEvent> {
    if url.is_empty() || url == fetched {
        return None;
    }
    *fetched = url.to_string();
    Some(match client.get_bytes(url).await {
        Ok(bytes) => InfoEvent::CoverImage {
            url: url.to_string(),
            bytes,
        },
        Err(e) => InfoEvent::CoverImageFailed {
            url: url.to_string(),
            error: e.to_string(),
        },
    })
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

/// `session_task` 手上那几条窗户。
///
/// 打成包（跟 `Wiring` / `ObsFill` 一个理由）：凭据一变，这个任务要同时通知
/// 弹幕那条链路、房间信息那条，还得能跟扫码那条说上话 —— 一个个摆出来就是八个参数，
/// 调用方和这里得永远保持同一个顺序，改一个就得两头对一遍。
struct CredentialPaths {
    /// 登录任务 -> 这里：拼好的 Cookie 串
    creds: mpsc::Receiver<String>,
    /// 界面 -> 这里：退出登录
    logout: mpsc::Receiver<()>,
    /// 这里 -> 登录任务：退完顺手再要一张码
    login_start: mpsc::Sender<()>,
    /// 这里 -> 弹幕那条链路：凭据变了，整条重来
    auth: watch::Sender<u64>,
    /// 这里 -> 房间信息那条：立刻重拉一次
    refresh: mpsc::Sender<()>,
}

/// 凭据变了之后要落地的那一串事。**扫码登录和退出登录都从这儿走**。
///
/// 两件事的落点一模一样：内存里的 client、磁盘上的 config.toml、弹幕那条链路。
/// 分成两个任务写就会各自漏一处 —— 比如退出时忘了重启弹幕，现象是
/// 「界面说退了，弹幕还挂着旧身份在跑」，最难查的一种。
///
/// 落盘失败**不算登录 / 退出失败**：内存里那套已经生效了，说一声
/// 「下次启动还得重扫 / 配置里没清干净」就够了，不能反过来告诉用户操作失败。
async fn session_task(
    client: Arc<BiliClient>,
    cfg_path: PathBuf,
    paths: CredentialPaths,
    evt: mpsc::Sender<LoginEvent>,
) {
    let CredentialPaths {
        mut creds,
        mut logout,
        login_start,
        auth,
        refresh,
    } = paths;
    loop {
        tokio::select! {
            Some(cookie) = creds.recv() => {
                apply_login(&client, &cfg_path, cookie, &auth, &refresh, &evt).await;
                // 界面没了（`evt.send` 失败）就收工，别再管下一条。
                if evt.is_closed() {
                    return;
                }
            }
            Some(()) = logout.recv() => {
                apply_logout(&client, &cfg_path, &login_start, &auth, &refresh, &evt).await;
                if evt.is_closed() {
                    return;
                }
            }
            // 两条通道都没了 = 界面没了，收工
            else => return,
        }
    }
}

/// 扫码成功后把新凭据落到三个地方：内存里的 client、磁盘上的 config.toml、
/// 以及两条正在跑的网络链路。
async fn apply_login(
    client: &BiliClient,
    cfg_path: &Path,
    cookie: String,
    auth: &watch::Sender<u64>,
    refresh: &mpsc::Sender<()>,
    evt: &mpsc::Sender<LoginEvent>,
) {
    client.set_cookie(&cookie).await;

    // 重新读一遍再写：只覆盖 cookie 那一行，别把用户（或者在别处）改过的
    // 房间号、单行显示这些冲掉。
    let saved = config::Config::load_or_create(Some(cfg_path)).and_then(|mut cfg| {
        cfg.cookie = cookie.clone();
        cfg.save_to(cfg_path)
    });
    let note = match saved {
        Ok(()) => String::new(),
        Err(e) => format!("（Cookie 没能写进配置：{e}）"),
    };

    // 房间信息那条是「每轮现读 cookie」的循环，不用重启，但得补一次手刷，
    // 不然要等满 30 秒才轮到新凭据。
    api::room::refresh(refresh);
    // 弹幕那条得整条重来（见 `supervise_danmaku`）。
    signal_credential_change(auth);

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

/// 退出登录：把本地凭据清掉，并把弹幕那条链路整条重启。
///
/// 四件事按顺序：**内存里的凭据** -> **配置里那一行** -> **两条网络链路** ->
/// **界面**。房间信息那条不用重启（它每轮现读 cookie），补一次手刷就行。
///
/// 特别说明：清掉之后这一场的弹幕会断到重新扫码为止 —— 这正是确认层那句文案
/// 要跟用户说清楚的事，所以这里不能「悄悄清、悄悄不断」。
async fn apply_logout(
    client: &BiliClient,
    cfg_path: &Path,
    login_start: &mpsc::Sender<()>,
    auth: &watch::Sender<u64>,
    refresh: &mpsc::Sender<()>,
    evt: &mpsc::Sender<LoginEvent>,
) {
    // 1. 内存里那套凭据（连带 nav 缓存）先清掉：之后所有请求都是匿名的。
    //    client 是 Arc 共享的，清的是里面那块 `Mutex<Auth>` ——
    //    弹幕 / 房间 / 发送三条链路都跟着变。
    client.set_cookie("").await;

    // 2. 磁盘上**只清 cookie 那一行**（先重读再写，见 `Config::clear_cookie`）：
    //    房间号 / 分区 / obs_* 都得原样留着。
    let note = match config::Config::clear_cookie(cfg_path) {
        Ok(()) => String::new(),
        Err(e) => format!("（配置里的 Cookie 没能清掉：{e}）"),
    };

    // 3. 两条网络链路。房间信息补一次手刷；弹幕那条整条重来 ——
    //    理由跟登录时一样：wss 的认证包在握手时就发完了，没有「换个身份」这一步。
    api::room::refresh(refresh);
    signal_credential_change(auth);

    // 4. 界面：账号那行回到「未登录（回车扫码）」。
    if evt
        .send(LoginEvent::LoggedOut("未登录（回车扫码）".into()))
        .await
        .is_err()
    {
        return; // 界面没了
    }
    // 顺手替用户要一张新码（跟账号栏那个「重新扫码」走同一条信号），
    // 别让他退出完还得到处找键。扫码任务正忙的时候这一下会被丢掉 —— 也不要紧，
    // 界面上「重新扫码」还在，退出的结果已经落地了。
    let _ = login_start.try_send(());
    let _ = evt
        .send(LoginEvent::Hint(format!(
            "已退出登录，弹幕已断开；正在生成新的二维码{note}"
        )))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::test_http::{self, Request};
    use std::collections::HashMap;
    use std::time::Duration;

    /// 表单体解成键值对（跟 `api::info::tests` 里那个同一个口径）。
    fn form_of(body: &str) -> HashMap<String, String> {
        url::form_urlencoded::parse(body.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    /// 收够 `n` 条事件。信息任务是一条条发的，中间不会插别的东西。
    async fn events(rx: &mut mpsc::Receiver<InfoEvent>, n: usize) -> Vec<InfoEvent> {
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(rx.recv().await.expect("信息任务该发够事件"));
        }
        out
    }

    /// 拉起信息那条链，返回（请求发送端、事件接收端）。
    fn spawn_info_task(
        base: &str,
        room_id: i64,
    ) -> (
        mpsc::Sender<InfoRequest>,
        mpsc::Receiver<InfoEvent>,
        tokio::task::JoinHandle<()>,
    ) {
        // 图床走主站，测试里也顶到同一个假服务器上
        let client = Arc::new(BiliClient::new("SESSDATA=abc; bili_jct=tok").unwrap().with_main_base(base));
        let (tx, rx) = mpsc::channel::<InfoRequest>(4);
        let (evt_tx, evt_rx) = mpsc::channel::<InfoEvent>(4);
        // 刷房间信息那条信号：这一组测的是信息任务，收下来别让它堵住（容量 1，不阻塞）
        let (refresh_tx, _refresh_rx) = mpsc::channel::<()>(1);
        let task = tokio::spawn(info_task(
            client,
            base.to_string(),
            room_id,
            refresh_tx,
            rx,
            evt_tx,
        ));
        (tx, evt_rx, task)
    }

    /// 进信息栏那一下：`get_info` 拉回标题和封面，**顺手把封面图抓回来**给预览用；
    /// 同一张图不重复抓（第二遍 LoadMeta 只该多一次 get_info，不该多下一次图）。
    #[tokio::test]
    async fn info_task_loads_the_meta_and_fetches_the_cover_image_once() {
        let srv = test_http::start(|r: &Request| match r.path.as_str() {
            "/room/v1/Room/get_info" => {
                // 端口是运行期才有的：拿请求里的 host 头把封面拼成绝对地址，
                // 这样抓图那次也会落在假服务器上（绝不会碰真图床）
                let host = r.header("host").unwrap().to_string();
                (
                    200,
                    format!(
                        r#"{{"code":0,"message":"0","data":{{"room_id":6,"uid":42,
                            "title":"原标题","live_status":0,
                            "live_time":"0000-00-00 00:00:00",
                            "user_cover":"http://{host}/cover.png"}}}}"#
                    ),
                )
            }
            "/cover.png" => (200, "PNG-BYTES".to_string()),
            other => (200, format!(r#"{{"code":-1,"message":"不认识的路径 {other}"}}"#)),
        })
        .await;

        let (tx, mut rx, task) = spawn_info_task(&srv.base, 6);
        tx.send(InfoRequest::LoadMeta).await.unwrap();
        let got = events(&mut rx, 2).await;
        assert_eq!(
            got[0],
            InfoEvent::Meta {
                title: "原标题".into(),
                cover: format!("{}/cover.png", srv.base),
            }
        );
        assert_eq!(
            got[1],
            InfoEvent::CoverImage {
                url: format!("{}/cover.png", srv.base),
                bytes: b"PNG-BYTES".to_vec(),
            },
            "封面图要一起抓回来：预览那一格等着它"
        );

        // 再拉一次：get_info 会再打一次，图不会
        tx.send(InfoRequest::LoadMeta).await.unwrap();
        events(&mut rx, 1).await;
        let paths: Vec<String> = srv.hits().iter().map(|r| r.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                "/room/v1/Room/get_info",
                "/cover.png",
                "/room/v1/Room/get_info"
            ],
            "同一张封面不重复抓：切栏、拉伸终端都不该重新下一次图"
        );
        // 抓图不带 cookie：图在 hdslb 上，那是另一个域
        assert!(
            srv.hits()[1].header("cookie").is_none(),
            "抓封面不该把登录凭据带过去"
        );
        assert!(srv.hits()[0].header("cookie").is_some(), "接口那条该带");
        task.abort();
    }

    /// 改标题 + 换封面（两步）整条打一遍：表单形状、顺序、以及「换完顺手把新图抓回来」。
    #[tokio::test]
    async fn info_task_updates_the_title_and_the_cover_in_two_steps() {
        let srv = test_http::start(|r: &Request| match r.path.as_str() {
            "/room/v1/Room/update" => (200, r#"{"code":0,"message":"0","data":{}}"#.to_string()),
            "/x/upload/web/image" => (
                200,
                // 刻意指向一个**连不上**的本地端口：假服务器没法应答 https，
                // 而图床地址会被升成 https。这样「抓新图」那一步只会立刻失败
                // （连 127.0.0.1 都被拒），绝不会把请求发到真图床上去。
                r#"{"code":0,"message":"0","data":{"location":"http://127.0.0.1:1/x.png"}}"#
                    .to_string(),
            ),
            "/xlive/app-blink/v1/preLive/UpdatePreLiveInfo" => {
                (200, r#"{"code":0,"message":"0","data":{}}"#.to_string())
            }
            other => (200, format!(r#"{{"code":-1,"message":"不认识的路径 {other}"}}"#)),
        })
        .await;

        let dir = std::env::temp_dir().join(format!("bililive-info-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cover = dir.join("new.png");
        std::fs::write(&cover, b"FAKE-PNG").unwrap();

        let (tx, mut rx, task) = spawn_info_task(&srv.base, 6);
        tx.send(InfoRequest::SetTitle("新标题".into())).await.unwrap();
        assert_eq!(
            events(&mut rx, 1).await[0],
            InfoEvent::Title {
                title: "新标题".into(),
                error: None
            }
        );

        tx.send(InfoRequest::SetCover(cover.to_str().unwrap().into()))
            .await
            .unwrap();
        let got = events(&mut rx, 2).await;
        assert_eq!(
            got[0],
            InfoEvent::Cover {
                cover: "https://127.0.0.1:1/x.png".into(),
                error: None
            },
            "换封面成功时给的是**图床那个地址**（本地路径走完图床就变成它了）"
        );
        assert!(
            matches!(got[1], InfoEvent::CoverImageFailed { .. }),
            "抓新图失败只让预览那一格写一句话，绝不能把「换封面成功」说成失败：{:?}",
            got[1]
        );

        let hits = srv.hits();
        let paths: Vec<&str> = hits.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "/room/v1/Room/update",
                "/x/upload/web/image",
                "/xlive/app-blink/v1/preLive/UpdatePreLiveInfo"
            ],
            "换封面必须**先传图床再写封面**，顺序反了服务端一定拒"
        );

        // 改标题那一下
        let f = form_of(&hits[0].body);
        assert_eq!(f.get("room_id").map(String::as_str), Some("6"));
        assert_eq!(f.get("title").map(String::as_str), Some("新标题"));
        assert_eq!(f.get("platform").map(String::as_str), Some("pc_link"));
        assert_eq!(f.get("csrf").map(String::as_str), Some("tok"));
        assert_eq!(f.get("csrf_token").map(String::as_str), Some("tok"));

        // 传图床那一下：multipart，文件内容原样
        assert!(hits[1].header("content-type").unwrap().contains("multipart/form-data"));
        assert!(hits[1].body.contains("name=\"file\""));
        assert!(hits[1].body.contains("filename=\"new.png\""));
        assert!(hits[1].body.contains("name=\"bucket\""));
        assert!(hits[1].body.contains("openplatform"));
        assert!(hits[1].body.contains("FAKE-PNG"));

        // 写封面那一下：cover 就是上一步拿到的地址，而且**没有 appkey / sign**
        let f = form_of(&hits[2].body);
        assert_eq!(
            f.get("cover").map(String::as_str),
            Some("https://127.0.0.1:1/x.png"),
            "location 是 http 的，写进直播间之前要升成 https"
        );
        assert_eq!(f.get("platform").map(String::as_str), Some("web"));
        assert_eq!(f.get("mobi_app").map(String::as_str), Some("web"));
        assert_eq!(f.get("build").map(String::as_str), Some("1"));
        assert!(!hits[2].body.contains("appkey"), "{}", hits[2].body);
        assert!(!hits[2].body.contains("sign="), "{}", hits[2].body);

        task.abort();
        let _ = std::fs::remove_file(&cover);
    }

    /// 配置里写的是短号（`6`）时，改标题要用 `get_info` 给的**规范房间号**：
    /// 短号也能查，但写操作拿规范号更稳（这条写在真账号上会怎样没验过，
    /// 所以宁可先用服务端自己认的那一个）。
    #[tokio::test]
    async fn info_task_prefers_the_canonical_room_id_from_get_info() {
        let srv = test_http::start(|r: &Request| match r.path.as_str() {
            "/room/v1/Room/get_info" => (
                200,
                r#"{"code":0,"message":"0","data":{"room_id":7734200,"uid":42,
                    "title":"原标题","live_status":0,"live_time":"0000-00-00 00:00:00",
                    "user_cover":""}}"#
                    .to_string(),
            ),
            other => (200, format!(r#"{{"code":0,"message":"0","data":{{"path":"{other}"}}}}"#)),
        })
        .await;

        let (tx, mut rx, task) = spawn_info_task(&srv.base, 6);
        tx.send(InfoRequest::LoadMeta).await.unwrap();
        assert_eq!(
            events(&mut rx, 1).await[0],
            InfoEvent::Meta {
                title: "原标题".into(),
                cover: String::new()
            },
            "没有封面（空串）不是错误，也不该去抓图"
        );

        tx.send(InfoRequest::SetTitle("新标题".into())).await.unwrap();
        events(&mut rx, 1).await;
        let f = form_of(&srv.hits()[1].body);
        assert_eq!(
            f.get("room_id").map(String::as_str),
            Some("7734200"),
            "短号 6 要换回 get_info 给的规范号"
        );
        let paths: Vec<String> = srv.hits().iter().map(|r| r.path.clone()).collect();
        assert_eq!(paths, vec!["/room/v1/Room/get_info", "/room/v1/Room/update"]);
        task.abort();
    }

    /// 图床那一步失败：**不许**接着去写封面（写上去的地址根本不是图床的，
    /// 只会换个 100402 回来），而且要说清楚是「传图床失败」。
    #[tokio::test]
    async fn info_task_stops_when_the_upload_fails() {
        let srv = test_http::start(|r: &Request| match r.path.as_str() {
            "/x/upload/web/image" => {
                (200, r#"{"code":-1,"message":"bucket 不对"}"#.to_string())
            }
            other => (200, format!(r#"{{"code":0,"message":"0","data":{{"path":"{other}"}}}}"#)),
        })
        .await;

        let dir = std::env::temp_dir().join(format!("bililive-info-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cover = dir.join("bad.png");
        std::fs::write(&cover, b"x").unwrap();

        let (tx, mut rx, task) = spawn_info_task(&srv.base, 6);
        tx.send(InfoRequest::SetCover(cover.to_str().unwrap().into()))
            .await
            .unwrap();
        let got = events(&mut rx, 1).await;
        let InfoEvent::Cover { error, .. } = &got[0] else {
            panic!("该是换封面的结果：{:?}", got[0]);
        };
        let error = error.as_ref().expect("图床失败了就该是个错误");
        assert!(error.contains("传图床失败"), "{error}");
        assert!(error.contains("bucket 不对"), "{error}");

        let paths: Vec<String> = srv.hits().iter().map(|r| r.path.clone()).collect();
        assert_eq!(
            paths,
            vec!["/x/upload/web/image"],
            "第一步没过就别去写第二步"
        );
        task.abort();
        let _ = std::fs::remove_file(&cover);
    }

    /// 房间号是 0（没配）：一个请求都不发，直接在顶栏说清楚该改哪儿。
    #[tokio::test]
    async fn info_task_without_a_room_does_not_hit_the_network() {
        let srv = test_http::start(|_| (200, r#"{"code":0,"message":"0"}"#.to_string())).await;
        let (tx, mut rx, task) = spawn_info_task(&srv.base, 0);
        tx.send(InfoRequest::LoadMeta).await.unwrap();
        let got = events(&mut rx, 1).await;
        let InfoEvent::MetaFailed(reason) = &got[0] else {
            panic!("该是「没拉到」：{:?}", got[0]);
        };
        assert!(reason.contains("直播间号"), "{reason}");
        assert!(srv.hits().is_empty());
        task.abort();
    }

    /// 分区那条链的两件事打一遍：拉表走接口，选定把 `area_id` / `area_name`
    /// 写回配置。中间那层（`area_task`）最容易接错线，所以按真通道走。
    #[tokio::test]
    async fn area_task_loads_the_list_and_writes_the_pick_back() {
        let srv = test_http::start(|_| {
            (
                200,
                r#"{"code":0,"message":"success","data":[{"id":2,"name":"网游",
                    "list":[{"id":"86","parent_id":"2","name":"英雄联盟"}]}]}"#
                    .to_string(),
            )
        })
        .await;

        let dir = std::env::temp_dir().join(format!("bililive-area-task-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("config.toml");
        let before = config::Config {
            cookie: "SESSDATA=abc; bili_jct=def".into(),
            room_id: 6,
            ..config::Config::default()
        };
        before.save_to(&cfg_path).unwrap();

        let client = Arc::new(BiliClient::new("").unwrap());
        let (req_tx, req_rx) = mpsc::channel::<AreaRequest>(4);
        let (evt_tx, mut evt_rx) = mpsc::channel::<AreaEvent>(8);
        let task = tokio::spawn(area_task(
            client,
            srv.base.clone(),
            cfg_path.clone(),
            req_rx,
            evt_tx,
        ));

        req_tx.send(AreaRequest::Load).await.unwrap();
        let AreaEvent::Loaded(areas) = evt_rx.recv().await.unwrap() else {
            panic!("第一条该是分区表")
        };
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].list[0].id, 86, "子分区 id 是字符串也要收下来");

        req_tx
            .send(AreaRequest::Pick {
                id: 86,
                name: "网游/英雄联盟".into(),
            })
            .await
            .unwrap();
        let AreaEvent::Saved { error, .. } = evt_rx.recv().await.unwrap() else {
            panic!("第二条该是落盘结果")
        };
        assert!(error.is_none(), "{error:?}");

        let after = config::Config::load_or_create(Some(&cfg_path)).unwrap();
        assert_eq!(after.area_id, 86);
        assert_eq!(after.area_name, "网游/英雄联盟");
        assert_eq!(
            after.cookie, "SESSDATA=abc; bili_jct=def",
            "别把 cookie 冲掉"
        );
        assert_eq!(after.room_id, 6, "别把房间号冲掉");

        task.abort();
        let _ = std::fs::remove_file(&cfg_path);
    }

    /// 退出登录那条链打一遍（离线，全在假通道 + 临时配置上）：
    /// 内存里的凭据清掉、配置里**只**清 cookie、弹幕那条链路收到重启信号、
    /// 房间信息补一次手刷、界面收到「未登录」和一句说明，还顺手要了一张新码。
    ///
    /// **绝不能拿 tc191 真在用的那份配置跑这个** —— 那会把他现在能用的登录态作废。
    /// 这里用的一律是临时路径。
    #[tokio::test]
    async fn session_task_logout_clears_everything_and_restarts_the_chain() {
        let dir = std::env::temp_dir().join(format!("bililive-logout-task-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("config.toml");
        config::Config {
            cookie: "SESSDATA=abc; bili_jct=def; DedeUserID=7".into(),
            room_id: 6,
            area_id: 371,
            area_name: "虚拟主播/虚拟日常".into(),
            obs_host: "127.0.0.1".into(),
            obs_port: 4455,
            ..config::Config::default()
        }
        .save_to(&cfg_path)
        .unwrap();

        // 退出这条路一个网络请求都不该发，所以这条 client 的 base 随便 ——
        // 真发了请求，`nav()` 那个默认地址会直接超时，测试当场就挂。
        let client = Arc::new(BiliClient::new("SESSDATA=abc; bili_jct=def").unwrap());
        let (creds_tx, creds_rx) = mpsc::channel::<String>(1);
        let (logout_tx, logout_rx) = mpsc::channel::<()>(1);
        let (login_tx, mut login_rx) = mpsc::channel::<()>(1);
        let (auth_tx, mut auth_rx) = watch::channel(0u64);
        let (refresh_tx, mut refresh_rx) = mpsc::channel::<()>(1);
        let (evt_tx, mut evt_rx) = mpsc::channel::<LoginEvent>(8);
        let task = tokio::spawn(session_task(
            client.clone(),
            cfg_path.clone(),
            CredentialPaths {
                creds: creds_rx,
                logout: logout_rx,
                login_start: login_tx,
                auth: auth_tx,
                refresh: refresh_tx,
            },
            evt_tx,
        ));

        assert!(client.logged_in().await, "开局手上是有凭据的");
        logout_tx.send(()).await.unwrap();

        // 界面：先「未登录」，再一句说清楚弹幕断了、正在出码
        let LoginEvent::LoggedOut(line) = evt_rx.recv().await.unwrap() else {
            panic!("第一条该是「未登录」")
        };
        assert!(line.contains("未登录"), "{line}");
        let LoginEvent::Hint(note) = evt_rx.recv().await.unwrap() else {
            panic!("第二条该是一句说明")
        };
        assert!(note.contains("退出登录"), "{note}");
        assert!(note.contains("弹幕已断开"), "{note}");

        // 内存里的凭据确实空了
        assert!(!client.logged_in().await, "退出之后内存里不该还有凭据");
        assert!(client.csrf().await.is_none());

        // 配置里**只有 cookie** 被清掉，别的字段逐字节还在
        let after = config::Config::load_or_create(Some(&cfg_path)).unwrap();
        assert_eq!(after.cookie, "");
        assert_eq!(after.room_id, 6, "房间号不许被冲掉");
        assert_eq!(after.area_id, 371, "刚选的分区不许被冲掉");
        assert_eq!(after.area_name, "虚拟主播/虚拟日常");
        assert_eq!(after.obs_host, "127.0.0.1", "OBS 那些也不许被冲掉");
        assert_eq!(after.obs_port, 4455);

        // 弹幕那条链路收到「整条重来」的信号（watch 的计数变了）
        tokio::time::timeout(Duration::from_secs(1), auth_rx.changed())
            .await
            .expect("退出登录之后弹幕那条链路得收到重启信号")
            .expect("watch 发送端还活着");
        // 房间信息那条不用重启，但得补一次手刷
        assert!(refresh_rx.try_recv().is_ok(), "房间信息该被补刷一次");
        // 顺手要了一张新二维码
        assert!(login_rx.try_recv().is_ok(), "退完就该去要一张新码");

        drop(creds_tx);
        drop(logout_tx);
        task.abort();
        let _ = std::fs::remove_file(&cfg_path);
    }

    // ------------------------------------------------------- 开播那条链

    /// 拉起开播那条链之后手上那几样东西。给个名字只是因为元组长得 clippy 不乐意。
    type LiveHarness = (
        mpsc::Sender<LiveRequest>,
        mpsc::Receiver<LiveEvent>,
        mpsc::Receiver<()>,
        mpsc::Receiver<String>,
        tokio::task::JoinHandle<()>,
    );

    /// 拉起开播那条链，返回（请求发送端、事件接收端、那条「重刷房间信息」的信号、
    /// OBS 那行字的接收端、任务句柄）。
    fn spawn_live_task(
        base: &str,
        room_id: i64,
        obs: obs::Config,
    ) -> LiveHarness {
        // nav 也顶到假服务器：人脸认证那条路要拿 mid 拼地址
        //（这是唯一会顺手问 nav 的地方，不然单测会打真网络）。
        let client = Arc::new(
            BiliClient::new("SESSDATA=abc; bili_jct=tok")
                .unwrap()
                .with_main_base(base),
        );
        let (tx, rx) = mpsc::channel::<LiveRequest>(4);
        let (evt_tx, evt_rx) = mpsc::channel::<LiveEvent>(8);
        let (refresh_tx, refresh_rx) = mpsc::channel::<()>(1);
        let (note_tx, note_rx) = mpsc::channel::<String>(4);
        let task = tokio::spawn(live_task(
            client,
            base.to_string(),
            room_id,
            refresh_tx,
            ObsFill {
                cfg: obs,
                notes: note_tx,
            },
            rx,
            evt_tx,
        ));
        (tx, evt_rx, refresh_rx, note_rx, task)
    }

    /// 开播整条打一遍：先 `get_info` 拿规范房间号，再版本号，再 startLive。
    /// 配置里写的是短号（`6`），**发出去的那个 room_id 必须是规范号**（`7734200`）。
    #[tokio::test]
    async fn live_task_starts_with_the_canonical_room_id() {
        let srv = test_http::start(|r: &Request| match r.path.as_str() {
            "/room/v1/Room/get_info" => (
                200,
                r#"{"code":0,"message":"0","data":{"room_id":7734200,"uid":42,
                    "title":"随便播播","live_status":0,"live_time":"0000-00-00 00:00:00",
                    "user_cover":""}}"#
                    .to_string(),
            ),
            api::live::LIVE_VERSION_PATH => (
                200,
                r#"{"code":0,"message":"0","data":{"curr_version":"9.9.9","build":12345}}"#
                    .to_string(),
            ),
            api::live::START_LIVE_PATH => (
                200,
                r#"{"code":0,"message":"0","data":{
                    "rtmp":{"addr":"rtmp://a/live","code":"?s=1"},
                    "protocols":[{"protocol":"srt","addr":"srt://b:1935","code":"?s=2"}]}}"#
                    .to_string(),
            ),
            other => (
                200,
                format!(r#"{{"code":-1,"message":"不认识的路径 {other}"}}"#),
            ),
        })
        .await;

        let (tx, mut rx, mut refresh, _notes, task) = spawn_live_task(&srv.base, 6, obs::Config::default());
        tx.send(LiveRequest::Start { area_v2: 371 }).await.unwrap();
        let ev = rx.recv().await.unwrap();
        let LiveEvent::Started(streams) = ev else {
            panic!("该是开播成功：{ev:?}");
        };
        assert_eq!(streams.len(), 2);
        assert_eq!(streams[0].full_url, "rtmp://a/live?s=1");
        assert_eq!(streams[1].kind, "srt-1");

        let paths: Vec<String> = srv.hits().iter().map(|r| r.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                "/room/v1/Room/get_info",
                api::live::LIVE_VERSION_PATH,
                api::live::START_LIVE_PATH
            ],
            "先拿规范房间号，再版本号，再开播"
        );
        let f = form_of(&srv.hits()[2].body);
        assert_eq!(
            f.get("room_id").map(String::as_str),
            Some("7734200"),
            "配置里是短号 6，写操作要用 get_info 回的规范号"
        );
        assert_eq!(f.get("area_v2").map(String::as_str), Some("371"));
        assert!(f.contains_key("sign"), "开播要带 app 签名：{f:?}");
        assert!(
            refresh.try_recv().is_ok(),
            "开播成功顺手让房间信息那条只读链重拉一次"
        );
        task.abort();
    }

    /// 下播：同样是写操作，同样要规范号（用户可能一进来就按 F5，那时手上还没有
    /// `get_info` 的结果），而且**不带 app 签名**。
    #[tokio::test]
    async fn live_task_stops_with_the_canonical_room_id_and_no_signature() {
        let srv = test_http::start(|r: &Request| match r.path.as_str() {
            "/room/v1/Room/get_info" => (
                200,
                r#"{"code":0,"message":"0","data":{"room_id":7734200,"uid":42,
                    "title":"t","live_status":1,"live_time":"2026-10-03 00:00:00",
                    "user_cover":""}}"#
                    .to_string(),
            ),
            api::live::STOP_LIVE_PATH => (
                200,
                r#"{"code":0,"message":"0","data":{}}"#.to_string(),
            ),
            other => (
                200,
                format!(r#"{{"code":-1,"message":"不认识的路径 {other}"}}"#),
            ),
        })
        .await;

        let (tx, mut rx, mut refresh, _notes, task) = spawn_live_task(&srv.base, 6, obs::Config::default());
        tx.send(LiveRequest::Stop).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), LiveEvent::Stopped);

        let hits = srv.hits();
        assert_eq!(hits[0].path, "/room/v1/Room/get_info");
        assert_eq!(hits[1].path, api::live::STOP_LIVE_PATH);
        let f = form_of(&hits[1].body);
        assert_eq!(
            f.get("room_id").map(String::as_str),
            Some("7734200"),
            "短号 6 要换成规范号"
        );
        assert!(!hits[1].body.contains("appkey"), "{}", hits[1].body);
        assert!(!hits[1].body.contains("sign="), "{}", hits[1].body);
        assert!(refresh.try_recv().is_ok(), "下播成功也刷一次房间信息");
        task.abort();
    }

    /// 只读的状态查询，以及 60024 要扫码那一条：开播任务得把它当成「要验证」，
    /// 而不是一句「开播失败」。
    #[tokio::test]
    async fn live_task_reports_status_and_the_verify_roadblock() {
        let srv = test_http::start(|r: &Request| match r.path.as_str() {
            "/room/v1/Room/get_info" => (
                200,
                r#"{"code":0,"message":"0","data":{"room_id":7734200,"uid":42,
                    "title":"t","live_status":0,"live_time":"0000-00-00 00:00:00",
                    "user_cover":""}}"#
                    .to_string(),
            ),
            api::live::LIVE_VERSION_PATH => (
                200,
                r#"{"code":0,"message":"0","data":{"curr_version":"9.9.9","build":12345}}"#
                    .to_string(),
            ),
            api::live::START_LIVE_PATH => (
                200,
                r#"{"code":60024,"message":"请扫码验证","data":{"qr":"https://www.bilibili.com/h5/v?t=a"}}"#
                    .to_string(),
            ),
            other => (
                200,
                format!(r#"{{"code":-1,"message":"不认识的路径 {other}"}}"#),
            ),
        })
        .await;

        let (tx, mut rx, _refresh, _notes, task) = spawn_live_task(&srv.base, 6, obs::Config::default());
        tx.send(LiveRequest::LoadStatus).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap(),
            LiveEvent::Status { live_status: 0 }
        );

        tx.send(LiveRequest::Start { area_v2: 371 }).await.unwrap();
        let ev = rx.recv().await.unwrap();
        let LiveEvent::Verify { kind, url, message } = ev else {
            panic!("60024 该是「要验证」而不是普通失败：{ev:?}");
        };
        assert_eq!(kind, api::live::VerifyKind::Qr);
        assert_eq!(url, "https://www.bilibili.com/h5/v?t=a");
        assert!(message.contains("扫码验证"), "{message}");
        task.abort();
    }

    /// 房间号是 0（没配）：一个请求都不发，直接在顶栏说清楚该改哪儿。
    #[tokio::test]
    async fn live_task_without_a_room_never_hits_the_network() {
        let srv = test_http::start(|_| (200, r#"{"code":0,"message":"0"}"#.to_string())).await;
        let (tx, mut rx, _refresh, _notes, task) = spawn_live_task(&srv.base, 0, obs::Config::default());

        tx.send(LiveRequest::LoadStatus).await.unwrap();
        let LiveEvent::Failed { action, message } = rx.recv().await.unwrap() else {
            panic!("该是「没查到」");
        };
        assert_eq!(action, LiveAction::Status);
        assert!(message.contains("直播间号"), "{message}");

        // 开播 / 下播也一样：先被房间号拦住
        tx.send(LiveRequest::Start { area_v2: 371 }).await.unwrap();
        let LiveEvent::Failed { action, .. } = rx.recv().await.unwrap() else {
            panic!("该是「没开成」");
        };
        assert_eq!(action, LiveAction::Start);
        tx.send(LiveRequest::Stop).await.unwrap();
        let LiveEvent::Failed { action, .. } = rx.recv().await.unwrap() else {
            panic!("该是「没下成」");
        };
        assert_eq!(action, LiveAction::Stop);

        assert!(srv.hits().is_empty(), "一个请求都不该发出去");
        task.abort();
    }

    // ------------------------------------------------------- OBS 联动

    /// 一条「开播必成」的假 B 站：`rtmp` 一组 + `protocols` 里再来一路 srt。
    /// OBS 那件事要拿**第一路 rtmp**，多给一路 srt 才好验「没抓错」。
    async fn bili_that_starts_live() -> test_http::FakeServer {
        test_http::start(|r: &Request| match r.path.as_str() {
            "/room/v1/Room/get_info" => (
                200,
                r#"{"code":0,"message":"0","data":{"room_id":7734200,"uid":42,
                    "title":"t","live_status":0,"live_time":"0000-00-00 00:00:00",
                    "user_cover":""}}"#
                    .to_string(),
            ),
            api::live::LIVE_VERSION_PATH => (
                200,
                r#"{"code":0,"message":"0","data":{"curr_version":"9.9.9","build":12345}}"#
                    .to_string(),
            ),
            api::live::START_LIVE_PATH => (
                200,
                r#"{"code":0,"message":"0","data":{
                    "rtmp":{"addr":"rtmp://a/live","code":"?streamname=s1&key=k1"},
                    "protocols":[{"protocol":"srt","addr":"srt://b:1935","code":"?s=2"}]}}"#
                    .to_string(),
            ),
            other => (
                200,
                format!(r#"{{"code":-1,"message":"不认识的路径 {other}"}}"#),
            ),
        })
        .await
    }

    /// 开播成功 → 顺手把**第一路 rtmp**填进 OBS。这条要整条走通：填错那一路
    /// （比如把 srt 塞进去）或者密钥被拼过一遍，都只会在真机上才看得出来。
    #[tokio::test]
    async fn a_successful_start_fills_obs_with_the_rtmp_stream() {
        let srv = bili_that_starts_live().await;
        let mut fake = obs::tests::fake_obs(
            None,
            serde_json::json!({
                "requestType": "SetStreamServiceSettings",
                "requestStatus": { "result": true, "code": 100 }
            }),
            true,
        )
        .await;
        let cfg = obs::Config {
            fill: true,
            host: "127.0.0.1".into(),
            port: fake.port,
            password: String::new(),
        };

        let (tx, mut rx, _refresh, mut notes, task) =
            spawn_live_task(&srv.base, 6, cfg);
        tx.send(LiveRequest::Start { area_v2: 371 }).await.unwrap();
        let LiveEvent::Started(streams) = rx.recv().await.unwrap() else {
            panic!("该是开播成功");
        };
        assert_eq!(streams[0].protocol, "rtmp");

        // 那一行字得回来（OBS 那条链是另一条任务，慢一点）
        let note = tokio::time::timeout(Duration::from_secs(3), notes.recv())
            .await
            .expect("OBS 的结果该回来")
            .expect("通道还在");
        assert!(note.contains("设置 → 推流"), "填好了要说清楚填哪儿：{note}");
        assert!(
            note.contains("你自己按"),
            "绝不能说成「已经开推」—— 推流那下得用户自己按：{note}"
        );

        // 假 OBS 真收到的那条 Request：服务器 / 密钥就是第一路 rtmp 的那一份
        let mut got = None;
        while let Ok(Some((op, d))) =
            tokio::time::timeout(Duration::from_secs(2), fake.frames.recv()).await
        {
            if op == 6 {
                got = Some(d);
                break;
            }
        }
        let d = got.expect("假 OBS 该收到一条 Request");
        assert_eq!(d["requestType"], "SetStreamServiceSettings");
        assert_eq!(d["requestData"]["streamServiceType"], "rtmp_custom");
        assert_eq!(
            d["requestData"]["streamServiceSettings"]["server"],
            "rtmp://a/live"
        );
        assert_eq!(
            d["requestData"]["streamServiceSettings"]["key"],
            "?streamname=s1&key=k1",
            "密钥要原样过去，谁都不许再拼一遍"
        );

        task.abort();
        fake.task.abort();
    }

    /// `obs_fill = false`：开播照旧，但一个字都不多说（也不去连任何东西）。
    #[tokio::test]
    async fn obs_fill_off_means_no_note_and_no_connection() {
        let srv = bili_that_starts_live().await;
        // 端口写 1：真去连的话会立刻被拒（连上还会多花时间），关掉就该碰都不碰
        let cfg = obs::Config {
            fill: false,
            host: "127.0.0.1".into(),
            port: 1,
            password: String::new(),
        };
        let (tx, mut rx, _refresh, mut notes, task) = spawn_live_task(&srv.base, 6, cfg);
        tx.send(LiveRequest::Start { area_v2: 371 }).await.unwrap();
        let _ = rx.recv().await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            notes.try_recv().is_err(),
            "关掉联动就不该有任何 OBS 的字"
        );
        task.abort();
    }

    /// 这次开播只给了 srt（没有 rtmp）：填不进 OBS 也得**只多一行字**，
    /// 而且连都不去连（没什么可填的）。
    #[tokio::test]
    async fn a_start_without_an_rtmp_stream_just_says_so() {
        let srv = test_http::start(|r: &Request| match r.path.as_str() {
            "/room/v1/Room/get_info" => (
                200,
                r#"{"code":0,"message":"0","data":{"room_id":7734200,"uid":42,
                    "title":"t","live_status":0,"live_time":"0000-00-00 00:00:00",
                    "user_cover":""}}"#
                    .to_string(),
            ),
            api::live::LIVE_VERSION_PATH => (
                200,
                r#"{"code":0,"message":"0","data":{"curr_version":"9.9.9","build":1}}"#
                    .to_string(),
            ),
            api::live::START_LIVE_PATH => (
                200,
                r#"{"code":0,"message":"0","data":{
                    "protocols":[{"protocol":"srt","addr":"srt://b:1935","code":"?s=2"}]}}"#
                    .to_string(),
            ),
            other => (
                200,
                format!(r#"{{"code":-1,"message":"不认识的路径 {other}"}}"#),
            ),
        })
        .await;
        let cfg = obs::Config {
            fill: true,
            host: "127.0.0.1".into(),
            port: 1,
            password: String::new(),
        };
        let (tx, mut rx, _refresh, mut notes, task) = spawn_live_task(&srv.base, 6, cfg);
        tx.send(LiveRequest::Start { area_v2: 371 }).await.unwrap();
        assert!(matches!(rx.recv().await.unwrap(), LiveEvent::Started(_)));
        let note = notes.recv().await.expect("该说一句没 rtmp");
        assert!(note.contains("没有") || note.contains("没给"), "{note}");
        assert!(note.contains("rtmp"), "{note}");
        task.abort();
    }
}
