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
        cfg_path.clone(),
        creds_rx,
        auth_tx,
        refresh_tx.clone(),
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
            area: area_tx,
            area_events: area_evt_rx,
            info: info_tx,
            info_events: info_evt_rx,
            live: live_tx,
            live_events: live_evt_rx,
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
async fn live_task(
    client: Arc<BiliClient>,
    base: String,
    room_id: i64,
    refresh: mpsc::Sender<()>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use api::test_http::{self, Request};
    use std::collections::HashMap;

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

    // ------------------------------------------------------- 开播那条链

    /// 拉起开播那条链，返回（请求发送端、事件接收端、那条「重刷房间信息」的信号）。
    fn spawn_live_task(
        base: &str,
        room_id: i64,
    ) -> (
        mpsc::Sender<LiveRequest>,
        mpsc::Receiver<LiveEvent>,
        mpsc::Receiver<()>,
        tokio::task::JoinHandle<()>,
    ) {
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
        let task = tokio::spawn(live_task(
            client,
            base.to_string(),
            room_id,
            refresh_tx,
            rx,
            evt_tx,
        ));
        (tx, evt_rx, refresh_rx, task)
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

        let (tx, mut rx, mut refresh, task) = spawn_live_task(&srv.base, 6);
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

        let (tx, mut rx, mut refresh, task) = spawn_live_task(&srv.base, 6);
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

        let (tx, mut rx, _refresh, task) = spawn_live_task(&srv.base, 6);
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
        let (tx, mut rx, _refresh, task) = spawn_live_task(&srv.base, 0);

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
}
