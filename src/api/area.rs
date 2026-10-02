//! 分区表：`room/v1/area/getList`。
//!
//! 一个 GET 就把两级都拿回来（父分区里套 `list` 子分区），不用参数、不用登录。
//! 全流程只读 —— 这一层只碰接口，不碰磁盘也不碰界面。
//!
//! 拆出来的字段只有界面真要用的那两个：子分区的 `id`（就是写进配置的 `area_id`，
//! 开播时那个 `area_v2`）和两级的名字。剩下的 `pic` / `pk_status` 之流这一轮用不上。

use anyhow::Result;
use serde_json::Value;

use crate::api::client::{BiliClient, int_of, str_of};

pub const AREA_LIST_PATH: &str = "/room/v1/area/getList";

/// 子分区。`id` 就是要落进配置的那一个。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubArea {
    pub id: i64,
    pub name: String,
}

/// 父分区。它自己也是树上一行（展开 / 收起的对象），所以 `list` 空着也要留着。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentArea {
    pub name: String,
    pub list: Vec<SubArea>,
}

/// 拉一次全部分区。
pub async fn fetch_areas(client: &BiliClient, base: &str) -> Result<Vec<ParentArea>> {
    let data = client.get_api(&format!("{base}{AREA_LIST_PATH}")).await?;
    Ok(parse_areas(&data))
}

/// 拆 `data` 那一层数组。
///
/// 单独拆出来只为边界能单独喂：空表、父分区没有 `list`、`list` 是 null，
/// 三种都得当成「这一支没有子分区」，**不能** `unwrap`
/// —— 这一层 panic 一次，整屏就没了。
pub fn parse_areas(data: &Value) -> Vec<ParentArea> {
    data.as_array()
        .map(|parents| {
            parents
                .iter()
                .map(|p| ParentArea {
                    name: str_of(&p["name"]),
                    // 子分区 id 在真接口里是**字符串**（`"id":"86"`），父分区 id 是数字。
                    // `int_of` 两种都收；直接用 `as_i64()` 的话每个子分区都会变成 0，
                    // 而 0 正好是「没配过分区」—— 用户会看到「选了个分区，下次启动又没了」。
                    list: p["list"]
                        .as_array()
                        .map(|subs| {
                            subs.iter()
                                .map(|s| SubArea {
                                    id: int_of(&s["id"]),
                                    name: str_of(&s["name"]),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 界面 -> 分区任务。
///
/// 拉表是网络、选定是写盘，两件事都不该由界面直接干，所以都从这条通道走。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AreaRequest {
    /// 拉一次分区表
    Load,
    /// 选定了这个分区：把 `area_id` / `area_name` 写回配置文件
    Pick { id: i64, name: String },
}

/// 分区任务 -> 界面。网络这半边只往通道里塞这个，别的什么都不干。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AreaEvent {
    /// 分区表到手了（可能是空表 —— 那不是错误）
    Loaded(Vec<ParentArea>),
    /// 没拉到（网络抖 / 接口报错），照原文显示，绝不 panic
    Failed(String),
    /// 选定分区的落盘结果。`error` 是 `Some` 时**这一栏已经选中了它**，
    /// 只是下次启动可能还得再选一次 —— 别把它说成「选定失败」。
    Saved { name: String, error: Option<String> },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::client::BiliClient;
    use crate::api::test_http;

    /// 照真接口的形状抄的（2026-10-03 实测）：父分区 id 是数字，**子分区 id 是字符串**。
    fn list_body() -> String {
        r#"{"code":0,"msg":"success","message":"success","data":[
            {"id":2,"name":"网游","list":[
                {"id":"86","parent_id":"2","name":"英雄联盟","area_type":0},
                {"id":"329","parent_id":"2","name":"无畏契约","area_type":0}]},
            {"id":9,"name":"虚拟主播","list":[
                {"id":"371","parent_id":"9","name":"虚拟日常","area_type":0}]}]}"#
            .to_string()
    }

    #[tokio::test]
    async fn area_list_request_shape_and_parse() {
        let srv = test_http::start(|_| (200, list_body())).await;
        let client = BiliClient::new("SESSDATA=abc; bili_jct=def").unwrap();
        let areas = fetch_areas(&client, &srv.base).await.unwrap();

        let hits = srv.hits();
        assert_eq!(hits.len(), 1, "一次请求就够，两级一起回来");
        let r = &hits[0];
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/room/v1/area/getList");
        assert_eq!(r.query, "", "这个接口不吃参数");
        // 风控认这套头，缺一个就可能返 -352
        assert!(r.header("user-agent").unwrap().contains("Mozilla"));
        assert_eq!(r.header("origin"), Some("https://live.bilibili.com"));

        assert_eq!(areas.len(), 2);
        assert_eq!(areas[0].name, "网游");
        assert_eq!(areas[0].list.len(), 2);
        assert_eq!(areas[0].list[0].id, 86, "子分区的 id 是字符串，也要收下来");
        assert_eq!(areas[0].list[0].name, "英雄联盟");
        assert_eq!(areas[1].list[0].id, 371);
        assert_eq!(areas[1].list[0].name, "虚拟日常");
    }

    /// 空表不是错误：接口今天不给数据也得画出一个空树，不能 panic 也不能报「失败」。
    #[tokio::test]
    async fn an_empty_list_is_not_an_error() {
        let srv = test_http::start(|_| {
            (
                200,
                r#"{"code":0,"message":"success","data":[]}"#.to_string(),
            )
        })
        .await;
        let client = BiliClient::new("").unwrap();
        assert!(fetch_areas(&client, &srv.base).await.unwrap().is_empty());
    }

    /// 父分区没有子分区（`list` 空 / 缺 / 是 null）三种形状都要能站住。
    #[tokio::test]
    async fn a_parent_without_children_is_kept_as_an_empty_branch() {
        let srv = test_http::start(|_| {
            (
                200,
                r#"{"code":0,"message":"success","data":[
                    {"id":16,"name":"购物"},
                    {"id":13,"name":"赛事","list":null},
                    {"id":1,"name":"娱乐","list":[]}]}"#
                    .to_string(),
            )
        })
        .await;
        let client = BiliClient::new("").unwrap();
        let areas = fetch_areas(&client, &srv.base).await.unwrap();
        assert_eq!(areas.len(), 3, "空父分区也要留在树上，不能整个丢掉");
        assert!(areas.iter().all(|a| a.list.is_empty()));
        assert_eq!(areas[0].name, "购物");
    }

    /// `code != 0` 要变成 Err 往上传（界面拿它显示「没拉到，回车重试」），
    /// 不能 panic，也不能当成一份空表糊过去。
    #[tokio::test]
    async fn api_error_becomes_err_not_panic() {
        let srv =
            test_http::start(|_| (200, r#"{"code":-412,"message":"请求被拦截"}"#.to_string()))
                .await;
        let client = BiliClient::new("").unwrap();
        let err = fetch_areas(&client, &srv.base)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("-412"), "{err}");
        assert!(err.contains("请求被拦截"), "{err}");
    }
}
