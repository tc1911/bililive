//! 封面预览：把一张图画进终端。
//!
//! 终端没有像素，只能拿一个字符格当两个像素使：上半个用前景色、下半个用背景色，
//! 字符用 `▀`（跟 `ui/qr.rs` 画二维码是同一个套路）。
//!
//! 抓图和解码都不在这儿 —— 图是信息任务抓回来、从 `InfoEvent::CoverImage` 喂进来的。
//! 这里只做三件事：解一次码、缩到一个小尺寸存着、**按控件当前大小现采样**。
//! 存原图缩放再采样是有意的：终端一拉伸画面自己就跟着变，不用重新抓图。

use anyhow::{Context, Result};
use image::RgbaImage;
use ratatui::prelude::*;

/// 存下来的采样上限（像素）。控件最大也就百来格宽，再大是白算
/// （上限本身跟 Go 版 `ui/cover` 一致）。
pub const SAMPLE_W: u32 = 96;
pub const SAMPLE_H: u32 = 140;

/// 预览那一格的状态。默认（没图没地址）显示一句「还没设封面」。
#[derive(Default)]
pub struct Cover {
    /// 手上这张图是哪个地址来的（跟传进来的比一比就知道要不要重画）
    url: String,
    /// 缩好的小图。`None` = 还没有图（没抓 / 抓失败 / 解不开）
    img: Option<RgbaImage>,
    /// 「加载失败：…」这类话，有它就显示它
    hint: String,
}

impl Cover {
    /// 收到一张图。解不开只留一句话 —— 一张坏图绝不能把整个界面带走（TUI 里 panic 就是整屏消失）。
    pub fn set_bytes(&mut self, url: &str, bytes: &[u8]) {
        match decode_small(bytes) {
            Ok(img) => {
                self.url = url.to_string();
                self.img = Some(img);
                self.hint.clear();
            }
            Err(e) => self.fail(url, &e.to_string()),
        }
    }

    /// 图没抓到（网络 / HTTP 状态码）。照样把地址记下来：地址变了再试一次，
    /// 同一张图不会因为一次失败就反复重抓。
    pub fn fail(&mut self, url: &str, error: &str) {
        self.url = url.to_string();
        self.img = None;
        self.hint = format!("封面没加载上：{error}");
    }

    /// 这一格现在该画什么。`expect_url` 是界面记着的封面地址 ——
    /// 有地址却还没图，说明抓取在路上（任务那边刚发出去），提示写「加载中」而不是「没有封面」。
    pub fn lines(&self, width: u16, height: u16, expect_url: &str) -> Vec<Line<'static>> {
        if let Some(img) = &self.img {
            return render(img, width, height);
        }
        let hint = if !self.hint.is_empty() {
            self.hint.clone()
        } else if !self.url.is_empty() || !expect_url.is_empty() {
            "封面加载中…".to_string()
        } else {
            "还没有封面".to_string()
        };
        // 一行说明占整格：宽字符按终端宽度算，超了让 ratatui 自己截
        vec![Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        ))]
        .into_iter()
        .take(height as usize)
        .collect()
    }
}

/// 解码 + 缩到 `SAMPLE_W × SAMPLE_H` 以内（保持比例，绝不放大）。
pub fn decode_small(bytes: &[u8]) -> Result<RgbaImage> {
    let img = image::load_from_memory(bytes).context("这张图解不开（不是 png / jpeg，或者文件坏了）")?;
    Ok(downscale(&img.to_rgba8(), SAMPLE_W, SAMPLE_H))
}

/// 按区域平均缩小。封面是几 MB 的照片，逐像素采样太慢，先缩一遍；
/// 区域平均（而不是隔点取样）是为了不把细密的图缩成一片雪花。
pub fn downscale(src: &RgbaImage, max_w: u32, max_h: u32) -> RgbaImage {
    let (sw, sh) = src.dimensions();
    if sw == 0 || sh == 0 || max_w == 0 || max_h == 0 {
        return RgbaImage::new(0, 0);
    }
    // 保持比例：原图更宽就以宽为准，更高就以高为准
    let (mut w, mut h) = (max_w, max_h);
    if sw * max_h > sh * max_w {
        h = (sh * max_w / sw).max(1);
    } else {
        w = (sw * max_h / sh).max(1);
    }
    let w = w.min(sw).max(1);
    let h = h.min(sh).max(1);

    let mut dst = RgbaImage::new(w, h);
    for y in 0..h {
        let y0 = y * sh / h;
        let y1 = ((y + 1) * sh / h).max(y0 + 1).min(sh);
        for x in 0..w {
            let x0 = x * sw / w;
            let x1 = ((x + 1) * sw / w).max(x0 + 1).min(sw);
            let mut sum = [0u64; 4];
            let mut n = 0u64;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = src.get_pixel(sx, sy).0;
                    for (i, v) in p.iter().enumerate() {
                        sum[i] += *v as u64;
                    }
                    n += 1;
                }
            }
            if n == 0 {
                continue;
            }
            let avg = [
                (sum[0] / n) as u8,
                (sum[1] / n) as u8,
                (sum[2] / n) as u8,
                (sum[3] / n) as u8,
            ];
            dst.put_pixel(x, y, image::Rgba(avg));
        }
    }
    dst
}

/// 把图按比例摆进 `width × (height * 2)` 个像素的框里居中，一格画上下两个像素。
///
/// 尺寸完全由**控件当前大小**决定（不是抓图时定的），所以终端一拉伸，
/// 下一帧就按新的宽高重新采样 —— 这就是「不用重新抓图」那件事。
pub fn render(img: &RgbaImage, width: u16, height: u16) -> Vec<Line<'static>> {
    let (w, h) = (width as u32, height as u32);
    let (iw, ih) = img.dimensions();
    if w == 0 || h == 0 || iw == 0 || ih == 0 {
        return Vec::new();
    }
    let (mut dw, mut dh) = (w, 2 * h);
    if iw * 2 * h > ih * w {
        dh = ih * w / iw;
    } else {
        dw = iw * 2 * h / ih;
    }
    let dw = dw.clamp(1, w);
    // 半格是两行一对：奇数行会让最下面那一行只剩上半格，先对齐到偶数
    let dh = (dh.clamp(1, 2 * h) / 2) * 2;
    let ox = (w - dw) / 2;
    let oy = (h - dh / 2) / 2;

    let mut lines: Vec<Line<'static>> = Vec::new();
    for _ in 0..oy {
        lines.push(Line::from("")); // 上下居中：上面留白
    }
    for row in 0..dh / 2 {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(dw as usize + 1);
        if ox > 0 {
            spans.push(Span::raw(" ".repeat(ox as usize))); // 左右居中
        }
        for col in 0..dw {
            let top = sample(img, col, row * 2, dw, dh);
            let bottom = sample(img, col, row * 2 + 1, dw, dh);
            spans.push(Span::styled("▀", Style::default().fg(top).bg(bottom)));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// 取缩放后第 (cx, cy) 个像素的颜色，cx/cy 是在 dw×dh 这个网格里。
fn sample(img: &RgbaImage, cx: u32, cy: u32, dw: u32, dh: u32) -> Color {
    let (iw, ih) = img.dimensions();
    let sx = (cx * iw / dw).min(iw.saturating_sub(1));
    let sy = (cy * ih / dh).min(ih.saturating_sub(1));
    let p = img.get_pixel(sx, sy).0;
    // 用真彩色（Rgb）而不是那 16 个调色板色：封面是照片，调色板色画出来是一块一块的
    Color::Rgb(p[0], p[1], p[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba};
    use std::io::Cursor;

    /// 造一张真 PNG（走编码器而不是塞假字节）：解码那条路要真跑一遍才算验过。
    fn png_of(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.put_pixel(x, y, Rgba(f(x, y)));
            }
        }
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn a_real_png_comes_back_at_the_same_size() {
        let img = decode_small(&png_of(4, 2, |_, _| [200, 30, 30, 255])).unwrap();
        assert_eq!(img.dimensions(), (4, 2), "比采样上限小的图不该被放大");
        assert_eq!(img.get_pixel(0, 0).0, [200, 30, 30, 255]);
    }

    /// 大图缩到上限以内，而且比例不变（封面是 16:9 的照片，缩完还得像那张照片）。
    #[test]
    fn a_big_image_is_downscaled_inside_the_cap() {
        let img = decode_small(&png_of(400, 200, |_, _| [1, 2, 3, 255])).unwrap();
        let (w, h) = img.dimensions();
        assert!(w <= SAMPLE_W && h <= SAMPLE_H, "{w}x{h}");
        assert_eq!((w, h), (96, 48), "400x200（2:1）该缩成 96x48");
    }

    /// 解不开的字节不能 panic：留一句话，界面照常画。
    #[test]
    fn garbage_bytes_are_a_message_not_a_panic() {
        let err = decode_small("这根本不是图".as_bytes())
            .unwrap_err()
            .to_string();
        assert!(err.contains("解不开"), "{err}");

        let mut cover = Cover::default();
        cover.set_bytes("https://i0.hdslb.com/x.png", b"nope");
        let lines = cover.lines(20, 3, "https://i0.hdslb.com/x.png");
        assert_eq!(lines.len(), 1);
        assert!(!cover.hint.is_empty());
    }

    /// 没有图的时候三句话分得清：还没设 / 在加载 / 加载失败。
    #[test]
    fn the_placeholder_says_which_state_it_is_in() {
        let cover = Cover::default();
        let text = |l: &[Line<'static>]| l[0].spans[0].content.to_string();
        assert!(text(&cover.lines(20, 3, "")).contains("还没有封面"));
        assert!(text(&cover.lines(20, 3, "https://i0.hdslb.com/x.png")).contains("加载中"));

        let mut failed = Cover::default();
        failed.fail("https://i0.hdslb.com/x.png", "HTTP 404");
        assert!(text(&failed.lines(20, 3, "https://i0.hdslb.com/x.png")).contains("HTTP 404"));
    }

    /// 半格字符 + 真彩色：一格一个 `▀`，前景是上半像素、背景是下半像素。
    #[test]
    fn each_cell_is_one_half_block_with_two_colors() {
        // 上半红、下半蓝的 2×2 图
        let img = RgbaImage::from_fn(2, 2, |_, y| {
            if y == 0 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 255])
            }
        });
        let lines = render(&img, 2, 1);
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 2, "2 格宽就该有 2 个 span：{spans:?}");
        for s in spans {
            assert_eq!(s.content, "▀");
            assert_eq!(s.style.fg, Some(Color::Rgb(255, 0, 0)), "上半个是前景色");
            assert_eq!(s.style.bg, Some(Color::Rgb(0, 0, 255)), "下半个是背景色");
        }
    }

    /// 尺寸跟着控件走：同一个图换个更宽的框，画出来的列数就跟着变 ——
    /// 终端一拉伸画面自己就变，不用重新抓图。
    #[test]
    fn the_drawing_follows_the_widget_size() {
        let img = RgbaImage::from_fn(40, 20, |x, _| Rgba([x as u8, 0, 0, 255]));
        let narrow = render(&img, 10, 10);
        let wide = render(&img, 30, 10);
        let width = |ls: &[Line<'static>]| ls.iter().map(|l| l.width()).max().unwrap_or(0);
        assert!(width(&wide) > width(&narrow), "{wide:?} vs {narrow:?}");
        // 宽框里图的宽度受高度限制（2:1 的图在 10 行 = 20 像素高的框里最多 40 列）
        assert!(width(&wide) <= 30);
    }

    /// 小终端 / 空框不许 panic，也不许画出一堆空 span（那一格本来就是 0 宽 0 高）。
    #[test]
    fn tiny_or_empty_areas_do_not_panic() {
        let img = RgbaImage::from_fn(8, 8, |_, _| Rgba([9, 9, 9, 255]));
        for (w, h) in [(0u16, 0u16), (1, 1), (1, 0), (0, 3), (3, 1)] {
            let lines = render(&img, w, h);
            assert!(lines.len() <= h as usize, "{w}x{h} 画出了 {} 行", lines.len());
        }
        let empty = RgbaImage::new(0, 0);
        assert!(render(&empty, 10, 3).is_empty());
        assert_eq!(downscale(&empty, 96, 140).dimensions(), (0, 0));
    }

    /// 区域平均：一整块同样的颜色缩完还是那个颜色（不是黑、也不是雪花）。
    #[test]
    fn downscaling_averages_the_area() {
        let src = RgbaImage::from_pixel(8, 8, Rgba([40, 80, 120, 255]));
        let small = downscale(&src, 2, 2);
        assert_eq!(small.dimensions(), (2, 2));
        assert_eq!(small.get_pixel(1, 1).0, [40, 80, 120, 255]);
    }
}
