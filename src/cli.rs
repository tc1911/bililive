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
    bililive [房间号] [选项]
    bililive 6                    # 等价于 -r 6

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
                // 裸的数字也当房间号：`bililive 6` 等于 `-r 6`。
                // 这个写法不是顺手加的 —— `cargo run -r 6` 里的 -r 会被 cargo 自己
                // 当成 --release 吃掉，程序只收到一个孤零零的 6，很容易踩。
                other if !other.starts_with('-') => {
                    cli.room = Some(
                        other
                            .parse()
                            .map_err(|_| anyhow::anyhow!("房间号得是数字：{other}"))?,
                    );
                }
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

    // 裸位置参数：cargo run -r 6 里的 -r 会被 cargo 吃掉，只剩个 6，
    // 那条路得能走通。
    #[test]
    fn bare_room_is_accepted() {
        assert_eq!(parse(&["6"]).unwrap().room, Some(6));
        assert_eq!(parse(&["-r", "1", "6"]).unwrap().room, Some(6)); // 后面的覆盖前面
    }

    #[test]
    fn bare_non_number_is_rejected() {
        assert!(parse(&["abc"]).is_err());
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
