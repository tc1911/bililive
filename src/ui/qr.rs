//! 二维码的半格字符渲染。
//!
//! 一格字符画上下两个模块（`▀` / `▄` / `█` / 空格），所以字符行数正好是
//! 模块行数的一半 —— 二维码在终端里才不至于占掉半屏。
//!
//! 颜色走**真彩色** `#000000` / `#ffffff`，不是 `Color::Black` / `Color::White`：
//! 那两个名字走的是终端调色板，浅色主题下「黑」会被映射成接近背景的颜色，
//! 整张码糊成一片、手机扫不出来（Go 版踩过这个坑，现象是「换了浅色主题就扫不动」）。

use qrcode::QrCode;
use ratatui::prelude::*;

/// 纠错等级。跟 Go 版一样用 **Low**：登录地址有一百来个字符，
/// 用默认的 Medium 会大一整个版本（真地址实测 41 模块 vs 49，即 25 行 vs 29 行），
/// 多出来的四行在终端里就是「二维码下半截被切掉、扫不出来」。
/// 屏幕上没有污损和反光，低纠错完全够用。
const EC: qrcode::EcLevel = qrcode::EcLevel::L;

/// 静默区（二维码四边那一圈白边）的宽度，单位是模块。
///
/// crate 给的模块矩阵是**不含**静默区的，而扫码要靠这圈白边把码从背景里框出来 ——
/// 不给它，摄像头常常定位不到。规范要求 4 个模块，micro 码是 2。
const QUIET: usize = 4;

/// 把内容编成二维码，返回一行行的半格字符（每行 `side` 个字符）。
///
/// 内容太长（编不出来）就返回 `Err`，由调用方画一行说明 —— **绝不 panic**。
pub fn half_blocks(content: &str) -> Result<Vec<String>, String> {
    let code = QrCode::with_error_correction_level(content.as_bytes(), EC)
        .map_err(|e| format!("二维码生成失败：{e}"))?;
    let width = code.width();
    let colors = code.to_colors();
    let side = width + QUIET * 2;

    let dark = |x: usize, y: usize| -> bool {
        if x < QUIET || y < QUIET || x >= QUIET + width || y >= QUIET + width {
            return false; // 静默区一律是「白」
        }
        colors[(y - QUIET) * width + (x - QUIET)] == qrcode::Color::Dark
    };

    let mut rows = Vec::with_capacity(side.div_ceil(2));
    let mut y = 0;
    while y < side {
        let mut line = String::with_capacity(side);
        for x in 0..side {
            let up = dark(x, y);
            // 行数是奇数时最后一行没有下半格，按「白」处理
            let down = y + 1 < side && dark(x, y + 1);
            line.push(match (up, down) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        rows.push(line);
        y += 2;
    }
    Ok(rows)
}

/// 画给 ratatui 的几行：黑字白底，一格一个模块的一半。
pub fn lines(content: &str) -> Vec<Line<'static>> {
    match half_blocks(content) {
        Ok(rows) => rows
            .into_iter()
            .map(|r| Line::from(Span::styled(r, module_style())))
            .collect(),
        // 编不出来也只是少一张码，说清楚就行
        Err(e) => vec![Line::from(Span::styled(
            e,
            Style::default().fg(Color::Red),
        ))],
    }
}

/// 前景黑、背景白。改这里之前先想想浅色主题。
fn module_style() -> Style {
    Style::default()
        .fg(Color::Rgb(0, 0, 0))
        .bg(Color::Rgb(255, 255, 255))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 行数 / 列数的关系：每行 `side` 个半格字符，行数是 `side` 的一半（向上取整）。
    /// 这条关系是「终端里画得下」的全部依据，格子数错了码就废了。
    #[test]
    fn rows_are_half_of_columns() {
        let rows = half_blocks("https://passport.bilibili.com/h5/login?qrcode_key=abcdef")
            .expect("这么短的内容编得出来");
        let cols = rows[0].chars().count();
        assert!(cols > 0);
        assert!(
            rows.iter().all(|r| r.chars().count() == cols),
            "每行的字符数必须一样，不然画出来是歪的"
        );
        assert_eq!(rows.len(), cols.div_ceil(2));
        // 「两倍关系」的另一种写法：2×行数 >= 列数，而且 (行数-1)×2 < 列数
        assert!(rows.len() * 2 >= cols && (rows.len() - 1) * 2 < cols);
    }

    /// 只许出现这四种字符。混进别的（比如 `#`）就说明有人改了画法，
    /// 那种字符在窄终端上宽度不定，整张码会错位。
    #[test]
    fn only_half_block_characters() {
        let rows = half_blocks("hello bililive").unwrap();
        for r in &rows {
            for c in r.chars() {
                assert!(
                    matches!(c, '█' | '▀' | '▄' | ' '),
                    "混进了不该有的字符 {c:?}"
                );
            }
        }
    }

    /// 静默区得真的是白的：四边各 4 个模块，一个都不能是黑的，
    /// 不然摄像头定位不到（这是扫码失败最常见的原因）。
    ///
    /// 按**半格**验，不按整行验：一个字符画上下两个模块，
    /// 正好压在边界上的那一行只有一半属于静默区（上半黑、下半白是正常的）。
    #[test]
    fn quiet_zone_is_white_on_all_four_sides() {
        let rows = half_blocks("https://live.bilibili.com/").unwrap();
        let side = rows[0].chars().count();
        for (i, row) in rows.iter().enumerate() {
            let (y0, y1) = (i * 2, i * 2 + 1);
            for (x, ch) in row.chars().enumerate() {
                let up_dark = matches!(ch, '█' | '▀');
                let down_dark = matches!(ch, '█' | '▄');
                let margin = x < QUIET || x >= side - QUIET;
                assert!(!margin || !(up_dark || down_dark), "第 {i} 行第 {x} 格是左右白边");
                assert!(
                    !(y0 < QUIET || y0 >= side - QUIET) || !up_dark,
                    "第 {i} 行第 {x} 格上半属于上下白边"
                );
                assert!(
                    !(y1 < QUIET || y1 >= side - QUIET) || !down_dark,
                    "第 {i} 行第 {x} 格下半属于上下白边"
                );
            }
        }
    }

    /// 编不出来的内容只给一行说明，绝不 panic。
    #[test]
    fn oversized_content_does_not_panic() {
        let huge = "a".repeat(10_000);
        let rows = half_blocks(&huge);
        assert!(rows.is_err(), "这么长编不出来，要老老实实报错");
        let lines = lines(&huge);
        assert_eq!(lines.len(), 1);
    }

    /// 把画出来的字符反解回模块，跟 crate 给的原始矩阵逐格对上。
    ///
    /// 少一个模块、或者上下翻了半格，二维码就扫不出来了 —— 而这种错
    /// 肉眼盯着半格字符是看不出来的。这一条是「画出来的码还是原来那张码」的证明。
    #[test]
    fn rendered_blocks_round_trip_to_the_module_matrix() {
        let content = "https://passport.bilibili.com/h5/login?qrcode_key=0123456789abcdef";
        let rows = half_blocks(content).unwrap();
        // 必须用同一个纠错等级编，不然比对的是两张不同的码
        let code = QrCode::with_error_correction_level(content.as_bytes(), EC).unwrap();
        let width = code.width();
        let colors = code.to_colors();
        for y in 0..width {
            for x in 0..width {
                let ch = rows[(y + QUIET) / 2].chars().nth(x + QUIET).unwrap();
                let dark = match ch {
                    '█' => true,
                    // 一个字符画上下两格，落在哪一半由行号的奇偶决定
                    '▀' => (y + QUIET).is_multiple_of(2),
                    '▄' => !(y + QUIET).is_multiple_of(2),
                    ' ' => false,
                    other => panic!("混进了不该有的字符 {other:?}"),
                };
                assert_eq!(
                    dark,
                    colors[y * width + x] == qrcode::Color::Dark,
                    "第 {y} 行第 {x} 列对不上"
                );
            }
        }
    }

    /// 颜色必须是真彩色：`Color::Black`/`White` 走终端调色板，
    /// 浅色主题下整张码会和背景糊在一起。
    #[test]
    fn rendered_lines_use_truecolor_not_the_palette() {
        let line = &lines("https://live.bilibili.com/")[0];
        let style = line.spans[0].style;
        assert_eq!(style.fg, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(style.bg, Some(Color::Rgb(255, 255, 255)));
    }
}

