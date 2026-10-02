//! 配置：`~/.config/bililive/config.toml`
//!
//! 字段名用 snake_case，跟老 Go 版那套 PascalCase 不兼容 —— 它俩的配置文件是两个文件，
//! 第一次跑会在新路径生成一份默认的。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
        }
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
}
