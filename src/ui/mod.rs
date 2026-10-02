//! 界面：ratatui 画图，crossterm 收键。
//!
//! 事件循环刻意不用 EventStream（那要拉 futures 依赖）：每 100 毫秒 poll 一下键盘，
//! 顺便把网络那边塞进 channel 的消息取走、重画一帧。TUI 这点开销无所谓，
//! 而 tokio 是多线程运行时，主线程堵这 100ms 不影响后台的网络任务。

mod area_tree;
mod control;
mod cover;
mod qr;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::*;
use ratatui::widgets::*;
use std::io::stdout;
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::api::area::{AreaEvent, AreaRequest};
use crate::api::danmaku::DanmuMsg;
use crate::api::info::{InfoEvent, InfoRequest};
use crate::api::live::{LiveEvent, LiveRequest};
use crate::api::login::LoginEvent;
use crate::api::room::{self, OnlineRankUser, RoomInfo};
use crate::config::Config;
use crate::timefmt;
use control::{Action, Control};

/// 软件名的艺术字（figlet 的 ANSI Shadow，手抄的）。每行都是 50 格宽、共 6 行。
pub const BANNER: [&str; 6] = [
    "██████╗ ██╗██╗     ██╗██╗     ██╗██╗   ██╗███████╗",
    "██╔══██╗██║██║     ██║██║     ██║██║   ██║██╔════╝",
    "██████╔╝██║██║     ██║██║     ██║██║   ██║█████╗  ",
    "██╔══██╗██║██║     ██║██║     ██║╚██╗ ██╔╝██╔══╝  ",
    "██████╔╝██║███████╗██║███████╗██║ ╚████╔╝ ███████╗",
    "╚═════╝ ╚═╝╚══════╝╚═╝╚══════╝╚═╝  ╚═══╝  ╚══════╝",
];

/// 顶栏占的行数：艺术字 + 上下边框。
pub const HEADER_ROWS: u16 = BANNER.len() as u16 + 2;

/// 弹幕列表最多留这么多行。TUI 一开一整天的话无上限增长迟早把内存吃光，
/// 而且旧的也没人会往上翻（Go 版是无上限往 TextView 里塞）。
const MAX_LINES: usize = 500;

/// 输入历史最多记这么多条（跟 Go 版一致，最旧的挤掉）。
const HISTORY_MAX: usize = 10;

/// 事件循环每一轮醒一次的时间：100 毫秒够弹幕跟手，也不至于空转把 CPU 烧掉。
const POLL_GAP: Duration = Duration::from_millis(100);

/// 滚轮一格滚几行。三行是「一格看得出来动了、又不至于翻掉半屏」的量。
///
/// 滚动**只走鼠标这一条路**：键盘一个键位都不占（`↑↓` 是发送历史、`Home`/`End`
/// 是行首行尾、`PgUp`/`PgDn` 也一并留给以后），回到底部的办法是「往下滚到底」。
const WHEEL_LINES: isize = 3;

pub async fn run(
    cfg: Config,
    w: Wiring,
) -> Result<()> {
    let mouse = cfg.mouse;
    let mut terminal = setup(mouse)?;
    let res = event_loop(&mut terminal, cfg, w).await;
    restore()?;
    res
}

/// 界面跟外面那几条链路之间的通道。
///
/// 打成包只因为 `run` 的参数已经七八个了 —— 一个个摆出来，调用方（main）
/// 和这里就得永远保持同一个顺序，改一个就得两头对一遍，迟早对错。
pub struct Wiring {
    pub danmaku: Receiver<DanmuMsg>,
    pub room: Receiver<RoomInfo>,
    pub refresh: Sender<()>,
    pub send: Sender<String>,
    /// 界面 -> 登录任务：开一张新二维码
    pub login_start: Sender<()>,
    /// 登录任务 -> 界面
    pub login_events: Receiver<LoginEvent>,
    /// 界面 -> 会话任务：退出登录（清掉本地凭据 + 重启弹幕那条链路）。
    ///
    /// 跟扫码那条分开走：扫出来的新凭据是「有一串 cookie 要落盘并换上去」，
    /// 退出是「把凭据清掉再重启链路」，落点一样但动作相反，
    /// 挤进同一条 `Sender<String>` 就得拿空串当暗号（谁都不愿意读这种代码）。
    pub logout: Sender<()>,
    /// 界面 -> 分区任务：拉分区表 / 选定分区
    pub area: Sender<AreaRequest>,
    /// 分区任务 -> 界面
    pub area_events: Receiver<AreaEvent>,
    /// 界面 -> 信息任务：拉当前标题 / 改标题 / 换封面
    pub info: Sender<InfoRequest>,
    /// 信息任务 -> 界面
    pub info_events: Receiver<InfoEvent>,
    /// 界面 -> 开播任务：查开播状态 / 开播 / 下播
    pub live: Sender<LiveRequest>,
    /// 开播任务 -> 界面
    pub live_events: Receiver<LiveEvent>,
    /// 开播任务 -> 界面：OBS 联动那件事的结果（一行字，摆在推流码栏末尾）。
    ///
    /// 单开一条通道、**不并进 `LiveEvent`**：它不是开播的结果，只是顺手多做的事，
    /// 晚几秒到也不能反过来把「已开播」改成别的。
    pub obs_notes: Receiver<String>,
}

/// 鼠标捕获到底还开着没有。
///
/// 真终端上的 escape 序列没法在单测里断言，所以单独记一个状态：退出 / panic
/// 有没有把它还回去，看这个就够 —— 留在捕获状态的话，用户没法用鼠标选中文、直接
/// 复制推流密钥，终端自己的滚动也废了，只能重开一个终端。
static MOUSE_CAPTURE: AtomicBool = AtomicBool::new(false);

fn setup(mouse: bool) -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    if mouse {
        set_mouse_capture(true)?;
    }
    // panic 之后也得有人收拾终端：默认那套只把消息打到屏幕上，而这时终端还在
    // alternate screen + raw mode 里，用户看到的是整屏被冲掉、鼠标还被程序吃着。
    install_panic_hook();
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

/// 打开 / 关掉鼠标捕获，顺手记下状态。
///
/// 状态**先记、命令后发**：真发不出去（比如 stdout 已经不是终端了）时宁可记成「开着」，
/// 退出时多发一次 `DisableMouseCapture` —— 关两次没有副作用，反过来漏关才是麻烦。
fn set_mouse_capture(on: bool) -> Result<()> {
    MOUSE_CAPTURE.store(on, Ordering::SeqCst);
    let mut out = stdout();
    let res = if on {
        execute!(out, EnableMouseCapture)
    } else {
        execute!(out, DisableMouseCapture)
    };
    Ok(res?)
}

/// 把鼠标还回去。**退出和 panic 都走这一个口子** —— 分成两处写，迟早有一处漏掉。
fn release_mouse() -> Result<()> {
    set_mouse_capture(false)
}

#[cfg(test)]
fn mouse_capture_enabled() -> bool {
    MOUSE_CAPTURE.load(Ordering::SeqCst)
}

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        prev(info);
    }));
}

fn restore() -> Result<()> {
    // 鼠标**先还**：raw mode / 备用屏那两下失败也不该把它留在捕获状态里。
    // 三件都试一遍再报错，别在第一件错掉的时候把后两件跳过。
    let mouse = release_mouse();
    let alt = execute!(stdout(), LeaveAlternateScreen);
    let raw = disable_raw_mode();
    mouse?;
    alt?;
    raw?;
    Ok(())
}

/// 界面上那点会变的状态。网络线程只往 channel 里塞东西，不碰这里。
#[derive(Default)]
struct App {
    /// 已经排好版的弹幕行：进来就算好，画的时候只截最后几行。
    lines: VecDeque<Line<'static>>,
    room: Option<RoomInfo>,
    /// 多行模式下上一次发言的人/类型/分钟，用来决定要不要重打一遍名字
    last_group: Option<(String, String, String)>,
    input: Input,
    /// 第二页（配置页）的状态。网络那半边从不碰它 —— 它只吃 `LoginEvent`。
    control: Control,
    /// 弹幕区看到哪儿了（粘底 / 上翻钉住）。
    view: Viewport,
    /// 这次运行被屏蔽了多少条（只记数、不落盘：它是「这次挡掉了几条」，
    /// 不是账号上的历史账，重启就该归零）。
    blocked: usize,
}

/// 弹幕视口。
///
/// 上翻时钉住的是**绝对行号**，不是「离底部还差多少行」：后者在新弹幕进来时会跟着
/// 底部一起往前走，屏幕上的内容被新来的弹幕一行一行推走 —— 那就不是「钉住」了。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Viewport {
    /// 视口第一行在 `App::lines` 里的下标。粘底时用不上（每帧现算）。
    top: usize,
    /// 跟着最新一条走。默认是它，滚到底部会自动回到这一态。
    follow: bool,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            top: 0,
            follow: true,
        }
    }
}

/// 底部那个输入框。
///
/// 缓冲区和光标都按**字符**存：中文一个字是一个元素。按字节存的话退格会退掉
/// 三分之一个字，终端上显示成一格方块，回车发出去还是乱码（风控也会拦）。
#[derive(Default)]
struct Input {
    buf: Vec<char>,
    /// 光标位置，取值区间 `0..=buf.len()`
    cursor: usize,
    /// 最近发出去的几条，最旧的在前面
    history: Vec<String>,
    /// 正在翻历史的位置；`== history.len()` 表示「没在翻，编辑的是新内容」。
    /// 口径跟 Go 版一致，翻到底的边界行为才对得上。
    hist_idx: usize,
}

impl Input {
    fn text(&self) -> String {
        self.buf.iter().collect()
    }

    fn set_text(&mut self, text: &str) {
        self.buf = text.chars().collect();
        self.cursor = self.buf.len();
    }

    fn insert(&mut self, c: char) {
        let at = self.cursor.min(self.buf.len());
        self.buf.insert(at, c);
        self.cursor = at + 1;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.buf.remove(self.cursor - 1);
            self.cursor -= 1;
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.buf.len() {
            self.buf.remove(self.cursor);
        }
    }

    fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.buf.len());
    }

    fn clear(&mut self) {
        self.buf.clear();
        self.cursor = 0;
    }

    /// ↑：往更旧的一条翻。已经在最旧的那条上就停住（不循环，翻历史时最怕绕圈）。
    fn history_up(&mut self) {
        if self.hist_idx > 0
            && let Some(t) = self.history.get(self.hist_idx - 1).cloned()
        {
            self.hist_idx -= 1;
            self.set_text(&t);
        }
    }

    /// ↓：往更新的一条翻；翻到最后一条再按一下就回到空白的新内容。
    /// Go 版就是这个行为 —— 翻到底还留着上一条，用户会以为按键没反应。
    fn history_down(&mut self) {
        if self.hist_idx + 1 < self.history.len() {
            self.hist_idx += 1;
            let t = self.history[self.hist_idx].clone();
            self.set_text(&t);
        } else {
            self.hist_idx = self.history.len();
            self.clear();
        }
    }

    /// 回车。返回要发出去的文本；空串（或只有空格）不算一条，也不进历史。
    /// 无论发不发，输入框都会清干净并回到「没在翻历史」的位置。
    fn submit(&mut self) -> Option<String> {
        let text = self.text().trim().to_string();
        self.clear();
        self.hist_idx = self.history.len();
        if text.is_empty() {
            return None;
        }
        self.history.push(text.clone());
        if self.history.len() > HISTORY_MAX {
            self.history.remove(0);
        }
        self.hist_idx = self.history.len();
        Some(text)
    }

    /// 喂一个按键。返回 `Some` 表示这一下要发出去。
    ///
    /// **不处理 Ctrl+C / Ctrl+R** —— 那两个是全局的，调用方在喂进来之前就拦掉了，
    /// 这里也不当文字收（见下面那条 `KeyCode::Char` 的守卫）。
    fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Option<String> {
        match code {
            KeyCode::Enter => return self.submit(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.buf.len(),
            KeyCode::Up => self.history_up(),
            KeyCode::Down => self.history_down(),
            // Ctrl+U 清空（跟 Go 版同一个键）。这条要排在下面那条普通字符之前，
            // 否则 Ctrl+U 会被当成「输入了一个 u」。
            KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => self.clear(),
            // 带 Ctrl/Alt 的键一律不当文字收：除了全局那几个，剩下的插进去
            // 也只是控制字符，屏幕上什么都看不见，还会莫名其妙地进请求体。
            KeyCode::Char(c) if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.insert(c)
            }
            _ => {}
        }
        None
    }
}

impl App {
    /// 弹幕进列表的**唯一**入口：屏蔽就在这儿判，命中只记数、不落盘。
    ///
    /// 位置选在「进列表之前」而不是解析那一步：B 站进房间时会先推一批**历史弹幕**
    /// （`danmaku::fetch_history`），它跟之后实时来的走的是同一条 channel ——
    /// 从这儿过一遍，历史和新弹幕才**一视同仁**，不然现象会是
    /// 「刚进房间还是满屏广播，过一会儿才干净」，用户会以为屏蔽没生效。
    fn feed_danmu(&mut self, m: &DanmuMsg, cfg: &Config) {
        if cfg.block.blocks(&m.kind, &m.author, &m.content) {
            self.blocked += 1;
            return;
        }
        self.push_danmu(m, cfg);
    }

    fn push_danmu(&mut self, m: &DanmuMsg, cfg: &Config) {
        let stamp = if cfg.show_time {
            timefmt::hhmm(m.time)
        } else {
            String::new()
        };

        if m.is_local() {
            self.lines.push_back(Line::from(vec![
                Span::styled(format!("[{stamp}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("system {}", m.content),
                    Style::default().fg(Color::Yellow),
                ),
            ]));
            self.last_group = None;
        } else if cfg.single_line {
            self.lines.push_back(Line::from(vec![
                Span::styled(format!("[{stamp}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{} ", m.author),
                    Style::default().fg(author_color(&m.kind)),
                ),
                Span::styled(m.content.clone(), Style::default().fg(content_color(&m.kind))),
            ]));
            self.last_group = Some((m.kind.clone(), m.author.clone(), stamp));
        } else {
            // 多行模式：同一个人在同一分钟里连着说话就不重复打名字，屏幕能清爽一半。
            let group = (m.kind.clone(), m.author.clone(), stamp.clone());
            if self.last_group.as_ref() != Some(&group) {
                let mut spans = vec![Span::styled(
                    format!("[{stamp}] "),
                    Style::default().fg(Color::DarkGray),
                )];
                if !m.author.is_empty() {
                    spans.push(Span::styled(
                        m.author.clone(),
                        Style::default().fg(author_color(&m.kind)),
                    ));
                }
                self.lines.push_back(Line::from(spans));
            }
            self.lines.push_back(Line::from(Span::styled(
                format!(" {}", m.content),
                Style::default().fg(content_color(&m.kind)),
            )));
            self.last_group = Some(group);
        }

        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
            // 上翻时钉住的是绝对行号，前面被挤掉一行就得跟着减一：
            // 不减的话屏幕上的内容会自己往下跳一行 —— 正好是「钉住」的反面。
            if !self.view.follow {
                self.view.top = self.view.top.saturating_sub(1);
            }
        }
    }

    /// 视口第一行。粘底时就是「最后 `rows` 行的第一行」（内容不够一屏时是 0）。
    fn view_top(&self, rows: usize) -> usize {
        let max_top = self.lines.len().saturating_sub(rows);
        if self.view.follow {
            max_top
        } else {
            self.view.top.min(max_top)
        }
    }

    /// 视口下面还有多少行没看见 —— 也就是「已上翻多少行」。粘底时是 0。
    fn hidden_below(&self, rows: usize) -> usize {
        self.lines
            .len()
            .saturating_sub(self.view_top(rows).saturating_add(rows))
    }

    /// 视口往下走 `delta` 行（看更新的），负数就是往上走（看更旧的）。
    ///
    /// 滚到底部**自动恢复「跟随最新」**，这是唯一的回到底部的办法（滚动不占键盘）。
    /// 内容不满一屏就没什么可滚的，顺手回到跟随态。
    fn scroll_by(&mut self, delta: isize, rows: usize) {
        let max_top = self.lines.len().saturating_sub(rows);
        if max_top == 0 {
            self.view = Viewport::default();
            return;
        }
        let from = self.view_top(rows);
        let to = if delta >= 0 {
            from.saturating_add(delta.unsigned_abs()).min(max_top)
        } else {
            from.saturating_sub(delta.unsigned_abs())
        };
        self.view.top = to;
        // 滚到（或者滚过）底部就恢复跟随：用户想看的就是最新那条。
        self.view.follow = to >= max_top;
    }

    /// 鼠标事件。只有滚轮、而且指针落在弹幕框里才动视口。
    ///
    /// 配置页整屏不是弹幕页，那儿的滚轮**不该动弹幕的视口** —— 切回来发现刚才那几行
    /// 还在原地，人只会以为自己看错了。
    fn on_mouse(&mut self, ev: MouseEvent, screen: Rect) {
        if self.control.page() != control::Page::Main {
            return;
        }
        let delta = match ev.kind {
            // 往上滚 = 看更旧的 = 视口往回走
            MouseEventKind::ScrollUp => -WHEEL_LINES,
            MouseEventKind::ScrollDown => WHEEL_LINES,
            _ => return,
        };
        let area = main_layout(screen).danmaku;
        if !inside(area, ev.column, ev.row) {
            return;
        }
        self.scroll_by(delta, danmaku_rows(area));
    }
}

/// 鼠标落点在不在这一块里。
///
/// 自己比而不是用 `Rect::contains`：`MouseEvent` 的 `column` / `row` 本来就是
/// 从 0 数的终端坐标，直接比少一层来回转换。
fn inside(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

/// 系统提示和「进入房间」用灰的，礼物类用洋红，普通弹幕用青色名字。
fn author_color(kind: &str) -> Color {
    match kind {
        "SEND_GIFT" | "COMBO_SEND" | "GUARD_BUY" => Color::Magenta,
        "INTERACT_WORD" => Color::DarkGray,
        _ => Color::Cyan,
    }
}

fn content_color(kind: &str) -> Color {
    match kind {
        "INTERACT_WORD" => Color::DarkGray,
        _ => Color::Gray,
    }
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    cfg: Config,
    w: Wiring,
) -> Result<()> {
    let Wiring {
        danmaku: mut danmu_rx,
        room: mut room_rx,
        refresh: refresh_tx,
        send: send_tx,
        login_start,
        login_events: mut login_rx,
        logout: logout_tx,
        area: area_tx,
        area_events: mut area_rx,
        info: info_tx,
        info_events: mut info_rx,
        live: live_tx,
        live_events: mut live_rx,
        obs_notes: mut obs_rx,
    } = w;
    let mut app = App::default();
    // 配置里记着的开播分区得先进界面：分区树要等分区表回来才建，而「展开哪个父分区、
    // 光标停在哪」全指着这一个数（Go 版在这儿没得靠，就写成了「展开列表里第一个」，
    // 结果永远展开「网游」，跟配置无关）。
    app.control.seed_area(cfg.area_id, &cfg.area_name);
    loop {
        // 先收网络那边的消息再画，画面永远是最新的。
        while let Ok(m) = danmu_rx.try_recv() {
            app.feed_danmu(&m, &cfg);
        }
        while let Ok(r) = room_rx.try_recv() {
            // 顺手把标题喂给配置页的「直播间信息」栏，省得那一栏空着让人以为坏了 ——
            // 封面也一样：地址先摆上，预览那张图由「进这一栏时拉一次」触发（`LoadRoomMeta`）。
            app.control.seed_title(&r.title);
            app.control.seed_cover(&r.cover);
            app.room = Some(r);
        }
        while let Ok(ev) = login_rx.try_recv() {
            app.control.on_login_event(ev);
        }
        while let Ok(ev) = area_rx.try_recv() {
            app.control.on_area_event(ev);
        }
        while let Ok(ev) = info_rx.try_recv() {
            app.control.on_info_event(ev);
        }
        while let Ok(ev) = live_rx.try_recv() {
            app.control.on_live_event(ev);
        }
        // OBS 那件事在另一条任务上跑，结果可能晚几秒才来；来一条记一条。
        while let Ok(note) = obs_rx.try_recv() {
            app.control.on_obs_note(note);
        }

        terminal.draw(|f| draw(f, &app, &cfg))?;

        if event::poll(POLL_GAP)? {
            // 一次把积压的事件收干净再重画：开了鼠标捕获之后终端连「鼠标移动」都送
            // （crossterm 的 `EnableMouseCapture` 顺带把 1003 也打开了），一条一条处理、
            // 每处理一条重画一帧的话，鼠标在窗口上划一下就能把 CPU 吃满。
            loop {
                match event::read()? {
                    // 滚轮是弹幕滚动**唯一**的入口（见 `App::on_mouse`），键盘一个键位都不占。
                    Event::Mouse(m) => {
                        // 落点按**当下**的终端尺寸算：窗口随时可能被缩放，
                        // 拿上一帧那个矩形当热区会算错一次。
                        let size = terminal.size()?;
                        app.on_mouse(m, Rect::new(0, 0, size.width, size.height));
                    }
                    Event::Key(KeyEvent {
                        code, modifiers, ..
                    }) => {
                        match (code, modifiers) {
                            // 退出只有 Ctrl+C：Esc 是「返回上一层」，别接成退出。
                            // 它排在最前面，所以输入框和配置页都抢不走。
                            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(()),
                            // 手动刷房间信息（跟 Go 版的 Ctrl+R 一致），不等那 30 秒。
                            // 配置页上按也行 —— Go 版就是全局 capture，两页共用一套。
                            (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                                room::refresh(&refresh_tx);
                                app.control.set_message("已请求刷新房间信息");
                            }
                            // 剩下的先问配置页：Shift+Tab / Tab / Esc / F2… 都归它分派。
                            // 它说「这一下归我」就到此为止，说「交给主页面」才轮到输入框 ——
                            // 所以弹幕页上按 Tab 还是输入框的键，抢不走。
                            _ => match app.control.handle_key(code, modifiers) {
                                Action::Handled => {}
                                Action::StartLogin => {
                                    if login_start.try_send(()).is_err() {
                                        // 队列只有一格：扫码任务正忙的时候再按就丢。
                                        // 说一句，比让用户对着一个没反应的键连按强。
                                        app.control.set_message("扫码任务正忙，等它一下再看看");
                                    }
                                }
                                // 退出登录在会话任务那边落地：清内存凭据、清配置里那一行、
                                // 重启弹幕那条链路（界面自己一件都不干）。
                                Action::Logout => {
                                    if logout_tx.try_send(()).is_err() {
                                        // 队列一格。没送出去就得把「正在退出」放掉，
                                        // 不然以后再按会被自己挡住，而那个请求根本不存在。
                                        app.control.logout_dropped();
                                    }
                                }
                                Action::LoadAreas => {
                                    if area_tx.try_send(AreaRequest::Load).is_err() {
                                        // 容量 4，正常按不出这个。真撞上了也得说清楚：
                                        // 屏幕上还写着「正在拉分区表…」，用户会一直等。
                                        app.control.set_message("分区任务正忙，等它一下再回车");
                                    }
                                }
                                Action::PickArea { id, name } => {
                                    if area_tx
                                        .try_send(AreaRequest::Pick {
                                            id,
                                            name: name.clone(),
                                        })
                                        .is_err()
                                    {
                                        app.control
                                            .set_message(format!("{name} 没能写进配置（分区任务正忙），再回车试一次"));
                                    }
                                }
                                // 改标题 / 换封面是写操作，全在信息任务那边落地（界面不碰网络）。
                                // 三条都只有容量 4 的队列，正常按不出「忙」；真撞上了也得说一句，
                                // 屏幕上还写着「正在提交…」，用户会一直等。
                                Action::LoadRoomMeta => {
                                    if info_tx.try_send(InfoRequest::LoadMeta).is_err() {
                                        // 这一下没送出去，就得把「正在问」的标志放掉：
                                        // 不然这一栏永远停在那儿等一个不会被发的请求
                                        app.control.info_request_dropped();
                                        app.control.set_message("信息任务正忙，等一下再切回这一栏");
                                    }
                                }
                                Action::SetTitle(title) => {
                                    if info_tx.try_send(InfoRequest::SetTitle(title)).is_err() {
                                        app.control.set_message("信息任务正忙，这条标题没提交，再回车试一次");
                                    }
                                }
                                Action::SetCover(cover) => {
                                    if info_tx.try_send(InfoRequest::SetCover(cover)).is_err() {
                                        app.control.set_message("信息任务正忙，这次换封面没提交，再回车试一次");
                                    }
                                }
                                // 开播那三下都归开播任务。**开播 / 下播都是写操作**：
                                // 只有走完确认层（`Action::StartLive`）才会发出去。
                                Action::LoadLiveStatus => {
                                    if live_tx.try_send(LiveRequest::LoadStatus).is_err() {
                                        // 这一下没送出去就得把「正在查」放掉：不然那一栏永远停在
                                        // 「正在查开播状态…」，而那个请求根本不会被发。
                                        app.control.live_request_dropped("查状态");
                                    }
                                }
                                Action::StartLive { area_v2 } => {
                                    if live_tx.try_send(LiveRequest::Start { area_v2 }).is_err() {
                                        app.control.live_request_dropped("开播");
                                    }
                                }
                                Action::StopLive => {
                                    if live_tx.try_send(LiveRequest::Stop).is_err() {
                                        app.control.live_request_dropped("下播");
                                    }
                                }
                                Action::ToMain => {
                                    if let Some(text) = app.input.handle_key(code, modifiers)
                                        && send_tx.try_send(text.clone()).is_err()
                                    {
                                        // 队列满、或者发送端没起来（比如没配房间号）。界面永远不等
                                        // 发送端，但这条得说清楚没发出去，不然用户对着空气等回显。
                                        // 也走 `feed_danmu`：本地消息的类型是 `LOCAL`，
                                        // 屏蔽配置一概拦不住它（见 `config::Block::blocks`）。
                                        app.feed_danmu(
                                            &DanmuMsg::local(format!("这条没发出去（发送端没起来）：{text}")),
                                            &cfg,
                                        );
                                    }
                                }
                            },
                        }
                    }
                    // 缩放 / 粘贴这类事件这一版用不上：丢掉就是，绝不能在这儿 panic
                    _ => {}
                }
                // 队列空了就回去画下一帧，别在这儿空转
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
    }
}

/// 主页面各块的矩形。
///
/// 画图（`draw`）和「鼠标落在哪一块」（滚轮的热区）共用这一份 ——
/// 两边各算一次的话，热区跟眼睛看到的框迟早对不上。
#[derive(Debug, Clone, Copy)]
struct MainLayout {
    header: Rect,
    info: Rect,
    viewers: Rect,
    danmaku: Rect,
    status: Rect,
    input: Rect,
}

fn main_layout(area: Rect) -> MainLayout {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(HEADER_ROWS),
        Constraint::Fill(1),
        Constraint::Length(3),
    ])
    .areas(area);

    let [left, right] =
        Layout::horizontal([Constraint::Ratio(1, 3), Constraint::Fill(1)]).areas(body);
    let [info, viewers] =
        Layout::vertical([Constraint::Length(6), Constraint::Fill(1)]).areas(left);
    let [status, input] =
        Layout::horizontal([Constraint::Ratio(1, 3), Constraint::Fill(1)]).areas(footer);

    MainLayout {
        header,
        info,
        viewers,
        danmaku: right,
        status,
        input,
    }
}

/// 弹幕框里能放几行字（上下边框各占一行）。
fn danmaku_rows(area: Rect) -> usize {
    area.height.saturating_sub(2) as usize
}

fn draw(f: &mut Frame, app: &App, _cfg: &Config) {
    // 第二页占满整屏：主页面那套一格里都不留（Go 版是 Pages 切换，一个意思）。
    if app.control.page() == control::Page::Config {
        control::draw(f, &app.control, f.area());
        return;
    }

    let l = main_layout(f.area());
    f.render_widget(banner(), l.header);
    f.render_widget(info_panel(app), l.info);
    f.render_widget(viewers_panel(app), l.viewers);
    danmaku_panel(f, app, l.danmaku);
    f.render_widget(stream_panel(app, l.status.width), l.status);
    f.render_widget(input_panel(app), l.input);
}

fn banner() -> Paragraph<'static> {
    Paragraph::new(BANNER.join("\n"))
        .block(Block::bordered())
        .style(Style::default().fg(Color::White))
}

/// 左边那格的高度固定 6 行，去掉上下边框只剩 4 行正文 —— 多一行都会被裁掉。
fn info_panel(app: &App) -> Paragraph<'static> {
    let title = match app.room.as_ref() {
        None => " 直播间信息 ".to_string(),
        Some(r) => match r.updated_at {
            // 「最后成功刷新的时间」写在框边上，数据新不新一眼能看出来。
            None => " 直播间信息 ".to_string(),
            Some(t) => {
                let stamp = timefmt::hhmm(t);
                if r.failed {
                    format!(" 直播间信息 · {stamp} 的数据（这次没刷上） ")
                } else {
                    format!(" 直播间信息 · {stamp} 更新 ")
                }
            }
        },
    };

    let lines = match app.room.as_ref() {
        None => vec![Line::from(Span::styled(
            "正在拉房间信息…",
            Style::default().fg(Color::DarkGray),
        ))],
        Some(r) => vec![
            Line::from(Span::styled(
                r.title.clone(),
                Style::default().fg(Color::White),
            )),
            Line::from(format!("房间: {}", r.room_id)),
            Line::from(format!("分区: {}/{}", r.parent_area_name, r.area_name)),
            Line::from(format!("在线: {}    粉丝: {}", r.online, r.attention)),
        ],
    };

    Paragraph::new(lines).block(Block::bordered().title(title))
}

fn viewers_panel(app: &App) -> Paragraph<'static> {
    let empty = Vec::<OnlineRankUser>::new();
    let users = app
        .room
        .as_ref()
        .map(|r| &r.online_rank_users)
        .unwrap_or(&empty);
    let title = format!(" 观众列表 ({}) ", users.len());

    let mut lines: Vec<Line<'static>> = Vec::new();
    if users.is_empty() {
        lines.push(Line::from(Span::styled(
            "还没有观众上榜",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (i, u) in users.iter().enumerate() {
            let medal = match i {
                0 => "👑 ",
                1 => "🥈 ",
                2 => "🥉 ",
                _ => "   ",
            };
            let color = if i < 3 { Color::Yellow } else { Color::Gray };
            lines.push(Line::from(Span::styled(
                format!("{medal}{}", u.name),
                Style::default().fg(color),
            )));
        }
    }

    Paragraph::new(lines).block(Block::bordered().title(title))
}

/// 弹幕区：粘底时看**最后**几行（进来的新弹幕就在眼前），上翻之后钉在那一行上
/// （`Viewport`），右边贴着一条滚动条。
fn danmaku_panel(f: &mut Frame, app: &App, area: Rect) {
    // 框的上下边框各占一行，剩下多少行就能放多少行字。
    let rows = danmaku_rows(area);
    let top = app.view_top(rows);
    let visible: Vec<Line<'static>> = app.lines.iter().skip(top).take(rows).cloned().collect();

    // 上翻时必须说一声：不然用户对着一屏旧弹幕，以为程序卡住了。
    // 回到底部的办法只有一个（往下滚到底），这句话就把它一起说了。
    let hidden = app.hidden_below(rows);
    let mut title = if hidden > 0 {
        format!(" 弹幕们 · 已上翻 {hidden} 行 · 滚到底恢复跟随")
    } else {
        " 弹幕们".to_string()
    };
    // 屏蔽过的条数也得让人看见：一条广播都不显示时，用户分不清「屏蔽在干活」和
    // 「B 站根本没发 / 程序收不到」。计数是本次运行累计，不落盘。
    if app.blocked > 0 {
        title.push_str(&format!(" · 已屏蔽 {} 条", app.blocked));
    }
    title.push(' ');

    f.render_widget(
        Paragraph::new(visible).block(Block::bordered().title(title)),
        area,
    );
    draw_scrollbar(f, area, app.lines.len(), rows, top);
}

/// 弹幕框右边界**内侧**那一条：`█` 是滑块、`│` 是轨道（跟浏览器一条意思）。
///
/// 内容不满一屏就干脆不画 —— 画一条占满的轨道，只会让人以为下面还有东西。
fn draw_scrollbar(f: &mut Frame, area: Rect, total: usize, rows: usize, top: usize) {
    let track = area.height.saturating_sub(2) as usize;
    // 窄到连「右边框内侧那一格」都不存在就不画：下面那个 `right() - 2` 在一列宽的框上
    // 会下溢（debug 下直接 panic）。终端真能被拖成这种怪尺寸。
    if area.width < 3 {
        return;
    }
    let Some((start, len)) = thumb(track, total, rows, top) else {
        return;
    };
    // 贴着右边框的那一格。画在图之后，所以长弹幕被它压住半格也算「盖在上面」的意思。
    let x = area.right() - 2;
    let buf = f.buffer_mut();
    for i in 0..track {
        let on_thumb = i >= start && i < start + len;
        let (ch, color) = if on_thumb {
            ("█", Color::Gray)
        } else {
            ("│", Color::DarkGray)
        };
        buf.set_string(
            x,
            area.y + 1 + i as u16,
            ch,
            Style::default().fg(color),
        );
    }
}

/// 滑块在轨道里的（起点，长度）。内容不满一屏就没有滚动条（`None`）。
///
/// 单拆出来是为了能直接断言「顶 / 中 / 底」三个位置，不用去数屏幕上的格子。
fn thumb(track: usize, total: usize, rows: usize, top: usize) -> Option<(usize, usize)> {
    if track == 0 || rows == 0 || total <= rows {
        return None;
    }
    // 滑块至少一格（内容再长也得抓得住），最长就是整条轨道
    let len = (track * rows / total).clamp(1, track);
    let max_top = total - rows;
    let start = (track - len) * top.min(max_top) / max_top;
    Some((start, len))
}

fn stream_panel(app: &App, width: u16) -> Paragraph<'static> {
    // 左右边框各占一格，能放字的就是这么宽。
    let inner = width.saturating_sub(2) as usize;
    let parts: Vec<(String, Style)> = match app.room.as_ref() {
        None => vec![(
            "○ 还没拿到房间状态".to_string(),
            Style::default().fg(Color::DarkGray),
        )],
        Some(r) => match r.live_status {
            // 主播最关心「现在到底有没有在推流」，所以第一眼就是直播中 / 未开播。
            1 => vec![
                ("● 直播中".to_string(), Style::default().fg(Color::Green)),
                (format!("  已播 {}", r.live_duration), Style::default()),
                (format!("   在线 {}", r.online), Style::default()),
            ],
            2 => vec![("● 轮播中".to_string(), Style::default().fg(Color::Yellow))],
            _ => vec![("○ 未开播".to_string(), Style::default().fg(Color::Gray))],
        },
    };
    Paragraph::new(fit_parts(inner, parts)).block(Block::bordered().title(" obs 推流状态 "))
}

/// 把若干「整段」的文本塞进 `inner` 格。
///
/// 之前是整行交给 ratatui 截，长时长会把后面的「在线 N」切成半个词 ——
/// 「25天14时4分」那种真的出现过（屏上是「… 在」）。这里自己决定丢哪段：
/// **整段一起丢**，只留一个「…」告诉用户后面还有，宁可少显示也别显示半句。
fn fit_parts(inner: usize, parts: Vec<(String, Style)>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    let mut dropped = false;
    for (i, (text, style)) in parts.into_iter().enumerate() {
        let w = Span::raw(text.clone()).width();
        // 第一段是状态本身（在播 / 未开播），哪怕格子再窄也得先摆上 ——
        // 否则超窄终端下这一格会变成一片空白，什么信息都没有。
        if i > 0 && used + w > inner {
            dropped = true;
            break;
        }
        used += w;
        spans.push(Span::styled(text, style));
    }
    // 省略号自己也要占一格，塞不下就不放（不能为了它再挤掉一个字）。
    if dropped && used < inner {
        spans.push(Span::styled("…", Style::default().fg(Color::DarkGray)));
    }
    Line::from(spans)
}

fn input_panel(app: &App) -> Paragraph<'static> {
    let input = &app.input;
    let spans = if input.buf.is_empty() {
        vec![Span::styled(
            "在这里打字，回车发送",
            Style::default().fg(Color::DarkGray),
        )]
    } else {
        // 光标画成反色的那一格。不画出来根本看不出打字的落点 ——
        // 进了 alternate screen 之后，终端自己的光标是不动的。
        let cursor = input.cursor.min(input.buf.len());
        let before: String = input.buf[..cursor].iter().collect();
        let mut spans = vec![Span::raw(before)];
        match input.buf.get(cursor) {
            Some(at) => {
                spans.push(Span::styled(
                    at.to_string(),
                    Style::default().add_modifier(Modifier::REVERSED),
                ));
                spans.push(Span::raw(input.buf[cursor + 1..].iter().collect::<String>()));
            }
            // 光标在最右边：用一个反色空格当光标
            None => spans.push(Span::styled(
                " ",
                Style::default().add_modifier(Modifier::REVERSED),
            )),
        }
        spans
    };
    Paragraph::new(Line::from(spans))
        .block(
            Block::bordered().title(" 弹幕输入框 · 回车发送/↑↓ 历史/Ctrl+U 清空/Ctrl+C 退出 "),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::room::OnlineRankUser;
    use ratatui::backend::TestBackend;
    use std::time::SystemTime;

    use crate::api::danmaku::parse_message;

    /// 把一帧画进内存里的终端再读回来 —— 不用真终端也能验「界面上到底有没有那行字」。
    fn render(app: &App, cfg: &Config, width: u16, height: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| draw(f, app, cfg)).unwrap();
        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    out.push_str(cell.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    /// 宽字符（汉字/emoji）在缓冲区里会额外占一格并留下一个空格，
    /// 所以断言前两边都把空白去掉再比，不然只会被这些格子气到。
    fn flat(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// 一个滚轮事件。落点先给 (0,0)，要用的测试自己挪到弹幕框里。
    fn wheel(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// 一屏弹幕 + 一块固定的屏幕矩形。
    fn screen_with_danmu(n: usize) -> (App, Config, Rect) {
        let cfg = Config::default();
        let mut app = App::default();
        for i in 0..n {
            app.push_danmu(&danmu(format!("人{i}"), format!("第{i}条")), &cfg);
        }
        (app, cfg, Rect::new(0, 0, 120, 30))
    }

    /// 把滚轮打进弹幕框里：热点就是 `main_layout` 算出来的那一块（跟画图同一个口径）。
    fn wheel_at(app: &mut App, screen: Rect, kind: MouseEventKind) {
        let area = main_layout(screen).danmaku;
        let mut ev = wheel(kind);
        ev.column = area.x + 1;
        ev.row = area.y + 1;
        app.on_mouse(ev, screen);
    }

    /// 弹幕框里那条滚动条的那一列（`█` / `│`），直接从缓冲区坐标上读。
    ///
    /// 不能拿 `render` 拼出来的字符串去数字符：中文这类宽字符在缓冲区里会多占一格
    /// （那半格是空 symbol），从字符串里数格子会被它骗。
    fn scrollbar_column(app: &App, cfg: &Config, width: u16, height: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| draw(f, app, cfg)).unwrap();
        let l = main_layout(Rect::new(0, 0, width, height));
        let buf = term.backend().buffer();
        let x = l.danmaku.right() - 2;
        (l.danmaku.y + 1..l.danmaku.bottom() - 1)
            .map(|y| {
                buf.cell((x, y))
                    .map(|c| c.symbol().to_string())
                    .unwrap_or_default()
            })
            .collect()
    }

    fn danmu(author: String, content: String) -> DanmuMsg {
        DanmuMsg {
            author,
            content,
            kind: "DANMU_MSG".to_string(),
            time: SystemTime::now(),
        }
    }

    #[test]
    fn danmaku_panel_keeps_only_the_latest_lines() {
        let cfg = Config::default();
        let mut app = App::default();
        for i in 0..(MAX_LINES + 20) {
            app.push_danmu(&danmu(format!("人{i}"), format!("第{i}条")), &cfg);
        }
        // 上限是硬要求：开一整天也不能无限长
        assert_eq!(app.lines.len(), MAX_LINES);

        let out = render(&app, &cfg, 120, 30);
        assert!(
            flat(&out).contains(&flat(&format!("第{}条", MAX_LINES + 19))),
            "最新那条要看得见"
        );
        assert!(!flat(&out).contains(&flat("第0条")), "最早那条已经被挤掉了");
    }

    // ------------------------------------------------------------ 弹幕滚动（鼠标滚轮）

    /// 粘底：停在底部时新弹幕跟着走（默认态，也是看直播的正常状态）。
    #[test]
    fn the_viewport_follows_new_danmaku_at_the_bottom() {
        let (mut app, cfg, screen) = screen_with_danmu(60);
        let rows = danmaku_rows(main_layout(screen).danmaku);
        assert_eq!(app.view_top(rows), 60 - rows, "默认粘底：看最后几行");
        assert_eq!(app.hidden_below(rows), 0, "粘底时没有「已上翻」这回事");

        app.push_danmu(&danmu("人x".into(), "新来的".into()), &cfg);
        assert_eq!(app.view_top(rows), 61 - rows, "新弹幕进来，视口跟着走");
    }

    /// 滚轮一格三行；上翻之后**钉住**：新来的弹幕不许把视口踹回底部。
    #[test]
    fn scrolling_up_pins_the_viewport_against_new_danmaku() {
        let (mut app, cfg, screen) = screen_with_danmu(60);
        let rows = danmaku_rows(main_layout(screen).danmaku);
        let bottom = 60 - rows;

        wheel_at(&mut app, screen, MouseEventKind::ScrollUp);
        assert_eq!(
            app.view_top(rows),
            bottom - WHEEL_LINES as usize,
            "一格三行"
        );
        assert_eq!(app.hidden_below(rows), 3, "标题要能数出「已上翻几行」");

        let pinned = app.view_top(rows);
        for i in 0..5 {
            app.push_danmu(&danmu(format!("新人{i}"), "新".into()), &cfg);
        }
        assert_eq!(app.view_top(rows), pinned, "上翻之后新弹幕不许推走视口");
        assert_eq!(app.hidden_below(rows), 8, "新来的都算在「下面还有几行」里");
    }

    /// 滚回底部自动恢复跟随 —— 这是唯一的回到底部的办法（键盘一个键都不占）。
    #[test]
    fn scrolling_back_to_the_bottom_resumes_following() {
        let (mut app, cfg, screen) = screen_with_danmu(60);
        let rows = danmaku_rows(main_layout(screen).danmaku);

        wheel_at(&mut app, screen, MouseEventKind::ScrollUp);
        wheel_at(&mut app, screen, MouseEventKind::ScrollUp);
        assert!(app.hidden_below(rows) > 0, "这时候是钉住的");

        wheel_at(&mut app, screen, MouseEventKind::ScrollDown);
        wheel_at(&mut app, screen, MouseEventKind::ScrollDown);
        assert_eq!(app.hidden_below(rows), 0, "滚到底就该恢复跟随");

        // 恢复了跟随，新弹幕就重新跟着走了
        app.push_danmu(&danmu("人x".into(), "新来的".into()), &cfg);
        assert_eq!(app.view_top(rows), 61 - rows);
    }

    /// 一路上滚到顶就停住：不绕回去、也不算成负数。
    #[test]
    fn scrolling_up_stops_at_the_oldest_line() {
        let (mut app, _, screen) = screen_with_danmu(60);
        let rows = danmaku_rows(main_layout(screen).danmaku);
        for _ in 0..50 {
            wheel_at(&mut app, screen, MouseEventKind::ScrollUp);
        }
        assert_eq!(app.view_top(rows), 0, "到顶就停在第一行");
        assert_eq!(app.hidden_below(rows), 60 - rows, "下面那些都还没看见");

        // 再往下滚一点点也不许跳过界
        wheel_at(&mut app, screen, MouseEventKind::ScrollDown);
        assert_eq!(app.view_top(rows), WHEEL_LINES as usize);
    }

    /// 内容不满一屏：没什么可滚的，滚动条也不画。
    #[test]
    fn a_short_list_has_nothing_to_scroll_and_no_scrollbar() {
        let (mut app, cfg, screen) = screen_with_danmu(3);
        let rows = danmaku_rows(main_layout(screen).danmaku);
        wheel_at(&mut app, screen, MouseEventKind::ScrollUp);
        assert_eq!(app.view_top(rows), 0);
        assert_eq!(app.hidden_below(rows), 0);

        let col = scrollbar_column(&app, &cfg, 120, 30);
        assert_eq!(col.trim(), "", "不满一屏不该画滚动条：{col:?}");
        assert_eq!(thumb(17, 3, 17, 0), None);
    }

    /// 滚动条：内容满了之后滑块在顶 / 中 / 底三个位置各对一次。
    #[test]
    fn scrollbar_thumb_sits_where_the_browser_would_put_it() {
        // 轨道 10 格、内容 100 行、一屏 10 行
        assert_eq!(thumb(10, 100, 10, 0), Some((0, 1)), "最上面：滑块贴着顶");
        assert_eq!(thumb(10, 100, 10, 45), Some((4, 1)), "中间");
        assert_eq!(thumb(10, 100, 10, 90), Some((9, 1)), "最下面：滑块贴着底");
        // 内容长、轨道也长的时候滑块跟着变长（一屏占的比例）
        assert_eq!(thumb(10, 20, 10, 0), Some((0, 5)));
        assert_eq!(thumb(10, 20, 10, 10), Some((5, 5)));
        // 内容不满一屏 / 没地方画：没有滚动条
        assert_eq!(thumb(10, 5, 10, 0), None);
        assert_eq!(thumb(0, 100, 10, 0), None);
        assert_eq!(thumb(10, 100, 0, 0), None);
        // 到头了再多滚也不会把滑块推出轨道
        assert_eq!(thumb(10, 100, 10, 999), Some((9, 1)));
    }

    /// 滑块真的画在那个位置上（从缓冲区里读回来），而且粘底时它在最下面。
    #[test]
    fn the_rendered_scrollbar_tracks_the_viewport() {
        let (mut app, cfg, screen) = screen_with_danmu(60);
        let rows = danmaku_rows(main_layout(screen).danmaku);

        // 60 行 / 一屏 17 行 -> 轨道 17 格，滑块占 17*17/60 = 4 格
        let col = scrollbar_column(&app, &cfg, 120, 30);
        assert_eq!(col.chars().count(), 17, "轨道就是框里那些行：{col:?}");
        assert_eq!(col.chars().filter(|c| *c == '█').count(), 4, "{col:?}");
        assert!(col.ends_with("████"), "粘底时滑块贴着最下面：{col:?}");
        assert!(col.starts_with('│'), "上面那截是轨道：{col:?}");

        // 上翻到最顶：滑块跑到最上面
        app.scroll_by(-999, rows);
        let col = scrollbar_column(&app, &cfg, 120, 30);
        assert!(col.starts_with("████"), "到顶时滑块贴着最上面：{col:?}");

        // 中间（22 行，不是滚轮的整倍数，顺手钉一下任意位置也算得对）
        app.scroll_by(22, rows);
        let col = scrollbar_column(&app, &cfg, 120, 30);
        assert_eq!(col.chars().position(|c| c == '█'), Some(6), "{col:?}");
    }

    /// 上翻的时候画面上要说明白「现在看的不是最新」，以及怎么回去。
    #[test]
    fn the_title_says_how_far_up_you_are() {
        let (mut app, cfg, screen) = screen_with_danmu(60);
        let bottom = flat(&render(&app, &cfg, 120, 30));
        assert!(bottom.contains(&flat("弹幕们")), "粘底时标题还是老样子");
        assert!(!bottom.contains(&flat("已上翻")), "没上翻就别写这句");

        wheel_at(&mut app, screen, MouseEventKind::ScrollUp);
        let out = flat(&render(&app, &cfg, 120, 30));
        assert!(out.contains(&flat("已上翻 3 行")), "{out}");
        assert!(out.contains(&flat("滚到底恢复跟随")), "{out}");
    }

    /// 滚动这件事**完全不碰键盘**：`↑↓` 是发送历史、`Home`/`End` 是行首行尾，
    /// `PgUp`/`PgDn` 也一并留着。这条测试就是拦「以后顺手加个键位」的 ——
    /// 谁加了，它先红在这儿。
    #[test]
    fn no_keyboard_key_scrolls_the_danmaku() {
        let (mut app, _, screen) = screen_with_danmu(60);
        let rows = danmaku_rows(main_layout(screen).danmaku);
        let bottom = app.view_top(rows);

        for (code, mods) in [
            (KeyCode::PageUp, KeyModifiers::NONE),
            (KeyCode::PageDown, KeyModifiers::NONE),
            (KeyCode::Home, KeyModifiers::NONE),
            (KeyCode::End, KeyModifiers::NONE),
            (KeyCode::Home, KeyModifiers::CONTROL),
            (KeyCode::End, KeyModifiers::CONTROL),
            (KeyCode::Up, KeyModifiers::NONE),
            (KeyCode::Down, KeyModifiers::NONE),
        ] {
            // 走的正是 `event_loop` 里键盘那条路：先问配置页，它说「交给主页面」才轮到输入框。
            // 这两处都没有「滚动」这个概念，视口因此一格都不该动。
            if app.control.handle_key(code, mods) == control::Action::ToMain {
                let _ = app.input.handle_key(code, mods);
            }
            assert_eq!(app.view_top(rows), bottom, "{code:?} 不该滚动弹幕");
        }
    }

    /// 指针不在弹幕框里（落在观众榜 / 输入框上）不该动视口。
    #[test]
    fn the_wheel_only_works_inside_the_danmaku_box() {
        let (mut app, _, screen) = screen_with_danmu(60);
        let l = main_layout(screen);
        let rows = danmaku_rows(l.danmaku);
        let bottom = app.view_top(rows);

        for area in [l.viewers, l.input, l.info, l.status, l.header] {
            let mut ev = wheel(MouseEventKind::ScrollUp);
            ev.column = area.x + 1;
            ev.row = area.y + 1;
            app.on_mouse(ev, screen);
        }
        assert_eq!(app.view_top(rows), bottom, "弹幕框外面的滚轮不归它管");
    }

    /// 配置页上滚轮不该动弹幕：切回来发现视口自己动了，人只会以为自己看错了。
    #[test]
    fn the_wheel_does_nothing_on_the_config_page() {
        let (mut app, _, screen) = screen_with_danmu(60);
        let rows = danmaku_rows(main_layout(screen).danmaku);
        let bottom = app.view_top(rows);

        // Shift+Tab 翻开配置页（弹幕页的键盘路径里就那么一个键归配置页管）
        app.control
            .handle_key(KeyCode::BackTab, KeyModifiers::SHIFT);
        wheel_at(&mut app, screen, MouseEventKind::ScrollUp);
        assert_eq!(app.view_top(rows), bottom, "配置页上不该动弹幕");
    }

    /// 退出（以及 panic，走的是同一个 `release_mouse`）必须把鼠标捕获还回去。
    /// 留在捕获状态里，用户没法用鼠标选中文、直接复制推流密钥，终端自己的滚动也废了。
    #[test]
    fn leaving_turns_the_mouse_capture_back_off() {
        set_mouse_capture(true).unwrap();
        assert!(mouse_capture_enabled(), "开了就该记着");
        release_mouse().unwrap();
        assert!(!mouse_capture_enabled(), "退出时没把鼠标还回去");
    }

    // ------------------------------------------------------------ 弹幕屏蔽

    /// 一条协议报文的原文 -> 进列表。测试里全走 `feed_danmu`，
    /// 也就是 `event_loop` 里收弹幕那条路（历史弹幕进的也是它）。
    fn feed_raw(app: &mut App, cfg: &Config, raw: &str) {
        let m = parse_message(raw.as_bytes(), SystemTime::now()).expect("这条报文该认得出来");
        app.feed_danmu(&m, cfg);
    }

    /// 端到端：一串真报文（广播 / 连击 / 聊天 / 真送礼）从解析到进列表走一遍，
    /// 留下的只该是聊天和真送礼 —— 默认屏蔽就是冲「广播刷屏把自己发的埋掉」去的。
    #[test]
    fn blocking_drops_room_broadcasts_and_keeps_the_chat() {
        let cfg = Config::default();
        let mut app = App::default();
        // 头两条就是截图里那种房间广播的形状（作者显示成 system）
        feed_raw(
            &mut app,
            &cfg,
            r#"{"cmd":"NOTICE_MSG","msg_self":"<%持续充能%>投喂<%弥实斯缪拉%>1个沧月神玺，快来围观"}"#,
        );
        feed_raw(
            &mut app,
            &cfg,
            r#"{"cmd":"COMBO_SEND","data":{"uname":"胖兔叽大王","r_uname":"三条","combo_num":1,"gift_name":"22号机"}}"#,
        );
        // 自己发的聊天、以及真有人送礼，都得留着
        feed_raw(
            &mut app,
            &cfg,
            r#"{"cmd":"DANMU_MSG","info":[[],"111",[7,"tc191"]]}"#,
        );
        feed_raw(
            &mut app,
            &cfg,
            r#"{"cmd":"SEND_GIFT","data":{"uname":"小红","num":1,"giftName":"辣条"}}"#,
        );

        let out = flat(&render(&app, &cfg, 120, 30));
        assert!(out.contains(&flat("111")), "聊天不该被埋掉：{out}");
        assert!(
            out.contains(&flat("投喂了 1 个 辣条")),
            "真送礼也得留着：{out}"
        );
        assert!(!out.contains(&flat("沧月神玺")), "房间广播该被挡掉：{out}");
        assert!(!out.contains(&flat("22号机")), "连击送礼也该被挡掉：{out}");
        assert_eq!(app.blocked, 2, "挡下来几条要数着");
        assert!(
            out.contains(&flat("已屏蔽 2 条")),
            "标题要说清楚挡掉了几条：{out}"
        );
    }

    /// 程序自己那几句话**永远不被屏蔽**：把默认那套塞满，再配上关键词和用户名，
    /// 「弹幕服务器已断开，正在重连」照样得在屏幕上 —— 它被自己吃掉的话，
    /// 用户看到的现象是「程序坏了」。
    #[test]
    fn our_own_messages_are_never_blocked() {
        let cfg = Config {
            block: crate::config::Block {
                types: vec!["LOCAL".into(), "NOTICE_MSG".into(), "SYSTEM".into()],
                keywords: vec!["断开".into()],
                users: vec!["system".into()],
            },
            ..Config::default()
        };
        let mut app = App::default();
        app.feed_danmu(&DanmuMsg::local("弹幕服务器已断开，正在重连"), &cfg);

        let out = flat(&render(&app, &cfg, 120, 30));
        assert!(out.contains(&flat("弹幕服务器已断开，正在重连")), "{out}");
        assert_eq!(app.blocked, 0, "本地提示不许算进屏蔽数");
        assert!(!out.contains(&flat("已屏蔽")), "一条都没挡就别写这句：{out}");
    }

    /// 关键词（包含、忽略大小写）和用户名（完全相等）在那条链路上一视同仁：
    /// **历史弹幕那批**也一样要拦得住 —— 它跟实时的是同一个入口（`DANMU_MSG`），
    /// 不然现象会是「刚进房间还是满屏，过一会儿才干净」。
    #[test]
    fn keywords_and_users_filter_history_and_live_alike() {
        let cfg = Config {
            block: crate::config::Block {
                // 这一条先让开，单看另外两条
                types: Vec::new(),
                keywords: vec![" 抽奖 ".into()],
                users: vec!["小明".into()],
            },
            ..Config::default()
        };
        let mut app = App::default();
        // 「历史弹幕」那批：`fetch_history` 里长得就是普通聊天
        app.feed_danmu(&danmu("路人".into(), "快来抽奖啊".into()), &cfg);
        app.feed_danmu(&danmu("小明".into(), "正常聊天".into()), &cfg);
        // 之后实时来的
        app.feed_danmu(&danmu("小红".into(), "HELLO 大家好".into()), &cfg);
        app.feed_danmu(&danmu("小明".into(), "又来一条".into()), &cfg);

        let out = flat(&render(&app, &cfg, 120, 30));
        assert!(out.contains(&flat("HELLO 大家好")), "没命中的照留：{out}");
        assert!(!out.contains(&flat("抽奖")), "关键词命中的，历史那批也得挡：{out}");
        assert!(!out.contains(&flat("正常聊天")), "用户名完全相等才算：{out}");
        assert!(!out.contains(&flat("又来一条")), "同一个人的新弹幕也挡");
        assert_eq!(app.blocked, 3);
    }

    /// 上翻时标题那两句都要在：既要说「现在看的不是最新」，也要说屏蔽走了几条。
    #[test]
    fn the_title_shows_the_blocked_count_while_scrolled_up() {
        let cfg = Config::default();
        let mut app = App::default();
        let screen = Rect::new(0, 0, 120, 30);
        for _ in 0..5 {
            feed_raw(&mut app, &cfg, r#"{"cmd":"NOTICE_MSG","msg_self":"广播"}"#);
        }
        for i in 0..60 {
            app.push_danmu(&danmu(format!("人{i}"), format!("第{i}条")), &cfg);
        }
        wheel_at(&mut app, screen, MouseEventKind::ScrollUp);

        let out = flat(&render(&app, &cfg, 120, 30));
        assert!(out.contains(&flat("已上翻 3 行")), "{out}");
        assert!(out.contains(&flat("已屏蔽 5 条")), "{out}");
    }

    #[test]
    fn info_title_carries_refresh_time_and_failure_flag() {
        let cfg = Config::default();
        let mut app = App::default();
        let mut room = RoomInfo::new(9527);
        room.title = "随便播播".to_string();
        room.updated_at = Some(SystemTime::now());
        app.room = Some(room.clone());

        let out = render(&app, &cfg, 150, 30);
        assert!(flat(&out).contains(&flat("随便播播")));
        assert!(flat(&out).contains(&flat("房间: 9527")));
        assert!(
            flat(&out).contains(&flat(&format!("{} 更新", timefmt::hhmm(SystemTime::now())))),
            "框标题要带上最后成功刷新的时间"
        );

        // 这一轮没拉到时要明说摆的是旧数据，别让用户以为房间突然空了
        room.failed = true;
        app.room = Some(room);
        let out = render(&app, &cfg, 150, 30);
        assert!(flat(&out).contains(&flat("这次没刷上")), "{out}");
    }

    #[test]
    fn viewers_panel_marks_top_three() {
        let cfg = Config::default();
        let mut app = App::default();
        let mut room = RoomInfo::new(9527);
        room.updated_at = Some(SystemTime::now());
        room.online_rank_users = ["甲", "乙", "丙", "丁"]
            .iter()
            .enumerate()
            .map(|(i, n)| OnlineRankUser {
                name: (*n).to_string(),
                score: 0,
                rank: i as i64 + 1,
            })
            .collect();
        app.room = Some(room);

        let out = render(&app, &cfg, 150, 30);
        assert!(
            flat(&out).contains(&flat("观众列表 (4)")),
            "标题上要有上榜人数"
        );
        for (medal, name) in [("👑", "甲"), ("🥈", "乙"), ("🥉", "丙")] {
            assert!(out.contains(medal), "缺了 {medal}");
            assert!(out.contains(name), "缺了 {name}");
        }
    }

    #[test]
    fn stream_panel_reflects_live_status() {
        let cfg = Config::default();
        let mut app = App::default();
        let mut room = RoomInfo::new(9527);
        room.live_status = 1;
        room.live_duration = "1天2时3分".to_string();
        room.online = 4321;
        app.room = Some(room.clone());
        let out = render(&app, &cfg, 150, 30);
        assert!(flat(&out).contains(&flat("直播中")));
        assert!(flat(&out).contains(&flat("已播 1天2时3分")));
        assert!(flat(&out).contains(&flat("在线 4321")));

        room.live_status = 0;
        app.room = Some(room);
        let out = render(&app, &cfg, 150, 30);
        assert!(flat(&out).contains(&flat("未开播")));
    }

    /// 终端被拉得很窄很矮时布局不能算出负数或者 panic（用户随时会缩窗口）。
    #[test]
    fn tiny_terminal_does_not_panic() {
        let cfg = Config::default();
        let mut app = App::default();
        app.push_danmu(&danmu("小明".into(), "你好".into()), &cfg);
        app.room = Some(RoomInfo::new(1));
        for (w, h) in [(20u16, 8u16), (40, 12), (5, 5), (1, 1), (1, 20), (2, 20), (3, 14)] {
            let _ = render(&app, &cfg, w, h);
        }

        // 上面那些尺寸下弹幕还不满一屏,滚动条那一段代码根本没走到。
        // 塞够一屏再来一遍:右边界内侧那一格是 `right() - 2` 算出来的,
        // 一列宽的终端上这个减法会下溢(debug 下直接 panic)。
        for i in 0..60 {
            app.push_danmu(&danmu(format!("人{i}"), format!("第{i}条")), &cfg);
        }
        for (w, h) in [(1u16, 20u16), (2, 20), (3, 20), (4, 20), (120, 14)] {
            let _ = render(&app, &cfg, w, h);
        }
    }

    #[test]
    fn multiline_mode_groups_same_speaker() {
        let cfg = Config {
            single_line: false,
            show_time: false,
            ..Config::default()
        };
        let mut app = App::default();
        app.push_danmu(&danmu("小明".into(), "一".into()), &cfg);
        app.push_danmu(&danmu("小明".into(), "二".into()), &cfg);
        // 同一个人连着说两句：只有一条名字行 + 两条内容行
        assert_eq!(app.lines.len(), 3);
    }

    /// 输入框的按键：插入 / 退格（中文按字符退）/ 左右移动 / 中途插入 / Ctrl+U 清空。
    #[test]
    fn input_edits_by_character() {
        let mut input = Input::default();
        for c in "中文ab".chars() {
            input.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(input.text(), "中文ab");

        // 退格退的是一个字（按字节存的话这里会剩半个「文」）
        input.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(input.text(), "中文a");
        input.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(input.text(), "中文");

        // 左移两格，在「中」和「文」中间插一个字符
        input.handle_key(KeyCode::Left, KeyModifiers::NONE);
        input.handle_key(KeyCode::Left, KeyModifiers::NONE);
        input.handle_key(KeyCode::Char('X'), KeyModifiers::NONE);
        assert_eq!(input.text(), "X中文");
        assert_eq!(input.cursor, 1);

        // 右移到头再按不越界
        for _ in 0..10 {
            input.handle_key(KeyCode::Right, KeyModifiers::NONE);
        }
        assert_eq!(input.cursor, input.buf.len());
        input.handle_key(KeyCode::Char('!'), KeyModifiers::NONE);
        assert_eq!(input.text(), "X中文!");

        // 光标在最左边时退格不能做任何事（也不能 panic）
        input.handle_key(KeyCode::Home, KeyModifiers::NONE);
        input.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(input.text(), "X中文!");

        // Delete 删的是光标右边那个
        input.handle_key(KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(input.text(), "中文!");
        input.handle_key(KeyCode::End, KeyModifiers::NONE);
        input.handle_key(KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(input.text(), "中文!");

        input.handle_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(input.text(), "");
        assert_eq!(input.cursor, 0);
    }

    /// Ctrl+C / Ctrl+R 是全局的：输入框不能把它们当文字收下（不然屏幕上会多出个 c），
    /// 也不能因为带 Ctrl 就把输入框弄坏。
    #[test]
    fn input_never_swallows_the_global_keys() {
        let mut input = Input::default();
        for key in ['c', 'r'] {
            let out = input.handle_key(KeyCode::Char(key), KeyModifiers::CONTROL);
            assert!(out.is_none(), "Ctrl+{key} 不该产生发送");
            assert_eq!(input.text(), "", "Ctrl+{key} 不能被当成普通字符插进去");
        }
    }

    /// 回车：非空才发；空回车既不发也不进历史（否则历史里全是空行）。
    #[test]
    fn enter_only_sends_something_worth_sending() {
        let mut input = Input::default();
        assert_eq!(input.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
        for c in "   ".chars() {
            input.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(input.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
        assert!(input.history.is_empty());
        assert_eq!(input.text(), "", "空回车也要把输入框清干净");

        for c in "你好".chars() {
            input.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(
            input.handle_key(KeyCode::Enter, KeyModifiers::NONE),
            Some("你好".to_string())
        );
        assert_eq!(input.history, vec!["你好".to_string()]);
        assert_eq!(input.hist_idx, 1, "发完要回到「没在翻历史」的位置");
    }

    /// ↑↓ 翻最近 10 条：超过 10 条最旧的被挤掉，翻到头、翻到底都不能绕圈。
    #[test]
    fn history_keeps_the_last_ten_and_walks_both_ways() {
        let mut input = Input::default();
        for i in 1..=12 {
            input.set_text(&format!("第{i}条"));
            assert_eq!(
                input.handle_key(KeyCode::Enter, KeyModifiers::NONE),
                Some(format!("第{i}条"))
            );
        }
        assert_eq!(input.history.len(), HISTORY_MAX);
        assert_eq!(input.history.first().unwrap(), "第3条", "最旧的两条该被挤掉");
        assert_eq!(input.history.last().unwrap(), "第12条");

        // ↑ 先给最近发的那条
        input.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(input.text(), "第12条");
        // 一路翻到最旧，再按就停在那儿（不能绕回最新）
        for _ in 0..(HISTORY_MAX + 5) {
            input.handle_key(KeyCode::Up, KeyModifiers::NONE);
        }
        assert_eq!(input.text(), "第3条");

        // ↓ 往回走一条，再翻到底就该清空
        input.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(input.text(), "第4条");
        for _ in 0..(HISTORY_MAX + 5) {
            input.handle_key(KeyCode::Down, KeyModifiers::NONE);
        }
        assert_eq!(input.text(), "");
        assert_eq!(input.hist_idx, input.history.len());
    }

    /// 打进去的字要真的出现在屏幕上，清掉之后要回到占位提示。
    #[test]
    fn input_panel_shows_what_you_type() {
        let cfg = Config::default();
        let mut app = App::default();
        let out = render(&app, &cfg, 150, 30);
        assert!(flat(&out).contains(&flat("在这里打字")), "{out}");

        for c in "在吗".chars() {
            app.input.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        let out = render(&app, &cfg, 150, 30);
        assert!(flat(&out).contains(&flat("在吗")), "{out}");
        assert!(flat(&out).contains(&flat("回车发送")), "标题要写清楚怎么发");

        app.input.handle_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
        let out = render(&app, &cfg, 150, 30);
        assert!(flat(&out).contains(&flat("在这里打字")), "{out}");
    }

    /// 取一行里前 `width` 格的字（宽字符占两格），用来单独看某一个小格里的内容。
    fn cell_text(line: &str, width: usize) -> String {
        let mut used = 0;
        let mut out = String::new();
        for c in line.chars() {
            let w = Span::raw(c.to_string()).width();
            if used + w > width {
                break;
            }
            used += w;
            out.push(c);
        }
        out
    }

    /// obs 推流状态那格太窄时整段让位：宁可只留「● 直播中…」，
    /// 也不能出现「已播 25天14」这种半截（6 号房那种长时长把「在线」截成过「在」）。
    #[test]
    fn stream_status_drops_whole_pieces_instead_of_cutting_words() {
        let parts = || {
            vec![
                ("● 直播中".to_string(), Style::default().fg(Color::Green)),
                ("  已播 25天14时4分".to_string(), Style::default()),
                ("   在线 4321".to_string(), Style::default()),
            ]
        };
        let text = |line: Line<'static>| line.to_string();

        // 够宽：三段都在
        let wide = text(fit_parts(80, parts()));
        assert!(wide.contains("已播 25天14时4分"), "{wide}");
        assert!(wide.contains("在线 4321"), "{wide}");

        // 只装得下状态：时长整段丢，补一个省略号
        let narrow = text(fit_parts(12, parts()));
        assert!(narrow.contains("直播中"), "{narrow}");
        assert!(!narrow.contains("已播"), "装不下就整段别放：{narrow}");
        assert!(narrow.ends_with('…'), "{narrow}");

        // 刚好装得下时长、装不下「在线」：时长得是完整的
        let mid = text(fit_parts(27, parts()));
        assert!(mid.contains("已播 25天14时4分"), "{mid}");
        assert!(!mid.contains('在'), "{mid}");
        assert!(mid.ends_with('…'), "{mid}");
    }

    /// 同一个 bug 走一遍真实渲染：底部那格只有整宽的 1/3。
    #[test]
    fn stream_panel_at_narrow_width_never_shows_half_a_word() {
        let cfg = Config::default();
        let mut app = App::default();
        let mut room = RoomInfo::new(9527);
        room.live_status = 1;
        room.live_duration = "25天14时4分".to_string();
        room.online = 4321;
        app.room = Some(room);

        let out = render(&app, &cfg, 60, 30);
        // 缓冲区里宽字符会多占一格留下空格，找行和断言都得先 flat 掉空白。
        let row = out
            .lines()
            .find(|l| flat(l).contains("直播中"))
            .expect("状态那行");
        // 底部左格就是前 20 格（连边框一起）
        let cell = flat(&cell_text(row, 20));
        assert!(cell.contains("直播中"), "{cell}");
        assert!(cell.contains('…'), "装不下时长时要留个省略号：{cell}");
        assert!(!cell.contains("已播") || cell.contains("分"), "{cell}");
        assert!(!cell.contains('在') || cell.contains("在线"), "{cell}");
    }

    /// 一路走到底：打字 -> 回车 -> 发送端真的构造出 `/msg/send` 请求。
    ///
    /// 请求形状由 `api::send` 的测试钉住；这里只证明「回车确实把这条送出去了」，
    /// 而不是把键盘事件吞在界面里。
    #[tokio::test]
    async fn enter_really_sends_the_typed_text() {
        use crate::api::client::BiliClient;
        use crate::api::send;
        use crate::api::test_http;
        use std::sync::Arc;

        let srv = test_http::start(|r| {
            if r.path.ends_with("web-interface/nav") {
                return (200, nav_body());
            }
            (200, r#"{"code":0,"message":"0","data":{}}"#.to_string())
        })
        .await;
        // nav 也指到假服务器：不然这条链路会去打真接口（单测必须纯离线）。
        let client = Arc::new(
            BiliClient::new("SESSDATA=abc; bili_jct=tok")
                .unwrap()
                .with_main_base(&srv.base),
        );
        let (send_tx, send_rx) = tokio::sync::mpsc::channel(4);
        let (danmu_tx, mut danmu_rx) = tokio::sync::mpsc::channel(4);
        let task = tokio::spawn(send::send_loop(
            client,
            srv.base.clone(),
            9527,
            send_rx,
            danmu_tx,
        ));

        let mut app = App::default();
        for c in "在吗".chars() {
            assert!(
                app.input
                    .handle_key(KeyCode::Char(c), KeyModifiers::NONE)
                    .is_none()
            );
        }
        let outgoing = app.input.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(outgoing.as_deref(), Some("在吗"));
        assert_eq!(app.input.text(), "", "发出去之后输入框要清干净");
        send_tx.send(outgoing.unwrap()).await.unwrap();

        let mut body = None;
        for _ in 0..400 {
            if let Some(h) = srv.hits().into_iter().find(|h| h.path == "/msg/send") {
                body = Some(h.body);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let body = body.expect("回车之后该有一个 /msg/send 请求");
        assert!(body.contains("msg=%E5%9C%A8%E5%90%97"), "{body}"); // 「在吗」
        assert!(body.contains("roomid=9527"), "{body}");
        assert!(danmu_rx.try_recv().is_err(), "发成功时不该塞系统弹幕");

        task.abort();
    }

    /// nav 的假响应：两个 wbi key 只要够长就能签发（不是凭据，是每天轮换的公开种子）。
    fn nav_body() -> String {
        r#"{"code":0,"message":"0","data":{"mid":7,"wbi_img":{
            "img_url":"https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png",
            "sub_url":"https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"}}}"#
            .to_string()
    }

    // ------------------------------------------------------------ 配置页（第二页）

    /// Shift+Tab 翻开第二页之后，整屏都是配置页：左栏四个功能、
    /// 顶栏「按键提示」+ 那一栏的按键、右栏标题是当前栏的名字。
    /// 主页面那几块（艺术字、弹幕、输入框）一个字都不该剩。
    #[test]
    fn config_page_takes_over_the_whole_screen() {
        let cfg = Config::default();
        let mut app = App::default();
        app.push_danmu(&danmu("小明".into(), "你好".into()), &cfg);
        app.room = Some(RoomInfo::new(9527));

        assert_eq!(
            app.control.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT),
            Action::StartLogin,
            "没登录：翻开账号栏顺手就要一张二维码"
        );

        let out = render(&app, &cfg, 120, 30);
        for want in [
            "按键提示",
            "功能",
            "账号",
            "分区",
            "直播间信息",
            "推流码",
            "回车 执行",
            "重新扫码",
            "退出登录",
            "未登录（回车扫码）",
        ] {
            assert!(
                flat(&out).contains(&flat(want)),
                "配置页上缺了「{want}」：\n{out}"
            );
        }
        assert!(!flat(&out).contains("弹幕们"), "主页面不该还在：\n{out}");
        assert!(!flat(&out).contains("在这里打字"), "输入框也不该还在：\n{out}");
        assert!(!out.contains(BANNER[0]), "艺术字是主页面的，配置页上不该有");
    }

    /// 左栏 16 格宽、当前项前面是 ▸、右栏的边框标题跟着当前栏走。
    #[test]
    fn sidebar_and_content_frame_follow_the_current_tab() {
        let cfg = Config::default();
        let mut app = App::default();
        // 先假装登录过：换栏本身不该翻出二维码来干扰这一屏
        app.control
            .on_login_event(LoginEvent::LoggedIn("小明 (uid 7)".into()));
        app.control.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT);

        let out = render(&app, &cfg, 120, 30);
        let rows: Vec<&str> = out.lines().collect();
        // 四个功能名都落在左栏那 16 格里（右栏、顶栏里出现的不算数）
        for name in ["账号", "分区", "直播间信息", "推流码"] {
            assert!(
                rows.iter()
                    .any(|l| flat(&first_cells(l, 16)).contains(&flat(name))),
                "「{name}」不在左栏里：\n{out}"
            );
        }
        // 当前项前面是 ▸，而且只有一项带它
        let marked = mark_row(&rows);
        assert!(flat(marked).contains("▸账号"));
        // 右栏那条边框的标题就是当前栏的名字，写在正文第一行上。
        // 认它靠「这一行有两条框的顶边」（顶栏只有一条）。
        let top = body_top(&rows);
        assert!(flat(top).contains("账号"), "{top}");

        // Tab 换到分区栏：▸ 和右栏标题都要跟着走
        app.control.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        let out = render(&app, &cfg, 120, 30);
        let rows: Vec<&str> = out.lines().collect();
        let marked = mark_row(&rows);
        assert!(flat(marked).contains("▸分区"));
        let top = body_top(&rows);
        assert!(flat(top).contains("分区"), "{top}");
        assert!(!flat(top).contains("账号"), "标题得跟着换：{top}");
    }

    /// 正文第一行：左栏和右栏两条框的顶边都在这上面。
    fn body_top<'a>(rows: &[&'a str]) -> &'a str {
        rows.iter()
            .find(|l| l.matches('┌').count() >= 2)
            .expect("正文第一行有两条框的顶边")
    }

    /// 找到带 ▸ 的那一行，并确认全屏只有这一行有 ——
    /// 两个 ▸ 的话用户根本不知道选的是谁。
    ///
    /// 只看左栏那 16 格：右栏自己也会用 ▸（账号栏的两个选项、直播间信息栏的两行
    /// 字段都是这个标记），全屏只剩一个 ▸ 已经不是现在这条规矩了。
    fn mark_row<'a>(rows: &[&'a str]) -> &'a str {
        let marked: Vec<&&str> = rows
            .iter()
            .filter(|l| first_cells(l, 16).contains('▸'))
            .collect();
        assert_eq!(marked.len(), 1, "左栏 ▸ 只该出现在当前那一项上：\n{rows:?}");
        marked[0]
    }

    /// 取一行里前 `n` 格。缓冲区里一格正好一个字符（宽字符的第二格是空格），
    /// 所以直接按字符切就是按格切 —— 别用上面的 `cell_text`，
    /// 那个是按显示宽度累加的，碰上汉字会把预算算多。
    fn first_cells(line: &str, n: usize) -> String {
        line.chars().take(n).collect()
    }

    /// 账号栏拿到二维码内容就画出来（半格字符），登录成功之后收掉。
    #[test]
    fn account_pane_draws_the_qr_and_drops_it_after_login() {
        let cfg = Config::default();
        let mut app = App::default();
        app.control.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT);
        app.control.on_login_event(LoginEvent::Qr(
            "https://passport.bilibili.com/h5/login?qrcode_key=abcdefgh".into(),
        ));

        // 二维码有 40 来行，终端得给够高度
        let out = render(&app, &cfg, 120, 48);
        assert!(
            out.contains('▀') || out.contains('█') || out.contains('▄'),
            "二维码没画出来：\n{out}"
        );
        assert!(flat(&out).contains(&flat("用哔哩哔哩 App 扫码登录")));
        assert!(flat(&out).contains(&flat("账号:")));

        app.control
            .on_login_event(LoginEvent::LoggedIn("小明 (uid 7)".into()));
        let out = render(&app, &cfg, 120, 48);
        assert!(flat(&out).contains(&flat("小明 (uid 7)")));
        assert!(
            !out.contains('▀') && !out.contains('▄'),
            "登录成功后二维码该收掉：\n{out}"
        );
    }

    /// 直播间信息栏：两行字段（选中的那行带 ▸）、说明、下面那一格是封面预览。
    /// 有地址还没图 = 「加载中」；图挂了 = 一句话。半格字符怎么画在 `ui/cover.rs` 里验。
    #[test]
    fn info_pane_shows_the_fields_and_the_cover_box() {
        let cfg = Config::default();
        let mut app = App::default();
        app.control
            .on_login_event(LoginEvent::LoggedIn("小明 (uid 7)".into()));
        app.control.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT);
        app.control.handle_key(KeyCode::F(6), KeyModifiers::NONE);

        // 一进来还没拉到：封面那一格写的是「还没有封面」
        let out = render(&app, &cfg, 100, 30);
        assert!(flat(&out).contains("直播间信息"), "{out}");
        assert!(flat(&out).contains("▸标题"), "选中的那行要带 ▸：{out}");
        assert!(flat(&out).contains("封面"), "{out}");
        assert!(flat(&out).contains("当前封面"), "预览那一格要有标题：{out}");

        // 拉到了：标题摆上、封面地址有了但图还没到 -> 「加载中」
        let url = "https://i0.hdslb.com/bfs/live/user_cover/x.png";
        app.control.on_info_event(InfoEvent::Meta {
            title: "今晚八点随便播播".into(),
            cover: url.into(),
        });
        let out = render(&app, &cfg, 100, 30);
        assert!(flat(&out).contains("今晚八点随便播播"), "{out}");
        assert!(flat(&out).contains("封面加载中"), "{out}");

        // 图是垃圾字节：只在那写一句话，整页照样画得出来
        app.control.on_info_event(InfoEvent::CoverImage {
            url: url.into(),
            bytes: b"nope".to_vec(),
        });
        let out = render(&app, &cfg, 100, 30);
        assert!(flat(&out).contains("封面没加载上"), "{out}");
        assert!(
            flat(&out).contains("今晚八点随便播播"),
            "别的地方不受影响：{out}"
        );
    }

    /// 分区栏：拉回来的两级分区表真的摆进了右栏 —— 父分区带展开标记，
    /// 没配过的时候一个子分区都不露出来（Go 版在这儿永远展开「网游」）。
    #[test]
    fn area_pane_draws_the_two_level_tree() {
        use crate::api::area::{AreaEvent, ParentArea, SubArea};

        let cfg = Config::default();
        let mut app = App::default();
        app.control
            .on_login_event(LoginEvent::LoggedIn("小明 (uid 7)".into()));
        app.control.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT);
        app.control.handle_key(KeyCode::Tab, KeyModifiers::NONE); // 分区栏
        app.control.on_area_event(AreaEvent::Loaded(vec![
            ParentArea {
                name: "网游".into(),
                list: vec![SubArea {
                    id: 86,
                    name: "英雄联盟".into(),
                }],
            },
            ParentArea {
                name: "虚拟主播".into(),
                list: vec![SubArea {
                    id: 371,
                    name: "虚拟日常".into(),
                }],
            },
        ]));

        let out = render(&app, &cfg, 100, 30);
        assert!(flat(&out).contains("全部分区"), "{out}");
        assert!(flat(&out).contains("▸网游"), "收起的父分区要带标记：{out}");
        assert!(flat(&out).contains("▸虚拟主播"), "{out}");
        assert!(
            !flat(&out).contains("英雄联盟"),
            "没配过时子分区不该露出来：{out}"
        );

        // 移到「网游」上按 → 展开：子分区就出现了
        app.control.handle_key(KeyCode::Down, KeyModifiers::NONE);
        app.control.handle_key(KeyCode::Right, KeyModifiers::NONE);
        let out = render(&app, &cfg, 100, 30);
        assert!(flat(&out).contains("▾网游"), "{out}");
        assert!(flat(&out).contains("英雄联盟"), "{out}");
    }
}


