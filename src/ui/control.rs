//! 配置页（第二页）：左栏挑功能，右栏干活。
//!
//! 布局按效果图来（顶栏 4 行：边框 + 按键提示 + 最近一条消息；下面左栏 16 格宽、
//! 右栏带边框且标题是当前栏的名字）：
//!
//!     +-- 按键提示 ------------------------------+
//!     | ↑↓ 选一项  回车 执行 ...                    |
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
//!   - `↑↓` 在栏里选（账号栏选「重新扫码 / 退出登录」、直播间信息栏选字段、
//!     分区栏在树里走）；推流码栏没有可上下选的东西，就顺手拿来换栏
//!   - `←→`：只在分区栏有意义，收起 / 展开光标那一行
//!   - `回车`：账号栏执行选中那一项（重新扫码 / 退出登录 —— 退出登录先过确认层）；
//!     直播间信息栏进编辑 / 提交；分区栏选定 / 展开收起；推流码栏重查开播状态
//!   - `Esc`：取消编辑 → 收起配置页，退到弹幕页就**停住**，退出只有 Ctrl+C
//!   - `F2` / `F3` / `F6` 直接翻开配置页并跳到账号 / 分区 / 直播间信息
//!   - `F4` 开播（**先弹确认层**，确认了才真发请求）/ `F5` 下播（不用确认）。
//!     这两个跟 `Ctrl+R` 一样是全局的，两页里都能按
//!
//! 屏幕上那个确认层（居中带边框、默认落在「取消」）归**两件事**用：开播和退出登录。
//! 它俩都是「手滑一下代价太大、而且事后不好收场」的那种，所以按钮 / 文案不同、
//! 规矩完全一样（`Tab`/`←→` 选、`回车` 执行、`Esc` 取消）。
//!
//! 这里只改界面状态，不碰网络也不碰磁盘 —— 那三件事分别归 `api::login`、`api::area`
//! 和 main（写回配置也是往通道里扔一个 `Action`，自己不落盘）。

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::*;

use super::area_tree::{self, AreaTree, Row};
use crate::api::area::AreaEvent;
use crate::api::info::{self, InfoEvent, MAX_TITLE_CHARS};
use crate::api::live::{LiveAction, LiveEvent, Stream, VerifyKind};
use crate::api::login::LoginEvent;
use crate::ui::cover::Cover;
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
    /// 「推流码」那栏的 F4 / F5 这一轮接上了，提示也跟着补回来。
    pub fn hint(self) -> &'static str {
        match self {
            Tab::Account => {
                "↑↓ 选一项    回车 执行    Ctrl+R 刷新房间信息    Tab 换功能    Shift+Tab 回弹幕页    Esc 返回"
            }
            Tab::Area => {
                "↑↓ 选分区    ←→ 展开/收起    回车 确认该分区    Tab 换功能    Shift+Tab 回弹幕页"
            }
            Tab::Info => {
                "↑↓ 选一项    回车 编辑    再回车 提交    Esc 取消    Tab 换功能    Shift+Tab 回弹幕页"
            }
            Tab::Stream => {
                "F4 开播    F5 下播    回车 重查状态    Tab 换功能    Shift+Tab 回弹幕页    Esc 返回"
            }
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
///
/// 带数据的那两条（`PickArea`）没法 `Copy`：界面拿到的是「要写进配置的那一对」，
/// 而不是一个「顺便去翻一下界面状态」的口信。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 配置页吃掉了（或者在这儿没意义），谁也别再管
    Handled,
    /// 交给弹幕页的输入框（只会在弹幕页出现）
    ToMain,
    /// 这一下要开一张新二维码（界面 -> 登录任务）
    StartLogin,
    /// 这一下要拉一次分区表（界面 -> 分区任务）。
    /// 切进分区栏时手上还没有表、或者在拉失败之后按回车，都会来这一下。
    LoadAreas,
    /// 选定了开播分区：把 `area_id` / `area_name` 写回配置文件。
    /// 落盘归 main（界面碰磁盘这件事已经说死了），所以只把这一对传出去。
    PickArea { id: i64, name: String },
    /// 切进「直播间信息」栏：去 `Room/get_info` 拉一次当前标题 / 封面
    /// （标题要预填、封面要地址，那一栏自己抓不了图）。一次运行只问一次。
    LoadRoomMeta,
    /// 改标题：`POST room/v1/Room/update` 那一下交给 main 的信息任务
    SetTitle(String),
    /// 换封面：值可能是本地路径（任务那边先传图床）也可能已经是 hdslb 链接
    SetCover(String),
    /// 进「推流码」栏 / 在那一栏按回车：去看一眼开播状态（只读的 `get_info`）
    LoadLiveStatus,
    /// 开播。**只有走完确认层**（确认层里选了「开播」再回车）才会出这一条 ——
    /// 它会让直播间立刻对外可见、给粉丝推开播推送，不能顺手就发出去。
    StartLive { area_v2: i64 },
    /// 下播（不用确认）
    StopLive,
    /// 退出登录：把本地凭据清掉（内存里的 + 配置里的）+ 重启弹幕那条链路。
    ///
    /// **只有走完确认层**（确认层里选了「退出登录」再回车）才会出这一条：
    /// 清掉之后到重新扫码之前，弹幕是断的。
    Logout,
}

/// 账号栏的两个动作。
///
/// 从前这一栏只有「重新扫码」一件事，`↑↓` 就被顺手拿去换栏了。现在有两件事，
/// 换栏归 `Tab`，`↑↓` 得让给选行 —— 跟「直播间信息」栏一个样子。
const ACCOUNT_ITEMS: [&str; 2] = ["重新扫码", "退出登录"];
/// 每一项后面那句灰字说明。
///
/// 第一句写的正是那个坑：**cookie 还有效时回车不出码**（`login_loop` 会回
/// 「已登录，无需重复扫码」），于是想换账号只能拿临时配置绕（`-c /tmp/x.toml`）——
/// 第二项就是给这条路准备的。
const ACCOUNT_NOTES: [&str; 2] = ["cookie 失效了换一张", "清掉本地凭据"];
/// 「退出登录」在 `ACCOUNT_ITEMS` 里的下标。
const ACCOUNT_LOGOUT: usize = 1;

/// 确认层里「取消」的下标。两个框的按钮都是「干活的那个 + 取消」，取消都在最后，
/// 所以共用一个下标。
///
/// **默认落在「取消」上**：开播不可逆（直播间立刻对外可见、粉丝收到推送），
/// 退出登录会让弹幕断到重新扫码为止 —— 两个都是手滑一个回车代价太大的事，
/// 宁可多按一下。
const CONFIRM_CANCEL: usize = 1;

/// 开播确认层的文案，照 Go 版一字不改；拆行只是为了那个框画得下，连起来读还是那一句
/// 「开播后直播间会立刻对外可见，粉丝会收到开播推送。确定开播？」。
/// **别把整句塞成一行**：44 格宽的框里会被截断，而这句话正是要给人看清楚的。
const CONFIRM_START_TEXT: [&str; 3] = [
    "开播后直播间会立刻对外可见，",
    "粉丝会收到开播推送。",
    "确定开播？",
];

/// 退出登录确认层的文案。要说清楚**后果**：退出之后到这个账号重新扫码登录之前，
/// 弹幕是断的（凭据清了，弹幕那条 wss 也整条重启了）。
/// 同样是拆成三行 —— 44 格的框里放不下一整句。
const CONFIRM_LOGOUT_TEXT: [&str; 3] = [
    "退出登录会清掉本地保存的 Cookie，",
    "在你重新扫码登录之前，弹幕会断开。",
    "确定退出登录？",
];

/// 确认层问的是哪件事。
///
/// 两件事共用同一个框（居中带边框、`Tab`/`←→` 选按钮、`回车` 确认、`Esc` 取消、
/// 默认落在「取消」），只是标题 / 文案 / 按钮不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmKind {
    StartLive,
    Logout,
}

impl ConfirmKind {
    fn title(self) -> &'static str {
        match self {
            ConfirmKind::StartLive => " 开播确认 ",
            ConfirmKind::Logout => " 退出登录确认 ",
        }
    }

    fn text(self) -> &'static [&'static str] {
        match self {
            ConfirmKind::StartLive => &CONFIRM_START_TEXT,
            ConfirmKind::Logout => &CONFIRM_LOGOUT_TEXT,
        }
    }

    fn buttons(self) -> &'static [&'static str] {
        match self {
            ConfirmKind::StartLive => &["开播", "取消"],
            ConfirmKind::Logout => &["退出登录", "取消"],
        }
    }

    /// 取消之后顶栏那一句话。
    fn cancelled(self) -> &'static str {
        match self {
            ConfirmKind::StartLive => "已取消开播",
            ConfirmKind::Logout => "已取消退出登录",
        }
    }
}

/// 确认层。`selected` 是 `kind.buttons()` 的下标。
struct Confirm {
    kind: ConfirmKind,
    selected: usize,
}

impl Confirm {
    fn new(kind: ConfirmKind) -> Self {
        Self {
            kind,
            selected: CONFIRM_CANCEL,
        }
    }
}

/// 「推流码」栏最近一次开播状态查询的结果。
///
/// 跟 `streams`（开播成功拿到的那几路凭据）**分开存**：凭据只有开播那一下会返回一次，
/// 而「刚开播那几秒服务端还可能回未开播」—— 拿一次状态查询去冲掉手上的凭据，
/// 用户就会看着一栏空白以为自己开播失败了。
enum LiveQuery {
    /// 还没查过（进这一栏会自动查一次）
    Idle,
    /// 正在查
    Loading,
    /// 查到了：`data.live_status`（0 未开播 / 1 直播中 / 2 轮播）
    Known(i64),
    /// 没查到（网络 / 接口报错），原话留在这一栏里
    Failed(String),
}

/// 分区栏的状态。
///
/// 分区表是**异步**拉回来的（界面 -> 任务 -> 界面），所以要显式区分这四种：
/// 光看「手上有没有树」分不清「还没开始拉」和「刚拉失败」，
/// 而这两种该给用户看的东西完全不一样（一个是「正在拉…」，一个是「回车重试」）。
enum AreaState {
    /// 还没有分区表：切进这一栏会自动去拉一次
    Idle,
    /// 正在拉。这期间回车不再发请求 —— 连按几下只是把任务队列塞满
    Loading,
    Ready(AreaTree),
    /// 拉失败的那句话（原样展示），留在屏幕上，回车重试
    Failed(String),
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
    /// 账号栏选中的是哪一项（`ACCOUNT_ITEMS` 的下标）。
    ///
    /// 默认停在「重新扫码」上：这一栏进来的绝大多数人是码失效了要换一张，
    /// 而「退出登录」是个清凭据的破坏性动作，不该是回车的第一落点。
    account_idx: usize,
    /// 退出登录请求还在路上（界面 -> 会话任务）。这期间不再发第二次。
    logout_pending: bool,
    /// 「直播间信息」栏选中的是哪一行
    field_idx: usize,
    /// 正在编辑时的缓冲，`None` 表示没在编辑
    edit: Option<Edit>,
    /// 直播间信息栏第一行（这轮只显示 / 本地记着，下一步才真去改标题）
    pub title: String,
    /// 直播间信息栏第二行
    pub cover: String,
    /// 封面预览那一格要显示哪个地址（提交成功后换成本地图走完图床拿到的那一个）
    cover_url: String,
    /// 封面预览本身（解码 / 缩放 / 画半格都在 `ui/cover.rs` 里）
    cover_preview: Cover,
    /// 正在问 `Room/get_info`：这期间别再问一次
    info_meta_pending: bool,
    /// 当前标题 / 封面已经问回来过了。成功之后切栏就不再问；
    /// 失败**不算问过**（下次切进这一栏会重试，但不会变成循环打接口）。
    info_meta_loaded: bool,
    /// 分区栏：状态机 + 分区树
    area: AreaState,
    /// 配置里记着的开播分区（`area_id`）。
    ///
    /// 树是拉到分区表之后才建的，但「该展开哪个父分区、光标停在哪」全指望这一个数
    /// —— 它只在内存里（配置是启动时读的，界面拿不到配置文件）。
    saved_area_id: i64,
    /// 配置里那个分区的名字。只用在「表还没拉到」的时候告诉用户现在配的是什么。
    saved_area_name: String,
    /// 「推流码」栏最近一次开播状态查询的结果
    live: LiveQuery,
    /// 开播成功拿到的各路推流凭据。下播成功后清掉。
    streams: Vec<Stream>,
    /// OBS 联动那件事的结果（填好了 / 没填上的原话），摆在推流码栏末尾。
    /// 空 = 还没发生（关掉了联动、还没开播、或者那条任务还没回话）。
    obs_note: String,
    /// 确认层（开播 / 退出登录共用一个框）开着的时候是 `Some`
    confirm: Option<Confirm>,
    /// 开播 / 下播请求还在路上。这期间不再发第二次（连按 F4/F5 只会被顶栏那句话挡住）
    start_pending: bool,
    stop_pending: bool,
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
            account_idx: 0,
            logout_pending: false,
            field_idx: 0,
            edit: None,
            title: String::new(),
            cover: String::new(),
            cover_url: String::new(),
            cover_preview: Cover::default(),
            info_meta_pending: false,
            info_meta_loaded: false,
            area: AreaState::Idle,
            saved_area_id: 0,
            saved_area_name: String::new(),
            live: LiveQuery::Idle,
            streams: Vec::new(),
            obs_note: String::new(),
            confirm: None,
            start_pending: false,
            stop_pending: false,
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

    /// 顶栏那行按键提示。分区栏得跟着状态变：表在路上的时候没法展开收起，
    /// 拉失败那一下更得写清楚「回车重试」—— 否则屏幕上只有一句错误，
    /// 用户不知道该干什么（Go 版把这句话写在面板底部，被页面盖住根本看不见）。
    pub fn hint(&self) -> String {
        match (&self.tab, &self.area) {
            (Tab::Area, AreaState::Loading) => {
                "正在拉分区表…    Tab 换功能    Shift+Tab 回弹幕页".to_string()
            }
            (Tab::Area, AreaState::Failed(_)) => {
                "分区表没拉到：回车 重试    Tab 换功能    Shift+Tab 回弹幕页".to_string()
            }
            // 推流码栏这两条同理：状态在路上的时候说一声，没查到就把「回车重试」摆出来。
            (Tab::Stream, _) if matches!(self.live, LiveQuery::Loading) => {
                "正在查开播状态…    F4 开播    F5 下播    Tab 换功能".to_string()
            }
            (Tab::Stream, _) if matches!(self.live, LiveQuery::Failed(_)) => {
                "开播状态没查到：回车 重试    F4 开播    F5 下播    Tab 换功能".to_string()
            }
            _ => self.tab.hint().to_string(),
        }
    }

    /// 房间信息到了先拿它把「直播间信息」栏的标题填上 ——
    /// 空着不好看，而且用户第一眼就想知道现在挂的是什么标题。
    /// 只填一次：用户改过的（或者正在编辑的）别被下一轮房间信息冲掉。
    pub fn seed_title(&mut self, title: &str) {
        if self.title.is_empty() && !title.is_empty() {
            self.title = title.to_string();
        }
    }

    /// 房间信息里那个 `user_cover` 顺手喂给封面那一行和预览。
    /// 口径跟 `seed_title` 一样：**只填空的**，用户敲了一半的（或者刚改过的）别冲掉。
    ///
    /// 光有地址还画不出图 —— 像素是信息任务抓到之后从 `InfoEvent::CoverImage` 送进来的，
    /// 那一次抓取由「进这一栏时拉一次 get_info」触发（`Action::LoadRoomMeta`）。
    pub fn seed_cover(&mut self, cover: &str) {
        if cover.is_empty() {
            return;
        }
        if self.cover.is_empty() {
            self.cover = cover.to_string();
        }
        if self.cover_url.is_empty() {
            // 接口有时给的是 `//i0.hdslb.com/...`：协议相对的地址扔给 reqwest
            // 会报「relative URL without a base」，先补成 https（http 的不动）
            self.cover_url = info::absolute_image_url(cover);
        }
    }

    /// 配置里记着的开播分区，启动时喂一次（`ui/mod.rs` 从 `cfg` 拿）。
    ///
    /// 因为树要等分区表回来才建，这个值必须先在手上 —— 拉到表的那一刻就靠它
    /// 决定展开哪个父分区。**不是**「展开第一个」：Go 版就是这么写的，
    /// 结果永远展开「网游」，跟用户配置里那个分区一点关系都没有。
    pub fn seed_area(&mut self, area_id: i64, area_name: &str) {
        self.saved_area_id = area_id;
        self.saved_area_name = area_name.to_string();
    }

    /// 分区任务那边来的消息。跟登录事件一样，只动界面状态。
    pub fn on_area_event(&mut self, ev: AreaEvent) {
        match ev {
            AreaEvent::Loaded(areas) if areas.is_empty() => {
                // 接口给了一张空表：不算错误，但也没什么可选的。
                // 退回「还没有表」，回车还能再拉一次（自动拉只在 Idle 上触发，
                // 不这么摆的话这一栏就永远停在「拉到了但什么都没有」上）。
                self.area = AreaState::Idle;
                self.set_message("分区表是空的，回车 再拉一次");
            }
            AreaEvent::Loaded(areas) => {
                let n = areas.len();
                self.area = AreaState::Ready(AreaTree::new(areas, self.saved_area_id));
                if self.saved_area_id != 0 {
                    self.set_message(format!(
                        "分区表已加载，光标停在配置里的分区（共 {n} 个父分区）"
                    ));
                } else {
                    self.set_message(format!("分区表已加载：{n} 个父分区，回车 选定"));
                }
            }
            AreaEvent::Failed(err) => {
                self.area = AreaState::Failed(err.clone());
                self.set_message(format!("分区表没拉到：{err}"));
            }
            AreaEvent::Saved { name, error } => match error {
                None => self.set_message(format!("开播分区已设为 {name}")),
                // 写盘失败**不算选定失败**：这一栏已经选中了它，就是下次启动可能
                // 还得再选一次。说清楚差在哪儿，别让用户白选一遍。
                Some(e) => self.set_message(format!("分区已选中，但写进配置失败：{e}")),
            },
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
                // 退出登录那件事（或者登录态真的失效了）到这就算落地了，
                // 把「正在退出」放掉，不然以后再按「退出登录」会被自己挡住。
                self.logout_pending = false;
            }
            LoginEvent::Qr(content) => {
                self.show_qr(&content, "用哔哩哔哩 App 扫码登录");
            }
            LoginEvent::Hint(text) => self.set_message(text),
            LoginEvent::Failed(text) => {
                // 放开「正在等扫码」，不然重进账号栏也没法再试一次
                self.login_pending = false;
                self.set_message(text);
            }
        }
    }

    /// 那一下没能送进信息任务（队列满 / 任务没了）。
    ///
    /// 必须把「正在问」的标志放掉：不放的话这一栏就永远停在「问过了」的状态上，
    /// 用户切走再切回来也不会再问一次（屏幕上什么都不会发生）。
    pub fn info_request_dropped(&mut self) {
        self.info_meta_pending = false;
    }

    /// 那一下没能送进开播任务（队列满 / 任务没了）。
    ///
    /// 两个「正在发」的标志必须放掉：不放的话后面按 F4 / F5 会一直被自己挡住
    /// （顶栏只说「请求还在路上」，而那个请求根本不存在）。
    pub fn live_request_dropped(&mut self, what: &str) {
        self.start_pending = false;
        self.stop_pending = false;
        // 查状态那一下没送出去，就得把「正在查」放掉：不放的话那一栏永远停在
        // 「正在查开播状态…」，而回车还会被自己挡住（`reload_live_status` 见它
        // 是 Loading 就只说一句「还在路上」）—— 屏幕上就再也没法让它重查了。
        if matches!(self.live, LiveQuery::Loading) {
            self.live = LiveQuery::Idle;
        }
        self.set_message(format!("{what}请求没送出去（开播任务正忙），再按一次试试"));
    }

    /// OBS 联动那条链回来的那行字（填好了 / 没填上的原话）。
    ///
    /// 它**只**往推流码栏末尾加一行：开播的成败早就由 `LiveEvent::Started` 定过了，
    /// 这条消息晚几秒到，绝不能反过来把「已开播」改写成失败。
    pub fn on_obs_note(&mut self, text: impl Into<String>) {
        self.obs_note = text.into().replace('\n', " ");
    }

    /// 把一段地址画进账号栏。登录二维码和开播验证码共用这一套渲染
    /// （半格字符 + 真彩色在 `ui/qr.rs`，别在别处再拼一遍）。
    fn show_qr(&mut self, content: &str, title: &str) {
        let mut lines = vec![
            Line::from(Span::styled(
                title.to_string(),
                Style::default().fg(Color::Yellow),
            )),
            Line::from(""),
        ];
        // 编不出来也只是画一行红字说明，绝不把整个界面带走
        lines.extend(qr::lines(content));
        self.qr = lines;
    }

    /// 开播任务那边来的消息（状态 / 推流凭据 / 两种验证 / 成功失败）。
    ///
    /// 跟别的几条链一个口径：只动显示状态，失败只写顶栏那一行，
    /// **绝不 panic、绝不退出**。
    pub fn on_live_event(&mut self, ev: LiveEvent) {
        match ev {
            LiveEvent::Status { live_status } => {
                self.live = LiveQuery::Known(live_status);
                // 手上已经有凭据的时候别被状态查询冲掉顶栏那句话：刚开播那几秒
                // 服务端还可能在回「未开播」，凭据其实是好的。
                if self.streams.is_empty() {
                    self.set_message(if live_status == 1 {
                        "正在直播中"
                    } else {
                        "还没开播"
                    });
                }
            }
            LiveEvent::Started(streams) => {
                self.start_pending = false;
                self.stop_pending = false;
                self.live = LiveQuery::Known(1);
                self.streams = streams;
                // 换了一场直播，上一场 OBS 填没填上那句话就不再是这一场的了
                //（开播那条任务过几秒还会再送一条新的过来）。
                self.obs_note.clear();
                // 开播成功自动切到推流码栏（Go 版就这么做的）：开完播人最想看的就是
                // 服务器和密钥，别让他再自己按一次 Tab。
                self.tab = Tab::Stream;
                self.page = Page::Config;
                self.set_message("已开播");
            }
            LiveEvent::Verify { kind, url, message } => {
                self.start_pending = false;
                self.stop_pending = false;
                // 两种验证都把码画到**账号栏**（复用那边的渲染），并且别说成「开播失败」——
                // 扫完码再按 F4 就能开播，用户要的是二维码，不是一句错误。
                self.tab = Tab::Account;
                self.page = Page::Config;
                if url.is_empty() {
                    // 没拿到验证地址（60024 没带 qr、或者人脸认证那次 nav 也没问出 mid）：
                    // 画不出码，只能让人再试一次。
                    self.qr = vec![Line::from(Span::styled(
                        "没拿到验证地址，按 F4 再试一次",
                        Style::default().fg(Color::Yellow),
                    ))];
                    self.set_message(format!("{message}（没拿到验证地址，按 F4 再试一次）"));
                } else {
                    let title = match kind {
                        VerifyKind::Qr => "开播需要验证：扫码后在手机上确认",
                        VerifyKind::FaceAuth => "开播需要人脸认证：扫码后在手机上完成",
                    };
                    self.show_qr(&url, title);
                    self.set_message(format!("{message}，扫完再按 F4"));
                }
            }
            LiveEvent::Stopped => {
                self.start_pending = false;
                self.stop_pending = false;
                self.live = LiveQuery::Known(0);
                // 下播了，凭据就没用了（服务端下一次开播会换一组），清掉。
                self.streams.clear();
                self.obs_note.clear();
                self.set_message("已下播，推流码已清掉");
            }
            LiveEvent::Failed { action, message } => {
                self.start_pending = false;
                self.stop_pending = false;
                let what = match action {
                    LiveAction::Status => {
                        // 状态没查到只影响这一栏：让它自己写一句话，别处不受影响。
                        self.live = LiveQuery::Failed(message.clone());
                        "读取开播状态失败"
                    }
                    LiveAction::Start => "开播失败",
                    LiveAction::Stop => "下播失败",
                };
                self.set_message(format!("{what}：{message}"));
            }
        }
    }

    /// 信息任务那边来的消息（`Room/get_info` 的结果 / 改标题改封面的结果 / 封面图本身）。
    ///
    /// 跟别的几条链一样：只动显示状态，失败只写顶栏那一行，**绝不 panic、绝不退出**。
    pub fn on_info_event(&mut self, ev: InfoEvent) {
        match ev {
            InfoEvent::Meta { title, cover } => {
                self.info_meta_pending = false;
                self.info_meta_loaded = true;
                // 慢网下用户可能已经敲上了，别把人家打的字冲掉（Go 版的 loadTitle 同理）
                if self.title.is_empty() && !title.is_empty() {
                    self.title = title;
                }
                let cover = info::absolute_image_url(&cover);
                if self.cover.is_empty() && !cover.is_empty() {
                    self.cover = cover.clone();
                }
                if self.cover_url.is_empty() {
                    self.cover_url = cover;
                }
            }
            InfoEvent::MetaFailed(err) => {
                // 读不到**不等于改不了**：输入框照用，人可以直接敲。所以只留一句话，
                // 而且不置 `info_meta_loaded` —— 下次再切进这一栏会重试一次。
                self.info_meta_pending = false;
                self.set_message(format!("读不到当前标题 / 封面，可以直接输入：{err}"));
            }
            InfoEvent::Title { title, error } => {
                // 值按用户填的那个留着（失败也不还原）：他多半想在那个基础上改
                self.title = title;
                match error {
                    None => self.set_message("标题已提交，生效要等几秒"),
                    Some(e) => self.set_message(format!("改标题失败：{e}")),
                }
            }
            InfoEvent::Cover { cover, error } => match error {
                None => {
                    if !cover.is_empty() {
                        // 真正生效的是这个 hdslb 地址（本地路径走完图床就变成它了），
                        // 字段里留着用户填的那个路径，预览换成新的那张
                        self.cover_url = cover;
                    }
                    self.set_message("封面已提交，生效要等几秒");
                }
                Some(e) => self.set_message(format!("换封面失败：{e}")),
            },
            // 图解码不开也只是预览那一格写一句话（`Cover` 自己兜着），界面别处不受影响
            InfoEvent::CoverImage { url, bytes } => self.cover_preview.set_bytes(&url, &bytes),
            InfoEvent::CoverImageFailed { url, error } => {
                self.cover_preview.fail(&url, &error);
            }
        }
    }

    /// 喂一个按键。返回这一下有没有被配置页吃掉。
    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Action {
        // 确认层开着的时候，除了它自己那几个键什么都不认。
        // 这一条**必须排在最前面**：`Tab` / `Esc` 漏到下面就会变成「换栏」「收起配置页」，
        // 屏上那个框还开着、其实已经没人管它了（Go 版在 `Esc` 那儿特意写过同一个坑）。
        if self.confirm.is_some() {
            return self.confirm_key(code);
        }

        // F2 / F3 / F6 是「直接翻开配置页并跳到某一栏」，F4 / F5 是开播 / 下播，
        // 在**哪一页**都该管用（跟 Ctrl+R / Ctrl+C 一样），所以排在
        // 「现在是不是已经开着」前面。
        match code {
            KeyCode::F(2) => return self.open_tab(Tab::Account),
            KeyCode::F(3) => return self.open_tab(Tab::Area),
            KeyCode::F(6) => return self.open_tab(Tab::Info),
            KeyCode::F(4) => return self.begin_start_live(),
            KeyCode::F(5) => return self.begin_stop_live(),
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
                    // 提交那一下要真把动作带出去（改标题 / 换封面都归信息任务）
                    return self.commit_edit();
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
                // 账号栏：执行选中那一项（重新扫码 / 退出登录）。
                // 「退出登录」先弹确认层，不是按了就清。
                Tab::Account => return self.account_enter(),
                Tab::Info => self.start_edit(),
                // 分区栏：停在子分区上 = 选定它（把 area_id / area_name 传出去写回配置）；
                // 停在父分区或「全部分区」上 = 展开 / 收起，跟 ←→ 一个意思。
                Tab::Area => return self.area_enter(),
                // 推流码栏的回车 = 重查一次开播状态（拉失败之后顶栏写的就是这个）。
                Tab::Stream => return self.reload_live_status(),
            },
            KeyCode::Up | KeyCode::Down => {
                let d = if code == KeyCode::Up { -1 } else { 1 };
                match self.tab {
                    // 账号栏：在「重新扫码 / 退出登录」两项里选
                    Tab::Account => self.move_account(d),
                    // 直播间信息栏：上下是在栏里选一项
                    Tab::Info => self.move_field(d),
                    // 分区栏：在**可见的行**里上下走。父分区也是可停的一行 ——
                    // Go 版只让光标停在「可选节点」上，父分区不可选就被整段跳过去，
                    // 用户报的是「分区没法选」。
                    Tab::Area => self.area_move(d),
                    // 推流码栏没有可上下选的东西，就顺手拿来换栏
                    Tab::Stream => return self.cycle_tab(d),
                }
            }
            // 分区栏的 ←→：收起 / 展开光标那一行。
            // 叶子上什么也不做（`AreaTree::toggle` 说这键不归它），
            // 但**不留任何副作用** —— 别顺手把光标挪走，用户按一下就少一行字。
            KeyCode::Left | KeyCode::Right if self.tab == Tab::Area => {
                self.area_toggle(code == KeyCode::Right)
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
    /// 账号栏：没登录、手上又还没有码，就直接把码摆出来，别让人先去找回车
    /// （Go 版 `fillTab` 是同一个思路）。
    ///
    /// 分区栏：手上还没有分区表就直接去拉一次 —— 一进来右栏就有东西，
    /// 而不是一句「自己按个键去拉」。**拉失败过的不在这儿自动重试**：
    /// 网络一抖的时候来回切栏会变成「每切一次打一次接口」，
    /// 重试是回车的事（顶栏那行提示会写）。
    fn enter_tab(&mut self) -> Action {
        if self.tab == Tab::Account && !self.logged_in && !self.login_pending && self.qr.is_empty() {
            self.login_pending = true;
            return Action::StartLogin;
        }
        if self.tab == Tab::Area && matches!(self.area, AreaState::Idle) {
            self.area = AreaState::Loading;
            return Action::LoadAreas;
        }
        // 直播间信息栏：没问过就自己去问一次（标题要预填、封面预览要地址）。
        // 「别覆盖用户敲了一半的」是**收到回复时**才判断的事（`on_info_event`），
        // 所以这儿不等字段空着 —— 主页那条房间信息链虽然也会预填标题，但它是 30 秒一轮的
        // 只读链路，不该指望它替这一栏把封面抓回来。
        if self.tab == Tab::Info && !self.info_meta_pending && !self.info_meta_loaded {
            self.info_meta_pending = true;
            return Action::LoadRoomMeta;
        }
        // 推流码栏：没查过状态就自己查一次 —— 一进来就知道开没开播，
        // 而不是摆一个空框让人自己找键。加载中 / 失败**不在这儿**自动重试
        //（网络一抖的时候来回切栏会变成「每切一次打一次接口」，重试是回车的事）。
        if self.tab == Tab::Stream && matches!(self.live, LiveQuery::Idle) {
            self.live = LiveQuery::Loading;
            return Action::LoadLiveStatus;
        }
        Action::Handled
    }

    /// 分区栏上下走一格（只在表到手之后才有意义）。
    fn area_move(&mut self, d: i32) {
        if let AreaState::Ready(tree) = &mut self.area {
            tree.move_cursor(d);
        }
    }

    /// 分区栏收起 / 展开光标那一行。表还没到、或者光标在叶子上，就什么也不做。
    fn area_toggle(&mut self, expand: bool) {
        if let AreaState::Ready(tree) = &mut self.area {
            tree.toggle(expand);
        }
    }

    /// 分区栏的回车。
    ///
    /// - 表还没到手（没拉过 / 拉回一张空表）：再拉一次
    /// - 拉失败：重试（顶栏那行提示写的就是这个）
    /// - 正在拉：说一声，不重复发请求
    /// - 停在子分区上：**选定它**，把 `(area_id, 名字)` 交给 main 写进配置
    /// - 停在父分区 / 「全部分区」上：展开 / 收起
    fn area_enter(&mut self) -> Action {
        // 先把「选中的是哪一对」算出来再动 `self`：`tree` 借着 `self.area`，
        // 借还在手上的时候 `self.set_message` 是编译不过的。
        let picked = match &mut self.area {
            AreaState::Ready(tree) => tree.select(),
            _ => None,
        };
        if let Some((id, name)) = picked {
            // 顺手记下来：分区表要是被重拉一次（比如按 F3），光标还得停在这个分区上。
            self.saved_area_id = id;
            self.saved_area_name = name.clone();
            self.set_message(format!("开播分区已设为 {name}（正在写配置）"));
            return Action::PickArea { id, name };
        }

        // 光标明明停在子分区上，`select()` 却什么都没给：接口没给 id（解析出来是 0）。
        // 说一句，别让人对着一个没反应的键连按 —— 真出这种情况多半是接口改了字段名。
        if matches!(&self.area, AreaState::Ready(t) if matches!(t.cursor(), Row::Sub(..))) {
            self.set_message("这个分区接口没给 id，选不了（回车 重拉一次分区表）");
            return Action::Handled;
        }

        if matches!(self.area, AreaState::Loading) {
            self.set_message("分区表还在路上…");
            return Action::Handled;
        }
        if matches!(self.area, AreaState::Ready(_)) {
            // 剩下能走到这儿的只有父分区 / 「全部分区」：回车跟 ←→ 一样只管展开收起
            if let AreaState::Ready(tree) = &mut self.area {
                tree.flip();
            }
            return Action::Handled;
        }
        // Idle（还没拉过 / 拉回一张空表）和 Failed（没拉到）：回车就是再拉一次
        self.area = AreaState::Loading;
        self.set_message("正在拉分区表…");
        Action::LoadAreas
    }

    /// 收起配置页，回弹幕页。退到弹幕页就停住 ——
    /// 弹幕页上再按 Esc 什么都不做，更不会退出。
    fn close(&mut self) {
        self.cancel_edit();
        self.page = Page::Main;
    }

    /// 重查一次开播状态（进这一栏时、以及在那一栏按回车）。
    fn reload_live_status(&mut self) -> Action {
        if matches!(self.live, LiveQuery::Loading) {
            self.set_message("开播状态还在路上…");
            return Action::Handled;
        }
        self.live = LiveQuery::Loading;
        self.set_message("正在查开播状态…");
        Action::LoadLiveStatus
    }

    /// `F4`：开播。**先弹确认层**，绝不把请求直接发出去。
    ///
    /// 这一条排在「现在哪一页」前面 —— 跟 `Ctrl+R` 一样是全局键，弹幕页上按也要管用
    /// （Go 版是全局 capture）。顺手把配置页翻开：确认框得画在第二页上。
    fn begin_start_live(&mut self) -> Action {
        self.page = Page::Config;
        if !self.logged_in {
            self.set_message("还没登录：先到「账号」栏扫码登录（回车 重新扫码）");
            return Action::Handled;
        }
        if self.saved_area_id <= 0 {
            // 分区是开播的必填项（`area_v2`），空着发出去只会换回一句听不懂的错。
            // 所以在**发请求之前**就说清楚去哪儿选。
            self.set_message("还没选开播分区：先到「分区」栏选一个（F3 直达）");
            return Action::Handled;
        }
        if self.start_pending {
            self.set_message("开播请求还在路上，等一下…");
            return Action::Handled;
        }
        if !self.streams.is_empty() || matches!(self.live, LiveQuery::Known(1)) {
            // 已经在播了：服务端再收一次 startLive 只会回一句「已经在直播中」，
            // 不如在这儿拦住，顺便告诉用户下播按哪个键。
            self.set_message("已经开播了（下播按 F5）");
            return Action::Handled;
        }
        self.confirm = Some(Confirm::new(ConfirmKind::StartLive));
        Action::Handled
    }

    /// `F5`：下播。**不用确认** —— 它不会让直播间突然对外可见，
    /// 误按的后果是这一场断了，再按一次 F4 就能回来。
    fn begin_stop_live(&mut self) -> Action {
        self.page = Page::Config;
        if !self.logged_in {
            self.set_message("还没登录：先到「账号」栏扫码登录（回车 重新扫码）");
            return Action::Handled;
        }
        if self.stop_pending {
            self.set_message("下播请求还在路上，等一下…");
            return Action::Handled;
        }
        if self.streams.is_empty() && matches!(self.live, LiveQuery::Known(s) if s != 1) {
            // 只有「确定没在播」才拦。状态还没查过时照发 —— F5 不该因为
            // 「没先看一眼状态」就什么都不做。
            self.set_message("现在没在直播（开播按 F4）");
            return Action::Handled;
        }
        self.stop_pending = true;
        self.set_message("正在下播…");
        Action::StopLive
    }

    /// 账号栏的回车：执行选中那一项。
    ///
    /// 「重新扫码」就是**以前那个行为**，一个字都没改：已经登录且凭据还灵的时候
    /// `login_loop` 会回一句「已登录，无需重复扫码」，不出新码 —— 想换账号得先
    /// 「退出登录」，不然屏幕上什么都不会发生，用户只会以为这键坏了。
    ///
    /// 「退出登录」**先过确认层**：清掉凭据之后弹幕就断了，还得重新扫码，
    /// 跟开播一样属于「手滑一下代价太大」的事，不能按了就干。
    fn account_enter(&mut self) -> Action {
        if self.account_idx != ACCOUNT_LOGOUT {
            return Action::StartLogin;
        }
        if self.logout_pending {
            self.set_message("退出登录还在进行中，等一下…");
            return Action::Handled;
        }
        self.confirm = Some(Confirm::new(ConfirmKind::Logout));
        Action::Handled
    }

    /// 账号栏 `↑↓` 选一项（两项，到头绕回去，跟「直播间信息」栏一个口径）。
    fn move_account(&mut self, d: i32) {
        let n = ACCOUNT_ITEMS.len() as i32;
        self.account_idx = ((self.account_idx as i32 + d).rem_euclid(n)) as usize;
    }

    /// 那一下没能送进会话任务（队列满 / 任务没了）。
    ///
    /// 跟 `live_request_dropped` 一个道理：不放掉「正在退出」的话，之后再按
    /// 「退出登录」只会被自己挡住（顶栏说「还在进行中」，而那个请求根本不存在）。
    pub fn logout_dropped(&mut self) {
        self.logout_pending = false;
        self.set_message("退出登录请求没送出去（会话任务正忙），再按一次试试");
    }

    /// 确认层里的按键：`Tab` / `←→` 换按钮，`回车` 按下去，`Esc` 取消。
    /// 这一套开播和退出登录共用，只有按钮和文案不同。
    fn confirm_key(&mut self, code: KeyCode) -> Action {
        // 框不在手上时按到这儿（理论上到不了）：按「取消」处理，什么都别干。
        let kind = self.confirm.as_ref().map_or(ConfirmKind::StartLive, |c| c.kind);
        let last = kind.buttons().len() - 1;
        match code {
            KeyCode::Enter => {
                let selected = self.confirm.take().map_or(CONFIRM_CANCEL, |c| c.selected);
                if selected == CONFIRM_CANCEL {
                    self.set_message(kind.cancelled());
                    return Action::Handled;
                }
                match kind {
                    ConfirmKind::StartLive => {
                        self.start_pending = true;
                        self.set_message("正在开播…");
                        Action::StartLive {
                            area_v2: self.saved_area_id,
                        }
                    }
                    ConfirmKind::Logout => {
                        self.logout_pending = true;
                        self.set_message("正在退出登录…");
                        Action::Logout
                    }
                }
            }
            KeyCode::Esc => {
                self.confirm = None;
                self.set_message(kind.cancelled());
                Action::Handled
            }
            KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                let Some(c) = self.confirm.as_mut() else {
                    return Action::Handled;
                };
                // Tab 在两头之间绕圈；←→ 到头就停住（只有两个按钮的框这样最好猜）。
                c.selected = match code {
                    KeyCode::Tab => (c.selected + 1) % (last + 1),
                    KeyCode::Right => (c.selected + 1).min(last),
                    _ => c.selected.saturating_sub(1),
                };
                Action::Handled
            }
            _ => Action::Handled,
        }
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

    /// 提交当前这一行：值留在字段里，真正那一下交给 main 的信息任务。
    ///
    /// 标题在**这儿**再按字符数拦一道（输入时已经拦过：预填、手滑都可能塞进来），
    /// 服务端超长只回一句谁也看不懂的错。封面留空 = 不改动，**不是错误**。
    fn commit_edit(&mut self) -> Action {
        let Some(e) = self.edit.take() else {
            return Action::Handled;
        };
        let value = e.text();
        let idx = self.field_idx;
        self.set_field(idx, value.clone());

        if idx == 0 {
            if let Err(err) = info::check_title(&value) {
                self.set_message(err.to_string());
                return Action::Handled;
            }
            self.set_message(format!("正在提交{}…", self.field_name(idx)));
            return Action::SetTitle(value);
        }
        if value.is_empty() {
            // 把封面那一行清空多半就是「算了，不换」：照 Go 版的样子说一声就完事
            self.set_message("封面留空，没有改动");
            return Action::Handled;
        }
        self.set_message(format!("正在处理封面：{value}"));
        Action::SetCover(value)
    }

    fn edit_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // 标题上限 40 个字：满了就不再收字（Go 版用的是 InputField 的 SetAcceptanceFunc，
        // 现象一样 —— 按键没反应，而不是等服务端回一句看不懂的错）。
        // 判断放在最前面：借用 `self.edit` 的时候没法再调 `self.set_message`。
        let typing = matches!(code, KeyCode::Char(_))
            && !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if typing
            && self.field_idx == 0
            && self
                .edit
                .as_ref()
                .is_some_and(|e| e.buf.len() >= MAX_TITLE_CHARS)
        {
            self.set_message(format!("标题最多 {MAX_TITLE_CHARS} 个字，已经满了"));
            return;
        }

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
        Line::from(Span::styled(c.hint(), Style::default().fg(Color::Yellow))),
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
        Tab::Info => draw_info(f, c, inner),
        Tab::Area => draw_area(f, c, inner),
        Tab::Stream => draw_stream(f, c, inner),
    }

    // 确认层画在最上层：它得盖住底下的栏。先 `Clear` 再画框 —— 不清的话
    // 框里会透出下面那层的文字（半句标题混在确认文案里就没法看了）。
    if let Some(confirm) = &c.confirm {
        draw_confirm(f, area, confirm);
    }
}

/// 确认层：屏幕正中的一个带边框的框、三行文案、一排按钮。
///
/// 自己在 ratatui 里搭（Go 版用 tview 的 Modal）：需要的东西就这一个框，
/// 为它铺一层 Pages 不值当。选中的按钮反色，另一个是灰的。
fn draw_confirm(f: &mut Frame, area: Rect, confirm: &Confirm) {
    // 宽 44 格够放下最长那行文案；终端比它还窄就跟着缩，缩到画不下干脆不画
    //（画一半的框比没有框更让人看不明白）。
    let width = 44.min(area.width);
    let height = 7.min(area.height);
    if width < 12 || height < 5 {
        return;
    }
    let text = confirm.kind.text();
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );

    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(confirm.kind.title())
        .style(Style::default().fg(Color::Yellow));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    // 文案几行由 `ConfirmKind::text` 说了算，别在这儿写死下标 ——
    // 少一行是个空框，多一行会被框裁掉（退出登录那句后果就白写了）。
    let mut lines: Vec<Line<'static>> =
        text.iter().map(|l| Line::from(l.to_string())).collect();
    lines.push(Line::from(""));
    lines.push(button_line(confirm.kind, confirm.selected));
    f.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        inner,
    );
}

/// `[ 开播 ]  [ 取消 ]`（退出登录那个框是 `[ 退出登录 ]  [ 取消 ]`），
/// 选中的那个反色 —— 不反色就看不出回车会按到哪个。
fn button_line(kind: ConfirmKind, selected: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, label) in kind.buttons().iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let style = if i == selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(format!("[ {label} ]"), style));
    }
    Line::from(spans)
}

/// 分区栏：一棵两级树（「全部分区 → 父分区 → 子分区」）。
///
/// 可见行和窗口都自己算（`ui/area_tree.rs`）：真接口今天给 12 个父分区、
/// 四百多个子分区，交给 `Paragraph` 自己往下截的话，光标一走到底下就从屏幕上
/// 消失了 —— 用户只会得出「按键失灵」这个结论。
fn draw_area(f: &mut Frame, c: &Control, area: Rect) {
    match &c.area {
        AreaState::Idle => draw_note(
            f,
            area,
            &with_saved("还没有分区表：回车 拉一次", &c.saved_area_name),
        ),
        AreaState::Loading => draw_note(f, area, &with_saved("正在拉分区表…", &c.saved_area_name)),
        AreaState::Failed(err) => f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    format!("分区表没拉到：{err}"),
                    Style::default().fg(Color::Red),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "回车 重试",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(Span::styled(
                    "Tab 换栏，Esc 返回 —— 别的都不受影响",
                    Style::default().fg(Color::DarkGray),
                )),
            ]),
            area,
        ),
        AreaState::Ready(tree) => {
            let rows = tree.visible();
            let labels = tree.lines();
            let cursor = tree.cursor_index();
            let from = area_tree::window(rows.len(), cursor, area.height as usize);
            let lines: Vec<Line<'static>> = rows
                .iter()
                .zip(labels)
                .skip(from)
                .enumerate()
                .map(|(i, (row, label))| {
                    // 选中的那一行整个反色：Go 版靠 tview 的高亮，
                    // 这里不反色的话屏幕上就没有任何东西指出「选的是谁」。
                    let style = if from + i == cursor {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else if matches!(row, Row::Sub(..)) {
                        Style::default().fg(Color::Gray)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    Line::from(Span::styled(label, style))
                })
                .collect();
            f.render_widget(Paragraph::new(lines), area);
        }
    }
}

/// 一句话占一格（分区栏的「还没有表 / 正在拉」就是这种）。
fn draw_note(f: &mut Frame, area: Rect, note: &str) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            note.to_string(),
            Style::default().fg(Color::DarkGray),
        ))),
        area,
    );
}

/// 表还没到手的时候顺带说一句配置里记着的是什么 —— 树的默认状态就靠它，
/// 光看一句「正在拉…」用户没法确认自己上次选的分区还在不在。
fn with_saved(note: &str, saved: &str) -> String {
    if saved.is_empty() {
        note.to_string()
    } else {
        format!("{note}（配置里记着的是 {saved}）")
    }
}

fn draw_account(f: &mut Frame, c: &Control, area: Rect) {
    // 账号那行占两行（Go 版的 accountView 也是 2），接着是两个可选项，
    // 剩下全留给二维码
    let [who, items, code] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(ACCOUNT_ITEMS.len() as u16),
        Constraint::Fill(1),
    ])
    .areas(area);
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
    f.render_widget(Paragraph::new(account_items(c)), items);
    if !c.qr.is_empty() {
        // 逐行居中：每行宽度一样，画出来就是正的。
        // 码比这一格宽的话 ratatui 会截断，跟 Go 版一样（终端太小就扫不动，认了）。
        f.render_widget(
            Paragraph::new(c.qr.clone()).alignment(Alignment::Center),
            code,
        );
    }
}

/// 账号栏那两个可选项：选中的那行带 `▸`（跟「直播间信息」栏一个套路），
/// 后面跟一句灰字说明它干什么。
///
/// 不反色、只靠 `▸` + 黄色：这一栏下面往往摆着一张二维码，一条反色的横杠会把
/// 视线从码上抢走（分区树那种整行反色，是因为那一栏本来就全是文字）。
fn account_items(c: &Control) -> Vec<Line<'static>> {
    ACCOUNT_ITEMS
        .iter()
        .zip(ACCOUNT_NOTES)
        .enumerate()
        .map(|(i, (label, note))| {
            let selected = c.account_idx == i;
            Line::from(vec![
                Span::styled(
                    if selected {
                        format!("▸ {label}")
                    } else {
                        format!("  {label}")
                    },
                    Style::default().fg(if selected {
                        Color::Yellow
                    } else {
                        Color::Gray
                    }),
                ),
                Span::raw("  "),
                Span::styled(note, Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect()
}

/// 「推流码」栏：手上有凭据就把服务器 / 密钥摆出来，否则按最近一次状态查询写一句话。
///
/// 密钥近百字符，**一行一个、不折行**（Go 版的教训：地址和密钥挤在一行里，
/// 在终端里选中复制出来是断的）。`Paragraph` 不设 `wrap` 就是截断 ——
/// 终端太窄时密钥尾部会被切掉，这是认了的取舍：折行会把一串密钥断成两行，更难复制。
fn draw_stream(f: &mut Frame, c: &Control, area: Rect) {
    if !c.streams.is_empty() {
        f.render_widget(Paragraph::new(stream_lines(&c.streams, &c.obs_note)), area);
        return;
    }
    let lines: Vec<Line<'static>> = match &c.live {
        LiveQuery::Idle | LiveQuery::Loading => vec![Line::from(Span::styled(
            "正在查开播状态…",
            Style::default().fg(Color::DarkGray),
        ))],
        LiveQuery::Known(1) => vec![
            Line::from(Span::styled("正在直播中", Style::default().fg(Color::Green))),
            Line::from(""),
            Line::from("推流地址与推流码只有 F4 开播那一下会返回一次。"),
            Line::from("要拿到就把这一场下播（F5）再开一次（F4）。"),
        ],
        LiveQuery::Known(_) => vec![
            Line::from(Span::styled(
                "还没开播，按 F4 开播",
                Style::default().fg(Color::Gray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "开播前先在「分区」栏选好开播分区；开播成功后这里会显示推流地址与推流码。",
                Style::default().fg(Color::DarkGray),
            )),
        ],
        LiveQuery::Failed(err) => vec![
            Line::from(Span::styled(
                format!("读不到开播状态：{err}"),
                Style::default().fg(Color::Red),
            )),
            Line::from(""),
            Line::from(Span::styled("回车 重试", Style::default().fg(Color::Yellow))),
            Line::from(Span::styled(
                "别的都不受影响：开播 / 下播照样能按",
                Style::default().fg(Color::DarkGray),
            )),
        ],
    };
    f.render_widget(Paragraph::new(lines), area);
}

/// 一路凭据几行字：类型 / 服务器 / 密钥 / 完整 URL。
/// 每个值**单独占一行**，就是为了让人能整行选中复制。
///
/// `obs_note` 非空时（OBS 联动那件事回话了）在最末尾多一行 —— 就是「多一行字」，
/// 不占凭据的位置，也不改上面那几行的样子。
fn stream_lines(streams: &[Stream], obs_note: &str) -> Vec<Line<'static>> {
    let label = |text: &'static str| {
        Line::from(Span::styled(text, Style::default().fg(Color::DarkGray)))
    };
    let mut lines = vec![
        Line::from(Span::styled(
            "推流地址与推流码",
            Style::default().fg(Color::Yellow),
        )),
        Line::from(""),
    ];
    for (i, s) in streams.iter().enumerate() {
        let mut head = vec![Span::styled(s.kind.clone(), Style::default().fg(Color::White))];
        if i == 0 {
            head.push(Span::styled(
                "（OBS 填这组）",
                Style::default().fg(Color::Yellow),
            ));
        }
        lines.push(Line::from(head));
        lines.push(label("服务器"));
        lines.push(Line::from(s.address.clone()));
        lines.push(label("密钥"));
        lines.push(Line::from(s.key.clone()));
        lines.push(label("完整 URL"));
        lines.push(Line::from(s.full_url.clone()));
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        "直播间已对外可见；下播按 F5",
        Style::default().fg(Color::DarkGray),
    )));
    if !obs_note.is_empty() {
        lines.push(Line::from(Span::styled(
            obs_note.to_string(),
            Style::default().fg(Color::Yellow),
        )));
    }
    lines
}

/// 「直播间信息」栏：上面两行字段（选中的那行带 ▸），中间两行说明，
/// 剩下的高度全给封面预览 —— 图按这一格的实际大小现采样，终端一拉伸画面自己就跟着变。
fn draw_info(f: &mut Frame, c: &Control, area: Rect) {
    let [fields, note, preview] =
        Layout::vertical([Constraint::Length(2), Constraint::Length(2), Constraint::Fill(1)])
            .areas(area);
    f.render_widget(Paragraph::new(info_fields(c)), fields);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "标题上限 40 字；封面填本地图片路径（~ 开头也行）或 .hdslb.com 链接，留空表示不改",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "本地图会先传 B 站图床，再把地址写进直播间（两步，慢一点）",
                Style::default().fg(Color::DarkGray),
            )),
        ]),
        note,
    );

    let block = Block::bordered().title(" 当前封面 ");
    let inner = block.inner(preview);
    f.render_widget(block, preview);
    // 终端太小的时候这一格是 0 行：边框画出来就够了，别再往下算
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let lines = c.cover_preview.lines(inner.width, inner.height, &c.cover_url);
    if !lines.is_empty() {
        f.render_widget(Paragraph::new(lines), inner);
    }
}

fn info_fields(c: &Control) -> Vec<Line<'static>> {
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
    use crate::api::area::{AreaEvent, ParentArea, SubArea};
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
            let action = c.handle_key(KeyCode::Tab, NONE);
            assert_eq!(c.tab(), want);
            // 分区栏一进去就自己去拉表（跟账号栏顺手要一张码是同一个「需要什么显示什么」）；
            // 直播间信息栏同理，去拉一次当前标题 / 封面；推流码栏去看一眼开播状态。
            // 别的栏换过去只是空换。
            match want {
                Tab::Area => assert_eq!(action, Action::LoadAreas),
                Tab::Info => assert_eq!(action, Action::LoadRoomMeta),
                Tab::Stream => assert_eq!(action, Action::LoadLiveStatus),
                _ => assert_eq!(action, Action::Handled),
            }
        }
        assert_eq!(c.page(), Page::Config, "换栏又不是收起来");
    }

    /// 每一栏的 ↑↓ 各是各的意思：账号栏在两项里选、分区栏在分区树里走、
    /// 直播间信息栏选字段；**推流码栏**没有可上下选的东西，就顺手拿来换栏。
    #[test]
    fn up_down_means_different_things_per_tab() {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE); // 账号
        assert_eq!(c.tab(), Tab::Account);
        assert_eq!(c.account_idx, 0);
        c.handle_key(KeyCode::Up, NONE);
        assert_eq!(c.account_idx, ACCOUNT_LOGOUT, "↑ 选到「退出登录」");
        assert_eq!(c.tab(), Tab::Account, "账号栏的 ↑↓ 是选行，不换栏");
        c.handle_key(KeyCode::Down, NONE);
        assert_eq!(c.account_idx, 0, "两项，到头绕回去");

        c.handle_key(KeyCode::Tab, NONE); // 分区
        c.handle_key(KeyCode::Up, NONE);
        assert_eq!(c.tab(), Tab::Area, "分区栏的 ↑↓ 是走树，不换栏");

        c.handle_key(KeyCode::Tab, NONE); // 直播间信息
        assert_eq!(c.field_idx, 0);
        c.handle_key(KeyCode::Down, NONE);
        assert_eq!(c.field_idx, 1);
        assert_eq!(c.tab(), Tab::Info, "选字段不换栏");
        c.handle_key(KeyCode::Down, NONE);
        assert_eq!(c.field_idx, 0, "两项，到头绕回去");

        c.handle_key(KeyCode::Tab, NONE); // 推流码
        assert_eq!(c.tab(), Tab::Stream);
        c.handle_key(KeyCode::Up, NONE);
        assert_eq!(c.tab(), Tab::Info, "推流码栏的 ↑↓ 还是换栏");
        c.handle_key(KeyCode::Tab, NONE); // 回到推流码
        assert_eq!(c.tab(), Tab::Stream);
        c.handle_key(KeyCode::Down, NONE);
        assert_eq!(c.tab(), Tab::Account, "↓ 从最后一栏绕回第一栏");
        assert_eq!(c.account_idx, 0, "换栏不许动账号栏里选中的那一项");
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
        assert_eq!(
            c.handle_key(KeyCode::F(3), NONE),
            Action::LoadAreas,
            "跳到分区栏时顺手就把表拉上（别让人进来再自己找键）"
        );
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

    /// 账号栏的两个可选项：`回车` 执行选中那一项。
    /// 「重新扫码」还是老行为（已登录时不出码那句话保留），
    /// 「退出登录」先弹确认层 —— 不是按了就清。
    #[test]
    fn enter_runs_the_selected_account_item() {
        let mut c = logged_in();
        assert_eq!(c.handle_key(KeyCode::BackTab, NONE), Action::Handled);
        assert_eq!(c.tab(), Tab::Account);

        // 默认停在第一项：重新扫码就是以前那一下
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::StartLogin);
        assert!(c.confirm.is_none(), "重新扫码不用确认");

        // 第二项：退出登录 —— 先弹确认层，弹出的那一下什么都别干
        c.handle_key(KeyCode::Down, NONE);
        assert_eq!(c.account_idx, ACCOUNT_LOGOUT);
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::Handled);
        assert_eq!(
            c.confirm.as_ref().map(|m| m.kind),
            Some(ConfirmKind::Logout),
            "退出登录得先问一遍"
        );
        assert!(!c.logout_pending, "还没确认，什么都不该发生");
    }

    /// 退出登录的确认层跟开播那个共用一套规矩：默认落在「取消」、`Esc` 不执行、
    /// 默认那一下 `回车` 也不执行 —— 选到「退出登录」再回车才真清。
    #[test]
    fn logging_out_asks_first_and_nothing_happens_until_confirmed() {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE);
        c.handle_key(KeyCode::Down, NONE);
        c.handle_key(KeyCode::Enter, NONE);
        assert_eq!(
            c.confirm.as_ref().map(|m| m.selected),
            Some(CONFIRM_CANCEL),
            "默认落在「取消」上"
        );

        // Esc：只收确认层，凭据一动不动
        assert_eq!(c.handle_key(KeyCode::Esc, NONE), Action::Handled);
        assert!(c.confirm.is_none());
        assert!(!c.logout_pending);
        assert!(c.message.contains("已取消"), "{}", c.message);
        assert!(c.logged_in, "取消之后登录态不该变");

        // 默认那一下回车 = 取消：不许冒出 Logout
        c.handle_key(KeyCode::Enter, NONE); // 再弹一次
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::Handled);
        assert!(c.confirm.is_none());
        assert!(!c.logout_pending);
        assert!(c.logged_in);

        // 选到「退出登录」再回车：这才真执行
        c.handle_key(KeyCode::Enter, NONE); // 再弹一次
        assert_eq!(c.handle_key(KeyCode::Left, NONE), Action::Handled);
        assert_eq!(c.confirm.as_ref().map(|m| m.selected), Some(0));
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::Logout);
        assert!(c.logout_pending);
        assert!(c.message.contains("正在退出登录"), "{}", c.message);
        assert!(c.confirm.is_none(), "确认层得收掉，不然它会一直盖着");

        // 请求还在路上：再按不重复发
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::Handled);
        assert!(c.message.contains("还在进行中"), "{}", c.message);

        // 那一下没送进会话任务（队列满）时得能重来一次
        c.logout_dropped();
        assert!(!c.logout_pending);
        assert!(c.message.contains("没送出去"), "{}", c.message);
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::Handled);
        assert_eq!(
            c.confirm.as_ref().map(|m| m.kind),
            Some(ConfirmKind::Logout),
            "被放掉之后要能重新来一次"
        );
    }

    /// 退出登录的确认层一样得吃下别的键：漏到下面就成了「换栏」「收起来」，
    /// 屏上那个框还开着、其实已经没人管它了。
    #[test]
    fn the_logout_confirm_layer_swallows_everything_else() {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE);
        c.handle_key(KeyCode::Down, NONE);
        c.handle_key(KeyCode::Enter, NONE);
        let tab = c.tab();
        for code in [
            KeyCode::BackTab,
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::F(3),
            KeyCode::F(4),
            KeyCode::F(5),
            KeyCode::Char('x'),
        ] {
            assert_eq!(c.handle_key(code, NONE), Action::Handled, "{code:?}");
        }
        assert_eq!(
            c.confirm.as_ref().map(|m| m.kind),
            Some(ConfirmKind::Logout),
            "这些键都不该把它关掉"
        );
        assert_eq!(c.page(), Page::Config, "也不该把配置页收掉");
        assert_eq!(c.tab(), tab, "也不该换栏");
    }

    /// 退出登录落地之后界面上那两样：账号行回到「未登录（回车扫码）」、
    /// `logged_in` 为假（顶栏那句「去重新扫码」由会话任务说）。
    #[test]
    fn after_logout_the_account_line_says_not_logged_in() {
        let mut c = logged_in();
        c.on_login_event(LoginEvent::LoggedOut("未登录（回车扫码）".into()));
        assert!(!c.logged_in);
        assert_eq!(c.account, "未登录（回车扫码）");
        assert!(!c.logout_pending, "「正在退出」的旗子得放掉");
        assert!(!c.login_pending);

        // 退完之后账号栏照旧能再来一遍：没登录、手上又没有码，翻开就自动要一张
        assert_eq!(c.handle_key(KeyCode::BackTab, NONE), Action::StartLogin);
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

    /// 站在「直播间信息」栏上的 Control（已经登录，免得进账号栏顺带要一张码）。
    /// 直接摆过去：这一组测的是字段那一套按键和事件，不是换栏。
    fn info_control() -> Control {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE); // 翻开配置页
        c.tab = Tab::Info;
        c
    }

    /// 提交那一下：值留在字段里、动作交给信息任务，顶栏立刻说一句「正在提交…」。
    #[test]
    fn committing_hands_the_value_to_the_task() {
        let mut c = info_control();
        c.handle_key(KeyCode::Enter, NONE);
        for ch in "新标题".chars() {
            c.handle_key(KeyCode::Char(ch), NONE);
        }
        assert_eq!(
            c.handle_key(KeyCode::Enter, NONE),
            Action::SetTitle("新标题".to_string())
        );
        assert!(!c.is_editing());
        assert_eq!(c.title, "新标题");
        assert!(c.message.contains("正在提交标题"), "{}", c.message);
    }

    /// 两行字段的编辑那套：进编辑 → 改动 → **Esc 还原**、
    /// 进编辑 → 改动 → **回车提交**，两条路都把值断言一遍。
    #[test]
    fn the_two_info_fields_commit_or_cancel() {
        let mut c = info_control();
        c.seed_title("原标题");

        // 标题：改动之后 Esc，值要回到进编辑之前的样子
        c.handle_key(KeyCode::Enter, NONE);
        assert!(c.is_editing());
        for ch in "改过的".chars() {
            c.handle_key(KeyCode::Char(ch), NONE);
        }
        assert_eq!(c.title, "原标题改过的", "边打边写回那一行");
        c.handle_key(KeyCode::Esc, NONE);
        assert!(!c.is_editing());
        assert_eq!(c.title, "原标题", "Esc 要还原");

        // 标题：再改一次，这回回车提交 —— 动作带着新值出去
        c.handle_key(KeyCode::Enter, NONE);
        for ch in "改过的".chars() {
            c.handle_key(KeyCode::Char(ch), NONE);
        }
        assert_eq!(
            c.handle_key(KeyCode::Enter, NONE),
            Action::SetTitle("原标题改过的".to_string())
        );
        assert_eq!(c.title, "原标题改过的");

        // 封面那一行同理：↓ 选到它，回车进编辑
        c.handle_key(KeyCode::Down, NONE);
        assert_eq!(c.field_idx, 1);
        c.handle_key(KeyCode::Enter, NONE);
        for ch in "~/图片/新封面.png".chars() {
            c.handle_key(KeyCode::Char(ch), NONE);
        }
        assert_eq!(
            c.handle_key(KeyCode::Esc, NONE),
            Action::Handled,
            "Esc 只是取消编辑，不是提交"
        );
        assert_eq!(c.cover, "", "封面本来就空，取消之后还是空");

        c.handle_key(KeyCode::Enter, NONE);
        for ch in "~/图片/新封面.png".chars() {
            c.handle_key(KeyCode::Char(ch), NONE);
        }
        assert_eq!(c.cover, "~/图片/新封面.png");
        assert_eq!(
            c.handle_key(KeyCode::Enter, NONE),
            Action::SetCover("~/图片/新封面.png".to_string()),
            "本地路径原样交给任务（~ 由那边展开，界面不碰文件系统）"
        );
        assert_eq!(c.cover, "~/图片/新封面.png", "提交之后那一行还是用户填的");
    }

    /// 标题上限 **40 个字符**：第 41 个按键就不收（中文和 emoji 一样按字符算，不按字节）。
    /// 服务端超了只回一句看不懂的错，所以要在按键这一层拦住（Go 版是 SetAcceptanceFunc）。
    #[test]
    fn the_title_field_refuses_the_forty_first_character() {
        for ch in ['汉', '😀'] {
            let mut c = info_control();
            c.handle_key(KeyCode::Enter, NONE);
            for _ in 0..MAX_TITLE_CHARS {
                c.handle_key(KeyCode::Char(ch), NONE);
            }
            assert_eq!(c.title.chars().count(), MAX_TITLE_CHARS);

            c.handle_key(KeyCode::Char(ch), NONE);
            assert_eq!(
                c.title.chars().count(),
                MAX_TITLE_CHARS,
                "第 41 个「{ch}」不该进去"
            );
            assert!(c.message.contains("最多 40"), "{}", c.message);

            // 正好 40 个字是能提交的
            assert_eq!(
                c.handle_key(KeyCode::Enter, NONE),
                Action::SetTitle(ch.to_string().repeat(MAX_TITLE_CHARS))
            );

            // 退一个之后又能再进一个（不是把这一行锁死）
            let mut c = info_control();
            c.handle_key(KeyCode::Enter, NONE);
            for _ in 0..MAX_TITLE_CHARS {
                c.handle_key(KeyCode::Char(ch), NONE);
            }
            c.handle_key(KeyCode::Backspace, NONE);
            assert_eq!(c.title.chars().count(), MAX_TITLE_CHARS - 1);
            c.handle_key(KeyCode::Char(ch), NONE);
            assert_eq!(c.title.chars().count(), MAX_TITLE_CHARS);
        }
    }

    /// 空标题不许提交（说一句话就完事，别等服务端拒绝）；封面留空 = 不改动，**不是错误**。
    #[test]
    fn an_empty_title_is_refused_and_an_empty_cover_is_a_no_op() {
        let mut c = info_control();
        c.handle_key(KeyCode::Enter, NONE);
        assert_eq!(
            c.handle_key(KeyCode::Enter, NONE),
            Action::Handled,
            "空标题一个请求都不该发"
        );
        assert!(c.message.contains("不能为空"), "{}", c.message);

        // 预填进来的超长标题（从别处拷的）也得在提交这一下拦住
        c.seed_title(&"汉".repeat(MAX_TITLE_CHARS + 1));
        c.handle_key(KeyCode::Enter, NONE);
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::Handled);
        assert!(c.message.contains("40"), "{}", c.message);

        let mut c = info_control();
        c.handle_key(KeyCode::Down, NONE);
        c.handle_key(KeyCode::Enter, NONE);
        assert_eq!(
            c.handle_key(KeyCode::Enter, NONE),
            Action::Handled,
            "封面留空 = 不改动，不是错误"
        );
        assert!(c.message.contains("没有改动"), "{}", c.message);
    }

    /// 进这一栏时手上还没有当前标题：自动去 `Room/get_info` 拉一次。
    /// 拉到过就不再问（别每切一次栏打一次接口）；拉失败过的那次不算拉到，会再试。
    #[test]
    fn entering_the_info_tab_asks_for_the_current_meta_once() {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE); // 账号栏
        c.handle_key(KeyCode::Tab, NONE); // 分区栏（顺手拉表，不管它）
        assert_eq!(
            c.handle_key(KeyCode::Tab, NONE),
            Action::LoadRoomMeta,
            "切进信息栏就该去拉当前标题 / 封面"
        );
        assert_eq!(
            c.handle_key(KeyCode::F(6), NONE),
            Action::Handled,
            "还在路上：别再打一次接口"
        );

        c.on_info_event(InfoEvent::Meta {
            title: "当前标题".into(),
            cover: "//i0.hdslb.com/bfs/x.png".into(),
        });
        assert_eq!(c.title, "当前标题");
        assert_eq!(
            c.cover, "https://i0.hdslb.com/bfs/x.png",
            "协议相对的地址要补成 https"
        );
        c.handle_key(KeyCode::BackTab, NONE); // 回弹幕页
        assert_eq!(
            c.handle_key(KeyCode::F(6), NONE),
            Action::Handled,
            "拿到过就别再问"
        );

        // 拉失败：说一句「可以直接输入」，而且**不算拿到过**
        let mut c = info_control();
        c.handle_key(KeyCode::BackTab, NONE);
        assert_eq!(c.handle_key(KeyCode::F(6), NONE), Action::LoadRoomMeta);
        c.on_info_event(InfoEvent::MetaFailed("网络不可达".into()));
        assert!(c.message.contains("可以直接输入"), "{}", c.message);
        assert!(c.message.contains("网络不可达"), "{}", c.message);
        c.handle_key(KeyCode::BackTab, NONE);
        assert_eq!(
            c.handle_key(KeyCode::F(6), NONE),
            Action::LoadRoomMeta,
            "失败之后切回来再试一次"
        );
        c.on_info_event(InfoEvent::Meta {
            title: "标题".into(),
            cover: String::new(),
        });
        c.handle_key(KeyCode::BackTab, NONE);
        assert_eq!(c.handle_key(KeyCode::F(6), NONE), Action::Handled);
    }

    /// 「那一下没送进信息任务」时要把「正在问」的标志放掉：
    /// 不放的话这一栏永远停在「问过了」，切走再切回来也不问（屏幕上什么都不发生）。
    #[test]
    fn a_dropped_info_request_can_be_asked_again() {
        let mut c = info_control();
        c.handle_key(KeyCode::BackTab, NONE);
        assert_eq!(c.handle_key(KeyCode::F(6), NONE), Action::LoadRoomMeta);
        c.handle_key(KeyCode::BackTab, NONE);
        assert_eq!(
            c.handle_key(KeyCode::F(6), NONE),
            Action::Handled,
            "还在路上，别重复问"
        );

        c.info_request_dropped(); // 界面那边 try_send 失败时走的就是这一下
        c.handle_key(KeyCode::BackTab, NONE);
        assert_eq!(
            c.handle_key(KeyCode::F(6), NONE),
            Action::LoadRoomMeta,
            "下没送出去就不该记成「问过了」"
        );
    }

    /// 信息任务来的消息只动显示状态：不覆盖用户敲了一半的字、
    /// 成功失败都只写顶栏那一行、封面图哪怕是垃圾字节也不许把界面带走。
    #[test]
    fn info_events_only_touch_the_display_state() {
        let mut c = info_control();

        // 慢网下用户已经敲上了，回复来了要把人家打的字留着
        c.handle_key(KeyCode::Enter, NONE);
        for ch in "我自己的".chars() {
            c.handle_key(KeyCode::Char(ch), NONE);
        }
        c.on_info_event(InfoEvent::Meta {
            title: "服务端的原标题".into(),
            cover: "https://i0.hdslb.com/bfs/x.png".into(),
        });
        assert_eq!(c.title, "我自己的", "用户敲了一半的字不能被冲掉");
        assert_eq!(c.cover, "https://i0.hdslb.com/bfs/x.png", "空的那一行该填上");

        c.on_info_event(InfoEvent::Title {
            title: "新标题".into(),
            error: None,
        });
        assert_eq!(c.title, "新标题");
        assert!(c.message.contains("已提交"), "{}", c.message);

        // 失败也只是一句话：值留着让人接着改，绝不 panic、绝不退出
        c.on_info_event(InfoEvent::Title {
            title: "新标题".into(),
            error: Some("-111 csrf 校验失败".into()),
        });
        assert!(c.message.contains("改标题失败"), "{}", c.message);
        assert!(c.message.contains("csrf 校验失败"), "{}", c.message);
        assert_eq!(c.title, "新标题");

        c.on_info_event(InfoEvent::Cover {
            cover: "https://i0.hdslb.com/bfs/new.png".into(),
            error: None,
        });
        assert!(c.message.contains("封面已提交"), "{}", c.message);
        c.on_info_event(InfoEvent::Cover {
            cover: "https://i0.hdslb.com/bfs/new.png".into(),
            error: Some("100402 图片地址不合法".into()),
        });
        assert!(c.message.contains("100402"), "{}", c.message);

        // 封面图：垃圾字节 -> 预览那一格写一句话；抓失败也是
        c.on_info_event(InfoEvent::CoverImage {
            url: "https://i0.hdslb.com/bfs/new.png".into(),
            bytes: b"nope".to_vec(),
        });
        c.on_info_event(InfoEvent::CoverImageFailed {
            url: "https://i0.hdslb.com/bfs/new.png".into(),
            error: "HTTP 404".into(),
        });
        assert_eq!(c.page(), Page::Config, "这些消息不许改页面状态");
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

    // ------------------------------------------------------------- 分区栏

    /// 三个父分区，配过的那个在第二个里 —— 用两个以上父分区才验得出
    /// 「只展开配置里那一个」（Go 版在这儿永远展开「网游」）。
    fn areas() -> Vec<ParentArea> {
        vec![
            ParentArea {
                name: "网游".into(),
                list: vec![
                    SubArea {
                        id: 86,
                        name: "英雄联盟".into(),
                    },
                    SubArea {
                        id: 329,
                        name: "无畏契约".into(),
                    },
                ],
            },
            ParentArea {
                name: "虚拟主播".into(),
                list: vec![SubArea {
                    id: 371,
                    name: "虚拟日常".into(),
                }],
            },
            ParentArea {
                name: "购物".into(),
                list: vec![SubArea {
                    id: 557,
                    name: "好物分享".into(),
                }],
            },
        ]
    }

    /// 站到分区栏上、表也灌好了。`saved` 就是配置里记着的 `area_id`。
    fn area_control(areas: Vec<ParentArea>, saved: i64) -> Control {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE); // 翻开配置页
        c.seed_area(saved, "");
        // 直接摆过去：这一组测的是分区栏的按键，不是换栏
        c.tab = Tab::Area;
        c.on_area_event(AreaEvent::Loaded(areas));
        c
    }

    fn tree(c: &Control) -> &AreaTree {
        match &c.area {
            AreaState::Ready(t) => t,
            _ => panic!("这一栏现在还没有分区树"),
        }
    }

    /// 切进分区栏时手上还没有表，就**自动**去拉一次（别让人进来再自己找键）；
    /// 表在飞的期间不许重复发请求。
    #[test]
    fn entering_the_area_tab_asks_for_the_table_once() {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE); // 账号栏
        assert_eq!(
            c.handle_key(KeyCode::Tab, NONE),
            Action::LoadAreas,
            "一进分区栏就该去拉表"
        );
        // 绕一圈回来：还在等表，不该再打一次接口
        for _ in 0..4 {
            c.handle_key(KeyCode::Tab, NONE);
        }
        assert_eq!(c.tab(), Tab::Area);
        assert_eq!(c.handle_key(KeyCode::F(3), NONE), Action::Handled);

        // 表到手之后再切回来，也不用重新拉
        c.on_area_event(AreaEvent::Loaded(areas()));
        assert_eq!(c.handle_key(KeyCode::F(3), NONE), Action::Handled);
    }

    /// 没配过（`area_id == 0`）：光标在「全部分区」上，所有父分区收起，
    /// 而且 ↓ 的第一下就停在父分区上（Go 版会整段跳过去，用户报的是「分区没法选」）。
    #[test]
    fn an_unconfigured_area_tab_starts_on_all_areas() {
        let mut c = area_control(areas(), 0);
        assert_eq!(tree(&c).cursor(), Row::All);
        assert!(
            !tree(&c).visible().iter().any(|r| matches!(r, Row::Sub(..))),
            "没配过就不该有子分区露在外面"
        );

        assert_eq!(c.handle_key(KeyCode::Down, NONE), Action::Handled);
        assert_eq!(tree(&c).cursor(), Row::Parent(0), "第一条 ↓ 停在父分区上");
        assert_eq!(c.handle_key(KeyCode::Up, NONE), Action::Handled);
        assert_eq!(tree(&c).cursor(), Row::All);
        c.handle_key(KeyCode::Up, NONE);
        assert_eq!(tree(&c).cursor(), Row::All, "顶上再往上还是顶上");
    }

    /// 配过：只展开**它所在的那个**父分区，光标就落在那个子分区上。
    #[test]
    fn a_saved_area_is_expanded_in_place() {
        let mut c = area_control(areas(), 371); // 371 在「虚拟主播」里，不是第一个父分区
        assert_eq!(tree(&c).cursor(), Row::Sub(1, 0));
        assert!(tree(&c).is_expanded(1));
        assert!(
            !tree(&c).is_expanded(0) && !tree(&c).is_expanded(2),
            "别的父分区必须全是收起的"
        );
        // 光标是从「它的父分区」走下来的：↑ 回到父分区，↓ 再回到它身上
        c.handle_key(KeyCode::Up, NONE);
        assert_eq!(tree(&c).cursor(), Row::Parent(1));
        c.handle_key(KeyCode::Down, NONE);
        assert_eq!(tree(&c).cursor(), Row::Sub(1, 0));
    }

    /// `←→` 只在父分区上收起 / 展开；叶子上什么都不动（更不许把光标顺手挪走）。
    #[test]
    fn left_and_right_toggle_only_parents() {
        let mut c = area_control(areas(), 0);
        c.handle_key(KeyCode::Down, NONE); // 网游
        assert_eq!(c.handle_key(KeyCode::Right, NONE), Action::Handled);
        assert!(tree(&c).is_expanded(0), "→ 展开");
        c.handle_key(KeyCode::Down, NONE);
        assert_eq!(tree(&c).cursor(), Row::Sub(0, 0));

        let before = tree(&c).visible();
        assert_eq!(c.handle_key(KeyCode::Left, NONE), Action::Handled);
        assert_eq!(c.handle_key(KeyCode::Right, NONE), Action::Handled);
        assert_eq!(tree(&c).visible(), before, "叶子上 ←→ 什么也不做");
        assert_eq!(tree(&c).cursor(), Row::Sub(0, 0));

        c.handle_key(KeyCode::Up, NONE);
        assert_eq!(c.handle_key(KeyCode::Left, NONE), Action::Handled);
        assert!(!tree(&c).is_expanded(0), "← 收起");
    }

    /// 回车：父分区（和「全部分区」）上展开 / 收起，子分区上**选定**它。
    #[test]
    fn enter_picks_a_sub_area_and_toggles_a_parent() {
        let mut c = area_control(areas(), 0);
        c.handle_key(KeyCode::Down, NONE); // 网游
        assert_eq!(
            c.handle_key(KeyCode::Enter, NONE),
            Action::Handled,
            "父分区上回车 = 展开"
        );
        assert!(tree(&c).is_expanded(0));
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::Handled);
        assert!(!tree(&c).is_expanded(0), "再回车就收起来");

        c.handle_key(KeyCode::Right, NONE);
        c.handle_key(KeyCode::Down, NONE);
        assert_eq!(tree(&c).cursor(), Row::Sub(0, 0));
        assert_eq!(
            c.handle_key(KeyCode::Enter, NONE),
            Action::PickArea {
                id: 86,
                name: "网游/英雄联盟".into()
            }
        );
        assert!(
            c.message.contains("英雄联盟") && c.message.contains("写配置"),
            "选定之后要说一句：{}",
            c.message
        );
        assert_eq!(c.saved_area_id, 86, "选定之后重拉表也得停在这个分区上");
    }

    /// 接口没给 id（解析出来是 0）的时候，回车要说一句 ——
    /// 那种情况下选定是选不了的，别让人对着一个没反应的键连按。
    #[test]
    fn enter_says_so_when_a_sub_area_has_no_id() {
        let mut c = area_control(
            vec![ParentArea {
                name: "网游".into(),
                list: vec![SubArea {
                    id: 0,
                    name: "英雄联盟".into(),
                }],
            }],
            0,
        );
        c.handle_key(KeyCode::Down, NONE);
        c.handle_key(KeyCode::Right, NONE);
        c.handle_key(KeyCode::Down, NONE);
        assert_eq!(tree(&c).cursor(), Row::Sub(0, 0));
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::Handled);
        assert!(c.message.contains("没给 id"), "{}", c.message);
    }

    /// 「全部分区」那一行也是个正经节点：回车 / ←→ 能把它收起来。
    #[test]
    fn the_all_areas_row_can_be_collapsed() {
        let mut c = area_control(areas(), 0);
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::Handled);
        assert_eq!(tree(&c).visible(), vec![Row::All]);
        assert_eq!(c.handle_key(KeyCode::Right, NONE), Action::Handled);
        assert_eq!(tree(&c).visible().len(), 4, "再展开就是「全部分区」+ 三个父分区");
    }

    /// 分区任务那边来的消息：只动显示状态，绝不 panic。
    #[test]
    fn area_events_only_touch_the_display_state() {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE); // 翻开配置页
        c.tab = Tab::Area;

        c.on_area_event(AreaEvent::Failed("请求被拦截".into()));
        assert!(c.message.contains("请求被拦截"), "{}", c.message);
        assert!(c.hint().contains("回车"), "得告诉人按回车重试：{}", c.hint());
        assert_eq!(
            c.handle_key(KeyCode::Enter, NONE),
            Action::LoadAreas,
            "拉失败之后回车 = 重试"
        );
        // 没有树的时候方向键也不能 panic、也不能把键让给别处
        assert_eq!(c.handle_key(KeyCode::Down, NONE), Action::Handled);
        assert_eq!(c.handle_key(KeyCode::Left, NONE), Action::Handled);

        c.on_area_event(AreaEvent::Loaded(Vec::new()));
        assert!(c.message.contains("空"), "{}", c.message);
        assert_eq!(
            c.handle_key(KeyCode::Enter, NONE),
            Action::LoadAreas,
            "给了一张空表也得能回车再拉"
        );

        c.on_area_event(AreaEvent::Saved {
            name: "虚拟主播/虚拟日常".into(),
            error: None,
        });
        assert!(c.message.contains("虚拟主播/虚拟日常"), "{}", c.message);
        c.on_area_event(AreaEvent::Saved {
            name: "虚拟主播/虚拟日常".into(),
            error: Some("磁盘满了".into()),
        });
        assert!(
            c.message.contains("磁盘满了") && c.message.contains("已选中"),
            "落盘失败也得说清楚「这一栏是选上了」：{}",
            c.message
        );
    }

    /// 顶栏那行按键提示跟着分区栏的状态走。
    #[test]
    fn the_area_hint_follows_the_state() {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE);
        c.tab = Tab::Area;
        assert_eq!(c.hint(), Tab::Area.hint(), "还没开始拉就是常规提示");
        c.area = AreaState::Loading;
        assert!(c.hint().contains("正在拉"), "{}", c.hint());
        c.area = AreaState::Failed("网络不可达".into());
        assert!(c.hint().contains("回车 重试"), "{}", c.hint());
    }

    /// 画一帧，按行返回（文字, 这一行有没有反色格）。
    /// 只看文字验不了「选中的那一行一眼就能看出来」。
    fn screen(c: &Control, w: u16, h: u16) -> Vec<(String, bool)> {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            let area = f.area();
            draw(f, c, area);
        })
        .unwrap();
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                let mut text = String::new();
                let mut reversed = false;
                for x in 0..buf.area.width {
                    if let Some(cell) = buf.cell((x, y)) {
                        text.push_str(cell.symbol());
                        reversed |= cell.modifier.contains(Modifier::REVERSED);
                    }
                }
                (text, reversed)
            })
            .collect()
    }

    /// 缓冲区里宽字符（汉字）会额外占一格、留下一个空格，
    /// 所以断言前两边都先把空白去掉再比。
    fn flat(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// 选中的那一行反色，而且整屏只有它一条 —— 两条反色的话用户不知道选的是谁。
    #[test]
    fn the_selected_area_row_is_the_only_highlighted_one() {
        let c = area_control(areas(), 371);
        let rows = screen(&c, 80, 24);
        let hit = rows
            .iter()
            .find(|(text, _)| flat(text).contains("虚拟日常"))
            .expect("选中那一行得画出来");
        assert!(hit.1, "选中的行要反色：\n{rows:#?}");
        assert_eq!(rows.iter().filter(|(_, r)| *r).count(), 1, "只该有一条选中");
    }

    /// 表比屏幕长：光标走到底下时窗口得跟着走，选中那行不能跑出屏幕
    /// （不自己算窗口的话，用户按 ↑↓ 只会得出「按键失灵」）。
    #[test]
    fn walking_down_a_long_list_keeps_the_cursor_row_on_screen() {
        let many: Vec<ParentArea> = (0..20)
            .map(|i| ParentArea {
                name: format!("父{i}"),
                list: Vec::new(),
            })
            .collect();
        let mut c = area_control(many, 0);
        for _ in 0..20 {
            c.handle_key(KeyCode::Down, NONE);
        }
        assert_eq!(tree(&c).cursor(), Row::Parent(19));
        let rows = screen(&c, 60, 12);
        assert!(
            rows.iter().any(|(t, r)| *r && flat(t).contains("父19")),
            "选中的行得留在屏幕上：\n{rows:#?}"
        );
        assert!(
            !rows.iter().any(|(t, _)| flat(t).contains("父0")),
            "窗口该跟着往下滚：\n{rows:#?}"
        );
    }

    /// 一棵真树在各种小终端尺寸下画一遍，只为了确认不 panic
    /// （右栏被挤成 0 行时窗口函数也不许越界）。
    #[test]
    fn drawing_a_real_area_tree_never_panics() {
        let c = area_control(areas(), 371);
        for (w, h) in [(120u16, 40u16), (16, 5), (1, 1), (40, 8)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| {
                let area = f.area();
                draw(f, &c, area);
            })
            .unwrap();
        }
    }

    // ------------------------------------------------------- 开播 / 下播

    /// 登录过、配置里也选好了分区的那种状态（开播的两个前提）。
    fn live_control() -> Control {
        let mut c = logged_in();
        c.seed_area(371, "虚拟主播/虚拟日常");
        c
    }

    fn stream() -> Stream {
        Stream {
            kind: "rtmp-1".into(),
            protocol: "rtmp".into(),
            address: "rtmp://live-push.bilivideo.com/live-bvc".into(),
            key: "?streamname=abc".into(),
            full_url: "rtmp://live-push.bilivideo.com/live-bvc?streamname=abc".into(),
        }
    }

    /// 一屏的字拼起来（画一帧再读回来），只看「有没有那句话」时用它。
    fn text_of(c: &Control, w: u16, h: u16) -> String {
        flat(
            &screen(c, w, h)
                .iter()
                .map(|(t, _)| t.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    /// F4 的确认层：默认落在「取消」上（手滑一个回车不该把直播间送出去），
    /// Tab 换按钮、回车按选中的那个、Esc 取消。**弹幕页上按也管用**。
    #[test]
    fn the_start_confirm_layer_defaults_to_cancel() {
        let mut c = live_control();
        assert_eq!(c.page(), Page::Main);
        assert_eq!(c.handle_key(KeyCode::F(4), NONE), Action::Handled);
        assert_eq!(c.page(), Page::Config, "F4 是全局键，顺便把配置页翻开");
        assert_eq!(
            c.confirm.as_ref().map(|m| m.selected),
            Some(CONFIRM_CANCEL),
            "默认选「取消」"
        );

        // 默认那一下回车 = 取消：不许冒出 StartLive
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::Handled);
        assert!(c.confirm.is_none(), "回车之后确认层要收起来");
        assert!(c.message.contains("已取消"), "{}", c.message);
        assert!(!c.start_pending);

        // Tab 换到「开播」再回车：这才真的发请求（带上配置里那个分区）
        c.handle_key(KeyCode::F(4), NONE);
        assert_eq!(c.handle_key(KeyCode::Tab, NONE), Action::Handled);
        assert_eq!(
            c.handle_key(KeyCode::Enter, NONE),
            Action::StartLive { area_v2: 371 }
        );
        assert!(c.start_pending);
        assert!(c.message.contains("正在开播"), "{}", c.message);
        assert!(c.confirm.is_none(), "确认层得收掉，不然它会一直盖着");

        // ←→ 也能选；Esc 只收确认层，不许把配置页一起收掉
        c.on_live_event(LiveEvent::Failed {
            action: LiveAction::Start,
            message: "x".into(),
        });
        c.handle_key(KeyCode::F(4), NONE);
        assert_eq!(c.handle_key(KeyCode::Left, NONE), Action::Handled);
        assert_eq!(c.confirm.as_ref().map(|m| m.selected), Some(0), "← 选到「开播」");
        assert_eq!(c.handle_key(KeyCode::Right, NONE), Action::Handled);
        assert_eq!(c.confirm.as_ref().map(|m| m.selected), Some(1));
        assert_eq!(c.handle_key(KeyCode::Esc, NONE), Action::Handled);
        assert!(c.confirm.is_none());
        assert_eq!(c.page(), Page::Config, "Esc 只该收确认层");
        assert!(c.message.contains("已取消"), "{}", c.message);
    }

    /// 确认层开着的时候别的键都得被它吃掉：漏到下面就成了「换栏」「收起配置页」，
    /// 屏上那个框还开着、其实已经没人管它了（Go 版在 Esc 那儿踩过同一个坑）。
    #[test]
    fn the_confirm_layer_swallows_everything_else() {
        let mut c = live_control();
        c.handle_key(KeyCode::F(4), NONE);
        let tab = c.tab();
        for code in [
            KeyCode::BackTab,
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::F(3),
            KeyCode::F(5),
            KeyCode::Char('x'),
        ] {
            assert_eq!(c.handle_key(code, NONE), Action::Handled, "{code:?}");
        }
        assert!(c.confirm.is_some(), "这些键都不该把它关掉");
        assert_eq!(c.page(), Page::Config, "也不该把配置页收掉");
        assert_eq!(c.tab(), tab, "也不该换栏");
    }

    /// 没选分区 / 没登录就按 F4：说清楚去哪儿办，**连确认层都不弹**
    /// （一个注定要失败的请求不该先让用户确认一遍）。
    #[test]
    fn f4_without_an_area_or_login_says_what_to_do_first() {
        let mut c = logged_in(); // 没 seed_area
        assert_eq!(c.handle_key(KeyCode::F(4), NONE), Action::Handled);
        assert!(c.confirm.is_none(), "没分区就不该弹确认层");
        assert!(c.message.contains("分区"), "{}", c.message);
        assert!(!c.start_pending);

        let mut c = Control::default(); // 没登录，但分区选好了
        c.seed_area(371, "虚拟主播/虚拟日常");
        assert_eq!(c.handle_key(KeyCode::F(4), NONE), Action::Handled);
        assert!(c.confirm.is_none());
        assert!(c.message.contains("登录"), "{}", c.message);
    }

    /// 已经在播（手上有凭据，或者状态说在播）：F4 不该再弹一次确认 ——
    /// 服务端再收一次 startLive 只会回一句「已经在直播中」。
    #[test]
    fn f4_when_already_live_does_not_ask_to_confirm_again() {
        let mut c = live_control();
        c.on_live_event(LiveEvent::Started(vec![stream()]));
        assert_eq!(c.handle_key(KeyCode::F(4), NONE), Action::Handled);
        assert!(c.confirm.is_none());
        assert!(c.message.contains("已经开播"), "{}", c.message);

        let mut c = live_control();
        c.on_live_event(LiveEvent::Status { live_status: 1 });
        c.handle_key(KeyCode::F(4), NONE);
        assert!(c.confirm.is_none(), "状态说在播，就别再确认一遍了");
        assert!(c.message.contains("已经开播"), "{}", c.message);
    }

    /// F5 下播：**不用确认**，弹幕页上按也行；路上不重复发；确定没在播时只写一句话。
    #[test]
    fn f5_stops_without_any_confirmation() {
        let mut c = live_control();
        assert_eq!(c.handle_key(KeyCode::F(5), NONE), Action::StopLive);
        assert_eq!(c.page(), Page::Config);
        assert!(c.confirm.is_none(), "下播不弹确认层");
        assert!(c.stop_pending);
        assert!(c.message.contains("正在下播"), "{}", c.message);

        // 请求还在路上：再按不重复发
        assert_eq!(c.handle_key(KeyCode::F(5), NONE), Action::Handled);
        assert!(c.message.contains("还在路上"), "{}", c.message);

        // 状态说没在播（手上也没有凭据）：拦下来，别发一个没意义的请求
        let mut c = live_control();
        c.on_live_event(LiveEvent::Status { live_status: 0 });
        assert_eq!(c.handle_key(KeyCode::F(5), NONE), Action::Handled);
        assert!(c.message.contains("没在直播"), "{}", c.message);
        assert!(!c.stop_pending);
    }

    /// 那一下没送进开播任务时要把「正在发」放掉：不放的话以后按 F4 / F5
    /// 永远被自己挡住（顶栏只说「还在路上」，而那个请求根本不存在）。
    #[test]
    fn a_dropped_live_request_can_be_tried_again() {
        let mut c = live_control();
        c.handle_key(KeyCode::F(4), NONE);
        c.handle_key(KeyCode::Tab, NONE);
        assert_eq!(
            c.handle_key(KeyCode::Enter, NONE),
            Action::StartLive { area_v2: 371 }
        );
        // 路上再按：被挡
        assert_eq!(c.handle_key(KeyCode::F(4), NONE), Action::Handled);
        assert!(c.confirm.is_none());

        c.live_request_dropped("开播");
        assert!(!c.start_pending);
        assert!(c.message.contains("没送出去"), "{}", c.message);
        c.handle_key(KeyCode::F(4), NONE);
        assert!(c.confirm.is_some(), "放掉之后要能重新来一次");
    }

    /// 查状态那一下没送出去也得能重来：不然那一栏永远停在「正在查开播状态…」，
    /// 按回车还被自己挡住（屏幕上再也没有别的路能让它重查）。
    #[test]
    fn a_dropped_status_query_can_be_asked_again() {
        let mut c = live_control();
        c.handle_key(KeyCode::BackTab, NONE);
        c.tab = Tab::Stream;
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::LoadLiveStatus);
        assert!(matches!(c.live, LiveQuery::Loading));

        c.live_request_dropped("查状态");
        assert!(matches!(c.live, LiveQuery::Idle), "要把「正在查」放掉");
        assert_eq!(
            c.handle_key(KeyCode::Enter, NONE),
            Action::LoadLiveStatus,
            "放掉之后回车能再查一次"
        );
    }

    /// 进「推流码」栏去看一眼开播状态（只读）；还在查的时候绕回来不再问，
    /// 查到过之后也不重问 —— 跟分区栏 / 信息栏一个口径。
    #[test]
    fn entering_the_stream_tab_asks_for_the_status_once() {
        let mut c = live_control();
        c.handle_key(KeyCode::BackTab, NONE); // 账号栏
        assert_eq!(c.handle_key(KeyCode::Tab, NONE), Action::LoadAreas);
        assert_eq!(c.handle_key(KeyCode::Tab, NONE), Action::LoadRoomMeta);
        assert_eq!(c.handle_key(KeyCode::Tab, NONE), Action::LoadLiveStatus);
        assert_eq!(c.tab(), Tab::Stream);

        // 还在查：绕一圈回来不再问
        for _ in 0..4 {
            c.handle_key(KeyCode::Tab, NONE);
        }
        assert_eq!(c.tab(), Tab::Stream);

        // 状态到了之后切走再切回来，也不自动重问
        c.on_live_event(LiveEvent::Status { live_status: 0 });
        c.handle_key(KeyCode::Tab, NONE);
        c.handle_key(KeyCode::Tab, NONE);
        c.handle_key(KeyCode::Tab, NONE);
        assert_eq!(
            c.handle_key(KeyCode::Tab, NONE),
            Action::Handled,
            "查到过就不再自动问"
        );
    }

    /// 推流码栏里回车 = 重查一次状态（拉失败之后顶栏写的就是「回车 重试」）。
    #[test]
    fn enter_in_the_stream_tab_rechecks_the_status() {
        let mut c = live_control();
        c.handle_key(KeyCode::BackTab, NONE); // 先翻开配置页（不然键都归弹幕页）
        c.tab = Tab::Stream;
        c.live = LiveQuery::Failed("网络不可达".into());
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::LoadLiveStatus);
        assert!(matches!(c.live, LiveQuery::Loading));
        // 还在路上再按：不重复发
        assert_eq!(c.handle_key(KeyCode::Enter, NONE), Action::Handled);
        assert!(c.message.contains("还在路上"), "{}", c.message);
    }

    /// 开播成功：自动切到推流码栏（Go 版就这么做的）、凭据摆上、顶栏写「已开播」。
    #[test]
    fn a_successful_start_switches_to_the_stream_tab() {
        let mut c = live_control();
        c.handle_key(KeyCode::F(4), NONE);
        c.handle_key(KeyCode::Tab, NONE);
        assert_eq!(
            c.handle_key(KeyCode::Enter, NONE),
            Action::StartLive { area_v2: 371 }
        );
        c.on_live_event(LiveEvent::Started(vec![stream()]));
        assert_eq!(c.tab(), Tab::Stream, "开播成功要自动切到推流码栏");
        assert_eq!(c.page(), Page::Config);
        assert_eq!(c.streams.len(), 1);
        assert_eq!(c.message, "已开播");
        assert!(!c.start_pending);

        // 那一栏真把服务器与密钥摆出来了
        let text = text_of(&c, 160, 40);
        assert!(text.contains("rtmp-1"), "{text}");
        assert!(
            text.contains("rtmp://live-push.bilivideo.com/live-bvc"),
            "{text}"
        );
        assert!(text.contains("?streamname=abc"), "{text}");
        assert!(text.contains("完整URL"), "{text}");
        assert!(text.contains("OBS填这组"), "{text}");
    }

    /// 下播成功：凭据清掉、状态回到未开播（那一栏又说「还没开播，按 F4 开播」）。
    #[test]
    fn stopping_clears_the_credentials_and_the_state() {
        let mut c = live_control();
        c.on_live_event(LiveEvent::Started(vec![stream()]));
        assert!(!c.streams.is_empty());
        assert!(text_of(&c, 160, 40).contains("rtmp-1"));

        c.on_live_event(LiveEvent::Stopped);
        assert!(c.streams.is_empty(), "下播成功要把推流码清掉");
        assert!(matches!(c.live, LiveQuery::Known(0)));
        assert!(c.message.contains("已下播"), "{}", c.message);
        let text = text_of(&c, 160, 40);
        assert!(text.contains("还没开播"), "{text}");
        assert!(!text.contains("rtmp-1"), "推流码不该还留在屏幕上：{text}");
    }

    /// OBS 联动那行字只往推流码栏末尾加，**绝不**改顶栏那句「已开播」——
    /// 它是另一条任务晚几秒送回来的，要是能改开播的结论，就等于「顺手填个 OBS
    /// 把开播成功说成失败」。
    #[test]
    fn the_obs_note_only_adds_a_line_to_the_stream_tab() {
        let mut c = live_control();
        c.on_live_event(LiveEvent::Started(vec![stream()]));
        assert_eq!(c.message, "已开播");

        c.on_obs_note("OBS：已把推流地址与密钥填进「设置 → 推流」，开始推流还是你自己按");
        assert_eq!(c.message, "已开播", "OBS 那行字不许动开播那句结论");
        let text = text_of(&c, 160, 48);
        assert!(text.contains("OBS：已把推流地址与密钥填进"), "{text}");
        assert!(text.contains("rtmp-1"), "凭据还得在：{text}");

        // 没填上也只是多一行字，凭据照样摆着
        c.on_obs_note("OBS 没填上：连不上 OBS 的 WebSocket");
        let text = text_of(&c, 160, 48);
        assert!(text.contains("OBS没填上"), "{text}");
        assert!(text.contains("rtmp-1"), "{text}");
        assert!(text.contains("已开播") || text.contains("直播间已对外可见"), "{text}");

        // 换一场直播：上一场那行字就不该还挂着
        c.on_live_event(LiveEvent::Stopped);
        c.on_live_event(LiveEvent::Started(vec![stream()]));
        let text = text_of(&c, 160, 48);
        assert!(!text.contains("OBS没填上"), "上一场的话不许留到这一场：{text}");
        assert!(!text.contains("OBS：已把"), "{text}");
    }

    /// 两种验证都画到**账号栏**、都说「扫完再按 F4」，**不是**「开播失败」。
    #[test]
    fn verification_draws_the_qr_on_the_account_tab() {
        let mut c = live_control();
        c.on_live_event(LiveEvent::Verify {
            kind: VerifyKind::Qr,
            url: "https://www.bilibili.com/h5/verify?token=abcdefgh".into(),
            message: "本次开播需要扫码验证：请扫码验证".into(),
        });
        assert_eq!(c.tab(), Tab::Account, "验证码画在账号栏");
        assert!(c.message.contains("扫完再按 F4"), "{}", c.message);
        assert!(!c.message.contains("失败"), "{}", c.message);
        let rows = screen(&c, 120, 48);
        assert!(
            rows.iter()
                .any(|(t, _)| t.contains('▀') || t.contains('▄') || t.contains('█')),
            "二维码要真画出来：\n{rows:#?}"
        );
        assert!(
            text_of(&c, 120, 48).contains("扫码后在手机上确认"),
            "码上面得写清楚这是干什么的"
        );

        // 人脸认证那条：地址是拼好的那个，也画成码
        let mut c = live_control();
        c.on_live_event(LiveEvent::Verify {
            kind: VerifyKind::FaceAuth,
            url: "https://www.bilibili.com/blackboard/live/face-auth-middle.html?source_event=400&mid=42"
                .into(),
            message: "本次开播需要人脸认证：请先完成人脸认证".into(),
        });
        assert_eq!(c.tab(), Tab::Account);
        assert!(c.message.contains("人脸认证"), "{}", c.message);
        assert!(c.message.contains("扫完再按 F4"), "{}", c.message);
        assert!(text_of(&c, 120, 48).contains("人脸认证"), "码上面得写清楚是去认证");

        // 没拿到验证地址：还是「要验证」这件事，只是画不出码，让人再按一次 F4
        let mut c = live_control();
        c.on_live_event(LiveEvent::Verify {
            kind: VerifyKind::Qr,
            url: String::new(),
            message: "本次开播需要扫码验证：请扫码验证".into(),
        });
        assert_eq!(c.tab(), Tab::Account);
        assert!(c.message.contains("没拿到验证地址"), "{}", c.message);
        assert!(c.message.contains("按 F4 再试一次"), "{}", c.message);
    }

    /// 失败只写顶栏那句话（带出服务端原话），绝不 panic、绝不退出；
    /// 状态查不到只影响推流码那一栏。
    #[test]
    fn a_failed_live_action_only_writes_a_line_in_the_top_bar() {
        let mut c = live_control();
        c.handle_key(KeyCode::F(4), NONE);
        c.handle_key(KeyCode::Tab, NONE);
        c.handle_key(KeyCode::Enter, NONE);
        c.on_live_event(LiveEvent::Failed {
            action: LiveAction::Start,
            message: "/room/v1/Room/startLive 返回 -400: 已经在直播中".into(),
        });
        assert!(c.message.contains("开播失败"), "{}", c.message);
        assert!(
            c.message.contains("已经在直播中"),
            "服务端原话要带出来：{}",
            c.message
        );
        assert!(!c.start_pending, "失败之后要能再试一次");
        assert_eq!(c.page(), Page::Config, "失败不该把页面也带走");
        assert!(c.confirm.is_none());

        c.on_live_event(LiveEvent::Failed {
            action: LiveAction::Status,
            message: "请求被拦截".into(),
        });
        assert!(matches!(c.live, LiveQuery::Failed(_)), "那一栏自己留着失败原因");
        assert!(c.message.contains("读取开播状态失败"), "{}", c.message);
        assert!(text_of(&c, 120, 30).contains("请求被拦截"));

        c.on_live_event(LiveEvent::Failed {
            action: LiveAction::Stop,
            message: "没在直播".into(),
        });
        assert!(c.message.contains("下播失败"), "{}", c.message);
        assert!(c.message.contains("没在直播"), "{}", c.message);
        assert!(!c.stop_pending);
    }

    /// 推流码栏的顶栏提示要写清楚 F4 / F5（这一轮才把两个键接上）；
    /// 状态在路上 / 没查到时换成对应的那句。
    #[test]
    fn the_stream_hint_mentions_f4_and_f5() {
        let mut c = live_control();
        c.tab = Tab::Stream;
        assert!(c.hint().contains("F4 开播"), "{}", c.hint());
        assert!(c.hint().contains("F5 下播"), "{}", c.hint());

        c.live = LiveQuery::Loading;
        assert!(c.hint().contains("正在查"), "{}", c.hint());
        assert!(c.hint().contains("F4 开播"), "{}", c.hint());

        c.live = LiveQuery::Failed("网络不可达".into());
        assert!(c.hint().contains("回车 重试"), "{}", c.hint());
    }

    /// 未开播 / 在播（但手上没凭据），那一栏各说各的（未开播时得告诉人按 F4）。
    #[test]
    fn the_stream_pane_explains_every_state() {
        let mut c = live_control();
        c.tab = Tab::Stream;

        c.on_live_event(LiveEvent::Status { live_status: 0 });
        let text = text_of(&c, 120, 30);
        assert!(text.contains("还没开播"), "{text}");
        assert!(text.contains("F4开播"), "{text}");

        c.on_live_event(LiveEvent::Status { live_status: 1 });
        let text = text_of(&c, 120, 30);
        assert!(text.contains("正在直播中"), "{text}");
        assert!(
            text.contains("只有F4开播那一下会返回一次"),
            "在播但手上没凭据时要说清楚为什么这一栏是空的：{text}"
        );
    }

    /// 密钥近百字符：服务器 / 密钥 / 完整 URL **一行一个、不折行** ——
    /// 折了行在终端里选中复制出来就断了。窄终端下宁可截断（这是认了的取舍）。
    #[test]
    fn the_stream_pane_puts_each_credential_on_its_own_line() {
        let key = format!("?streamname={}&key={}", "a".repeat(40), "b".repeat(40));
        let mut c = live_control();
        c.on_live_event(LiveEvent::Started(vec![Stream {
            kind: "rtmp-1".into(),
            protocol: "rtmp".into(),
            address: "rtmp://live-push.bilivideo.com/live-bvc".into(),
            key: key.clone(),
            full_url: format!("rtmp://live-push.bilivideo.com/live-bvc{key}"),
        }]));

        // 够宽：整条密钥待在一行里，标签也都在
        let rows = screen(&c, 200, 40);
        let hit = rows
            .iter()
            .find(|(t, _)| t.contains(&key[..20]))
            .expect("密钥那一行得画出来");
        assert!(flat(&hit.0).contains(&flat(&key)), "完整的密钥要在一行里");
        let text = text_of(&c, 200, 40);
        for label in ["服务器", "密钥", "完整URL"] {
            assert!(text.contains(label), "缺了「{label}」：{text}");
        }

        // 窄终端：不折行 —— 密钥的尾巴不会跑到别的行上去（被截断）
        let rows = screen(&c, 60, 40);
        let tail: String = key.chars().skip(60).take(10).collect();
        assert!(
            !rows.iter().any(|(t, _)| flat(t).contains(&flat(&tail))),
            "密钥不许折行：\n{rows:#?}"
        );
    }

    /// 确认层 + 推流码栏在各种小终端下画一遍，只为了确认不 panic
    /// （框比屏幕宽时得自己让位，不能算出负数）。
    #[test]
    fn drawing_the_confirm_layer_never_panics() {
        let sizes = [(120u16, 40u16), (16, 5), (1, 1), (40, 8)];

        let mut c = live_control();
        c.on_live_event(LiveEvent::Started(vec![stream()]));
        assert_eq!(c.tab(), Tab::Stream);
        for (w, h) in sizes {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| {
                let area = f.area();
                draw(f, &c, area);
            })
            .unwrap();
        }

        let mut c = live_control();
        c.handle_key(KeyCode::F(4), NONE);
        assert!(c.confirm.is_some());
        for (w, h) in sizes {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| {
                let area = f.area();
                draw(f, &c, area);
            })
            .unwrap();
        }
    }

    /// 确认框真画出来了：标题 + 两行文案 + 两个按钮，而且**只有一个按钮反色**
    /// （两个都反色用户就不知道回车会按到哪个）。
    #[test]
    fn the_confirm_box_shows_both_buttons_and_only_one_selected() {
        let mut c = live_control();
        c.handle_key(KeyCode::F(4), NONE);
        let rows = screen(&c, 100, 30);
        let text = flat(
            &rows
                .iter()
                .map(|(t, _)| t.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        for want in [
            "开播确认",
            "开播后直播间会立刻对外可见，",
            "粉丝会收到开播推送。",
            "确定开播？",
            "[开播]",
            "[取消]",
        ] {
            assert!(text.contains(&flat(want)), "确认框里缺了「{want}」：\n{text}");
        }

        let reversed: Vec<&(String, bool)> = rows.iter().filter(|(_, r)| *r).collect();
        assert_eq!(reversed.len(), 1, "只该有一个按钮反色：\n{rows:#?}");
        assert!(
            flat(&reversed[0].0).contains("[取消]"),
            "默认选中的是「取消」：{}",
            reversed[0].0
        );

        // Tab 之后换成「开播」反色
        c.handle_key(KeyCode::Tab, NONE);
        let rows = screen(&c, 100, 30);
        let reversed: Vec<&(String, bool)> = rows.iter().filter(|(_, r)| *r).collect();
        assert_eq!(reversed.len(), 1);
        assert!(flat(&reversed[0].0).contains("[开播]"), "{}", reversed[0].0);
    }

    /// 账号栏那两个可选项画出来了：两项都在、选中那项前面是 `▸`（跟「直播间信息」
    /// 栏一个套路），`↓` 之后 `▸` 跟着走。
    #[test]
    fn the_account_pane_shows_the_two_items_and_marks_the_selected_one() {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE);
        let rows = screen(&c, 100, 30);
        let text = flat(
            &rows
                .iter()
                .map(|(t, _)| t.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        for want in ["▸重新扫码", "退出登录", "cookie 失效了换一张", "清掉本地凭据"] {
            assert!(text.contains(&flat(want)), "账号栏缺了「{want}」：\n{text}");
        }
        assert!(
            rows.iter()
                .any(|(t, _)| flat(t).contains(&flat("▸重新扫码"))),
            "默认选中的是「重新扫码」：\n{text}"
        );

        c.handle_key(KeyCode::Down, NONE);
        let rows = screen(&c, 100, 30);
        let text = flat(
            &rows
                .iter()
                .map(|(t, _)| t.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        assert!(
            rows.iter()
                .any(|(t, _)| flat(t).contains(&flat("▸退出登录"))),
            "↓ 之后 ▸ 该挪到「退出登录」上：\n{text}"
        );
        assert!(
            !rows
                .iter()
                .any(|(t, _)| flat(t).contains(&flat("▸重新扫码"))),
            "▸ 只该有一个：\n{text}"
        );
    }

    /// 退出登录那个框：标题、三行文案（要把后果说清楚）、两个按钮，
    /// 默认反色的是「取消」。
    #[test]
    fn the_logout_confirm_box_says_the_danmaku_will_break() {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE);
        c.handle_key(KeyCode::Down, NONE);
        c.handle_key(KeyCode::Enter, NONE);

        let rows = screen(&c, 100, 30);
        let text = flat(
            &rows
                .iter()
                .map(|(t, _)| t.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        for want in [
            "退出登录确认",
            "退出登录会清掉本地保存的 Cookie，",
            "在你重新扫码登录之前，弹幕会断开。",
            "确定退出登录？",
            "[退出登录]",
            "[取消]",
        ] {
            assert!(text.contains(&flat(want)), "确认框里缺了「{want}」：\n{text}");
        }

        let reversed: Vec<&(String, bool)> = rows.iter().filter(|(_, r)| *r).collect();
        assert_eq!(reversed.len(), 1, "只该有一个按钮反色：\n{rows:#?}");
        assert!(
            flat(&reversed[0].0).contains("[取消]"),
            "默认选中的是「取消」：{}",
            reversed[0].0
        );
    }

    /// 退出登录的框在窄终端下也不许 panic（框比屏幕宽时得自己让位）。
    #[test]
    fn drawing_the_logout_confirm_never_panics() {
        let mut c = logged_in();
        c.handle_key(KeyCode::BackTab, NONE);
        c.handle_key(KeyCode::Down, NONE);
        c.handle_key(KeyCode::Enter, NONE);
        assert_eq!(c.confirm.as_ref().map(|m| m.kind), Some(ConfirmKind::Logout));
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
