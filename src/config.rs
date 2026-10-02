//! 配置：`~/.config/bililive/config.toml`
//!
//! 字段名用 snake_case，跟老 Go 版那套 PascalCase 不兼容 —— 它俩的配置文件是两个文件，
//! 第一次跑会在新路径生成一份默认的。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::api::danmaku::LOCAL_KIND;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 登录 cookie，形如 `SESSDATA=…; bili_jct=…; DedeUserID=…; DedeUserID__ckMd5=…;`
    pub cookie: String,
    /// 直播间号
    pub room_id: i64,
    /// 开播分区（area_v2），选过之后写回来
    pub area_id: i64,
    /// 分区名，仅显示用
    pub area_name: String,
    /// 开播后把推流地址与密钥填进 OBS
    pub obs_fill: bool,
    /// OBS WebSocket 地址，空则 127.0.0.1
    pub obs_host: String,
    /// OBS WebSocket 端口，0 表示去读 OBS 自己的配置
    pub obs_port: u16,
    /// OBS WebSocket 密码，空表示去读 OBS 自己的配置
    pub obs_password: String,
    /// 弹幕单行显示
    pub single_line: bool,
    /// 弹幕显示时间
    pub show_time: bool,
    /// 鼠标滚轮能不能滚弹幕（默认开）。
    ///
    /// **默认开的代价**：开了之后终端会把鼠标事件交给程序，终端自己那套
    /// 「按住拖拽选中文字 / 双击选中一个词」就不管用了 —— 想选中文、复制推流密钥，
    /// 得**按住 Shift 再拖**（多数终端都留着这个后门）。受不了就在这儿写 false。
    pub mouse: bool,
    /// 弹幕屏蔽（只影响本地看到什么，见 `Block`）
    pub block: Block,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cookie: String::new(),
            room_id: 0,
            area_id: 0,
            area_name: String::new(),
            obs_fill: default_true(),
            obs_host: String::new(),
            obs_port: 0,
            obs_password: String::new(),
            single_line: true,
            show_time: true,
            mouse: true,
            block: Block::default(),
        }
    }
}

/// 弹幕屏蔽：命中就不往弹幕框里放。
///
/// **只管本地看到什么**：B 站账号侧那套「屏蔽设置」是另一个接口，这里一个字都不动 ——
/// 被挡掉的消息服务端照发、房间里的人照看见，只是我们这边不画。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Block {
    /// 按消息类型（弹幕协议里的 `cmd`）屏蔽，**完全相等**才算命中。
    ///
    /// 默认挡两种**房间广播**：`NOTICE_MSG`（「<%持续充能%>投喂<%某某%>1个沧月神玺，
    /// 快来围观」那种全房间滚动广播）和 `COMBO_SEND`（连击送礼，同一个人送礼刷出来的一串）。
    /// 这俩一刷起来能把弹幕框铺满，自己发的聊天被**埋掉**——就是它们的锅。
    /// 真有人送礼的 `SEND_GIFT`、以及所有聊天一律留着，那才是直播间的内容。
    pub types: Vec<String>,
    /// 按关键词屏蔽：内容里**包含**就算命中，不区分大小写；配置里前后的空格会被忽略。
    pub keywords: Vec<String>,
    /// 按用户名屏蔽：**完全相等**才算命中（只同几个字不算）。
    pub users: Vec<String>,
}

impl Default for Block {
    fn default() -> Self {
        Self {
            types: vec!["NOTICE_MSG".to_string(), "COMBO_SEND".to_string()],
            keywords: Vec::new(),
            users: Vec::new(),
        }
    }
}

impl Block {
    /// 这条消息该不该被挡掉。`kind` 是协议里的 `cmd`（`DANMU_MSG` / `SEND_GIFT`…）。
    ///
    /// 只吃三个 `&str`、不碰 `DanmuMsg`：纯函数，好直接断言。
    pub fn blocks(&self, kind: &str, author: &str, content: &str) -> bool {
        // 程序自己塞进弹幕框的那句话（断线重连 / 未登录 / 没配房间号）永远放行，
        // **用户在配置里写 `LOCAL` 也拦不住它**：屏幕上能告诉用户「出什么事了」的
        // 就这几句话，把它们一起屏蔽掉，现象是「程序坏了」，而没人会想到是过滤在干活。
        if kind == LOCAL_KIND {
            return false;
        }
        if self.types.iter().any(|t| t.trim() == kind) {
            return true;
        }
        // 空的（或只有空格的）关键词当没写：留着的话它会**匹配所有内容**，
        // 一条手滑的空串就等于把弹幕框清空，还得让人找半天原因。
        if self.keywords.iter().any(|k| {
            let k = k.trim();
            !k.is_empty() && content.to_lowercase().contains(&k.to_lowercase())
        }) {
            return true;
        }
        // 用户名**不 trim**：昵称里真可能有空格（或者前后正好有几个），
        // 掐掉就变成「照抄下来还是配不上」。
        self.users.iter().any(|u| u == author)
    }
}

impl Config {
    /// 读配置；文件不存在就写一份默认的再读回来。
    pub fn load_or_create(path: Option<&std::path::Path>) -> Result<Self> {
        let path = Self::resolve_path(path)?;
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("建配置目录失败: {}", dir.display()))?;
            }
            let cfg = Config::default();
            // 写的是**解析出来的那个**路径：`-c /tmp/x.toml` 指到别处时，
            // 以前会往默认路径写一份、然后当作「已经建好了」返回，那个文件根本不存在。
            cfg.save_to(&path)?;
            return Ok(cfg);
        }

        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("读不了配置文件: {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("配置解析失败: {}", path.display()))
    }

    /// 存到指定路径。
    ///
    /// **这是唯一的写入口**：以前还有一个写死默认路径的 `save()`，而 `-c` 给过别的
    /// 路径时那份配置根本不在那儿 —— 扫码登录成功要落盘，写错地方就变成
    /// 「这次登录下次启动就没了」。删掉它就是为了别再有人顺手用回去。
    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text).with_context(|| format!("写配置失败: {}", path.display()))
    }

    /// 这次运行到底该读/写哪个文件：显式给的（`-c`）优先，否则默认路径。
    pub fn resolve_path(explicit: Option<&std::path::Path>) -> Result<PathBuf> {
        match explicit {
            Some(p) => Ok(p.to_path_buf()),
            None => Self::path(),
        }
    }

    /// 选定开播分区：只动 `area_id` / `area_name` 两个字段。
    ///
    /// **先读一遍再写**。内存里那份配置是启动时读的，之后用户可能刚扫码登录过
    /// （`apply_login` 往同一个文件里写进了新 cookie），也可能在外面手改过房间号。
    /// 拿内存里那份旧配置整个覆盖上去，就等于把刚扫的登录、刚改的房间号一起冲掉
    /// —— 现象是「登录成功了，选个分区又变回未登录」。
    pub fn save_area(path: &Path, area_id: i64, area_name: &str) -> Result<()> {
        let mut cfg = Config::load_or_create(Some(path))?;
        cfg.area_id = area_id;
        cfg.area_name = area_name.to_string();
        cfg.save_to(path)
    }

    /// 退出登录：把 `cookie` 清成空串，**别的字段一个都不许动**。
    ///
    /// 形状跟 `save_area` 一样，同样是**先重读一遍再写**。退出登录常常发生在
    /// 「刚选完分区 / 刚改过房间号」之后，拿内存里那份启动时的配置整个覆盖上去，
    /// 就等于把用户刚改的东西一起冲掉了 —— 而界面看着一切正常。
    pub fn clear_cookie(path: &Path) -> Result<()> {
        let mut cfg = Config::load_or_create(Some(path))?;
        cfg.cookie = String::new();
        cfg.save_to(path)
    }

    pub fn path() -> Result<PathBuf> {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => {
                let home = std::env::var_os("HOME").context("拿不到 HOME")?;
                PathBuf::from(home).join(".config")
            }
        };
        Ok(base.join("bililive").join("config.toml"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试用自己的一份临时配置：测试是并行跑的，共用一个文件名会互相踩。
    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bililive-test-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    /// 写回分区只许动那两个字段：cookie、房间号、OBS 那些都得原封不动。
    #[test]
    fn save_area_touches_only_the_two_area_fields() {
        let path = temp_path("save-area");
        let cfg = Config {
            cookie: "SESSDATA=abc; bili_jct=def".into(),
            room_id: 9527,
            obs_fill: false,
            show_time: false,
            ..Config::default()
        };
        cfg.save_to(&path).unwrap();

        Config::save_area(&path, 371, "虚拟主播/虚拟日常").unwrap();

        let back = Config::load_or_create(Some(&path)).unwrap();
        assert_eq!(back.area_id, 371);
        assert_eq!(back.area_name, "虚拟主播/虚拟日常");
        assert_eq!(
            back.cookie, "SESSDATA=abc; bili_jct=def",
            "cookie 不许被冲掉"
        );
        assert_eq!(back.room_id, 9527, "房间号不许被冲掉");
        assert!(!back.obs_fill);
        assert!(!back.show_time);
        let _ = std::fs::remove_file(&path);
    }

    /// 内存里那份配置是**启动时**读的，选分区之前可能已经有别的链路写过盘
    /// （`apply_login` 就是这么干的）。写回分区必须重新读一遍。
    #[test]
    fn save_area_rereads_the_file_before_writing() {
        let path = temp_path("save-area-reread");
        let startup = Config {
            room_id: 6,
            ..Config::default()
        };
        startup.save_to(&path).unwrap();

        // 模拟「扫码登录刚落地」：另一条链路把新 cookie 写进了同一个文件
        let mut later = Config::load_or_create(Some(&path)).unwrap();
        later.cookie = "SESSDATA=fresh".into();
        later.save_to(&path).unwrap();

        Config::save_area(&path, 235, "娱乐/视频唱见").unwrap();

        let back = Config::load_or_create(Some(&path)).unwrap();
        assert_eq!(back.cookie, "SESSDATA=fresh", "写回分区前必须重读一遍文件");
        assert_eq!(back.area_id, 235);
        assert_eq!(back.room_id, 6);
        let _ = std::fs::remove_file(&path);
    }

    /// 退出登录只许清 `cookie`：文件里除了那一行，别的行必须**逐字节**还是原样。
    /// 用户退出登录多半是为了换个账号重扫，把他刚选的分区 / 房间号 / OBS 设置
    /// 一起冲掉，等于为了换个登录态把配置重置了一遍。
    #[test]
    fn clear_cookie_empties_only_that_field() {
        let path = temp_path("clear-cookie");
        let cfg = Config {
            cookie: "SESSDATA=abc; bili_jct=def; DedeUserID=7".into(),
            room_id: 9527,
            area_id: 371,
            area_name: "虚拟主播/虚拟日常".into(),
            obs_fill: false,
            obs_host: "192.168.1.9".into(),
            obs_port: 4455,
            obs_password: "hunter2".into(),
            single_line: false,
            show_time: false,
            mouse: false,
            block: Block {
                types: vec!["NOTICE_MSG".into(), "DANMU_MSG".into()],
                keywords: vec!["抽奖".into()],
                users: vec!["某人".into()],
            },
        };
        cfg.save_to(&path).unwrap();

        let before = std::fs::read_to_string(&path).unwrap();
        Config::clear_cookie(&path).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();

        let cookie_line = |text: &str| {
            text.lines()
                .find(|l| l.trim_start().starts_with("cookie"))
                .map(str::to_string)
        };
        assert_eq!(cookie_line(&after).as_deref(), Some("cookie = \"\""));

        let rest = |text: &str| -> Vec<String> {
            text.lines()
                .filter(|l| !l.trim_start().starts_with("cookie"))
                .map(str::to_string)
                .collect()
        };
        assert_eq!(
            rest(&before),
            rest(&after),
            "除了 cookie 那一行，别的行必须逐字节没变"
        );

        // 再解析一遍：文件长得一样，值也得一样
        let back = Config::load_or_create(Some(&path)).unwrap();
        assert_eq!(back.cookie, "", "cookie 该是空的");
        assert_eq!(back.room_id, 9527);
        assert_eq!(back.area_id, 371);
        assert_eq!(back.area_name, "虚拟主播/虚拟日常");
        assert_eq!(back.obs_host, "192.168.1.9");
        assert_eq!(back.obs_port, 4455);
        assert_eq!(back.obs_password, "hunter2");
        assert!(!back.obs_fill);
        assert!(!back.single_line);
        assert!(!back.show_time);
        assert!(!back.mouse, "滚轮开关也是「别的字段」，不许被顺手改掉");
        assert_eq!(
            back.block.types,
            vec!["NOTICE_MSG", "DANMU_MSG"],
            "屏蔽配置也是「别的字段」"
        );
        assert_eq!(back.block.keywords, vec!["抽奖"]);
        assert_eq!(back.block.users, vec!["某人"]);
        let _ = std::fs::remove_file(&path);
    }

    /// 跟 `save_area` 同理：清 cookie 之前必须重读文件。
    /// 内存里那份是启动时读的，中途可能已经有过一轮登录 / 改房间号落了盘 ——
    /// 整份覆盖回去，用户会看到「刚退完登录，分区和房间号变回默认值了」。
    #[test]
    fn clear_cookie_rereads_the_file_before_writing() {
        let path = temp_path("clear-cookie-reread");
        Config {
            cookie: "SESSDATA=stale".into(),
            ..Config::default()
        }
        .save_to(&path)
        .unwrap();

        // 模拟「用户刚在别处改过房间号」：另一条链路往同一个文件里写了新的值
        let mut later = Config::load_or_create(Some(&path)).unwrap();
        later.room_id = 6;
        later.area_id = 235;
        later.save_to(&path).unwrap();

        Config::clear_cookie(&path).unwrap();

        let back = Config::load_or_create(Some(&path)).unwrap();
        assert_eq!(back.cookie, "", "该清的清掉了");
        assert_eq!(back.room_id, 6, "清 cookie 之前必须重读一遍文件");
        assert_eq!(back.area_id, 235);
        let _ = std::fs::remove_file(&path);
    }

    /// 老配置文件里根本没有 `[block]` 这一段，也得照样跑起来，而且拿到的是那三个默认值：
    /// 挡房间广播（`NOTICE_MSG`）+ 连击送礼（`COMBO_SEND`），聊天和真送礼一律照留。
    #[test]
    fn an_old_config_without_the_block_section_gets_the_defaults() {
        let raw = "room_id = 6\nsingle_line = true\nmouse = true\n";
        let cfg: Config = toml::from_str(raw).expect("老配置必须能解析");
        assert_eq!(cfg.block.types, vec!["NOTICE_MSG", "COMBO_SEND"]);
        assert!(cfg.block.keywords.is_empty());
        assert!(cfg.block.users.is_empty());
    }

    /// 写了 `[block]` 但只写了其中一两项：没写的那些还按默认来
    /// （不能因为写了 `keywords` 就把默认要挡的广播放回来）。
    #[test]
    fn a_partial_block_section_keeps_the_other_defaults() {
        let cfg: Config = toml::from_str("[block]\nkeywords = [\"抽奖\"]\n").unwrap();
        assert_eq!(
            cfg.block.types,
            vec!["NOTICE_MSG", "COMBO_SEND"],
            "没写的字段还按默认"
        );
        assert_eq!(cfg.block.keywords, vec!["抽奖"]);
        assert!(cfg.block.users.is_empty());
    }

    /// 配置写坏了（比如把类型写成数字）要说人话别崩：报的那句话里得有
    /// 「解析失败 + 哪个文件」，用户才知道去改哪儿。
    #[test]
    fn a_broken_block_is_a_plain_error_not_a_panic() {
        let path = temp_path("broken-block");
        std::fs::write(&path, "[block]\ntypes = 1\n").unwrap();
        let err = Config::load_or_create(Some(&path))
            .expect_err("写成数字该是一句错误")
            .to_string();
        assert!(err.contains("配置解析失败"), "{err}");
        assert!(err.contains(&path.display().to_string()), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    /// 三条规则各来一发：类型**完全相等**、关键词**包含**、用户名**完全相等**。
    #[test]
    fn block_matches_types_keywords_and_users() {
        let b = Block {
            types: vec!["NOTICE_MSG".into()],
            keywords: vec!["抽奖".into()],
            users: vec!["小明".into()],
        };
        assert!(b.blocks("NOTICE_MSG", "随便谁", "随便什么话"));
        assert!(!b.blocks("DANMU_MSG", "随便谁", "随便什么话"), "聊天不在列表里");
        assert!(b.blocks("DANMU_MSG", "小红", "快来抽奖啊"));
        assert!(b.blocks("DANMU_MSG", "小明", "他说什么都挡"));
    }

    /// 类型只认完全相等，而且**不忽略大小写** —— cmd 是协议里定死的大写。
    #[test]
    fn block_type_is_exact() {
        let b = Block {
            types: vec!["NOTICE_MSG".into()],
            ..Block::default()
        };
        assert!(!b.blocks("NOTICE_MSGG", "", ""), "多一个字母不算");
        assert!(!b.blocks("notice_msg", "", ""), "类型不忽略大小写");
    }

    /// 关键词：内容里包含就算命中、**不区分大小写**、配置里前后的空格不算数。
    #[test]
    fn block_keyword_contains_ignores_case_and_surrounding_spaces() {
        let b = Block {
            keywords: vec!["  HeLLo ".into()],
            ..Block::default()
        };
        assert!(b.blocks("DANMU_MSG", "", "hello world"));
        assert!(b.blocks("DANMU_MSG", "", "有人 HELLO 你"));
        assert!(b.blocks("DANMU_MSG", "", "hello"));
        assert!(!b.blocks("DANMU_MSG", "", "helo"), "不是包含关系就别挡");
    }

    /// 用户名只认**完全相等**：多一个字、少一个字都不算命中。
    #[test]
    fn block_user_is_exact_and_a_partial_name_does_not_hit() {
        let b = Block {
            users: vec!["小明".into()],
            ..Block::default()
        };
        assert!(b.blocks("DANMU_MSG", "小明", ""));
        assert!(!b.blocks("DANMU_MSG", "小明明", ""), "多一个字不算");
        assert!(!b.blocks("DANMU_MSG", "明", ""), "少一个字不算");
    }

    /// 三个列表全空就是「什么都别挡」（用户把默认那两条删掉时配出来的样子）。
    #[test]
    fn an_empty_block_lets_everything_through() {
        let b = Block {
            types: Vec::new(),
            keywords: Vec::new(),
            users: Vec::new(),
        };
        assert!(!b.blocks("NOTICE_MSG", "system", "快来围观"));
        assert!(!b.blocks("DANMU_MSG", "小明", "随便说点什么"));
    }

    /// 空的（或只有空格的）关键词当没写：留着的话它会**匹配所有内容** ——
    /// 一条手滑的空串等于把弹幕框清空，还得让人找半天原因。
    #[test]
    fn an_empty_keyword_matches_nothing() {
        let b = Block {
            keywords: vec!["".into(), "   ".into()],
            ..Block::default()
        };
        assert!(!b.blocks("DANMU_MSG", "小明", "随便什么"));
    }

    /// 程序自己那几句话**永远不受屏蔽配置影响** —— 连把 `LOCAL` 写进 `types` 也拦不住它。
    /// 默认那两条本来就是拿来挡房间广播的，早期本地提示若借用 `system` / `NOTICE_MSG`
    /// 这类协议里的名字，一过滤就会连「弹幕服务器已断开，正在重连」一起吃掉，
    /// 用户只会以为程序坏了。
    #[test]
    fn block_never_hides_our_own_messages() {
        let b = Block {
            types: vec!["LOCAL".into(), "NOTICE_MSG".into(), "SYSTEM".into()],
            keywords: vec!["断开".into()],
            users: vec!["system".into()],
        };
        assert!(!b.blocks(LOCAL_KIND, "system", "弹幕服务器已断开，正在重连"));
    }
}
