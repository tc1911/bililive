//! 「直播间信息」栏背后那两个动作：改标题、换封面。
//!
//! 这一层只碰接口 —— 不读配置、不画界面。跟 `api/area.rs` 一样，跟界面之间用
//! 两条消息类型说话（`InfoRequest` / `InfoEvent`），真正「先传图床再写封面」那套
//! 顺序落在 `main::info_task` 里（界面两件事都不许自己干）。
//!
//! 三件容易踩的事都写在下面各自的注释里：标题按**字符**算、封面必须两步、
//! `UpdatePreLiveInfo` **不能**带 app 签名。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::api::client::{BiliClient, str_of};

/// 改标题。同一个接口带上 `area_id` / `add_tag` 理论上也能改分区和标签，
/// 这里只用到标题那部分。
pub const UPDATE_TITLE_PATH: &str = "/room/v1/Room/update";
/// B 站图床。本地图片必须先落到这儿，拿到 `.hdslb.com` 的地址才能当封面用。
pub const UPLOAD_IMAGE_PATH: &str = "/x/upload/web/image";
/// 写直播间封面。
///
/// 路径挂在 `app-blink`（直播姬）下，但**网页端只带 csrf 就能过**，
/// 不要给它加 appkey / appsec 那套签名 —— 那是开播接口才用的（下一轮的事）。
pub const UPDATE_COVER_PATH: &str = "/xlive/app-blink/v1/preLive/UpdatePreLiveInfo";

/// 标题上限，**按字符算不是按字节**。
///
/// 超了服务端只回一句看不懂的错（Go 版实测），所以界面在输入时就要拦住；
/// 这个数在界面和接口两边共用，别各写一份。
/// 口径跟 Go 的 `utf8.RuneCountInString` 一致：一个 emoji 算一个字符
/// （带 ZWJ 的组合 emoji 会算好几个，那种标题本来也没人用）。
pub const MAX_TITLE_CHARS: usize = 40;

/// 图床分桶。`openplatform` 是公开图床桶，返回的同样是 `i0.hdslb.com` 的地址，
/// 封面接口认它；换别的桶会被拒（Go 版踩过）。
pub const UPLOAD_BUCKET: &str = "openplatform";

/// 标题能不能提交。空标题和超长都在这儿拦下来 ——
/// 服务端那两句错（一句含糊、一句没有），用户看不懂也改不了。
pub fn check_title(title: &str) -> Result<()> {
    let n = title.chars().count();
    if n == 0 {
        bail!("标题不能为空");
    }
    if n > MAX_TITLE_CHARS {
        bail!("标题最多 {MAX_TITLE_CHARS} 个字，现在有 {n} 个");
    }
    Ok(())
}

/// `csrf` / `csrf_token` 都取 cookie 里的 `bili_jct`。
///
/// 取不到就**在发请求之前**说清楚缺什么：不然服务端只会回一句含糊的 `-111`
/// （发弹幕那条链踩过同样的坑）。
async fn csrf(client: &BiliClient) -> Result<String> {
    client.csrf().await.context(
        "Cookie 里没有 bili_jct（改标题 / 换封面拿它当 csrf），把登录后的完整 Cookie 补进配置",
    )
}

/// 改直播间标题。
pub async fn update_title(
    client: &BiliClient,
    base: &str,
    room_id: i64,
    title: &str,
) -> Result<()> {
    check_title(title)?;
    let csrf = csrf(client).await?;
    let form = [
        ("room_id", room_id.to_string()),
        ("title", title.to_string()),
        ("platform", "pc_link".to_string()),
        ("csrf", csrf.clone()),
        ("csrf_token", csrf),
    ];
    client
        .post_form(&format!("{base}{UPDATE_TITLE_PATH}"), &form)
        .await
        .map(|_| ())
}

/// 换直播间封面。
///
/// `cover` 只认 `.hdslb.com` 下的地址，本地图先走 [`upload_image`]；
/// 别处的链接服务端一律回 `100402`（图片地址不合法）。
pub async fn update_cover(client: &BiliClient, base: &str, cover: &str) -> Result<()> {
    let cover = normalize_image_url(cover);
    if cover.is_empty() {
        bail!("封面地址是空的");
    }
    let csrf = csrf(client).await?;
    // 形状照网页端：build=1、platform/mobi_app 都是 web。**没有 appkey / sign**。
    let form = [
        ("platform", "web".to_string()),
        ("mobi_app", "web".to_string()),
        ("build", "1".to_string()),
        ("csrf", csrf.clone()),
        ("csrf_token", csrf),
        ("cover", cover),
    ];
    client
        .post_form(&format!("{base}{UPDATE_COVER_PATH}"), &form)
        .await
        .map(|_| ())
}

/// 把本地图片传到图床，返回 `.hdslb.com` 下的地址。
///
/// `base` 是主站（`api.bilibili.com`）—— 图床不在直播那台机器上，
/// 传错域会被 404 掉。字段名和分桶对齐网页端上传组件。
pub async fn upload_image(client: &BiliClient, base: &str, path: &str) -> Result<String> {
    let path = expand_home(path);
    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("读不了这张图：{}", path.display()))?;
    let csrf = csrf(client).await?;

    // multipart 的字段名固定是 file / bucket / csrf。
    // content-type 用 octet-stream：Go 版（实测过）就是这个，别自作主张按后缀猜
    // —— 图床不看它，猜错了反而多一处能出错的地方。
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(file_name_of(&path))
        .mime_str("application/octet-stream")?;
    let form = reqwest::multipart::Form::new()
        .text("bucket", UPLOAD_BUCKET)
        .text("csrf", csrf)
        .part("file", part);

    let data = client
        .post_multipart(&format!("{base}{UPLOAD_IMAGE_PATH}"), form)
        .await?;
    pick_image_url(&data)
}

/// 从图床的 `data` 里挑出图片地址。
///
/// 两个字段都得认：网页端上传组件读 `image_url`，老接口给 `location`（还是 `http://`）。
/// **两个都没有就报错**，绝不返回空串 —— 空串继续往下走的话，
/// 用户看到的是「封面已提交」，而服务端早就用 `100402` 拒了。
pub fn pick_image_url(data: &Value) -> Result<String> {
    let image = str_of(&data["image_url"]);
    if !image.is_empty() {
        return Ok(normalize_image_url(&image));
    }
    let location = normalize_image_url(&str_of(&data["location"]));
    if location.is_empty() {
        bail!("图床没返回图片地址（image_url / location 都是空的）");
    }
    Ok(location)
}

/// 协议相对的地址补上 `https:`。
///
/// 房间接口的 `user_cover` 有时就是 `//i0.hdslb.com/...`：这种地址直接扔给 `reqwest`
/// 会报「relative URL without a base」，预览那一格就永远停在「加载中」。
/// **不动 `http://`**：`user_cover` 走 http 也抓得到，没必要改服务端给的地址。
pub fn absolute_image_url(url: &str) -> String {
    match url.strip_prefix("//") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    }
}

/// 图床给的地址：`http://` 一并升成 `https://`。
///
/// 实测 `location` 字段就是 `http://i0.hdslb.com/...`，而写封面那个接口
/// 只认 https 的地址（http 会被当不安全链接拒掉）。
/// 只换开头那一个：整串替换会把 query 里可能出现的 `http://` 一起改坏。
pub fn normalize_image_url(url: &str) -> String {
    let url = absolute_image_url(url);
    match url.strip_prefix("http://") {
        Some(rest) => format!("https://{rest}"),
        None => url,
    }
}

/// 填进来的是不是一条链接。是链接就直接拿去改封面，否则当本地路径传图床。
pub fn is_link(src: &str) -> bool {
    src.starts_with("http://") || src.starts_with("https://") || src.starts_with("//")
}

/// 把开头的 `~` 换成家目录。
///
/// 让人在 TUI 里手敲一长串绝对路径不现实，但 `File::open` / `tokio::fs::read`
/// 都不认 `~`（跟 Go 的 `os.Open` 一样），这活儿得自己干。`./` `../` 不用管，
/// 相对路径本来就是相对当前工作目录。
pub fn expand_home(path: &str) -> PathBuf {
    expand_home_with(path, std::env::var("HOME").ok().as_deref())
}

/// 上面那个的可测版本：家目录从外面喂进来，单测里不用碰真环境变量。
pub fn expand_home_with(path: &str, home: Option<&str>) -> PathBuf {
    let rest = if path == "~" {
        Some("")
    } else {
        path.strip_prefix("~/")
    };
    match (rest, home.filter(|h| !h.is_empty())) {
        (Some(rest), Some(home)) => Path::new(home).join(rest),
        // 拿不到家目录就原样交出去：下一步 `read` 会失败，报的是
        // 「读不了这张图：~/x.png」，比在这儿编一句「不知道家目录在哪儿」更有用。
        _ => PathBuf::from(path),
    }
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("cover")
        .to_string()
}

/// 界面 -> 信息任务。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InfoRequest {
    /// 拉一次当前标题 / 封面（进这一栏、字段还是空的时候）
    LoadMeta,
    /// 改标题（值就是输入框里那一串）
    SetTitle(String),
    /// 换封面。值可能是本地路径，也可能是 `.hdslb.com` 链接，判断在任务那边做。
    SetCover(String),
}

/// 信息任务 -> 界面。跟其他几条链一样：只动显示状态，失败也只写一句话，绝不 panic。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InfoEvent {
    /// `Room/get_info` 拉回来的当前标题 / 封面（值可能是空的，那不是错误）
    Meta { title: String, cover: String },
    /// 连当前标题都没拉到。**不是失败** —— 读不到不等于改不了，输入框照用。
    MetaFailed(String),
    /// 改标题的结果。`error` 是 `Some` 时值已经写回界面了，人可以直接改了重试。
    Title { title: String, error: Option<String> },
    /// 换封面的结果。成功时 `cover` 是**最后真正写下去的那个 hdslb 地址**
    /// （本地路径走完图床就变成它了）。
    Cover { cover: String, error: Option<String> },
    /// 封面的图本身，给预览那一格用。任务那边抓到就发。
    CoverImage { url: String, bytes: Vec<u8> },
    CoverImageFailed { url: String, error: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_http;
    use std::collections::HashMap;

    /// 表单体解成键值对。自己按 x-www-form-urlencoded 解一遍而不是拿字符串找子串：
    /// 子串匹配会把 `csrf` 和 `csrf_token` 看成一回事。
    fn form_of(body: &str) -> HashMap<String, String> {
        url::form_urlencoded::parse(body.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn client() -> BiliClient {
        BiliClient::new("SESSDATA=abc; bili_jct=tok123").unwrap()
    }

    /// 标题按**字符**拦，不按字节：中文一个字 3 字节，按字节算 14 个字就满了。
    /// emoji 跟中文一样按 Unicode 标量算（跟 Go 的 rune 口径一致）。
    #[test]
    fn the_title_limit_counts_characters_not_bytes() {
        assert!(check_title(&"汉".repeat(40)).is_ok(), "40 个中文该放行");
        assert!(check_title(&"汉".repeat(41)).is_err(), "41 个中文该拦下");
        assert!(
            check_title(&"汉".repeat(14)).is_ok(),
            "14 个中文才 42 字节，按字节算就被冤枉了"
        );
        assert!(check_title(&"😀".repeat(40)).is_ok(), "40 个 emoji 该放行");
        assert!(check_title(&"😀".repeat(41)).is_err(), "41 个 emoji 该拦下");
        assert!(check_title("").is_err(), "空标题不是合法标题");
    }

    /// 改标题的表单：五个字段一个不多一个不少，csrf 两个字段同一个值。
    #[tokio::test]
    async fn update_title_sends_exactly_those_five_fields() {
        let srv = test_http::start(|_| (200, r#"{"code":0,"message":"0","data":{}}"#.to_string()))
            .await;
        update_title(&client(), &srv.base, 9527, "新标题 空格")
            .await
            .unwrap();

        let r = &srv.hits()[0];
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, UPDATE_TITLE_PATH);
        assert!(
            r.header("content-type")
                .unwrap()
                .contains("x-www-form-urlencoded")
        );
        let f = form_of(&r.body);
        assert_eq!(f.get("room_id").map(String::as_str), Some("9527"));
        assert_eq!(f.get("title").map(String::as_str), Some("新标题 空格"));
        assert_eq!(f.get("platform").map(String::as_str), Some("pc_link"));
        assert_eq!(f.get("csrf").map(String::as_str), Some("tok123"));
        assert_eq!(
            f.get("csrf_token").map(String::as_str),
            Some("tok123"),
            "两个字段同一个值：老接口读 csrf、新接口读 csrf_token"
        );
        assert_eq!(f.len(), 5, "别顺手多塞字段：{f:?}");
    }

    /// 超长标题在**发请求之前**就被拦下：服务端那句错谁也看不懂，白跑一趟没意义。
    #[tokio::test]
    async fn an_over_long_title_never_reaches_the_network() {
        let srv = test_http::start(|_| (200, r#"{"code":0,"message":"0"}"#.to_string())).await;
        let err = update_title(&client(), &srv.base, 9527, &"汉".repeat(41))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("40"), "{err}");
        assert!(srv.hits().is_empty(), "一个请求都不该发出去");
    }

    /// 没有 `bili_jct` 就说清楚缺什么，别拿空串去换一句含糊的 `-111`。
    #[tokio::test]
    async fn without_bili_jct_nothing_is_sent() {
        let srv = test_http::start(|_| (200, r#"{"code":0,"message":"0"}"#.to_string())).await;
        let no_csrf = BiliClient::new("SESSDATA=abc").unwrap();
        for err in [
            update_title(&no_csrf, &srv.base, 9527, "标题")
                .await
                .unwrap_err(),
            update_cover(&no_csrf, &srv.base, "https://i0.hdslb.com/x.png")
                .await
                .unwrap_err(),
        ] {
            assert!(err.to_string().contains("bili_jct"), "{err}");
        }
        assert!(srv.hits().is_empty(), "一个请求都不该发出去");
    }

    /// 传图床那一次的 multipart 形状：字段名、文件名、文件内容、csrf。
    #[tokio::test]
    async fn upload_image_sends_the_three_multipart_fields() {
        let srv = test_http::start(|_| {
            (
                200,
                r#"{"code":0,"message":"0","data":{"image_url":"https://i0.hdslb.com/bfs/x.png"}}"#
                    .to_string(),
            )
        })
        .await;

        let dir = std::env::temp_dir().join(format!("bililive-upload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pic.png");
        // 文件内容故意用可见的 ASCII：假服务器那边 body 是按 UTF-8 解的，
        // 真图片是二进制，断言起来全是问号。
        std::fs::write(&file, b"FAKE-PNG-BYTES").unwrap();

        let url = upload_image(&client(), &srv.base, file.to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(url, "https://i0.hdslb.com/bfs/x.png");

        let r = &srv.hits()[0];
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, UPLOAD_IMAGE_PATH);
        let ct = r.header("content-type").unwrap();
        assert!(ct.contains("multipart/form-data"), "{ct}");
        assert!(ct.contains("boundary="), "{ct}");
        assert!(r.body.contains("name=\"bucket\""), "{}", r.body);
        assert!(r.body.contains(UPLOAD_BUCKET), "{}", r.body);
        assert!(r.body.contains("name=\"csrf\""), "{}", r.body);
        assert!(r.body.contains("tok123"), "{}", r.body);
        assert!(r.body.contains("name=\"file\""), "{}", r.body);
        assert!(r.body.contains("filename=\"pic.png\""), "{}", r.body);
        assert!(r.body.contains("FAKE-PNG-BYTES"), "文件内容要原样传上去");
        // 二进制图片经 lossy 转换会变，所以原始字节也留着（`body_raw`）
        assert!(
            r.body_raw
                .windows(14)
                .any(|w| w == b"FAKE-PNG-BYTES"),
            "原始字节里还得有那段内容"
        );

        let _ = std::fs::remove_file(&file);
    }

    /// 读不到文件要说清楚是哪个路径，而且不能 panic（用户在 TUI 里敲错一个字母很正常）。
    #[tokio::test]
    async fn a_missing_file_is_an_error_not_a_panic() {
        let srv = test_http::start(|_| (200, r#"{"code":0,"message":"0"}"#.to_string())).await;
        let err = upload_image(&client(), &srv.base, "/definitely/not/here.png")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("读不了这张图"), "{err}");
        assert!(err.contains("not/here.png"), "{err}");
        assert!(srv.hits().is_empty());
    }

    /// 换封面的表单：六个字段，**里面不能有 appkey / sign** ——
    /// 加了那套直播姬签名反而会被拒（网页端只认 csrf）。
    #[tokio::test]
    async fn update_cover_sends_no_app_signature() {
        let srv = test_http::start(|_| (200, r#"{"code":0,"message":"0"}"#.to_string())).await;
        update_cover(&client(), &srv.base, "https://i0.hdslb.com/bfs/x.png")
            .await
            .unwrap();

        // 路径钉死：这个接口挂在 app-blink 下，别被「顺手统一成直播姬那套」改走
        assert_eq!(
            UPDATE_COVER_PATH,
            "/xlive/app-blink/v1/preLive/UpdatePreLiveInfo"
        );
        let r = &srv.hits()[0];
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, UPDATE_COVER_PATH);
        let f = form_of(&r.body);
        assert_eq!(f.get("platform").map(String::as_str), Some("web"));
        assert_eq!(f.get("mobi_app").map(String::as_str), Some("web"));
        assert_eq!(f.get("build").map(String::as_str), Some("1"));
        assert_eq!(f.get("csrf").map(String::as_str), Some("tok123"));
        assert_eq!(f.get("csrf_token").map(String::as_str), Some("tok123"));
        assert_eq!(
            f.get("cover").map(String::as_str),
            Some("https://i0.hdslb.com/bfs/x.png")
        );
        assert_eq!(f.len(), 6, "别顺手多塞字段：{f:?}");
        assert!(!r.body.contains("appkey"), "不许带 appkey：{}", r.body);
        assert!(!r.body.contains("sign="), "不许带签名：{}", r.body);
    }

    /// `location` 是 `http://`，必须升成 `https://`；两个都给就优先 `image_url`。
    #[test]
    fn the_image_address_is_picked_from_either_field() {
        let both = serde_json::json!({
            "image_url": "https://i0.hdslb.com/a.png",
            "location": "http://i0.hdslb.com/b.png"
        });
        assert_eq!(pick_image_url(&both).unwrap(), "https://i0.hdslb.com/a.png");

        let only_location = serde_json::json!({"location": "http://i0.hdslb.com/b.png"});
        assert_eq!(
            pick_image_url(&only_location).unwrap(),
            "https://i0.hdslb.com/b.png",
            "location 是 http 的，要升成 https"
        );

        let only_image = serde_json::json!({"image_url": "//i0.hdslb.com/c.png"});
        assert_eq!(
            pick_image_url(&only_image).unwrap(),
            "https://i0.hdslb.com/c.png",
            "协议相对地址也要补上 https"
        );

        // 都不给：报错，绝不返回空串继续往下走（那样用户会看到「封面已提交」，
        // 而服务端其实用 100402 拒了）
        for empty in [
            serde_json::json!({}),
            serde_json::json!({"image_url": "", "location": ""}),
            serde_json::json!(null),
        ] {
            let err = pick_image_url(&empty).unwrap_err().to_string();
            assert!(err.contains("图床没返回图片地址"), "{err}");
        }
    }

    #[test]
    fn normalize_only_touches_the_scheme_prefix() {
        assert_eq!(normalize_image_url("http://a/x"), "https://a/x");
        assert_eq!(normalize_image_url("//a/x"), "https://a/x");
        assert_eq!(normalize_image_url("https://a/x"), "https://a/x");
        assert_eq!(
            normalize_image_url("https://a/x?u=http://b"),
            "https://a/x?u=http://b",
            "只换开头那一个"
        );
        // `user_cover` 那条路只补协议，不动 http —— 服务端给什么就抓什么
        assert_eq!(absolute_image_url("//a/x"), "https://a/x");
        assert_eq!(absolute_image_url("http://a/x"), "http://a/x");
        assert_eq!(absolute_image_url("https://a/x"), "https://a/x");
        assert!(is_link("https://i0.hdslb.com/x.png"));
        assert!(is_link("//i0.hdslb.com/x.png"));
        assert!(!is_link("~/photos/x.png"));
    }

    /// `~` 得自己展开：`File::open` / `tokio::fs::read` 都不认它。
    #[test]
    fn tilde_is_expanded_by_hand() {
        assert_eq!(
            expand_home_with("~/pic.png", Some("/home/tc191")),
            PathBuf::from("/home/tc191/pic.png")
        );
        assert_eq!(
            expand_home_with("~", Some("/home/tc191")),
            PathBuf::from("/home/tc191")
        );
        // 只有开头的 ~ 才是家目录，别处的原样留着
        assert_eq!(
            expand_home_with("/tmp/~/x.png", Some("/home/tc191")),
            PathBuf::from("/tmp/~/x.png")
        );
        assert_eq!(
            expand_home_with("./rel.png", Some("/home/tc191")),
            PathBuf::from("./rel.png")
        );
        // 拿不到家目录：原样交出去，让后面那句「读不了这张图」去说
        assert_eq!(expand_home_with("~/x.png", None), PathBuf::from("~/x.png"));
        assert_eq!(
            expand_home_with("~/x.png", Some("")),
            PathBuf::from("~/x.png")
        );
    }

    /// 服务端 `code != 0` 要变成 Err 往上传（界面拿它写顶栏那一行），
    /// 绝不能 panic。
    #[tokio::test]
    async fn an_api_error_becomes_err_not_panic() {
        let srv = test_http::start(|_| {
            (
                200,
                r#"{"code":100402,"message":"图片地址不合法"}"#.to_string(),
            )
        })
        .await;
        let err = update_cover(&client(), &srv.base, "https://example.com/x.png")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("100402"), "{err}");
        assert!(err.contains("图片地址不合法"), "{err}");
    }
}
