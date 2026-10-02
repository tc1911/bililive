//! 直播姬（bilibili link）的 app 签名。
//!
//! 开播 / 下播 / 版本号这三个接口用这一套：写死的 appkey + appsec，
//! 参数按 **key 排序** 拼成表单串，`sign = md5(串 + appsec)`，最后把 `&sign=` 接上去。
//!
//! **跟 web 端那套 WBI 是两码事**（`wbi.rs`：nav 下发的 img_key/sub_key 算 `w_rid`）。
//! 两套混着用服务端只回一句含糊的签名错，查起来极费劲。别的接口
//! （改标题 / 换封面 / 分区表 / 房间信息）也**不要**顺手加这套 ——
//! 它们只认 csrf，加了反而被拒。

use md5::{Digest, Md5};

/// 直播姬的 appkey。不是机密，但**必须**是这一个：换了服务端就认不出我们是谁。
pub const APP_KEY: &str = "aae92bc66f3edfab";
/// 直播姬的 appsec，签名用。它同样不是登录凭据，泄露了也开不了别人的播。
pub const APP_SECRET: &str = "af125a0d5279fd576c1b4418a3e8276d";

/// 按 Go 的 `url.QueryEscape`（query 组件那一套）编码一个值。
///
/// 为什么不用 `url::form_urlencoded::Serializer`：那个走 WHATWG 规范，
/// 把 `~` 编成 `%7E`、又把 `*` 留着不编，跟 Go 的 `url.Values.Encode()`
/// 正好在这两个字符上相反。签名是对着**这一串**算的，差一个字符服务端就算不出
/// 同一个 sign，而它只会回一句含糊的「签名校验失败」—— 极易错查成「appkey 不对」。
///
/// 本文件所有取值都是字母数字（时间戳、房间号、csrf），正常碰不到这两个字符；
/// 这一条留着是为了「跟 Go 对齐」这件事有据可查（单测里有黄金值钉死）。
pub fn go_query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match *b {
            // Go 的 unreserved：字母数字 + `-_.~`
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            // query 组件里空格变 `+`（不是 `%20`）
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// 把参数拼成要发出去的表单串；`sign` 为真时顺带算 app 签名。
///
/// 拼法跟 Go 的 `live/client.go:encodeParams` 一字不差：
/// 先加 `appkey`，一起按 key 排序，签名算的是**排序后的整串 + appsec**。
/// 所以排序必须发生一次、签名和发送用同一个串（各自排一遍也得一样，
/// 但多排一次就多一处能写错的地方）。
///
/// 空参数按 Go 的样子直接给空串（那一步连 appkey 都不加）。
pub fn encode_params(params: &[(&str, String)], sign: bool) -> String {
    if params.is_empty() {
        return String::new();
    }

    let mut kv: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect();
    if sign {
        kv.push(("appkey".to_string(), APP_KEY.to_string()));
    }
    kv.sort_by(|a, b| a.0.cmp(&b.0));

    let query = kv
        .iter()
        .map(|(k, v)| format!("{}={}", go_query_escape(k), go_query_escape(v)))
        .collect::<Vec<_>>()
        .join("&");

    if !sign {
        return query;
    }

    let mut hasher = Md5::new();
    hasher.update(query.as_bytes());
    hasher.update(APP_SECRET.as_bytes());
    format!("{query}&sign={}", hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 黄金值是拿 python 独立算的（只看规则、不看这份实现）：
    // 排序 -> Go 口径的表单编码 -> 加 appkey -> md5(串 + appsec) -> 接 &sign=。
    // **绝不能拿 encode_params 自己的输出当期望值** —— 那是循环论证，
    // 签名错的时候测试会跟着一起错，等于没测。

    /// 版本号接口：`system_version=2&ts=<毫秒>`（题面里的那个 GET）。
    #[test]
    fn golden_version_query_matches_python() {
        let params = [
            ("system_version", "2".to_string()),
            ("ts", "1700000000000".to_string()),
        ];
        assert_eq!(
            encode_params(&params, true),
            "appkey=aae92bc66f3edfab&system_version=2&ts=1700000000000\
             &sign=0145560363728c74c6e3f829a34d8991"
        );
    }

    /// 开播：九个字段 + appkey + sign。字段多、排序里 appkey 正好排第一个
    /// （`appkey` < `area_v2`），这条同时钉住「排序」和「拼接」。
    #[test]
    fn golden_start_live_query_matches_python() {
        let params = [
            ("room_id", "7734200".to_string()),
            ("platform", "pc_link".to_string()),
            ("backup_stream", "0".to_string()),
            ("csrf", "tok123".to_string()),
            ("csrf_token", "tok123".to_string()),
            ("area_v2", "371".to_string()),
            ("version", "9.9.9".to_string()),
            ("build", "12345".to_string()),
            ("ts", "1700000000000".to_string()),
        ];
        assert_eq!(
            encode_params(&params, true),
            "appkey=aae92bc66f3edfab&area_v2=371&backup_stream=0&build=12345\
             &csrf=tok123&csrf_token=tok123&platform=pc_link&room_id=7734200\
             &ts=1700000000000&version=9.9.9&sign=b4a62a22554557746afca5f135299228"
        );
    }

    /// 转义口径那一条：`~` 不转义、空格 `+`、`*` / `+` / `=` 全体百分号编码。
    /// 这正是「不能直接用 url crate 那套 WHATWG 编码」的原因，黄金值来自 python。
    #[test]
    fn golden_escaping_query_matches_python() {
        let params = [
            ("a", "x~y z*w".to_string()),
            ("b", "a+b=c".to_string()),
            ("empty", String::new()),
        ];
        assert_eq!(
            encode_params(&params, true),
            "a=x~y+z%2Aw&appkey=aae92bc66f3edfab&b=a%2Bb%3Dc&empty=\
             &sign=f75013d76cc338d5a92a03ac89782c3c"
        );

        // 跟 WHATWG 那套差在 `~` 和 `*` 上：两边都给出来，
        // 以后谁想「顺手换成 url crate 的编码」先看看这条会红。
        let whatwg =
            |s: &str| -> String { url::form_urlencoded::byte_serialize(s.as_bytes()).collect() };
        assert_eq!(go_query_escape("x~y z*w"), "x~y+z%2Aw");
        assert_eq!(whatwg("x~y z*w"), "x%7Ey+z*w");
        assert_ne!(go_query_escape("x~y z*w"), whatwg("x~y z*w"));
    }

    /// 不带签名的那些（下播）既没有 appkey 也没有 sign，只有排序好的字段本身。
    #[test]
    fn unsigned_query_has_neither_appkey_nor_sign() {
        let params = [
            ("room_id", "6".to_string()),
            ("platform", "pc_link".to_string()),
            ("csrf", "tok".to_string()),
            ("csrf_token", "tok".to_string()),
        ];
        assert_eq!(
            encode_params(&params, false),
            "csrf=tok&csrf_token=tok&platform=pc_link&room_id=6"
        );
    }

    /// 空参数照 Go 的样子给空串：那一步连 appkey 都不加。
    #[test]
    fn empty_params_sign_nothing() {
        assert_eq!(encode_params(&[], true), "");
        assert_eq!(encode_params(&[], false), "");
    }
}
