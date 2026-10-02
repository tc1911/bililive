//! 配置页（第二页）：左栏挑功能，右栏干活。
//!
//! 布局按效果图来（顶栏 4 行：边框 + 按键提示 + 最近一条消息；下面左栏 16 格宽、
//! 右栏带边框且标题是当前栏的名字）：
//!
//!     +-- 按键提示 ------------------------------+
//!     | 回车 重新扫码 ...                          |
//!     | 最近一条消息                               |
//!     +--------------+---------------------------+
//!     | > 账号       |                           |
//!     |   分区       |         实际内容           |
//!     |   直播间信息 |                           |
//!     |   推流码     |                           |
//!     +--------------+---------------------------+
//!
//! （上面用 ASCII 画的只是示意，屏幕上画的是真框线字符。）
//!
//! 按键分工照 Go 版 `ui/control/control.go`，别串：
//!   - `Shift+Tab` 在弹幕页和配置页之间来回切
//!   - `Tab` 在配置页里换功能栏；**弹幕页上绝对不能抢**，那是输入框的键
//!   - `↑↓` 在栏里选；账号 / 推流码这两栏没有可上下选的东西，就顺手拿来换栏
//!   - `回车`：账号栏重新扫码；直播间信息栏进编辑 / 提交
//!   - `Esc`：取消编辑 → 收起配置页，退到弹幕页就**停住**，退出只有 Ctrl+C
//!   - `F2` / `F3` / `F6` 直接翻开配置页并跳到账号 / 分区 / 直播间信息
//!
//! 这里只改界面状态，不碰网络也不碰磁盘 —— 那两件事分别归 `api::login` 和 main。

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::api::login::LoginEvent;
use crate::ui::qr;

/// 左栏宽度。16 格 = 边框两格 + 「▸ 直播间信息」12 格 + 一点余量，
/// 再窄就得截字（「直播间信息」五个汉字就占 10 格）。
const SIDEBAR_WIDTH: u16 = 16;

/// 现在屏上摆的是哪一页。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// 第一页：弹幕
    Main,
    /// 第二页：配置
    Config,
}

/// 配置页的功能栏。顺序就是左栏从上到下的顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Account,
    Area,
    Info,
    Stream,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Account, Tab::Area, Tab::Info, Tab::Stream];

    pub fn name(self) -> &'static str {
        match self {
            Tab::Account => "账号",
            Tab::Area => "分区",
            Tab::Info => "直播间信息",
            Tab::Stream => "推流码",
        }
    }

    /// 顶栏那行按键提示。文案照抄 Go 版的 `tabHints` ——
    /// 只有「推流码」那栏把「F4 开播 / F5 下播」去掉了：这两个键这一轮还没接，
    /// 提示里挂一个按了没反应的键，比不写更坑人。
    pub fn hint(self) -> &'static str {
        match self {
            Tab::Account => {
                "回车 重新扫码    Ctrl+R 刷新房间信息    Tab 换功能    Shift+Tab 回弹幕页    Esc 返回"
            }
            Tab::Area => {
                "↑↓ 选分区    ←→ 展开/收起    回车 确认该分区    Tab 换功能    Shift+Tab 回弹幕页"
            }
            Tab::Info => {
                "↑↓ 选一项    回车 编辑    再回车 提交    Esc 取消    Tab 换功能    Shift+Tab 回弹幕页"
            }
            Tab::Stream => "Tab 换功能    Shift+Tab 回弹幕页    Esc 返回    （开播 / 下播下一步接）",
        }
    }

    /// 换栏，从头 / 从尾绕回去。`d` 传 ±1。
    fn step(self, d: i32) -> Tab {
        let n = Tab::ALL.len() as i32;
        let i = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0) as i32;
        Tab::ALL[(i + d).rem_euclid(n) as usize]
    }
}

/// 按键处理的结果，界面按它决定这一下还要不要再往下传。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 配置页吃掉了（或者在这儿没意义），谁也别再管
    Handled,
    /// 交给弹幕页的输入框（只会在弹幕页出现）
    ToMain,
    /// 这一下要开一张新二维码（界面 -> 登录任务）
    StartLogin,
}

/// 「直播间信息」栏里那一行正在编辑的内容。
///
/// 按**字符**存：按字节存的话，中文退格会退掉三分之一个字，
/// 屏幕上留下一格方块（弹幕输入框踩过同样的坑）。
#[derive(Default)]
struct Edit {
    /// 进编辑之前的值，Esc 用它还原
    orig: String,
    buf: Vec<char>,
    /// 光标，取值区间 `0..=buf.len()`
    cursor: usize,
}

impl Edit {
    fn new(orig: &str) -> Self {
        Self {
            orig: orig.to_string(),
            buf: orig.chars().collect(),
            cursor: orig.chars().count(),
        }
    }

    fn text(&self) -> String {
        self.buf.iter().collect()
    }
}

pub struct Control {
    page: Page,
    tab: Tab,
    /// 顶栏第二行：最近一条消息
    message: String,
    /// 账号那行的字（「小明 (uid 7)」/「未登录（回车扫码）」…）
    account: String,
    logged_in: bool,
    /// 账号栏画的那几行（标题 + 二维码）。空表示还没生成 / 还没有。
    qr: Vec<Line<'static>>,
    /// 正在等扫码。这期间回车不重复生成：屏幕上两张二维码，用户扫了哪张都说不清。
    login_pending: bool,
    /// 「直播间信息」栏选中的是哪一行
    field_idx: usize,
    /// 正在编辑时的缓冲，`None` 表示没在编辑
    edit: Option<Edit>,
    /// 直播间信息栏第一行（这轮只显示 / 本地记着，下一步才真去改标题）
    pub title: String,
    /// 直播间信息栏第二行
    pub cover: String,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            page: Page::Main,
            tab: Tab::Account,
            message: String::new(),
            account: "未登录（回车扫码）".to_string(),
            logged_in: false,
            qr: Vec::new(),
            login_pending: false,
            field_idx: 0,
            edit: None,
            title: String::new(),
            cover: String::new(),
        }
    }
}

impl Control {
    pub fn page(&self) -> Page {
        self.page
    }

    /// 只给测试用：按键那几条断言看的就是「现在是哪一栏、在不在编辑」。
    #[allow(dead_code)]
    pub fn tab(&self) -> Tab {
        self.tab
    }

    #[allow(dead_code)]
    pub fn is_editing(&self) -> bool {
        self.edit.is_some()
    }

    pub fn set_message(&mut self, text: impl Into<String>) {
        // 只有一行，换行会把下面的内容顶掉（Go 版特意做过这个替换）
        self.message = text.into().replace('\n', " ");
    }

    /// 房间信息到了先拿它把「直播间信息」栏的标题填上 ——
    /// 空着不好看，而且用户第一眼就想知道现在挂的是什么标题。
    /// 只填一次：用户改过的（或者正在编辑的）别被下一轮房间信息冲掉。
    pub fn seed_title(&mut self, title: &str) {
        if self.title.is_empty() && !title.is_empty() {
            self.title = title.to_string();
        }
    }

    /// 登录任务那边来的消息。只动界面状态，不做任何网络 / 磁盘的事。
    pub fn on_login_event(&mut self, ev: LoginEvent) {
        match ev {
            LoginEvent::LoggedIn(line) => {
                self.logged_in = true;
                self.account = line;
                self.qr.clear(); // 二维码没用了，收掉
                self.login_pending = false;
                // 别在这儿自己编「登录成功」：开局那次「配置里的凭据还有效」也走这个分支，
                // 真扫码成功则由 apply_login 补一句更准的话。
            }
            LoginEvent::LoggedOut(reason) => {
                self.logged_in = false;
                self.account = reason;
                self.login_pending = false;
            }
            LoginEvent::Qr(content) => {
                let mut lines = vec![
                    Line::from(Span::styled(
                        "用哔哩哔哩 App 扫码登录",
                        Style::default().fg(Color::Yellow),
                    )),
                    Line::from(""),
                ];
                // 编不出来也只是画一行红字说明，绝不把整个界面带走
                lines.extend(qr::lines(&content));
                self.qr = lines;
            }
            LoginEvent::Hint(text) => self.set_message(text),
            LoginEvent::Failed(text) => {
                // 放开「正在等扫码」，不然重进账号栏也没法再试一次
                self.login_pending = false;
                self.set_message(text);
            }
        }
    }

    /// 喂一个按键。返回这一下有没有被配置页吃掉。
    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Action {
        // F2 / F3 / F6 是「直接翻开配置页并跳到某一栏」，在**哪一页**都该管用，
        // 所以排在「现在是不是已经开着」前面。
        match code {
            KeyCode::F(2) => return self.open_tab(Tab::Account),
            KeyCode::F(3) => return self.open_tab(Tab::Area),
            KeyCode::F(6) => return self.open_tab(Tab::Info),
            _ => {}
        }

        if self.page == Page::Main {
            // 弹幕页上只有 Shift+Tab 归配置页管。
            // `Tab` 千万不能抢：那是输入框的键（Go 版的输入框拿它当补全用），
            // 抢了之后正在打字的人按一下 Tab，字就不知道跑哪儿去了。
            if code == KeyCode::BackTab {
                return self.open_tab(self.tab);
            }
            return Action::ToMain;
        }

        // 编辑中：回车提交、Esc 还原，其余都进缓冲。
        // Tab / Shift+Tab 例外：编辑到一半想跑，不该逼人先按一次 Esc。
        if self.edit.is_some() {
            match code {
                KeyCode::Esc => {
                    self.cancel_edit();
                    self.set_message("已取消，没有改动");
                    return Action::Handled;
                }
                KeyCode::Enter => {
                    self.commit_edit();
                    return Action::Handled;
                }
                KeyCode::Tab => return self.cycle_tab(1),
                KeyCode::BackTab => {
                    self.close();
                    return Action::Handled;
                }
                _ => {
                    self.edit_key(code, mods);
                    return Action::Handled;
                }
            }
        }

        match code {
            // Shift+Tab 在第一页（弹幕）和第二页（配置）之间来回切。
            KeyCode::BackTab => self.close(),
            KeyCode::Tab => return self.cycle_tab(1),
            // Esc 是「返回上一层」：取消编辑 → 收起配置页 → 就此停住。
            // **不是退出**，退出只有 Ctrl+C。
            KeyCode::Esc => self.close(),
            KeyCode::Enter => match self.tab {
                Tab::Account => return Action::StartLogin,
                Tab::Info => self.start_edit(),
                Tab::Area | Tab::Stream => {
                    let name = self.tab.name();
                    self.set_message(format!("「{name}」这轮还没干活，下一步接"));
                }
            },
            KeyCode::Up | KeyCode::Down => {
                let d = if code == KeyCode::Up { -1 } else { 1 };
                match self.tab {
                    // 直播间信息栏：上下是在栏里选一项
                    Tab::Info => self.move_field(d),
                    // 账号 / 推流码这两栏本来没有可上下选的东西，就顺手拿来换栏
                    Tab::Account | Tab::Stream => return self.cycle_tab(d),
                    // 分区栏将来要上下选分区，这轮先不动
                    Tab::Area => {}
                }
            }
            _ => {}
        }
        Action::Handled
    }

    /// 露出配置页并切到指定栏（已经开着就当换栏）。
    fn open_tab(&mut self, tab: Tab) -> Action {
        self.page = Page::Config;
        self.tab = tab;
        self.enter_tab()
    }

    fn cycle_tab(&mut self, d: i32) -> Action {
        // 编辑到一半按 Tab 跑掉：那一行还原成进编辑之前的值，
        // 不然半截输入会留在框里，下次进来还以为值本来就那样。
        self.cancel_edit();
        self.tab = self.tab.step(d);
        self.enter_tab()
    }

    /// 换到某一栏之后的「需要什么显示什么」。
    ///
    /// 这轮只有账号栏有需求：没登录、手上又还没有码，就直接把码摆出来，
    /// 别让人先去找回车（Go 版 `fillTab` 是同一个思路）。
    fn enter_tab(&mut self) -> Action {
        if self.tab == Tab::Account && !self.logged_in && !self.login_pending && self.qr.is_empty() {
            self.login_pending = true;
            return Action::StartLogin;
        }
        Action::Handled
    }

    /// 收起配置页，回弹幕页。退到弹幕页就停住 ——
    /// 弹幕页上再按 Esc 什么都不做，更不会退出。
    fn close(&mut self) {
        self.cancel_edit();
        self.page = Page::Main;
    }

    /// 只有两行，到头就绕回去。
    fn move_field(&mut self, d: i32) {
        self.field_idx = ((self.field_idx as i32 + d).rem_euclid(2)) as usize;
    }

    fn start_edit(&mut self) {
        let value = self.field_value(self.field_idx).to_string();
        self.edit = Some(Edit::new(&value));
        self.set_message("编辑中：回车提交，Esc 取消");
    }

    /// 取消编辑：值还原成进编辑之前的样子。
    fn cancel_edit(&mut self) {
        if let Some(e) = self.edit.take() {
            self.set_field(self.field_idx, e.orig);
        }
    }

    /// 提交当前这一行。
    ///
    /// 这轮**不真提交** —— 改标题 / 传封面是下一步。但值要记下来、
    /// 提示里也得说清楚还没发给 B 站，不然用户以为已经生效了。
    fn commit_edit(&mut self) {
        let Some(e) = self.edit.take() else {
            return;
        };
        let value = e.text();
        let name = self.field_name(self.field_idx);
        self.set_field(self.field_idx, value);
        self.set_message(format!("{name}改好了，但还没提交给 B 站 —— 提交下一步接"));
    }

    fn edit_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let Some(e) = self.edit.as_mut() else {
            return;
        };
        match code {
            KeyCode::Backspace => {
                if e.cursor > 0 {
                    e.cursor -= 1;
                    e.buf.remove(e.cursor);
                }
            }
            KeyCode::Delete => {
                if e.cursor < e.buf.len() {
                    e.buf.remove(e.cursor);
                }
            }
            KeyCode::Left => e.cursor = e.cursor.saturating_sub(1),
            KeyCode::Right => e.cursor = (e.cursor + 1).min(e.buf.len()),
            KeyCode::Home => e.cursor = 0,
            KeyCode::End => e.cursor = e.buf.len(),
            // 带 Ctrl/Alt 的键一律不当文字收：插进去的只是控制字符，
            // 屏幕上什么都看不见，却会莫名其妙地进请求体。
            // （Ctrl+C / Ctrl+R 在更上面就被全局那两条拦走了。）
            KeyCode::Char(c) if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                let at = e.cursor.min(e.buf.len());
                e.buf.insert(at, c);
                e.cursor = at + 1;
            }
            _ => {}
        }
        // 边打边写回那一行：Go 版用的是真 InputField，框里的字一直是活的。
        // 只在提交时才落值的话，Esc 之前那一栏显示的就会是旧值，
        // 用户会以为「根本打不进去」。
        let value = e.text();
        self.set_field(self.field_idx, value);
    }

    fn field_name(&self, idx: usize) -> &'static str {
        if idx == 0 { "标题" } else { "封面" }
    }

    fn field_value(&self, idx: usize) -> &str {
        if idx == 0 { &self.title } else { &self.cover }
    }

    fn set_field(&mut self, idx: usize, value: String) {
        if idx == 0 {
            self.title = value;
        } else {
            self.cover = value;
        }
    }
}

// ------------------------------------------------------------------ 画

pub fn draw(f: &mut Frame, c: &Control, area: Rect) {
    let [bar, body] = Layout::vertical([Constraint::Length(4), Constraint::Fill(1)]).areas(area);

    // 顶栏 4 行：边框 + 按键提示 + 最近一条消息（跟效果图一致）
    let hint = Paragraph::new(Text::from(vec![
        Line::from(Span::styled(c.tab.hint(), Style::default().fg(Color::Yellow))),
        Line::from(Span::styled(
            c.message.clone(),
            Style::default().fg(Color::White),
        )),
    ]))
    .block(Block::bordered().title(" 按键提示 "));
    f.render_widget(hint, bar);

    let [side, content] =
        Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Fill(1)]).areas(body);

    let items: Vec<Line<'static>> = Tab::ALL
        .iter()
        .map(|t| {
            let (mark, style) = if *t == c.tab {
                ("▸ ", Style::default().fg(Color::Yellow))
            } else {
                ("  ", Style::default().fg(Color::Gray))
            };
            Line::from(Span::styled(format!("{mark}{}", t.name()), style))
        })
        .collect();
    f.render_widget(
        Paragraph::new(items).block(Block::bordered().title(" 功能 ")),
        side,
    );

    // 右栏：边框的标题就是当前栏的名字
    let block = Block::bordered().title(format!(" {} ", c.tab.name()));
    let inner = block.inner(content);
    f.render_widget(block, content);
    match c.tab {
        Tab::Account => draw_account(f, c, inner),
        Tab::Info => f.render_widget(Paragraph::new(info_lines(c)), inner),
        Tab::Area => draw_placeholder(
            f,
            inner,
            "分区",
            "这里会是一棵两级分区树：↑↓ 选、←→ 展开/收起、回车把分区记进配置",
        ),
        Tab::Stream => draw_placeholder(
            f,
            inner,
            "推流码",
            "开播之后，这里会显示推流地址与推流码（下一步接）",
        ),
    }
}

fn draw_account(f: &mut Frame, c: &Control, area: Rect) {
    // 账号那行占两行（Go 版的 accountView 也是 2），下面全留给二维码
    let [who, code] = Layout::vertical([Constraint::Length(2), Constraint::Fill(1)]).areas(area);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("账号: ", Style::default().fg(Color::Gray)),
            Span::styled(
                c.account.clone(),
                Style::default().fg(if c.logged_in {
                    Color::Green
                } else {
                    Color::White
                }),
            ),
        ])),
        who,
    );
    if !c.qr.is_empty() {
        // 逐行居中：每行宽度一样，画出来就是正的。
        // 码比这一格宽的话 ratatui 会截断，跟 Go 版一样（终端太小就扫不动，认了）。
        f.render_widget(
            Paragraph::new(c.qr.clone()).alignment(Alignment::Center),
            code,
        );
    }
}

fn draw_placeholder(f: &mut Frame, area: Rect, name: &str, note: &str) {
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!("{name} —— 下一步接"),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                note.to_string(),
                Style::default().fg(Color::DarkGray),
            )),
        ]),
        area,
    );
}

fn info_lines(c: &Control) -> Vec<Line<'static>> {
    let fields = [("标题", c.title.as_str()), ("封面", c.cover.as_str())];
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, (name, value)) in fields.iter().enumerate() {
        let selected = c.field_idx == i;
        // 平时没有光标，得有个 ▸ 告诉用户选的是哪一行（Go 版的 markFields）
        let mut spans = vec![Span::styled(
            if selected {
                format!("▸ {name} ")
            } else {
                format!("  {name} ")
            },
            Style::default().fg(if selected {
                Color::Yellow
            } else {
                Color::Gray
            }),
        )];
        match &c.edit {
            Some(e) if selected => spans.extend(cursor_spans(e)),
            _ if value.is_empty() => spans.push(Span::styled(
                "（空）",
                Style::default().fg(Color::DarkGray),
            )),
            _ => spans.push(Span::raw((*value).to_string())),
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "标题上限 40 字；封面填本地图片路径或 .hdslb.com 链接，留空表示不改",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        "这轮先搭骨架：能选、能打字、Esc 还原；真提交下一步接",
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

/// 编辑中那一行的光标：反色的一格。不画出来根本看不出打字的落点
/// （弹幕输入框也是这么画的）。
fn cursor_spans(e: &Edit) -> Vec<Span<'static>> {
    let cursor = e.cursor.min(e.buf.len());
    let mut spans = vec![Span::raw(e.buf[..cursor].iter().collect::<String>())];
    match e.buf.get(cursor) {
        Some(at) => {
            spans.push(Span::styled(
                at.to_string(),
                Style::default().add_modifier(Modifier::REVERSED),
            ));
            spans.push(Span::raw(e.buf[cursor + 1..].iter().collect::<String>()));
        }
        // 光标在最右边：拿一个反色空格当光标
        None => spans.push(Span::styled(
            " ",
            Style::default().add_modifier(Modifier::REVERSED),
        )),
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    const NONE: KeyModifiers = KeyModifiers::NONE;

    fn logged_in() -> Control {
        let mut c = Control::default();
        // 假装已经登录：这样换栏不会顺带触发扫码，测的是纯按键语义
        c.on_login_event(LoginEvent::LoggedIn("小明 (uid 7)".into()));
        c
    }

    /// Shift+Tab 是「第一页 ↔ 第二页」那一对键，来回切。
    #[test]
    fn shift_tab_switches_between_the_two_pages() {
        let mut c = logged_in();
        assert_eq!(c.page(), Page::Main);

        assert_eq!(
            c.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT),
            Action::Handled
        );
        assert_eq!(c.page(), Page::Config);
        assert_eq!(c.tab(), Tab::Account, "翻开时停在原来那一栏");

        assert_eq!(
            c.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT),
            Action::Handled
        );
        assert_eq!(c.page(), Page::Main);
    }

    /// 弹幕页上：`Tab` 是输入框的键（不能抢），`Esc` 什么都不做（更不能退出）。
    #[test]
    fn the_danmaku_page_keeps_tab_and_esc() {
        let mut c = logged_in();
        assert_eq!(c.handle_key(KeyCode::Tab, NONE), Action::ToMain);
        assert_eq!(c.handle_key(KeyCode::Esc, NONE), Action::ToMain);
        assert_eq!(c.handle_key(KeyCode::Char('a'), NONE), Action::ToMain);
        assert_eq!(c.page(), Page::Main, "这几个键都不该把配置页翻出来");
    }

    /// 配置页里 Tab 换栏，到头绕回第一栏。
    #[test]
    fn tab_cycles_through_the_four_tabs() {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE);
        for want in [Tab::Area, Tab::Info, Tab::Stream, Tab::Account] {
            assert_eq!(c.handle_key(KeyCode::Tab, NONE), Action::Handled);
            assert_eq!(c.tab(), want);
        }
        assert_eq!(c.page(), Page::Config, "换栏又不是收起来");
    }

    /// 账号 / 推流码这两栏没有可上下选的东西，↑↓ 就拿来换栏；
    /// 分区栏的 ↑↓ 留给将来的分区列表；直播间信息栏的 ↑↓ 是选字段。
    #[test]
    fn up_down_means_different_things_per_tab() {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE); // 账号
        assert_eq!(c.tab(), Tab::Account);
        c.handle_key(KeyCode::Up, NONE);
        assert_eq!(c.tab(), Tab::Stream, "↑ 从账号绕到最后一栏");
        c.handle_key(KeyCode::Down, NONE);
        assert_eq!(c.tab(), Tab::Account, "↓ 再绕回来");

        c.handle_key(KeyCode::Tab, NONE); // 分区
        c.handle_key(KeyCode::Up, NONE);
        assert_eq!(c.tab(), Tab::Area, "分区栏的 ↑↓ 这轮先不换栏");

        c.handle_key(KeyCode::Tab, NONE); // 直播间信息
        assert_eq!(c.field_idx, 0);
        c.handle_key(KeyCode::Down, NONE);
        assert_eq!(c.field_idx, 1);
        assert_eq!(c.tab(), Tab::Info, "选字段不换栏");
        c.handle_key(KeyCode::Down, NONE);
        assert_eq!(c.field_idx, 0, "两项，到头绕回去");
    }

    /// Esc 是「返回上一层」：编辑中先取消编辑，再按才收起配置页，
    /// 到了弹幕页就停住 —— 退出只有 Ctrl+C。
    #[test]
    fn esc_walks_back_one_level_and_then_stops() {
        let mut c = logged_in();
        c.seed_title("原标题");
        c.handle_key(KeyCode::BackTab, NONE);
        c.handle_key(KeyCode::Tab, NONE);
        c.handle_key(KeyCode::Tab, NONE); // 到直播间信息栏

        c.handle_key(KeyCode::Enter, NONE);
        assert!(c.is_editing());
        for ch in "改过的".chars() {
            c.handle_key(KeyCode::Char(ch), NONE);
        }
        assert_eq!(c.title, "原标题改过的");

        // 第一层：取消编辑，人还留在配置页
        c.handle_key(KeyCode::Esc, NONE);
        assert!(!c.is_editing());
        assert_eq!(c.title, "原标题", "取消要还原成进编辑之前的值");
        assert_eq!(c.page(), Page::Config);

        // 第二层：收起配置页
        c.handle_key(KeyCode::Esc, NONE);
        assert_eq!(c.page(), Page::Main);

        // 第三层就不存在了：停在弹幕页，绝不退出
        assert_eq!(c.handle_key(KeyCode::Esc, NONE), Action::ToMain);
        assert_eq!(c.page(), Page::Main);
    }

    /// F2 / F3 / F6：不管在哪一页，都直接翻开配置页并跳到指定栏。
    #[test]
    fn function_keys_jump_straight_to_a_tab() {
        let mut c = logged_in();
        assert_eq!(c.handle_key(KeyCode::F(3), NONE), Action::Handled);
        assert_eq!(c.page(), Page::Config);
        assert_eq!(c.tab(), Tab::Area);

        c.handle_key(KeyCode::BackTab, NONE);
        assert_eq!(c.page(), Page::Main);

        c.handle_key(KeyCode::F(6), NONE);
        assert_eq!((c.page(), c.tab()), (Page::Config, Tab::Info));

        c.handle_key(KeyCode::F(2), NONE);
        assert_eq!((c.page(), c.tab()), (Page::Config, Tab::Account));
    }

    /// 没登录时翻开账号栏要顺手把码摆出来；已经在等扫码了就别再生成一张。
    #[test]
    fn the_account_tab_asks_for_a_code_once() {
        let mut c = Control::default();
        assert!(!c.logged_in);
        assert_eq!(
            c.handle_key(KeyCode::BackTab, NONE),
            Action::StartLogin,
            "没登录：翻开账号栏就该去拿二维码"
        );
        c.handle_key(KeyCode::BackTab, NONE); // 收起来
        assert_eq!(
            c.handle_key(KeyCode::BackTab, NONE),
            Action::Handled,
            "正在等扫码，屏幕上不该出现第二张码"
        );

        // 回车永远是「重新扫码」
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::StartLogin);

        // 已经登录了就别再问接口
        let mut c = logged_in();
        assert_eq!(c.handle_key(KeyCode::BackTab, NONE), Action::Handled);
    }

    /// 编辑缓冲按字符走：中文退格退一个整字，光标能回到中间插字。
    #[test]
    fn editing_is_by_character() {
        let mut c = logged_in();
        c.seed_title("中文ab");
        c.handle_key(KeyCode::BackTab, NONE);
        c.handle_key(KeyCode::Tab, NONE);
        c.handle_key(KeyCode::Tab, NONE);
        c.handle_key(KeyCode::Enter, NONE);
        c.handle_key(KeyCode::Backspace, NONE);
        assert_eq!(c.title, "中文a", "退掉的是一个「b」");
        for _ in 0..4 {
            c.handle_key(KeyCode::Backspace, NONE);
        }
        assert_eq!(c.title, "", "退到空了也不许 panic");
        c.handle_key(KeyCode::Backspace, NONE);

        // 在中间插字
        for ch in "甲乙".chars() {
            c.handle_key(KeyCode::Char(ch), NONE);
        }
        c.handle_key(KeyCode::Left, NONE);
        c.handle_key(KeyCode::Char('X'), NONE);
        assert_eq!(c.title, "甲X乙");
        // 带 Ctrl 的字符不能被当成文字收下（Ctrl+C 是退出，别在这留下一堆 c）
        c.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(c.title, "甲X乙");
    }

    /// 编辑到一半按 Tab 跑掉，那一行得还原 —— 半截输入不能留在框里。
    #[test]
    fn leaving_while_editing_restores_the_field() {
        let mut c = logged_in();
        c.seed_title("原标题");
        c.handle_key(KeyCode::BackTab, NONE);
        c.handle_key(KeyCode::Tab, NONE);
        c.handle_key(KeyCode::Tab, NONE);
        c.handle_key(KeyCode::Enter, NONE);
        c.handle_key(KeyCode::Char('X'), NONE);
        assert_eq!(c.title, "原标题X");

        c.handle_key(KeyCode::Tab, NONE);
        assert!(!c.is_editing());
        assert_eq!(c.title, "原标题");
        assert_eq!(c.tab(), Tab::Stream);

        // Shift+Tab 直接收起来也一样
        c.handle_key(KeyCode::BackTab, NONE);
        c.handle_key(KeyCode::F(6), NONE);
        c.handle_key(KeyCode::Enter, NONE);
        c.handle_key(KeyCode::Char('Y'), NONE);
        assert_eq!(c.title, "原标题Y");
        c.handle_key(KeyCode::BackTab, NONE);
        assert_eq!(c.page(), Page::Main);
        assert_eq!(c.title, "原标题");
    }

    /// 提交只是「记下来」，还得说清楚没有真的发给 B 站。
    #[test]
    fn committing_says_it_has_not_been_sent_yet() {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE);
        c.handle_key(KeyCode::Tab, NONE);
        c.handle_key(KeyCode::Tab, NONE);
        c.handle_key(KeyCode::Enter, NONE);
        for ch in "新标题".chars() {
            c.handle_key(KeyCode::Char(ch), NONE);
        }
        c.handle_key(KeyCode::Enter, NONE);
        assert!(!c.is_editing());
        assert_eq!(c.title, "新标题");
        assert!(
            c.message.contains("还没提交") && c.message.contains("标题"),
            "{}",
            c.message
        );
    }

    /// 登录任务来的消息：成功收掉二维码、失败留个说法、任何一条都不能 panic。
    #[test]
    fn login_events_only_touch_the_display_state() {
        let mut c = Control::default();
        c.on_login_event(LoginEvent::Qr(
            "https://passport.bilibili.com/h5/login?qrcode_key=abc".into(),
        ));
        assert!(c.qr.len() > 10, "二维码该画出来");
        assert!(c.message.is_empty());

        c.on_login_event(LoginEvent::Hint("已扫码，请在手机上点确认".into()));
        assert_eq!(c.message, "已扫码，请在手机上点确认");

        c.on_login_event(LoginEvent::Failed("二维码已过期".into()));
        assert_eq!(c.message, "二维码已过期");
        assert!(!c.login_pending, "失败之后要能重来一次");

        c.on_login_event(LoginEvent::LoggedIn("小明 (uid 7)".into()));
        assert!(c.logged_in);
        assert_eq!(c.account, "小明 (uid 7)");
        assert!(c.qr.is_empty(), "登录成功了二维码就得收掉");
        // 这里**故意**不自己写「登录成功」：开局那次「配置里的凭据还有效」也走这个分支，
        // 真扫码成功由 apply_login 另发一句更准的 Hint。所以只查状态，不查那句话。
        assert_eq!(c.message, "二维码已过期", "LoggedIn 不该改最近一条消息");

        c.on_login_event(LoginEvent::LoggedOut("登录态失效（回车扫码）".into()));
        assert!(!c.logged_in);
        assert!(c.account.contains("登录态失效"));
    }

    /// 一行里塞了换行会把下面的内容顶掉，所以要压成一行。
    #[test]
    fn messages_never_contain_newlines() {
        let mut c = Control::default();
        c.set_message("第一行\n第二行");
        assert_eq!(c.message, "第一行 第二行");
    }

    /// 整页画一遍，只为了确认「布局没算出负数、也没 panic」；
    /// 细看内容在 ui/mod.rs 用 TestBackend 那些测试里。
    #[test]
    fn drawing_the_page_never_panics() {
        let mut c = Control::default();
        c.on_login_event(LoginEvent::Qr(
            "https://www.bilibili.com/h5/login?k=1".into(),
        ));
        for (w, h) in [(120u16, 40u16), (16, 5), (1, 1), (40, 8)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| {
                let area = f.area();
                draw(f, &c, area);
            })
            .unwrap();
        }
    }
}
