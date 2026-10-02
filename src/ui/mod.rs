//! 界面：ratatui 画图，crossterm 收键。
//!
//! 事件循环刻意不用 EventStream（那要拉 futures 依赖）：每 100 毫秒 poll 一下键盘，
//! 顺便把网络那边塞进 channel 的消息取走、重画一帧。TUI 这点开销无所谓，
//! 而 tokio 是多线程运行时，主线程堵这 100ms 不影响后台的网络任务。

mod control;
mod qr;

use std::collections::VecDeque;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::*;
use ratatui::widgets::*;
use std::io::stdout;
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::api::danmaku::DanmuMsg;
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

pub async fn run(
    cfg: Config,
    w: Wiring,
) -> Result<()> {
    let mut terminal = setup()?;
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
}

fn setup() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn restore() -> Result<()> {
    execute!(stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
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
    fn push_danmu(&mut self, m: &DanmuMsg, cfg: &Config) {
        let stamp = if cfg.show_time {
            timefmt::hhmm(m.time)
        } else {
            String::new()
        };

        if m.is_system() {
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
        }
    }
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
    } = w;
    let mut app = App::default();
    loop {
        // 先收网络那边的消息再画，画面永远是最新的。
        while let Ok(m) = danmu_rx.try_recv() {
            app.push_danmu(&m, &cfg);
        }
        while let Ok(r) = room_rx.try_recv() {
            // 顺手把标题喂给配置页的「直播间信息」栏，省得那一栏空着让人以为坏了 ——
            // 这轮它自己不发请求（真去改标题是下一步）。
            app.control.seed_title(&r.title);
            app.room = Some(r);
        }
        while let Ok(ev) = login_rx.try_recv() {
            app.control.on_login_event(ev);
        }

        terminal.draw(|f| draw(f, &app, &cfg))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(KeyEvent {
                code, modifiers, ..
            }) = event::read()?
        {
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
                    Action::ToMain => {
                        if let Some(text) = app.input.handle_key(code, modifiers)
                            && send_tx.try_send(text.clone()).is_err()
                        {
                            // 队列满、或者发送端没起来（比如没配房间号）。界面永远不等
                            // 发送端，但这条得说清楚没发出去，不然用户对着空气等回显。
                            app.push_danmu(
                                &DanmuMsg::system(format!("这条没发出去（发送端没起来）：{text}")),
                                &cfg,
                            );
                        }
                    }
                },
            }
        }
    }
}

fn draw(f: &mut Frame, app: &App, _cfg: &Config) {
    // 第二页占满整屏：主页面那套一格里都不留（Go 版是 Pages 切换，一个意思）。
    if app.control.page() == control::Page::Config {
        control::draw(f, &app.control, f.area());
        return;
    }

    let area = f.area();
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

    f.render_widget(banner(), header);
    f.render_widget(info_panel(app), info);
    f.render_widget(viewers_panel(app), viewers);
    f.render_widget(danmaku_panel(app, right), right);
    f.render_widget(stream_panel(app, status.width), status);
    f.render_widget(input_panel(app), input);
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

/// 弹幕区永远显示**最后**几行 —— 进来的新弹幕就在眼前，不用去翻页。
fn danmaku_panel(app: &App, area: Rect) -> Paragraph<'static> {
    // 框的上下边框各占一行，剩下多少行就能放多少行字。
    let height = area.height.saturating_sub(2) as usize;
    let start = app.lines.len().saturating_sub(height);
    let visible: Vec<Line<'static>> = app.lines.iter().skip(start).cloned().collect();
    Paragraph::new(visible).block(Block::bordered().title(" 弹幕们 "))
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
        for (w, h) in [(20u16, 8u16), (40, 12), (5, 5), (1, 1)] {
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
            "回车 重新扫码",
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
    fn mark_row<'a>(rows: &[&'a str]) -> &'a str {
        let marked: Vec<&&str> = rows.iter().filter(|l| l.contains('▸')).collect();
        assert_eq!(marked.len(), 1, "▸ 只该出现在当前那一项上：\n{rows:?}");
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
}


