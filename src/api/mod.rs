//! 跟 B 站说话的部分。除了 HTTP 和 JSON，别的什么都不干 —— 界面那边不碰 URL。

pub mod client;
pub mod danmaku;
pub mod login;
pub mod room;
pub mod send;
pub mod wbi;

/// 只给测试用的假 HTTP 服务器。
#[cfg(test)]
pub mod test_http;
