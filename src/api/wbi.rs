//! WBI 签名。
//!
//! B 站 2025-05-26 起对 `getDanmuInfo` 这类接口强制签名，缺签名一律返 `-352` 风控、
//! `host_list` 为空。算法：拿 nav 下发的 img_key/sub_key 按固定置换表拼出 mixin key，
//! 参数按 key 排序拼成 query，再拼 wts 和 mixin key 做 md5 得到 w_rid。

use md5::{Digest, Md5};

/// 置换表，来自 B 站 web 端 JS。**不要改**，改一个字符签名就全错。
const MIXIN_KEY_ENC_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// 从 nav 给的 img_url/sub_url 里抠出 key（去掉目录和 .png）。
pub fn key_from_url(url: &str) -> String {
    url.rsplit('/')
        .next()
        .unwrap_or(url)
        .trim_end_matches(".png")
        .to_string()
}

/// 按置换表把 img_key/sub_key 拼成 mixin key。
pub fn mixin_key(img_key: &str, sub_key: &str) -> String {
    let raw: Vec<char> = format!("{img_key}{sub_key}").chars().collect();
    let mut out = String::with_capacity(32);
    for &i in MIXIN_KEY_ENC_TAB.iter() {
        if let Some(c) = raw.get(i) {
            out.push(*c);
        }
    }
    out.truncate(32);
    out
}

/// 给参数补上 wts、算出 w_rid，返回可以直接拼在 URL 后面的 query 串。
pub fn sign(params: &[(&str, String)], mixin_key: &str, wts: i64) -> String {
    let mut kv: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (k.to_string(), filter_value(v)))
        .collect();
    kv.push(("wts".to_string(), wts.to_string()));
    kv.sort_by(|a, b| a.0.cmp(&b.0));

    let query = kv
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");

    let mut hasher = Md5::new();
    hasher.update(query.as_bytes());
    hasher.update(mixin_key.as_bytes());
    let w_rid = hex::encode(hasher.finalize());

    format!("{query}&w_rid={w_rid}")
}

/// 值里的 `!'()*` 必须先剔掉，否则服务端算出来的 w_rid 跟我们对不上。
fn filter_value(v: &str) -> String {
    v.chars().filter(|c| !"!'()*".contains(*c)).collect()
}

/// 跟 JS 的 encodeURIComponent 对齐：只留 `A-Za-z0-9-_.!~*'()`，
/// 其余按 UTF-8 百分号编码，空格是 `%20` 而不是 `+`。
fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}


#[cfg(test)]
mod tests {
    use super::*;

    // 这两个 key 是 nav 公开下发的种子（每天轮换），只用来钉住算法，不是凭据。
    const GOLDEN_IMG: &str = "7cd084941338484aae1ad9425b84077c";
    const GOLDEN_SUB: &str = "4932caff0ff746eab6f01bf08b70ac45";
    const GOLDEN_MIXIN: &str = "ea1db124af3c7062474693fa704f4ff8";

    // 跟 Go 版一字不差的两个黄金值：置换表或拼接口径错了就全错。
    #[test]
    fn mixin_key_matches_go() {
        assert_eq!(mixin_key(GOLDEN_IMG, GOLDEN_SUB), GOLDEN_MIXIN);
    }

    #[test]
    fn sign_matches_go() {
        let params = [
            ("id", "123456".to_string()),
            ("type", "0".to_string()),
            ("web_location", "444.8".to_string()),
        ];
        let got = sign(&params, GOLDEN_MIXIN, 1_700_000_000);
        assert_eq!(
            got,
            "id=123456&type=0&web_location=444.8&wts=1700000000&w_rid=54a1e0ac653a45674a24985c37fe7e32"
        );
    }

    #[test]
    fn sign_strips_banned_chars() {
        let a = sign(&[("q", "a!b'c(d)e*f".to_string())], GOLDEN_MIXIN, 1_700_000_000);
        let b = sign(&[("q", "abcdef".to_string())], GOLDEN_MIXIN, 1_700_000_000);
        assert_eq!(a, b);
    }
}
