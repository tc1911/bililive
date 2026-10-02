//! 命令行参数。就三个选项，手写解析够了 —— 为这点东西引 clap 不值。
//!
//!   -r/--room <房间号>   这次看别的房间，只影响本次运行，不写回配置
//!   -c/--config <路径>   换一份配置
//!   -h/--help

use anyhow::{Result, bail};
use std::path::PathBuf;

#[derive(Debug, Default, PartialEq)]
pub struct Cli {
    pub room: Option<i64>,
    pub config: Option<PathBuf>,
    pub help: bool,
}

impl Cli {
    pub const USAGE: &'static str = "\
bililive —— 哔哩哔哩直播弹幕 TUI

用法:
    bililive [选项]

选项:
    -r, --room <房间号>    这次看别的房间（只影响本次运行，不写回配置）
    -c, --config <路径>    换一份配置文件，默认 ~/.config/bililive/config.toml
    -h, --help             看这页";

    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self> {
        let mut cli = Cli::default();
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "-r" | "--room" => {
                    let v = it.next().unwrap_or_default();
                    cli.room = Some(
                        v.parse()
                            .map_err(|_| anyhow::anyhow!("房间号得是数字：{v}"))?,
                    );
                }
                "-c" | "--config" => cli.config = it.next().map(PathBuf::from),
                "-h" | "--help" => cli.help = true,
                other => bail!("不认识的参数：{other}（-h 看用法）"),
            }
        }
        Ok(cli)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli> {
        Cli::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn empty_args() {
        assert_eq!(parse(&[]).unwrap(), Cli::default());
    }

    #[test]
    fn room_short_and_long() {
        assert_eq!(parse(&["-r", "123"]).unwrap().room, Some(123));
        assert_eq!(parse(&["--room", "9527"]).unwrap().room, Some(9527));
    }

    #[test]
    fn config_path() {
        assert_eq!(
            parse(&["-c", "/tmp/x.toml"]).unwrap().config,
            Some(PathBuf::from("/tmp/x.toml"))
        );
    }

    // 房间号写错了要当场说清楚，别等连不上弹幕再猜。
    #[test]
    fn bad_room_is_rejected() {
        let err = parse(&["-r", "abc"]).unwrap_err().to_string();
        assert!(err.contains("房间号"), "{err}");
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(parse(&["--nope"]).is_err());
    }

    // 少给一个值不能让程序崩 —— 空房间号会走到「得是数字」那条错上。
    #[test]
    fn missing_value_does_not_panic() {
        assert!(parse(&["-r"]).is_err());
        assert_eq!(parse(&["-c"]).unwrap().config, None);
    }
}
