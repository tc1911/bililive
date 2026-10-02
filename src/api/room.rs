//! 房间信息与观众榜。
//!
//! 两个接口都不用签名，但请求头照带 —— 少一个头就多一次风控命中。
//! 「拉成功才换」：某一轮网络抖一下不能把界面刷白，界面靠 `failed` 自己决定怎么提示。

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::sleep;

use crate::api::client::{BiliClient, int_of, str_of};
use crate::timefmt;

/// 房间信息和观众榜的刷新间隔，跟 Go 版一致。
pub const SYNC_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub struct OnlineRankUser {
    pub name: String,
    pub score: i64,
    pub rank: i64,
}

#[derive(Debug, Clone)]
pub struct RoomInfo {
    pub room_id: i64,
    pub uid: i64,
    pub title: String,
    pub parent_area_name: String,
    pub area_name: String,
    pub online: i64,
    pub attention: i64,
    /// 0 未开播 / 1 直播中 / 2 轮播
    pub live_status: i64,
    /// 已播时长，形如「1天2时3分」；没开播时是空串
    pub live_duration: String,
    /// 最后一次**成功**拉到的时间。界面的框标题靠它显示数据新旧。
    pub updated_at: Option<SystemTime>,
    /// 这一轮没拉到，屏幕上摆的是上一版
    pub failed: bool,
    pub online_rank_users: Vec<OnlineRankUser>,
}

impl RoomInfo {
    pub fn new(room_id: i64) -> Self {
        Self {
            room_id,
            uid: 0,
            title: String::new(),
            parent_area_name: String::new(),
            area_name: String::new(),
            online: 0,
            attention: 0,
            live_status: 0,
            live_duration: String::new(),
            updated_at: None,
            failed: false,
            online_rank_users: Vec::new(),
        }
    }
}

/// `/room/v1/Room/get_info`。路径大写 R 是官方写法，Go 版写的小写也能通（路由不区分大小写）。
pub async fn fetch_room_info(client: &BiliClient, base: &str, room_id: i64) -> Result<RoomInfo> {
    let url = format!("{base}/room/v1/Room/get_info?room_id={room_id}");
    let d = client.get_api(&url).await?;

    let live_status = int_of(&d["live_status"]);
    let mut info = RoomInfo::new(room_id);
    info.uid = int_of(&d["uid"]);
    info.title = str_of(&d["title"]);
    info.parent_area_name = str_of(&d["parent_area_name"]);
    info.area_name = str_of(&d["area_name"]);
    info.online = int_of(&d["online"]);
    info.attention = int_of(&d["attention"]);
    info.live_status = live_status;

    // 没开播时 live_time 是 "0000-00-00 00:00:00"。当零值收下去再减，
    // 界面上就会出现「739891天」（Go 版真出现过）。只在真在播时算。
    if live_status == 1 {
        info.live_duration = timefmt::live_duration(&str_of(&d["live_time"]), timefmt::now_epoch());
    }
    Ok(info)
}

/// `/xlive/general-interface/v1/rank/getOnlineGoldRank`。`ruid` 是主播 uid，不是房间号。
pub async fn fetch_online_rank(
    client: &BiliClient,
    base: &str,
    uid: i64,
    room_id: i64,
) -> Result<Vec<OnlineRankUser>> {
    let url = format!(
        "{base}/xlive/general-interface/v1/rank/getOnlineGoldRank\
         ?ruid={uid}&roomId={room_id}&page=1&pageSize=50"
    );
    let d = client.get_api(&url).await?;
    let users = d["OnlineRankItem"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|u| OnlineRankUser {
                    name: str_of(&u["name"]),
                    score: int_of(&u["score"]),
                    rank: int_of(&u["userRank"]),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(users)
}

/// 每 30 秒拉一次，或者被 `refresh` 叫醒。
pub async fn sync_loop(
    room_id: i64,
    client: Arc<BiliClient>,
    base: String,
    mut refresh: Receiver<()>,
    tx: Sender<RoomInfo>,
) {
    let mut last = RoomInfo::new(room_id);
    loop {
        match fetch_room_info(&client, &base, room_id).await {
            Ok(fresh) => {
                // 这一版最全，替换掉 last 里的对应字段；uid 留着给观众榜用。
                last.uid = fresh.uid;
                last.title = fresh.title;
                last.parent_area_name = fresh.parent_area_name;
                last.area_name = fresh.area_name;
                last.online = fresh.online;
                last.attention = fresh.attention;
                last.live_status = fresh.live_status;
                last.live_duration = fresh.live_duration;
                last.updated_at = Some(SystemTime::now());
                last.failed = false;
            }
            Err(_) => {
                // 保留上一版，只标一下：界面会写成「15:04 的数据（这次没刷上）」。
                last.failed = true;
            }
        }

        // 房间信息没拉到就没有 uid，拿 0 去问观众榜只会再换一个错误回来。
        if last.uid > 0
            && let Ok(users) = fetch_online_rank(&client, &base, last.uid, room_id).await
        {
            // 只有真拿到列表才换。Go 版会在 code != 0 时把列表清空，
            // 界面上表现为「观众榜单突然全没了」，这里保留上一版。
            last.online_rank_users = users;
        }

        if tx.send(last.clone()).await.is_err() {
            return; // 界面没了
        }

        tokio::select! {
            _ = sleep(SYNC_INTERVAL) => {}
            got = refresh.recv() => {
                if got.is_none() {
                    return; // 发送端没了
                }
            }
        }
    }
}

/// 让界面把房间信息立刻重拉一次的入口。容量 1 + `try_send`：
/// 连按几下也只会补一次，而且永远不阻塞 —— 它跑在界面的事件循环里，堵住就是把 TUI 冻住。
pub fn refresh(tx: &Sender<()>) {
    let _ = tx.try_send(());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::client::BiliClient;
    use crate::api::test_http::{self, Request};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    fn room_body(live_status: i64, live_time: &str) -> String {
        format!(
            r#"{{"code":0,"message":"0","data":{{
                "room_id":9527,"uid":42,"title":"随便播播",
                "parent_area_name":"虚拟主播","area_name":"虚拟日常",
                "online":1234,"attention":56789,
                "live_status":{live_status},"live_time":"{live_time}"}}}}"#
        )
    }

    fn rank_body() -> String {
        r#"{"code":0,"message":"0","data":{"OnlineRankItem":[
            {"name":"榜一","score":100,"userRank":1},
            {"name":"榜二","score":50,"userRank":2}]}}"#
            .to_string()
    }

    #[tokio::test]
    async fn room_info_request_shape_and_parse() {
        let srv = test_http::start(|_| (200, room_body(0, "0000-00-00 00:00:00"))).await;
        let client = BiliClient::new("SESSDATA=abc; bili_jct=def").unwrap();
        let info = fetch_room_info(&client, &srv.base, 9527).await.unwrap();

        let hits = srv.hits();
        assert_eq!(hits.len(), 1);
        let r = &hits[0];
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/room/v1/Room/get_info");
        assert_eq!(r.query_param("room_id"), Some("9527"));
        // 风控认这套头，缺一个就可能返 -352
        assert!(r.header("user-agent").unwrap().contains("Mozilla"));
        assert_eq!(r.header("referer"), Some("https://live.bilibili.com/"));
        assert_eq!(r.header("origin"), Some("https://live.bilibili.com"));
        assert!(r.header("cookie").unwrap().contains("SESSDATA=abc"));

        assert_eq!(info.title, "随便播播");
        assert_eq!(info.uid, 42);
        assert_eq!(info.parent_area_name, "虚拟主播");
        assert_eq!(info.area_name, "虚拟日常");
        assert_eq!(info.online, 1234);
        assert_eq!(info.attention, 56789);
        assert_eq!(info.live_status, 0);
        // 没开播时 live_time 是零值，算出来必须是空串，不能是「739891天」
        assert_eq!(info.live_duration, "");
        // 时间戳由 sync_loop 打，接口函数本身不碰
        assert!(info.updated_at.is_none());
    }

    #[tokio::test]
    async fn live_duration_only_computed_when_live() {
        let srv = test_http::start(|_| (200, room_body(1, "2020-01-01 00:00:00"))).await;
        let client = BiliClient::new("").unwrap();
        let info = fetch_room_info(&client, &srv.base, 9527).await.unwrap();
        assert!(info.live_duration.ends_with('分'), "{}", info.live_duration);
        assert!(info.live_duration.contains('天'), "{}", info.live_duration);
    }

    #[tokio::test]
    async fn online_rank_request_shape_and_parse() {
        let srv = test_http::start(|_| (200, rank_body())).await;
        let client = BiliClient::new("").unwrap();
        let users = fetch_online_rank(&client, &srv.base, 42, 9527).await.unwrap();

        let r = &srv.hits()[0];
        assert_eq!(r.path, "/xlive/general-interface/v1/rank/getOnlineGoldRank");
        assert_eq!(r.query_param("ruid"), Some("42"), "ruid 是主播 uid，不是房间号");
        assert_eq!(r.query_param("roomId"), Some("9527"));
        assert_eq!(r.query_param("page"), Some("1"));
        assert_eq!(r.query_param("pageSize"), Some("50"));

        assert_eq!(users.len(), 2);
        assert_eq!(users[0].name, "榜一");
        assert_eq!(users[0].score, 100);
        assert_eq!(users[0].rank, 1);
    }

    /// 接口返 code != 0 时要变成 Err 往上传，不能 panic 也不能当成空数据。
    #[tokio::test]
    async fn api_error_becomes_err_not_panic() {
        let srv = test_http::start(|_| {
            (
                200,
                r#"{"code":-352,"message":"风控校验失败"}"#.to_string(),
            )
        })
        .await;
        let client = BiliClient::new("").unwrap();
        let err = fetch_room_info(&client, &srv.base, 9527)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("-352"), "{err}");
        assert!(err.contains("风控校验失败"), "{err}");
    }

    /// 「拉成功才换」：某一轮失败时界面拿到的是上一版数据 + 一个 failed 标记，
    /// 而不是一份空房间（以前那版会把界面刷白）。
    #[tokio::test]
    async fn sync_loop_keeps_last_good_room_when_a_round_fails() {
        let round = Arc::new(AtomicUsize::new(0));
        let counter = round.clone();
        let srv = test_http::start(move |r: &Request| {
            if r.path.contains("getOnlineGoldRank") {
                return (200, rank_body());
            }
            // 第一轮成功，之后每一轮都被拦
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                (200, room_body(1, "2020-01-01 00:00:00"))
            } else {
                (200, r#"{"code":-412,"message":"请求被拦截"}"#.to_string())
            }
        })
        .await;

        let client = Arc::new(BiliClient::new("").unwrap());
        let (tx, mut rx) = mpsc::channel(8);
        let (refresh_tx, refresh_rx) = mpsc::channel(1);
        let task = tokio::spawn(sync_loop(
            9527,
            client,
            srv.base.clone(),
            refresh_rx,
            tx,
        ));

        let first = rx.recv().await.expect("第一轮该推一份出来");
        assert!(!first.failed);
        assert_eq!(first.title, "随便播播");
        assert!(first.updated_at.is_some());
        assert_eq!(first.online_rank_users.len(), 2);

        // 手动刷一下（Ctrl+R 走的就是这条路），第二轮必然失败
        refresh_tx.send(()).await.unwrap();
        let second = rx.recv().await.unwrap();
        assert!(second.failed, "这一轮该标上「没刷上」");
        assert_eq!(second.title, first.title, "拉失败不能把界面刷白");
        assert_eq!(second.updated_at, first.updated_at, "时间戳还是上一版的");
        assert_eq!(second.online_rank_users.len(), 2, "观众榜也不能被清空");

        task.abort();
    }
}
