//! 界面：ratatui 画图，crossterm 收键。
//!
//! 事件循环刻意不用 EventStream（那要拉 futures 依赖）：每 100 毫秒 poll 一下键盘，
//! 顺便把网络那边塞进 channel 的消息取走、重画一帧。TUI 这点开销无所谓，
//! 而 tokio 是多线程运行时，主线程堵这 100ms 不影响后台的网络任务。

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
use crate::api::room::{self, OnlineRankUser, RoomInfo};
use crate::config::Config;
use crate::timefmt;

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

pub async fn run(
    cfg: Config,
    danmu_rx: Receiver<DanmuMsg>,
    room_rx: Receiver<RoomInfo>,
    refresh_tx: Sender<()>,
) -> Result<()> {
    let mut terminal = setup()?;
    let res = event_loop(&mut terminal, cfg, danmu_rx, room_rx, refresh_tx).await;
    restore()?;
    res
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
    mut danmu_rx: Receiver<DanmuMsg>,
    mut room_rx: Receiver<RoomInfo>,
    refresh_tx: Sender<()>,
) -> Result<()> {
    let mut app = App::default();
    loop {
        // 先收网络那边的消息再画，画面永远是最新的。
        while let Ok(m) = danmu_rx.try_recv() {
            app.push_danmu(&m, &cfg);
        }
        while let Ok(r) = room_rx.try_recv() {
            app.room = Some(r);
        }

        terminal.draw(|f| draw(f, &app, &cfg))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(KeyEvent {
                code, modifiers, ..
            }) = event::read()?
        {
            match (code, modifiers) {
                // 退出只有 Ctrl+C：Esc 是「返回上一层」，别接成退出。
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(()),
                // 手动刷房间信息（跟 Go 版的 Ctrl+R 一致），不等那 30 秒。
                (KeyCode::Char('r'), KeyModifiers::CONTROL) => room::refresh(&refresh_tx),
                _ => {}
            }
        }
    }
}

fn draw(f: &mut Frame, app: &App, _cfg: &Config) {
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
    f.render_widget(stream_panel(app), status);
    f.render_widget(input_panel(), input);
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

fn stream_panel(app: &App) -> Paragraph<'static> {
    let line = match app.room.as_ref() {
        None => Line::from(Span::styled(
            "○ 还没拿到房间状态",
            Style::default().fg(Color::DarkGray),
        )),
        Some(r) => match r.live_status {
            // 主播最关心「现在到底有没有在推流」，所以第一眼就是直播中 / 未开播。
            1 => Line::from(vec![
                Span::styled("● 直播中", Style::default().fg(Color::Green)),
                Span::raw(format!("  已播 {}   在线 {}", r.live_duration, r.online)),
            ]),
            2 => Line::from(Span::styled(
                "● 轮播中",
                Style::default().fg(Color::Yellow),
            )),
            _ => Line::from(Span::styled("○ 未开播", Style::default().fg(Color::Gray))),
        },
    };
    Paragraph::new(line).block(Block::bordered().title(" obs 推流状态 "))
}

fn input_panel() -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::styled(
        "这一版只读：发送、开播、OBS 都还没接",
        Style::default().fg(Color::DarkGray),
    )))
    .block(Block::bordered().title(" 弹幕输入框 · Ctrl+R 刷新 / Ctrl+C 退出 "))
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
}
