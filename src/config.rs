//! 配置：`~/.config/bililive/config.toml`
//!
//! 字段名用 snake_case，跟老 Go 版那套 PascalCase 不兼容 —— 它俩的配置文件是两个文件，
//! 第一次跑会在新路径生成一份默认的。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
