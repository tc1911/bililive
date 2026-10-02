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
    use api::test_http;

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
}
