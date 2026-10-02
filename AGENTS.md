# AGENTS.md

给在这个仓库里干活的 AI / 人看的项目说明。命令、结构、硬约束、踩过的坑都在这儿。

## 这是什么

哔哩哔哩**直播弹幕 TUI 客户端**，Rust + ratatui 重写版。
参照物是本机的 Go 版 `../bilibili_live_tui+`（读它的代码摸行为，**不抄代码**，
也**不要改那个仓库**）。两边共用同一份账号思路，但配置文件和字段名是不同的两套。

**当前进度：读 + 发弹幕 + 扫码登录 / 退出登录 + 选分区 + 改标题 / 换封面 + 开播 / 下播 /
推流码 + OBS 联动** —— 看弹幕、看房间信息、看观众榜，底部输入框能打字、回车发弹幕
（超 20 字自动切段连发）；第二页（配置页）能翻了，账号栏是两项（重新扫码 / 退出登录），
扫码登录会把 cookie 写回配置，退出登录把 cookie 清掉并重启弹幕那条链路（见第 17 条），
分区栏是一棵真分区树（拉分区表 / 选分区 / 只把 area_id + area_name 写回配置），
直播间信息栏能改标题、换封面（本地图先传 B 站图床）、下半格画当前封面；
`F4` 开播（先过确认层）/ `F5` 下播，推流码栏摆服务器 / 密钥 / 完整 URL，
开播成功后通过 obs-websocket 把第一路 rtmp 填进 OBS 的「设置 → 推流」
（**只管填，绝不代按「开始推流」**，见第 16 条）。
弹幕栏能往上翻了（**只有鼠标滚轮**，上翻之后钉住、右边界内侧一条滚动条，
见第 19 条）；cookie 改成**整罐**存取，不再挑字段（见第 18 条）。

**还没在真账号上验过的**：`startLive` / `stopLive`（AI 一律不许真开播）、
app 签名只有假服务器覆盖、OBS 联动一次都没连过真 OBS。

## 常用命令

```bash
cargo build              # 构建
cargo test               # 全部测试，纯离线，不碰网络
cargo clippy --all-targets
cargo run                # 跑 TUI（Ctrl+R 手动刷房间信息，Ctrl+C 退出）
```

配置在 `~/.config/bililive/config.toml`（跟 Go 版的 `~/.config/bili/config.toml` 是两个文件）。
第一次跑会生成一份默认的。crates 走 rsproxy 镜像，**不要改** `~/.cargo/config.toml`。

## 两页的按键分工（照 Go 版，别串）

| 键 | 弹幕页（第一页） | 配置页（第二页） |
|---|---|---|
| `Shift+Tab` | 翻开配置页 | 收起来，回弹幕页 |
| `Tab` | **归输入框，配置页不许抢** | 换功能栏（账号 → 分区 → 直播间信息 → 推流码 → 绕回） |
| `↑↓` | 翻输入历史 | 账号栏在**两个可选项**（重新扫码 / 退出登录）里选；直播间信息栏里选一项；分区栏在**可见的行**里走（父分区也是可停的一行）；**推流码**栏没东西可选，就顺手拿来换栏 |
| `←→` | 什么都不做 | 只在分区栏有意义：收起 / 展开光标那一行（叶子 / 没有子分区的父分区上不吞键） |
| `回车` | 发弹幕 | 账号栏执行选中那一项（重新扫码 / 退出登录 —— **退出登录先过确认层**）；直播间信息栏进编辑 / 提交；分区栏停在子分区上 = **选定它**（写回配置），停在父分区 / 「全部分区」上 = 展开 / 收起；推流码栏 = 重查开播状态 |
| `Esc` | 什么都不做（**不是退出**） | 取消编辑 → 收起配置页；退到弹幕页就停住 |
| `F2` / `F3` / `F6` | 翻开配置页并跳到账号 / 分区 / 直播间信息 | 同上（这几栏之间直接跳） |
| `F4` | 开播（**先弹确认层**） | 同上（确认框画在配置页上） |
| `F5` | 下播（不用确认） | 同上 |
| `Ctrl+R` | 立刻刷房间信息 | 同上（两页都能按） |
| `Ctrl+C` | 退出（唯一出口） | 退出 |
| `鼠标滚轮` | **只认它**：指针落在弹幕框里时，一格滚 3 行（见第 19 条） | 什么都不做（不许动弹幕的视口） |

`F4` / `F5` 跟 `Ctrl+R` 一样是**全局**的：弹幕页上按也管用（Go 版是全局 capture）。
**滚动没有键盘键位**：`↑↓`（发送历史）、`Home`/`End`（行首行尾）、`PgUp`/`PgDn` 一个都不占，
回到底部的办法只有「往下滚到底」。
确认层（居中带边框、默认落在「取消」）**开播和退出登录共用**，规矩一条都不许变：
`Tab` / `←→` 选按钮、`回车` 执行选中的那个、`Esc` 取消。开着的时候别的键什么都不认 ——
漏到下面就会变成「换栏」「收起配置页」，屏上那个框还开着、其实已经没人管它了。

翻配置页时如果**没登录**，进账号栏会自动去拿一张二维码（Go 版 `fillTab` 的思路），
不用先去找回车。正在等扫码时再按回车不会生成第二张 —— 屏幕上两张码，
用户扫了哪张都说不清。`F2` 只在没登录时才顺带拿码，已登录就只跳栏。

**开局停在弹幕页**（Go 版是没登录就直接把配置页顶出来）。这一条是**故意的不一样**：
第一页才是这个软件的主体，一进来就抢屏会把弹幕盖掉；进账号栏自动出码已经把
「别让人先去找 F2」这件事补上了。

## 代码结构

```text
main.rs          出入口：读配置 -> 建 client -> 起后台任务（弹幕 / 房间 / 发送 /
                 扫码登录 / 会话落盘）-> ui::run；登录后重启弹幕链路也在这儿
config.rs        config.toml 读写
timefmt.rs       时间：本地 HH:MM、已播时长、「北京时间的墙上时间」还原（纯函数为主）
obs.rs           OBS 联动：obs-websocket 5.x 的 JSON 客户端（Hello/Identify/Identified/
                 Request/Response）+ 鉴权（黄金值钉住）+ Resolve（补端口 / 密码）。
                 **不是 B 站的接口**，所以不塞 `api/`；只管填配置，不管推流
api/
  area.rs        分区表（两级一次拿回来）+ 与界面之间那两条消息类型
  appsign.rs     直播姬那套 **app 签名**（appkey/appsec + Go 口径的表单编码），开播 / 下播用
  client.rs      HTTP 客户端：cookie + 浏览器头 + get_api/post_form + nav（WBI 种子的缓存）
  wbi.rs         WBI 签名（三个黄金值测试钉住算法，别动置换表和 !'()* 过滤）
  room.rs        房间信息 + 观众榜 + 30 秒轮询
  danmaku.rs     弹幕：上半是**纯函数**（拆包/解压/解析），下半才是 wss 和重连
  send.rs        发弹幕：切段纯函数 + POST /msg/send + 发送任务（唯一的写链路）
  info.rs        直播间信息：改标题 / 传 B 站图床 / 写封面（三个接口 + 两条消息类型）
  live.rs        开播 / 下播 / 推流凭据组装 + 两种验证（60024 / 60043）+ 两条消息类型
  login.rs       扫码登录：poll 的 code → 下一步（纯函数）+ cookie 拼装（纯函数）
                 + generate/poll 两个接口 + 扫码任务
  test_http.rs   只给测试用的假 HTTP 服务器（不引 wiremock）
ui/mod.rs        第一页（弹幕）：画图 + 收键 + 输入框状态；只碰 channel，不碰网络
ui/control.rs    第二页（配置）：页面 / 功能栏状态机 + 布局 + 信息栏的选/编辑/提交
                 （提交只扔一个 `Action` 出去），同样不碰网络
ui/area_tree.rs  分区树的**纯逻辑**：可见行 / 光标 / 展开状态 / 窗口（只算不画）
ui/qr.rs         二维码：半格字符 + 真彩色（纯函数）
ui/cover.rs      封面预览：解码 -> 区域平均缩图 -> 按控件大小现采样成半格字符（跟二维码一个套路）
```

数据流是单向的，别绕开：

```text
danmaku::supervisor --(mpsc<DanmuMsg>)--> ui 的弹幕列表（系统提示也走这条）
room::sync_loop      --(mpsc<RoomInfo>)--> ui 的房间信息 / 观众列表 / 推流状态
ui 的弹幕输入框       --(mpsc<String>, 容量 32, try_send)--> send::send_loop
                                                              └─ 失败时变成系统弹幕回弹幕列表
ui 的 Ctrl+R         --(mpsc<()>, 容量 1)--> room::sync_loop 立刻重拉
ui 的配置页           --(mpsc<()>, 容量 1)--> login::login_loop 开一张新二维码
login::login_loop    --(mpsc<LoginEvent>)--> ui 的配置页（账号那行 / 二维码 / 提示）
login::login_loop    --(mpsc<String>)------> main::session_task（拼好的 Cookie 串）
ui 的配置页           --(mpsc<()>, 容量 1)--> main::session_task 退出登录
main::session_task   --(client.set_cookie)--> BiliClient 里的 Mutex<Auth>（换 / 清凭据）
main::session_task   --(watch<u64>)--------> main::supervise_danmaku 整条重连
main::session_task   --(mpsc<()>)----------> room::sync_loop 立刻用新凭据重拉
main::session_task   --(写盘)--------------> config.toml（登录只改 cookie；退出只清 cookie）
main::session_task   --(mpsc<()>)----------> login::login_loop（退出登录后顺手再要一张码）
ui 的配置页           --(mpsc<AreaRequest>, 容量 4)--> main::area_task
main::area_task      --(api::area::fetch_areas)----> api.live.bilibili.com/room/v1/area/getList
main::area_task      --(config::save_area 写盘)----> config.toml（只动 area_id / area_name）
main::area_task      --(mpsc<AreaEvent>)---------> ui 的配置页（分区树 / 提示）
ui 的配置页           --(mpsc<InfoRequest>, 容量 4)--> main::info_task
main::info_task      --(api::room::fetch_room_info)--> api.live.bilibili.com/room/v1/Room/get_info
main::info_task      --(api::info::update_title)-----> api.live.bilibili.com/room/v1/Room/update
main::info_task      --(api::info::upload_image)-----> api.bilibili.com/x/upload/web/image（multipart）
main::info_task      --(api::info::update_cover)-----> api.live.bilibili.com/xlive/app-blink/v1/preLive/UpdatePreLiveInfo
main::info_task      --(client.get_bytes)-----------> 封面图（i0.hdslb.com，**不带 cookie**）
main::info_task      --(mpsc<InfoEvent>, 容量 4)----> ui 的配置页（标题 / 封面 / 提示 / 封面图）
main::info_task      --(mpsc<()>)------------------> room::sync_loop（改完顺手重拉房间信息）
ui 的配置页           --(mpsc<LiveRequest>, 容量 4)--> main::live_task
main::live_task      --(api::room::fetch_room_info)--> api.live.bilibili.com/room/v1/Room/get_info（拿规范房间号）
main::live_task      --(api::live::live_version)------> .../xlive/app-blink/v1/liveVersionInfo/getHomePageLiveVersion
main::live_task      --(api::live::start_live)-------> api.live.bilibili.com/room/v1/Room/startLive（**app 签名**）
main::live_task      --(api::live::stop_live)--------> api.live.bilibili.com/room/v1/Room/stopLive（不带签名）
main::live_task      --(mpsc<LiveEvent>, 容量 8)----> ui 的配置页（状态 / 推流码 / 验证码 / 提示）
main::live_task      --(mpsc<()>)------------------> room::sync_loop（开播 / 下播后立刻重拉状态）
main::spawn_obs_fill --(obs::fill)-----------------> OBS 的 WebSocket（默认 ws://127.0.0.1:4455/）
main::spawn_obs_fill --(mpsc<String>, 容量 4)------> ui 的配置页（推流码栏末尾那一行字）
```

**写在两个地方的东西只有两个**：`login_loop` 只跟接口说话（不碰磁盘、不碰界面），
`session_task` 只管落盘和重启链路（不碰网络请求的构造）。分界别混。
**扫码登录和退出登录都从 `session_task` 走**（`apply_login` / `apply_logout` 两个落点）：
两件事的落点一模一样（内存里的 client、config.toml、弹幕那条链路），
拆成两个任务写，迟早有一边漏一处 —— 比如退出时忘了重启弹幕，
现象是「界面说退了，弹幕还挂着旧身份在跑」。
分区那条照抄这个分工：`api/area.rs` 只管接口，`main::area_task` 才是「拉表 + 落盘」的落点
（界面两件事都不许自己干，所以两种请求都从同一条 `mpsc<AreaRequest>` 进去）。
直播间信息那条同理：`api/info.rs` 只管接口，`main::info_task` 才是「先传图床再写封面」
这个**顺序**的落点（界面不许自己发请求，所以三条请求都从 `mpsc<InfoRequest>` 进去）。
开播那条也一样：`api/live.rs` 只管接口（含 60024 / 60043 的分诊和推流凭据组装），
`main::live_task` 才是「先 `get_info` 拿规范房间号、再开播 / 下播」这个**顺序**的落点。
OBS 那条是**唯一一条不跟 B 站说话**的链路：`obs.rs` 只管协议和「连哪儿 / 拿什么密码」，
`main::spawn_obs_fill` 才是「开播成了才填、填的结果只变成一行字」的落点。

## 硬约束（改代码前必读）

### 1. 任何一步失败都不许 panic

这是 TUI，panic 会把整屏内容抹掉，用户看到的现象是「一打开就没了」，连报错都看不到。
网络错误一律 `Result` 往上传，由 `danmaku::supervisor` 变成一条**系统弹幕**塞进弹幕框，
然后退避重连；房间信息失败就保留上一版、在框标题上标「这次没刷上」。

已经为此写了两处专门的防御，别删：
- `client.rs::sanitize_cookie`：配置里的 Cookie 带 `\r\n` 时，`reqwest` 的 `.header()`
  内部是 `expect`，会**直接 panic**（老 Go 版自动生成的默认值还是一句中文）。
  现在脏了就不带 Cookie，退化成未登录。
- `danmaku.rs` 的拆包/解压全程不索引越界：长度字段坏了就停，解压结果有 8MiB 上限
  （压缩炸弹会 OOM，同样是整屏消失）。

### 2. getDanmuInfo 必须 WBI 签名 + 浏览器请求头，缺一个返 -352

B 站 2025-05-26 起强制签名。光有签名还不够：`User-Agent` / `Accept-Language` /
`Origin` / `Referer` / `Pragma` / `Cache-Control` 风控都会看，`api/client.rs::BROWSER_HEADERS`
是唯一来源，别在别处另起一份 header 或者另建 `reqwest::Client`。

**实测（2026-10-03）**：带着这套头 + WBI 签名，`getDanmuInfo` 正常返回 token 与 6 个
host（`*.chat.bilibili.com`），没有 -352；wss 认证包发出后收到 `op=8 {"code":0}`。

### 3. 弹幕二进制协议的坑

- 包头 16 字节全大端：总长(4) 魔数(2，恒为 16) 版本(2) 操作码(4) 序列(4)
- **protover 2 的包体是 zlib 压过的「一坨包」**：先解压、再按同样的包头拆第二层。
  实测现在很多房间直接发 `ver=0` 的单包（明文 JSON），两条路都要能走
- 心跳：op=2，包体就是 `[object Object]` 这 15 个字节（不是 JSON `{}`），每 30 秒一个
- 认证：op=7，protover 2，包体是那串 JSON（`uid`/`roomid`/`key`/`platform=web`/`type=2`）
- `DANMU_MSG` 的 cmd 有时带后缀（`DANMU_MSG:4:0:2:2:2:0`），不剥掉整条弹幕会被当成未知
  cmd 丢掉 —— 代码里先 `split(':').next()`
- 正文在 `info[1]`、昵称在 `info[2][1]`。Go 版直接类型断言，字段一缺就 panic；
  这里缺昵称就退回用 uid 当名字

### 4. `live_time` 没开播时是 `"0000-00-00 00:00:00"`

当零值收下去再和当前时间相减，界面上会出现「739891天」（Go 版真出现过）。
现在的规矩：**只在 `live_status == 1` 时才算已播时长**，且解析失败就是空串。
`timefmt` 里那条 `zero_live_time_is_rejected` 测试就是钉这个的。

顺带：`live_time` / 历史弹幕的 `timeline` 都是**北京时间但不带时区**，
`timefmt::beijing_epoch` 负责还原，别拿 `parse_datetime_utc` 的结果直接当时间戳用。

### 5. 房间信息是「拉成功才换」

`room::sync_loop` 里的 `last` 是「目前为止最完整的那一版」，推给界面的一直是它：
- 某一轮失败**不能**把界面刷白，只把 `failed` 标上，框标题写成
  「15:04 的数据（这次没刷上）」
- 观众榜同理：Go 版在 `code != 0` 时会把列表清空（界面上表现为「榜单突然全没了」），
  这里只有真拿到列表才换
- 房间信息没拉到就没有 uid，这时跳过观众榜请求（`ruid=0` 只会再换一个错误回来）
- 手动刷新走容量 1 的 channel + `try_send`，**永远不阻塞** ——
  它跑在界面的事件循环里，堵住就是把整个 TUI 冻住

### 6. 接口路径与参数

| 用途 | 路径 | 备注 |
|---|---|---|
| WBI 种子 + uid | `api.bilibili.com/x/web-interface/nav` | 没登录也返 `-101`，但 `wbi_img` 照样下发，**不能按 code != 0 当失败** |
| 房间信息 | `api.live.bilibili.com/room/v1/Room/get_info?room_id=` | Go 版写的 `room/get_info` 小写也能通（路由不区分大小写） |
| 观众榜 | `.../xlive/general-interface/v1/rank/getOnlineGoldRank?ruid=&roomId=&page=1&pageSize=50` | **`ruid` 是主播 uid，不是房间号** |
| 弹幕服务器 | `.../xlive/web-room/v1/index/getDanmuInfo` | 要签名 |
| 历史弹幕 | `.../xlive/web-room/v1/dM/gethistory?roomid=` | 不需要签名；**进程级只拉一次**，重连再拉会把同一批重放一遍 |
| 发弹幕 | `.../msg/send`（POST） | URL 带 WBI 签名（**只签 `web_location=444.8`**），表单见下面第 10 条 |
| 分区表 | `.../room/v1/area/getList` | 不带参数、不用登录，一次两级（父分区里套 `list`）；子分区 `id` 是**字符串**，见第 13 条 |
| 改标题 | `.../room/v1/Room/update`（POST） | 只带 `room_id` / `title` / `platform=pc_link` / `csrf` / `csrf_token`，见第 14 条 |
| 传图床 | `api.bilibili.com/x/upload/web/image`（POST multipart） | 字段 `file` / `bucket=openplatform` / `csrf`；**主站域，不是直播域** |
| 写封面 | `.../xlive/app-blink/v1/preLive/UpdatePreLiveInfo`（POST） | 挂在 app-blink 下但只带 csrf，**不要 app 签名**，见第 14 条 |
| 开播版本号 | `.../xlive/app-blink/v1/liveVersionInfo/getHomePageLiveVersion?system_version=2&ts=`（GET） | **要 app 签名**（直播姬那套，不是 WBI），见第 15 条 |
| 开播 | `.../room/v1/Room/startLive`（POST） | **要 app 签名**；会让直播间立刻对外可见，见第 15 条 |
| 下播 | `.../room/v1/Room/stopLive`（POST） | **不要 app 签名**（Go 版就是不带签名的） |

`nav` 的 img_key/sub_key 和 uid 一起缓存 30 分钟（key 每天轮换，但也别每个请求都问一遍）。

### 7. 断线重连用指数退避，别写死

1 秒起步翻倍、上限 60 秒；连接活过 30 秒才把退避归位（被风控时别拿重连去撞墙）。
Go 版是写死 30 秒，那是历史包袱，不要照抄。

### 8. 界面上的死规矩

- 弹幕列表**有上限**（`MAX_LINES = 500` 行），默认粘底、只看最后几行 —— 开一整天
  不能无限长；往上翻能看旧的（**只有滚轮**，键位一个都不占，见第 19 条）
- 单行 / 多行、显不显示时间，跟 `config.toml` 的 `single_line` / `show_time` 走
- 多行模式下同一个人在同一分钟连着说话不重打名字（跟 Go 版行为一致）
- 观众榜前三名 👑🥈🥉
- 退出只有 Ctrl+C；Esc 是「返回上一层」的语义，**别接成退出**
- 顶部的艺术字和四宫格布局是定稿，别动

### 9. 本地时间只能问 libc

std 不提供本地时区（要自己解 TZif）。`timefmt::hhmm` 走 `libc::localtime_r`
（用 `_r` 那版：tokio 是多线程运行时，`localtime` 返回共享静态缓冲区，两个线程一起格式化会串）。
`libc` 本来就是间接依赖，不是新引入的一棵树。

### 10. 发弹幕（第一条写链路）

- `POST https://api.live.bilibili.com/msg/send`，**URL 上带 WBI 签名，参数只有
  `web_location=444.8`**（`wbi::sign` 顺手补上 wts/w_rid）。表单字段：
  `msg` / `color=16777215` / `fontsize=25` / `rnd` / `roomid` / `csrf` / `csrf_token`。
  形状照 bili-live-hime 的 `sendComment` 来，跟 Go 版（biligo）有两处不同，别当成 bug 改回去：
  - `rnd`：这里和 bili-live-hime 都是**毫秒**（`Date.now()`），biligo 用的是秒，
    官方文档也写「秒级时间戳」。它只是给服务端去重的随机数，**不参与校验**；
    哪天真发出不去且只有这一处可疑，再换秒试。
  - biligo 还带 `mode=1` / `bubble=0`，网页端不带，我们也不带。
- **`csrf` 和 `csrf_token` 都取 cookie 里的 `bili_jct`，两个字段同一个值**
  （只给一个老接口不认）。cookie 里没有 `bili_jct` 就**在发请求之前**报错
  （`client.rs::cookie_value`），别拿空串去换一个含糊的 `-101`。
- 单条上限 20 个字，超了切段连发、段间 1 秒（Go 版 `sender/sender.go` 的行为）。
  `send::split_segments` 是纯函数且**按字符切**：按字节切会把汉字劈成半个，
  发出去是乱码，风控还要记一笔。
- `code != 0` 一律算失败，把接口那句 `message`（「发送过于频繁」…）变成一条**系统弹幕**。
  所以这条链用 `post_form_raw`（不拆外壳）而不是 `post_form`：先被 `unwrap`
  折成「返回 10030」，唯一能解释原因的东西就没了。
- 发送在**单独的任务**里（`send::send_loop`）：段间要 sleep 1 秒，搁在界面的事件循环里
  就是把 TUI 冻住。界面侧容量 32 + `try_send`，永远不等发送端；入队失败
  （没配房间号 / 发送端没起来 / 队列满）就往弹幕框里塞一句说明。
- 一条消息内部才有 1 秒间隔，**两条独立消息之间不额外等**（Go 版每条弹幕一个 goroutine，
  间隔会重叠交叉，这里串行反而更不容易撞上「发送过于频繁」）。
- 发成功**不自己塞一份**：那条会从弹幕 websocket 回显回来，再塞一条就是重复。

### 11. 输入框的键不能吃掉全局键

`Ctrl+C`（唯一退出）和 `Ctrl+R`（刷房间信息）在 `event_loop` 的 match 里排在最前面，
剩下的才交给 `Input::handle_key`。所以输入框**永远不要**自己处理这两个键，
`handle_key` 里 `KeyCode::Char(c) if !mods.intersects(CONTROL | ALT)` 那条守卫就是干这个的：
带 Ctrl/Alt 的键一律不当文字收（不然屏幕上会莫名多出一个 `c`）。
缓冲区按**字符**存（`Vec<char>` + 光标下标）：按字节存的话中文退格会剩半个字。

第二页同理，多了一层：`Control::handle_key` 返回 `Handled` / `ToMain` / `StartLogin`，
`event_loop` 只在 `ToMain` 时才把键交给输入框。**弹幕页上的 `Tab` 必须是 `ToMain`** ——
它一旦变成 `Handled`，正在打字的人按一下 Tab 就会发现字没了。

配置页的编辑缓冲（`ui/control.rs::Edit`）也按字符存，而且**边打边写回那一行**：
只在提交时才落值的话，`Esc` 之前那一栏显示的还是旧值，用户会以为「根本打不进去」
（Go 版用的是真 `InputField`，框里的字一直是活的）。

### 12. 扫码登录（第二个页面 + 第三条链路）

**接口**（`api/login.rs`，全流程只读）：

| 用途 | 路径 | 备注 |
|---|---|---|
| 申请二维码 | `passport.bilibili.com/x/passport-login/web/qrcode/generate` | 给 `url` + `qrcode_key` |
| 查扫码状态 | `passport.bilibili.com/x/passport-login/web/qrcode/poll?qrcode_key=` | 2 秒一次、最多 90 次（3 分钟） |

- **要看的是 `data.code`，不是外壳那个 `code`**（外壳恒为 0）。取值：
  `0` 成功 / `86038` 过期 / `86101` 未扫码 / `86090` 已扫码待确认。
  照外壳判断的话，码过期了程序还在那儿傻等三分钟。`next_step` 是这四个值到「下一步」的
  纯函数，全流程的分支都挂在它上面。
- **凭据有两条路，兜底那条别省**：正常在响应的 `Set-Cookie` 里；`Set-Cookie` 有可能
  被中间几跳吃掉，那就只剩成功跳转 URL 的 query 上那一份。query 是**百分号编码**的，
  必须先解码 —— SESSDATA 里的逗号在 URL 上是 `%2C`，原样存进配置下次带上去服务端不认，
  表现却只是「登录态时好时坏」，最难查的那种。
- 只存五个字段（`SESSDATA` / `bili_jct` / `DedeUserID` / `DedeUserID__ckMd5` / `sid`），
  顺序固定（config.toml 每次保存长得一样，diff 才看得出改了哪条）。
  值是 `deleted` 的跳过 —— 那是服务端在**清**这个 cookie，照收下来配置里就多一串
  `SESSDATA=deleted`，比没有还糟。
- 拿全了才走成功分支：**`SESSDATA` 和 `bili_jct` 缺一个都不算登录成功**。
  少了后者所有写操作都会被服务端回一句「csrf 校验失败」，而用户以为自己已经登录了。
- 单次 poll 失败（网络抖）**不算**登录失败，2 秒后再问；真一直不通，最后那条
  「过期」的提示也够用了。

**二维码怎么画**（`ui/qr.rs`）：

- 一格字符画上下两个模块（`▀` / `▄` / `█` / 空格），所以**字符行数是模块行数的一半**。
- 颜色必须是**真彩色** `#000000` / `#ffffff`，不能写 `Color::Black` / `Color::White` ——
  那两个名字走终端调色板，浅色主题下「黑」会被映射成接近背景的颜色，整张码糊掉、
  手机扫不出来（Go 版踩过，现象是「换个主题就扫不动」）。
- **静默区要自己加**：`qrcode` crate 的 `to_colors()` 给的是**不含**静默区的模块矩阵，
  扫码要靠那圈白边把码从背景里框出来，我们补 4 个模块。
- 纠错等级用 **Low**（跟 Go 版一致）。真登录地址一百来个字符，Medium 会大一整个版本：
  实测 41 模块（25 行）vs 49 模块（29 行），多出来那四行在终端里就是「下半截被切掉」。
- 「画出来的码还是原来那张码」由 `rendered_blocks_round_trip_to_the_module_matrix` 钉住：
  把字符反解回模块，跟 crate 给的矩阵逐格比。少一格、上下翻半格都扫不出来，而肉眼看不出来。

**登录成功后新凭据怎么生效**（`main.rs`）：

- `BiliClient` 手上的凭据是 `Mutex<Auth>`：client 被好几条链路 `Arc` 共享着，
  整只换不掉，只能让里面这块可变（`set_cookie`）。换的时候顺手清掉 nav 缓存 ——
  那份缓存里带着上一个身份的 uid 和 WBI 种子，不清的话界面上还是旧账号。
- **弹幕那条 wss 必须整条重来**：认证包（op=7）在握手时就发完了，里面带着 uid，
  之后没有「换个身份」这个操作。`supervise_danmaku` 收到 watch 信号就 abort 再 spawn
  （`JoinHandle` 一 drop 就是 abort），新一轮会重新 getDanmuInfo + 重新认证。
  **发这个信号只能走 `signal_credential_change`**：写成
  `auth.send_replace(auth.borrow().wrapping_add(1))` 会同一个线程自己锁死自己
  （读借用活到语句结束，而 `send_replace` 要拿同一把锁的写权），
  现象是成功那一刻进程卡住、界面上什么都不再动（第八轮才被逼出来）。
- **房间那条不用重启**，也**不要**重启：它是「每轮现读 cookie」的循环，重启只会把
  刷新 channel 丢掉、把上一版房间信息丢掉。登录后补一次 `room::refresh` 就够，
  不然要等满 30 秒才轮到新凭据。
- 发弹幕那条每次发送都现取 `csrf()`，跟着 client 一起生效，不用管。
- **落盘失败不算登录失败**：凭据在内存里已经能用了，说一声「下次启动还得重扫」就够了，
  不能反过来告诉用户「登录失败」让他白扫一次。
- 存配置只有 `Config::save_to(&cfg_path)` 一个口子。原来还另有一个写死默认路径的
  `save()`，`-c` 指到别处时等于这次登录白登（这个坑是这轮现挖的），
  已经把它删了 —— 别再顺手加回来。

### 13. 分区树（配置页的「分区」栏）

**接口**（`api/area.rs`，只读、不用登录、不带参数）：

| 用途 | 路径 | 备注 |
|---|---|---|
| 分区表 | `api.live.bilibili.com/room/v1/area/getList` | 一次两级：父分区里套 `list` 子分区 |

- **父分区的 `id` 是数字，子分区的 `id` 是字符串**（实测长这样：`"id":"86"`）。子分区 id 就是
  开播要用的 `area_v2`，必须走 `client::int_of`（数字 / 字符串都收）。用 `as_i64()` 会一律
  得到 0，而 0 正是配置里「还没选过」的意思 —— 现象是「明明选好了，下次启动又是空的」。
  `api/area.rs` 测试里那份 body 就是照真响应抄的，别顺手改成数字。
- 2026-10-03 实测：12 个父分区 / 450 个子分区，今天没有空 `list` 的父分区
  —— 但空 `list`、缺 `list`、`list: null` 三种都要能站住（单测各一条）。
- `code != 0` 一律 `Err` 往上传：界面把它显示在右栏和顶栏提示里，**绝不 panic、绝不退出**。

**这两个坑都是从 Go 版踩出来的**（行为照抄，代码是重写的）：

1. **父分区必须是「可停的一行」**。Go 版用 tview 的 `TreeView`，上下键只在「可选节点」上停留，
   父分区不可选就被整段跳过去 —— 用户报的现象是「分区没法选」。
   这里的 `area_tree::Row` 里父分区是一等公民（`Row::Parent(usize)`），`↑↓` 一定停得到它上面。
2. **默认展开谁只看配置**。Go 版拿 `SetExpanded(是不是列表第一项)` 当判据，
   结果每次进来都展开「网游」，跟配置里那个分区半点关系没有。现在的规矩：

   - 没配过（`area_id == 0`）：光标停在「全部分区」上，**所有父分区收起**
   - 配过：**只展开它所在的那个**父分区，光标定到那个子分区上
     （`area_tree::tests` 用三个父分区钉死「别的父分区必须收起」）
   - 配过的分区今天不在表里（分区下线了）：退回「全部分区 + 全收起」，绝不随便展开一个

顺带一条同源的坑（写回配置那一半）：**必须先重读文件再写**（`Config::save_area`）。
内存里那份配置是启动时读的，中间可能刚扫码登录过（`apply_login` 往同一个文件写进了
cookie），整个覆盖过去就是「登录成功 → 选个分区 → 变回未登录」。

**按键语义**（`ui/control.rs::handle_key`，只在分区栏这一栏里生效）：

| 键 | 行为 |
|---|---|
| `↑↓` | 在**可见的行**里上下走，到顶 / 到底停住（不绕圈）；父分区也是可停的一行 |
| `←→` | 收起 / 展开光标那一行（含「全部分区」）。叶子和没有子分区的父分区上什么也不做，**也不吞键**（Go 版是交回 tview，这里意味着这一行没绑定，别写成 `return` 把键吃掉） |
| `回车` | 子分区上 = **选定它**（`area_id` + `area_name` 写回配置，名字形如「虚拟主播/虚拟日常」）；父分区 / 「全部分区」上 = 展开 / 收起；表还没到手 = 拉一次（也是拉失败后的重试） |
| 其他 | 维持原样：`Tab` 换栏、`Esc` 逐层返回、`Ctrl+C` 退出 |

- **切进分区栏时手上还没有表就自动拉一次**（`enter_tab` 返回 `Action::LoadAreas`），
  别让人先进来再自己找键；正在拉的期间不重复发请求。
  **拉失败后不在切栏时自动重试**（网络一抖就会变成「每切一次打一次接口」），
  顶栏那行提示这时写的是「分区表没拉到：回车 重试」，右栏里也写一遍。
- 分区表是个 125KB 的大家伙，**可见行和滚动窗口都自己算**（`area_tree::window`）：
  交给 `Paragraph` 自己往下截的话，光标一走到底下就从屏幕上消失 ——
  用户只会得出「按键失灵」这个结论。选中的那一行**整行反色**（Go 版靠 tview 的高亮）。
- 「接口给了一张空表」不算失败：退回「还没有表」的状态并说一句，回车还能再拉。

### 14. 直播间信息（配置页的「直播间信息」栏）

两个字段（标题 / 封面）：`↑↓` 选、`回车` 进编辑、再 `回车` 提交、`Esc` 还原。
提交结果（成功 / 失败原因）写在顶栏那条「最近一条消息」里 —— 失败**只写一句话**，
绝不 panic、绝不退出，值也照着用户填的留在那一行（他多半想在那个基础上改）。

**接口**（`api/info.rs`，三个都是写操作）：

| 用途 | 路径 | 表单 |
|---|---|---|
| 改标题 | `api.live.bilibili.com/room/v1/Room/update` | `room_id` / `title` / `platform=pc_link` / `csrf` / `csrf_token` |
| 传图床 | `api.bilibili.com/x/upload/web/image` | multipart：`file`（文件名取路径最后一段）/ `bucket=openplatform` / `csrf` |
| 写封面 | `api.live.bilibili.com/xlive/app-blink/v1/preLive/UpdatePreLiveInfo` | `platform=web` / `mobi_app=web` / `build=1` / `csrf` / `csrf_token` / `cover` |

- **`csrf` 和 `csrf_token` 都取 cookie 里的 `bili_jct`，同一个值**（老接口读 csrf、
  新接口读 csrf_token，只给一个总有半边不认）。取不到 `bili_jct` 就**在发请求之前**
  报错（`api/info.rs::csrf`），别拿空串去换一句含糊的 `-111`。
- **标题上限 40 个字符，不是 40 字节**（`MAX_TITLE_CHARS`，界面和接口共用这一个常量）。
  服务端超了只回一句看不懂的错，所以**输入时**就拦：第 41 个按键直接不收
  （Go 版用的是 InputField 的 `SetAcceptanceFunc`），提交前再 `check_title` 兜一道。
  口径按 Unicode 标量算（一个汉字 = 一个字符，一个 emoji = 一个字符；
  带 ZWJ 的组合 emoji 会算好几个 —— 跟 Go 的 `RuneCountInString` 一致）。
- **换封面是两步，顺序不能反**：先把本地图传到 B 站图床拿到 `.hdslb.com` 地址，
  再拿那个地址去 `UpdatePreLiveInfo`。别处的链接服务端一律回 `100402`（图片地址不合法）。
  第一步没过**就别去写第二步**（写上去的地址根本不是图床的，只会换个 100402 回来）。
- **`UpdatePreLiveInfo` 不要 app 签名**：它虽然挂在 `app-blink`（直播姬）下，
  但网页端只带 csrf 就能过。appkey / appsec 那套是**开播**接口才用的（下一轮的事，
  别顺手加）。单测里直接断言请求体里没有 `appkey` / `sign=`。
- 图床地址两个字段都得认：`data.image_url` 或 `data.location`。
  **`location` 是 `http://`，要升成 `https://`**（这一条来自 Go 版（实测过图床的那个版本）
  的注释：http 的地址会被下游当不安全链接拒掉。**我们这边没真传过图**，
  第一次传完记得回来看一眼 `data` 里到底是哪个字段、是不是 http）。
  **两个都没有就报错**，绝不返回空串继续往下走 —— 那样用户看到的是「封面已提交」，
  而服务端早就用 100402 拒了。`//i0.hdslb.com/...` 这种协议相对地址也补成 https
  （直接扔给 `reqwest` 会报「relative URL without a base」）。
- 封面路径支持 `~`：`File::open` / `tokio::fs::read` 都不认它，得自己展开
  （`api::info::expand_home`，家目录从外面喂进去所以能单测）。`./` `../` 不用管。
- **封面留空 = 不改动**，不是错误：说一句「封面留空，没有改动」就完事。
- 房间号：用**配置里那个**（可能是短号 `6`）去查 `get_info`，拿回来的**规范号**
  （响应里的 `room_id`，比如 `7734200`）才用来写标题 —— 短号也查得到，
  但写操作拿规范号更稳。`RoomInfo::room_id` 现在以服务端回的为准，别改回去。

**界面这一半**：

- 切进这一栏时**自动**去 `Room/get_info` 拉一次（`Action::LoadRoomMeta`）：
  标题要预填、封面要地址。一次运行只问一次；**拉失败不算问过**，下次切回来会重试一次
  （但不会变成「每切一次栏打一次接口」——跟分区栏一个口径）。读不到**不等于改不了**：
  输入框照用，界面上只留一句「可以直接输入：…」。
- 预填**只填空的那一行**（`seed_title` / `seed_cover` / `on_info_event` 三处同一口径）：
  慢网下用户可能已经敲了一半，回复来了不能把人家打的字冲掉（Go 版 `loadTitle` 同理）。
- 提交是**动作**不是副作用：`handle_key` 返回 `Action::SetTitle` / `Action::SetCover`，
  `event_loop` 把它们塞进 `mpsc<InfoRequest>`（容量 4），网络那一下在 `main::info_task`。
  **别把这条链收回界面** —— 界面不碰网络、不碰磁盘是这个项目写死的分工。
- 改完（成功时）顺手给房间信息那条链路发一次手动刷新，别让人对着旧标题等满 30 秒
  （服务端也可能要几秒才生效，那就等下一轮）。

**封面预览**（`ui/cover.rs`）：

- 图是信息任务抓的（`client.get_bytes`，**不带 cookie**：图在 `hdslb.com` 上，
  那是另一个域）。抓不到只让预览那一格写一句话，**不能说成「换封面失败」**。
  同一个地址不重复抓（切栏、拉伸终端都不重新下载图片）。
- 解码 / 缩放 / 采样都在**界面**这边：过来的是原始字节，`Cover::set_bytes` 一次解码 +
  区域平均缩到 96×140 存着；画的时候按**控件当前大小**现采样 ——
  终端一拉伸下一帧就跟着变，不用重新抓图。`image` crate 只开 `jpeg,png`
  （`Cargo.toml` 里显式写的，**别开 default**：那串 avif/exr/webp 用不上。
  它本来就是 qrcode 带进来的依赖树，显式加一条只是为了能直接用）。
- 一格 = 上下两个像素（`▀` + 前景色 / 背景色），跟二维码一个套路。
  解码跑在事件循环里（几十毫秒一次，只在换图那一下）——真碰上超大图会卡一帧，
  比多铺一条通道简单，认了。

### 15. 开播 / 下播 / 推流码（app 签名那套）

三个接口，**两套规矩**，别串：

| 用途 | 路径 | 签名 |
|---|---|---|
| 开播版本号 | `api.live.bilibili.com/xlive/app-blink/v1/liveVersionInfo/getHomePageLiveVersion?system_version=2&ts=<毫秒>` | **app 签名** |
| 开播 | `.../room/v1/Room/startLive`（POST 表单） | **app 签名** |
| 下播 | `.../room/v1/Room/stopLive`（POST 表单） | **不带签名**（Go 版就是不带） |

**app 签名**（`api/appsign.rs`，直播姬 / bilibili link 那一对）：

```
appKey = "aae92bc66f3edfab"
appSec = "af125a0d5279fd576c1b4418a3e8276d"

参数按 key 排序 -> 表单编码拼成 query -> 加 appkey=<appKey>
-> sign = md5(query + appSec) -> 最终串 = query + "&sign=" + sign
```

- **跟 web 端那套 WBI（`wbi.rs`）是两码事**：那套用 nav 下发的 img_key/sub_key 算
  `w_rid`；这套是写死的 appkey/appsec 算 `sign`。**别混用**，也别给别的接口
  （改标题 / 换封面 / 分区表 / 房间信息）乱加 —— 它们只认 csrf，加了反而被拒
  （第 14 条里那三个接口的实测结论就是这么来的）。
- 编码口径照 **Go 的 `url.Values.Encode()`**：key 排序、空格变 `+`、`~` **不转义**、
  `*` 要转义。**不能**直接用 `url::form_urlencoded::Serializer`（WHATWG 那套）：
  它把 `~` 编成 `%7E`、又留着 `*` 不编，正好在这两个字符上相反 —— 签名是对着
  **那一串**算的，差一个字符服务端就算不出同一个 sign，而它只会回一句含糊的
  「签名校验失败」（极易错查成「appkey 不对」）。所以自己写了
  `appsign::go_query_escape`，并且有 python 独立算的黄金值钉着（见「测试」那一节）。
- 签名和发送**必须是同一个串**：签名算出来之后就原样发出去，
  别再让 `reqwest` / `url` crate 编码一遍。`BiliClient::post_body` 就是给这个用的
  （`post_form_raw` 会重编码，口径不同）。

**startLive 的表单**（九个字段，都是必填）：

```
room_id=<规范房间号>  platform=pc_link  backup_stream=0  csrf=<bili_jct>
csrf_token=<bili_jct>  area_v2=<配置里的 area_id>  version=<版本接口的 curr_version>
build=<版本接口的 build>  ts=<毫秒>
```

（另加签名那两下：`appkey` + `sign`。）**stopLive** 只有
`room_id` / `platform=pc_link` / `csrf` / `csrf_token` 四个。

- **写操作用服务端回的规范房间号**：配置里那个可能是短号（`6`），
  `get_info` 的 `data.room_id`（`7734200`）才是服务端认的那个。开播和**下播**
  都是先 `get_info` 再发（下播也要 —— 用户可能一进来就按 F5，那时手上还没有
  `get_info` 的结果）。这一条在 `main::canonical_room` 里落一次，两处共用。
- **先拿版本号再开播**：`liveVersion` 的 `data.curr_version` / `data.build`
  就是 `startLive` 的 `version` / `build`。缺了服务端回「版本不对」之类的话。
- **推流凭据组装**（`api::live::assemble_streams`）：`data.rtmp{addr,code}` 算一路，
  `data.protocols[]{protocol,addr,code}` 每项一路，按同协议出现次数编号
  （`rtmp-1` / `rtmp-2` / `srt-1`）。每路给 类型 / 服务器 / 密钥 / 完整 URL。
  拼完整 URL 时那个怪情况：**密钥以 `?` 开头、或者地址以 `/` 结尾时不要再补 `/`**
  （`addr + key`），否则 OBS 里表现成「连不上服务器」。地址或密钥缺一个的那一路
  直接丢掉；一路都没有 = 服务端没给地址，**报错**（别假装开播成功）。
- **两种验证要认出来，别报成普通失败**：
  - `code == 60024`：需要扫码验证，验证地址在 `data.qr` 里；
  - `code == 60043`：需要人脸认证，响应里**没有**二维码，得拿 `nav` 的 `mid`
    自己拼 `https://www.bilibili.com/blackboard/live/face-auth-middle.html?source_event=400&mid=<mid>`。
  两种都落在 `StartOutcome::Verify`（**不是** `Err`）：界面把地址画成二维码放到
  **账号栏**（复用 `ui/qr.rs` 那套渲染），顶栏写「…，扫完再按 F4」。
  没拿到地址（60024 没给 qr / nav 也没问出 mid）时仍然算「要验证」，
  只是画不出码，得说「按 F4 再试一次」。
  别的 code（`-400 已经在直播中` 之类）才是普通失败，原话带进顶栏。
- **`stopLive` 清状态**：成功之后把推流码栏清掉、状态改回未开播（`LiveQuery::Known(0)`）。

**界面这一半**：

- `F4` 开播**必须先过确认层**（自己在 ratatui 里搭的：屏幕正中一个带边框的框 +
  两行文案 + 两个按钮，`Tab` / `←→` 换、`回车` 按选中的、`Esc` 取消）。
  文案照 Go 版：「开播后直播间会立刻对外可见，粉丝会收到开播推送。确定开播？」。
  **默认选中的是「取消」**（Go 版 tview 的 Modal 默认落在第一个按钮上）：
  开播不可逆，一个手滑的回车就出去的代价太大，宁可多按一下 Tab。
  没登录 / 没选分区 / 已经在播 / 请求还在路上时有各自的话，**连确认层都不弹**。
- `F5` 下播**不用确认**；确知没在播时只写一句话，不发请求。
- 「推流码」栏进栏先看一眼状态（只读的 `get_info`）。手上**没有**凭据时按状态写话：
  未开播 → 「还没开播，按 F4 开播」；在播 → 说清楚「推流地址与推流码只有 F4 开播
  那一下会返回一次，要拿到就 F5 再 F4」（凭据只有 `startLive` 的响应里才有）。
- **凭据与状态分开存**（`streams` / `LiveQuery`）：手上已经有凭据时，
  一次状态查询**不许**把它冲掉 —— 刚开播那几秒服务端还可能回未开播。
- 开播成功**自动切到推流码栏**（Go 版就这么做的）。
- 密钥近百字符：**一行一个、不折行**（`Paragraph` 不设 `wrap` 就是截断）。
  地址和密钥挤在一行里，在终端里选中复制出来是断的（Go 版的教训）。
  窄终端下密钥尾部会被截断，这是认了的取舍 —— 折行会把一串密钥断成两行，更难复制。
- 三种失败都只写顶栏那一行 / 推流码那一栏，**绝不 panic、绝不退出**。

### 16. OBS 联动（只管填，不管推）

开播（F4）**成功**之后，把第一路 rtmp 的服务器 + 密钥通过
[obs-websocket](https://github.com/obsproject/obs-websocket) 5.x 写进 OBS 的
「设置 → 推流」，省得手抄那串近百字符的密钥。代码在 `src/obs.rs`（不是 B 站接口，
别塞 `api/`），接进 `main::live_task` 的 `Started` 分支。

**协议**（一条 JSON 连接走完，操作码就是 `op` 字段）：

```text
Hello(op 0) -> Identify(op 1) -> Identified(op 2) -> Request(op 6) -> Response(op 7)
```

- 要鉴权时 `Hello.d.authentication{challenge, salt}`，`Identify.d.authentication` 填应答；
  Hello 里**没有** `authentication` 就**不许**带这个字段（凭空塞一个串 = 鉴权失败）。
- 鉴权公式（`obs::auth_string`，分两步、中间那串 base64 是下一步的输入）：

  ```text
  secret = base64(sha256(password + salt))
  auth   = base64(sha256(secret + challenge))
  ```

  **黄金值**（python 独立算的，钉在 `obs.rs` 的测试里；换算法或写错一步它就会红）：
  password `secret` + salt `salt123` + challenge `chal456`
  → `yo3DuCXyQQheiGKNZpyXB//3OodP2GoXULXeX19lE4M=`
- 要发的就是一条 `SetStreamServiceSettings`：

  ```json
  {"streamServiceType":"rtmp_custom",
   "streamServiceSettings":{"server":"<地址>","key":"<密钥>","use_auth":false}}
  ```

  服务类型必须是 `rtmp_custom`：B 站给的密钥自带 `?streamname=…`，**整串原样**塞进
  `key`，让 OBS 自己去拼。谁都不许在这儿「顺手」拼地址或转义 ——
  拼错的表现是 OBS 里一按「开始推流」就断，而界面上什么异常都看不出来。
- `requestStatus.result = false` 是**失败**，不是成功：收着回话就说「填好了」的话，
  用户会在 OBS 里对着一份没换过的密钥纳闷。
- 等 `Response` 的时候可能先来一条事件（`op 5`）：跳过它接着等，别当成错误。
- 每一步都套超时（连接 3 秒、读写 5 秒）：**「连上了但半天不吭声」是真实存在的坑**，
  不设超时那条任务就永远挂着，用户永远等不到那一行字。

**连哪儿 / 拿什么密码**（`obs::resolve`，四个字段就是 `config.toml` 里那四个）：

| 字段 | 空 / 0 表示 |
|---|---|
| `obs_fill` | `false` = 什么都不做（连都不连，一个字都不多说） |
| `obs_host` | 空 = `127.0.0.1` |
| `obs_port` | 0 = 去读 OBS 自己的配置 |
| `obs_password` | 空 = 去读 OBS 自己的配置 |

- 读的是 `$XDG_CONFIG_HOME/obs-studio/plugin_config/obs-websocket/config.json`
  （没设 `XDG_CONFIG_HOME` 就退回 `~/.config/…`），**认 `XDG_CONFIG_HOME`**。
- **配置里写了的优先**：端口和密码两样都写了就一个文件都不读（用户既然写死，
  说明他清楚自己连的是哪台 OBS）。
- `server_enabled: false` → 直接给那句人话
  「OBS 里的 WebSocket 服务器没开：工具 → WebSocket 服务器设置 → 勾上『启用 WebSocket 服务器』」，
  **别只说「连不上」** —— 用户会跑去折腾端口和防火墙。
- 没有那个文件 → 另一句人话：说清楚路径 + 「先去 OBS 里 工具 → WebSocket 服务器设置 打开它」。
- `server_enabled` 字段**缺失**（老版本配置）不算「没开」；端口缺了才退回 4455。

**什么时候填 / 填不上怎么办**：

- 只在开播**成功**（`StartOutcome::Started`）之后填，拿**第一路 rtmp**
  （`streams.iter().find(|s| s.protocol == "rtmp")`；srt 那几路 OBS 不认）。
- 填这件事在**另一条 `tokio::spawn`** 上跑，不在开播那条链上：连 OBS 可能要等到超时，
  挡在那儿就等于「OBS 没开机时开播成功也要多等几秒才显示」。
- **只填配置，绝不代按「开始推流」**：填配置是准备动作，开播那下得用户自己在 OBS 里按。
  成功那句话也是这么写的（「开始推流还是你自己按」）。
- 填失败 / 连不上 / OBS 没开：**只往推流码栏末尾多一行字**
  （`mpsc<String>` 那条 `obs_notes` 通道 → `Control::on_obs_note`），
  **绝不**让开播本身显示失败 —— 它走的通道都跟 `LiveEvent` 分开，
  就是为了让这个语义在类型上就成立（晚几秒到的联动结果改不了开播的结论）。
- 下一场开播 / 下播时把那一行清掉（`LiveEvent::Started` / `Stopped` 里 `clear`）。

### 17. 退出登录（配置页账号栏的第二项）

**为什么会有这一条**：账号栏从前只有「重新扫码」，而**cookie 还有效时回车不出码**
（`login_loop` 回「已登录，无需重复扫码」，这句提示是照 Go 版留着的）——
于是想换账号、或者 cookie 脏了想重扫，用户只能拿临时配置绕
（`-c /tmp/xxx.toml`）。tc191 自己刚踩过这个坑，这一条就是补这个洞。

**界面**（`ui/control.rs`）：账号栏跟「直播间信息」栏一个套路，
两项 + 选中那行前面的 `▸`：

```text
▸ 重新扫码     cookie 失效了换一张
  退出登录     清掉本地凭据
```

- `↑↓` 在这一栏里选（**以前这一栏的 `↑↓` 是拿去换栏的**，现在换栏只归 `Tab`），
  回车执行选中那一项。默认停在「重新扫码」：进来的人大多是码失效了要换一张，
  而退出登录是个破坏性动作，不该是回车的第一落点。
- 「重新扫码」就是**以前那一下**，一个字没改（包括「已登录，无需重复扫码」那句）。
- 「退出登录」**先过确认层**，而且**不管现在是不是登录态都能选** ——
  「cookie 脏了（nav 已经失败、界面显示未登录）但文件里那串还在」正是要清的那种，
  拿 `logged_in` 把这一项拦掉就把这个场景漏了。
- 确认层跟开播共用一个框（`ConfirmKind::StartLive` / `Logout`），文案必须说清后果：
  「退出登录会清掉本地保存的 Cookie，在你重新扫码登录之前，弹幕会断开。」
  按钮是 `[ 退出登录 ]  [ 取消 ]`，**默认落在「取消」**，`Esc` 也一样是取消。

**真退出时做的四件事**（`main.rs::apply_logout`，一条都不能漏）：

1. **配置里只清 `cookie`**：`Config::clear_cookie(path)` —— 形状跟 `save_area` 一样
   （**先重读文件再写**，只动那一个字段）。整份内存配置覆盖回去的话，
   `room_id` / `area_id` / `area_name` / `obs_*` 全会被冲回默认值，
   而界面看着一切正常。测试是逐行比对的：除了 `cookie` 那一行，文件里别的行必须
   逐字节没变。
2. **内存里的凭据也要清**：`client.set_cookie("")` —— 走的就是换凭据那一个口子
   （`Mutex<Auth>` 换掉、nav 缓存一并清掉）。漏了这里，退出之后发给 B 站的请求
   还会挂着旧身份出去，是最难查的「看着退了其实没退」。
3. **弹幕那条链路整条重启**：`watch` 计数 + `supervise_danmaku` 那一套照抄登录那条路。
   理由一样 —— wss 的认证包（op=7）在握手时就发完了，没有「换个身份」这个操作。
   **房间信息那条不用重启**（它每轮现读 cookie），补一次 `room::refresh` 就够。
4. **界面**：`LoginEvent::LoggedOut("未登录（回车扫码）")` 让账号那行回去，
   再顺手往 `login::login_loop` 的启动信号里塞一下 —— 退完直接开始生成一张新码，
   别让用户再去找键（扫码任务正忙时这一下会被丢掉，那边顶栏的「重新扫码」还在，
   不影响退出的结果已经落地）。界面上原来那张码**不清掉**：它可能正是扫码任务
   正在轮询的那一张，扫了照样能登回来；清了反而在「新码还没到」这段里让用户干等。

**写盘失败不算退出失败**：内存里那套已经清干净了，顶栏那句提示里补一句
「（配置里的 Cookie 没能清掉：…）」就够 —— 反过来说「退出失败」会让用户白退一次。

**别做的事**：不要为了省一条通道把退出塞进 `creds` 那条 `mpsc<String>` 里
（拿空串当暗号，读的人得猜）；也不要让界面自己去清配置 / 清凭据 ——
界面只发一个 `Action::Logout`，落盘和重启全在 `session_task` 那边。

### 18. cookie 整罐存（连接稳定性）

**为什么**：老做法是一张白名单 —— 只把 `SESSDATA` / `bili_jct` / `DedeUserID` /
`DedeUserID__ckMd5` / `sid` 五个字段写进 `config.toml`。于是 `buvid3` 这类
**设备标识**每次重启都丢，程序在服务端眼里每次都是台**陌生设备**，
弹幕 / 房间信息那些接口的风控看的正是它们 —— 现象就是时不时莫名其妙地断。
整罐存之后 B 站加什么新字段都自动跟上，不用再改代码。

**怎么存**：`api/login.rs` 里那三个纯函数是一处，谁都得从这儿走。

- `cookies_from_set_cookie`：一条 `Set-Cookie` 收一个 cookie，**不挑名字**；
  只取第一个 `=` 到第一个 `;` 之间那一段（再往后是 `Path` / `Domain` / `Expires`
  这些属性，当成值存进去下次服务端就不认了）；空值不要，`deleted` 不要
  （那是服务端在**清**这个 cookie）。Set-Cookie 的值是**原样**的，**不解百分号编码**。
- `cookies_from_redirect`：跳转 URL 那条**兜底**路（`Set-Cookie` 被中间几跳吃掉时用）。
  query 要解一次百分号编码（`%2C` 是 SESSDATA 里的逗号），但要挡掉
  `NON_COOKIE_QUERY` 里那几个**路由参数**（`gourl` / `c` / …）—— 那不是 cookie，
  照收的话配置里会多一行看不懂的东西。
- `merge_cookie(base, extra)`：罐的语义。`extra` 盖 `base`（**留在原位**），
  `base` 里有、`extra` 里没有的照旧留着，新字段追加在后面。顺序稳 = `config.toml`
  每次保存长得一样，git diff 里才看得出改了哪一条。
- 落盘时传的 `base` 是 **client 手上那串**（`BiliClient::raw_cookie()`）：
  这一趟响应没重发 `buvid3`，那份设备标识也不该被洗掉。
- **判登录成功只看这一趟新拿到的**（`has_cookie(&extra, …)`，不看并完的罐）：
  罐里那两份可能是上次留下的旧值，拿它去判断会把「什么都没拿到」当成登录成功。

**发请求那半边本来就是整罐**：`Auth.header` 就是原串（`sanitize_cookie` 只掐控制字符，
非 ASCII 才整条丢掉），所以「服务端给什么就带什么」不用改，但**别顺手在那儿加过滤** ——
`api/client.rs` 的 `the_whole_jar_goes_out_on_every_request` 那条测试钉着它。

**这一轮顺手量到的（详见「实测过」里那节）**：程序真会调的那些只读接口
**一个都不返** `buvid3` / `buvid4` / `b_nut` —— 别指望整罐存能把它们捞回来。

**别做的事**：不要为了「配置里干净」再把字段挑一遍；也不要自己去加一个
「申请设备号」的接口 —— 要不要主动要一个 `buvid3` 由 tc191 定，那是另一步。

### 19. 弹幕滚动（只认滚轮 + 鼠标捕获）

**只有鼠标滚轮**，一个键盘键位都不占（`↑↓` 是发送历史、`Home`/`End` 是行首行尾、
`PgUp`/`PgDn` 也一并留着）。回到底部的办法只有一个：**往下滚到底**。
`no_keyboard_key_scrolls_the_danmaku` 那条测试就是拦「以后顺手加个键位」的。

- 指针**落在弹幕框矩形里**才算（矩形由 `main_layout` 算，画图和热区共用一份，
  两边各算一次的话热区迟早跟眼睛看到的框对不上），一格 3 行（`WHEEL_LINES`）。
  配置页上滚轮**不许**动弹幕的视口。
- **粘底 / 钉住**：默认粘底（新弹幕跟着走），上翻之后**钉住绝对行号** ——
  不能存「离底部还差几行」，那样新弹幕进来会一行一行把内容推走，就不是钉住了。
  滚回底部自动恢复粘底（`Viewport::follow`）。
- **`MAX_LINES` 挤掉头部那行时，钉住的下标要跟着减一**：不减的话屏幕上的内容
  会自己往下跳一行。
- 右边界**内侧**一条进度：`█` 滑块、`│` 轨道（`thumb()` 算位置，单拆出来是为了
  能直接断言顶 / 中 / 底三个位置）。内容不满一屏**不画**；框**窄到 3 格以下也不画** ——
  一列宽的终端上那个 `area.right() - 2` 会下溢（debug 直接 panic），
  `tiny_terminal_does_not_panic` 里那几个「窄而高」的尺寸把它钉住了。
- 上翻时标题写「已上翻 N 行 · 滚到底恢复跟随」—— 不写这一句，用户对着一屏旧弹幕
  只会以为程序卡住了。

**鼠标捕获（`EnableMouseCapture` / `DisableMouseCapture`）**：

- 开关是 `config.toml` 的 `mouse`（**默认开**）。**默认开的代价**：终端会把鼠标事件
  交给程序，终端自己那套「按住拖拽选中文字 / 双击选中一个词」就不管用了 ——
  想选中文、复制推流密钥得**按住 `Shift` 再拖**（多数终端留着这个后门）。
  这条代价在 `config.rs` 的字段注释里也写了一份。
- **退出和 panic 都必须还回去**：`restore()` 第一件事就是 `release_mouse()`，
  panic 钩子装的也是同一个 `restore()`。留在捕获状态里，用户没法选中文、
  终端自己的滚动也废了，只能重开一个终端。
- 「还回去了没有」是个可查的状态（`MOUSE_CAPTURE` + `mouse_capture_enabled()`）：
  真终端上的 escape 序列没法在单测里断言，但 `leaving_turns_the_mouse_capture_back_off`
  能钉住这一步（`restore` 和 panic 钩子都走 `release_mouse` 这一个口子）。
- 事件循环**一次把积压的事件收干净再重画**：crossterm 的 `EnableMouseCapture`
  顺带把「鼠标移动」（1003）也打开了，一条一条处理、每处理一条重画一帧的话，
  鼠标在窗口上划一下就能把 CPU 吃满。

## 测试

`cargo test` 必须全绿，且**不许引入真实网络请求**（真接口只许手工验，见下）。

- `api/danmaku.rs`：自己造二进制包喂给 `unpack` —— 拆包、protover 2 解压、坏长度不越界、
  压缩炸弹有上限、`DANMU_MSG` 带后缀、缺昵称退回 uid、认证包/心跳包的形状
- `api/room.rs`：假 HTTP 服务器断言**请求路径 + 参数形状 + 浏览器头**，
  外加 `sync_loop` 的「拉失败保留上一版」和 Ctrl+R 那条刷新路
- `api/client.rs`：`post_form` 的表单编码；脏 Cookie 不能把进程带走；
  `cookie_value` 取 `bili_jct`（缺键/空值/带换行）；换凭据（`set_cookie`）要换掉
  `Mutex<Auth>` 里的两样**并且**清掉 nav 缓存；**换成空串之后 `logged_in()` / `csrf()`
  都得为假 / 为空**（退出登录走的就是这一个口子）；**整罐原样发出去**
  （`buvid3` / `b_nut` 这些一个都不许被挑掉）
- `api/send.rs`：`split_segments` 的边界（空串、正好 20、21、41 个中文、`limit=0` 不 panic）；
  假服务器上断言**路径 + WBI 参数（逐字节等于 `wbi::sign` 的结果）+ 七个表单字段 +
  cookie/浏览器头**；`code != 0` 带上服务端的 message；没有 `bili_jct` 时一个请求都不发；
  21 个字切两段且真有间隔；一段失败要变成系统弹幕
- `ui/mod.rs`：用 `TestBackend` 把一帧画进内存再读回来，验弹幕上限、框标题的刷新时间与
  「没刷上」、前三名奖牌、推流状态、超小终端不 panic（**窄而高**的那几组也要留着：
  `(1,20)` / `(2,20)` 这种尺寸才走得到滚动条那段代码，原来的小尺寸高度都不够）；
  另外验输入框（按字符编辑 / 历史 10 条与翻到头翻到底 / Ctrl+C 和 Ctrl+R 不被当文字收 /
  空回车不发）、推流状态格的整段截断，以及**打字 -> 回车 -> 假服务器上真的出现
  `/msg/send` 请求**这条端到端的路
  （注意：缓冲区里宽字符会多占一格留下空格，断言前两边都 `flat()` 掉空白再比）
- `ui/mod.rs`（弹幕滚动）：粘底时新弹幕跟着走 / 上翻之后**钉住**（来新弹幕视口不动）/
  滚回底部恢复跟随 / 滚到顶停住 / 不满一屏没什么可滚且**不画滚动条**；
  `thumb()` 的顶 / 中 / 底三个位置 + 溢出不超过界；滚动条真的画在右边界内侧那一格上
  （从 `TestBackend` 缓冲区按坐标读，**不能**去数拼出来的字符串 —— 宽字符后面
  那半格是空 symbol，会把格子数骗歪）；标题写出「已上翻 N 行」；
  鼠标落点在弹幕框外 / 在配置页上都不动视口；**键盘键位一个都不许滚**
  （`PgUp` / `PgDn` / `Home` / `End` / `Ctrl+Home` / `Ctrl+End` / `↑↓` 全试一遍）；
  退出会把鼠标捕获还回去（`MOUSE_CAPTURE` 那个可查状态）
- `api/login.rs`：`next_step` 四种 code 各一条（外加没见过的 code 不能硬猜成成功/过期）；
  cookie 从 `Set-Cookie`（属性里的 `=`、`deleted`、**整罐都收**、同名只留先到的）
  和跳转 URL 的 query（`%2C` 要解码、不认识的字段照收、`gourl` 这种路由参数要挡掉）
  里抠得对不对；**罐的合并**（新的盖旧的且留在原位、这次没重发的 `buvid3` 要留着、
  新字段追加在后面、空罐/空串不 panic）；端到端那条**带着 `Set-Cookie` 的设备号
  走到交接出来的 Cookie 串里**（用 `test_http::start_with_headers`）；
  整条流程在假服务器上打一遍（generate → poll 三次不同 code → 成功），
  断言请求路径、`qrcode_key` 参数、轮询次数，以及最后交接出来的那串 Cookie
  （`LoginCtx` 的 `gap` / `attempts` 做成字段就是为了这里能传 0，别让单测真睡 2 秒）
- `ui/qr.rs`：行数是列数的一半、只有那四种半格字符、静默区四边都是白的
  （按**半格**验：压在边界上的那一行只有一半属于静默区）、真彩色不是调色板、
  编不出来的内容只给一行说明、以及**反解回模块矩阵的往返测试**
- `ui/control.rs`：按键语义（Shift+Tab 开合两页、弹幕页上 Tab/Esc 不归它管、
  Tab 换栏绕圈、↑↓ 在四栏里各是什么、Esc 一层层往回走然后停住、F2/F3/F6 直接跳栏、
  没登录进账号栏才要码）、账号栏那一组（两个选项 / `↑↓` 选行 / 回车各触发什么 ——
  「重新扫码」给 `Action::StartLogin`、「退出登录」先弹确认层；退出登录的确认层
  默认落在「取消」、`Esc` 不执行、默认那个回车也不执行、只有选到「退出登录」再回车
  才给 `Action::Logout`；那一下没送出去（`logout_dropped`）还能重来；
  退出登录落地后账号行回到「未登录（回车扫码）」、`logged_in` 为假、账号栏照旧能再要码；
  账号栏两个选项画得出来且 `▸` 只在一个上）、编辑骨架（按字符编辑、
  Tab 跑掉要还原、提交要说清楚没真发）、
  登录事件只动显示状态、提示语里不许有换行、各种小终端尺寸下画一页不 panic；
  分区栏另有一组：切进来只自动拉一次表、没配过停在「全部分区」且父分区都收起、
  配过的展开它所在的那一个、`←→` 只在父分区上有动作、回车在子分区上给
  `Action::PickArea { id, name }`（在父分区上只是展开收起）、拉失败后回车重试、
  空表也能回车再拉、顶栏提示跟着状态换、选中的行反色且整屏只有它一条、
  长列表走到底选中行还在屏幕上
- `ui/mod.rs`（配置页）：整屏被第二页接管（主页面那几块一个字都不剩）、
  左栏 16 格里是那四个功能名、**左栏**只有一个 ▸（右栏自己也用 ▸：账号栏两个选项、
  直播间信息栏两行字段）、右栏标题跟着当前栏换、
  拿到二维码就画出来且登录后收掉
- `api/area.rs`：假服务器上断言**请求路径 + 不带参数 + 浏览器头**；照真响应抄的 body
  里子分区 id 是字符串也要收对；空表不算错；父分区缺 `list` / `list: null` / `list: []`
  三种都留成空支；`code != 0` 变 `Err`
- `api/info.rs`：标题上限按**字符**（40 个中文放行、41 个拦下、40/41 个 emoji 各一条、
  14 个中文不能因为「42 字节」被冤枉）；`Room/update` 的表单**五个字段一个不多一个不少**、
  超长标题一个请求都不发、没有 `bili_jct` 时一个请求都不发；图床那次的 multipart
  （字段名 / 文件名 / 文件内容 / csrf，二进制内容按 `body_raw` 比）；
  `UpdatePreLiveInfo` 的六个字段**且没有 appkey / sign**；`pick_image_url` 的五种形状
  （都给 / 只 `location`（http 升 https）/ 只 `image_url` / 协议相对 / 都不给要报错）；
  `~` 展开（`~`、`~/x`、`/tmp/~/x`、`./x`、拿不到家目录）
- `ui/cover.rs`：真 PNG 编码再解码的往返（小图不放大、400×200 缩成 96×48）；垃圾字节
  只留一句话不 panic；三种没图时的提示（还没有 / 加载中 / 加载失败）；
  一格一个 `▀` 且前景 = 上半个像素、背景 = 下半个像素（真彩色，
  跟调色板色区分开）；**同一个图换个更宽的框，画出来就跟着宽**
  （「尺寸跟控件走」那条）；0×0 / 1×1 / 0 高的框都不越界
- `ui/control.rs`（信息栏那一组）：40 个字的上限按按键拦（中文、emoji 各一条，
  退一个还能再进一个）；进编辑 → 改动 → `Esc` 还原 / `回车` 提交两条路都断言值
  （提交出去的是 `Action::SetTitle` / `SetCover`，带的是用户填的那个串）；
  空标题不许提交、封面留空只是「没有改动」；切进这一栏自动拉一次 meta、
  拉到过就不再问、失败过会再试一次；信息事件只动显示状态（预填不覆盖用户敲的字、
  成功失败各留一句话、垃圾封面图不许把界面带走）；`try_send` 失败时
  `info_request_dropped` 要把「正在问」放掉（否则这一栏永远停在「问过了」）
- `main.rs`（信息链）：`LoadMeta` 拉回标题 + 顺手抓封面图（**同一张只抓一次**，
  抓图那次不带 cookie）、`SetTitle` / `SetCover` 的表单与**两步顺序**（先图床后封面）、
  图床失败就别去写封面、配置里是短号时用 `get_info` 给的规范号写标题、
  没配房间号时一个请求都不发
- `ui/area_tree.rs`：可见行的拉平（收起 / 展开各一条）、父分区是可停的行、
  到顶 / 到底不绕圈、叶子上 `←→` 不吃键、没配过时全收起、配过时**只展开它所在的那个**
  父分区（三个父分区，另外两个必须收起）、配过的分区没了就退回全收起、
  没给 id（0）的分区不认账、窗口函数把光标留在可视区里
- `config.rs`：`save_area` 只动 `area_id` / `area_name`（cookie、房间号原封不动）、
  写之前必须重读一遍文件（模拟扫码登录刚写进新 cookie）；
  `clear_cookie`（退出登录）**逐行比对**——除了 `cookie` 那一行，文件里别的行必须
  逐字节没变，而且同样要先重读（模拟用户刚在别处改过房间号 / 分区）
- `main.rs`：`area_task` 按真通道打一遍 —— `Load` 走假服务器拿回分区表，
  `Pick` 把两个字段写进临时配置文件，别的不许动
- `main.rs`（会话链）：退出登录按真通道打一遍（全离线，只在**临时配置文件**上）——
  界面收到 `LoggedOut` + 一句「弹幕已断开」的说明、内存里的凭据空掉（`logged_in()`
  为假）、配置里**只有** cookie 被清（房间号 / 分区 / OBS 三项原样还在）、
  弹幕那条链路收到 watch 的重启信号、房间信息补了一次手刷、`login_start` 上
  也收到了「再要一张码」那一下。
  **这条测试同时钉住一个自锁的坑**：`auth.send_replace(auth.borrow()…)` 会
  同一个线程自己锁死自己（见 `signal_credential_change` 的注释）—— 改回那种写法，
  这条测试会当场挂住不返回
- `api/appsign.rs`：**三组 python 独立算的黄金值**（版本号那次的 query+sign、
  开播那次的十一个字段、带 `~` / 空格 / `*` / `+` / `=` 的转义那组），
  外加「不带签名时没有 appkey / sign」和「空参数给空串」。
  黄金值**必须**独立算（python / openssl 都行），**绝不能拿 `encode_params` 自己的
  输出当期望值** —— 那是循环论证，签名错的时候测试会跟着一起错。转义那条还给出了
  WHATWG 那套的结果做对照（`~` 和 `*` 正好相反），谁想换回 `url` crate 的编码先看这条
- `api/live.rs`：版本号是 GET + app 签名（appkey 对得上、`ts` 13 位、sign 32 位）；
  开播那一次九个字段 + appkey + sign **一个不多一个不少**，而且
  **sign 必须等于 `md5(真发出去的那一串去掉 &sign= + appsec)`**（验的是「签名与发送
  同一个串」）；下播只有四个字段**且没有 appkey / sign**；
  `60024` → `Verify{Qr, data.qr}`、`60043` → `Verify{FaceAuth, 用人脸页模板拼的地址}`
  且 mid 来自 nav（假服务器上给的 42）；没给 qr 时仍然算「要验证」；
  别的 code 变 `Err` 且带服务端原话；密钥以 `?` 开头 / 地址以 `/` 结尾 / 缺地址缺密钥 /
  一路都没有这几种形状；人脸页地址模板钉死；没分区 / 没 csrf 时一个请求都不发
- `main.rs`（开播链）：`Start` 先 `get_info` 再版本号再开播，发出去的 `room_id`
  是**规范号**（配置里写的是短号 6）；`Stop` 同样先拿规范号且请求体里没有签名；
  `LoadStatus` 只读；开播成功 / 下播成功都顺手发一次房间信息刷新；
  `60024` 走 `LiveEvent::Verify` 而不是 `Failed`；房间号是 0 时一个请求都不发
- `ui/control.rs`（开播那一组）：确认层默认选「取消」、`Tab` / `←→` 换按钮、
  `回车` 按选中的那个、`Esc` 只收确认层（配置页还开着）、别的键一律被它吃掉；
  没分区 / 没登录 / 已经在播 / 请求在路上时**连确认层都不弹**；`F5` 不用确认、
  路上不重复发、确知没在播时只写一句话；`try_send` 失败后 `live_request_dropped`
  要把「正在发」/「正在查」放掉（不然那几个键以后永远被自己挡住）；
  进推流码栏只自动查一次状态、回车是重查；
  开播成功自动切到推流码栏；下播后凭据清掉且那一栏又写「还没开播」；
  两种验证都画到账号栏（真出现半格字符）且不说「失败」；失败只写顶栏那一句；
  推流码栏的提示带 F4 / F5；密钥在宽终端下完整地待在一行、窄终端下不折行；
  确认框画出来有两个按钮且**只有一个反色**；确认层 + 推流码栏在超小终端下不 panic
- `obs.rs`：鉴权**黄金值**（python 独立算的，别拿 `auth_string` 自己的输出当期望值）；
  `SetStreamServiceSettings` 的请求体（`rtmp_custom`、`use_auth: false`、
  **key 原样没被改**、`requestId` 非空）；**假 OBS**（`tokio-tungstenite` 的 `accept_async`
  那侧）上走完 Hello → Identify → Identified → Request → Response 全程，
  断言 Identify 里就是那个黄金值、请求体形状对；Hello 里没有 `authentication` 时
  Identify **不许**带这个字段；OBS 要密码而配置里没有 → 一句提到 `obs_password` 的话；
  `requestStatus.result = false` → 是错误（带 code / comment）而不是成功；
  连上但不回话 → 超时放弃（拿小超时跑，别真等）；
  端口上没人听 → 「连不上」；`resolve` 那套（`XDG_CONFIG_HOME` 指到临时目录造假的 OBS
  配置：配置为空 → 读文件、只写端口 → 端口以配置为准而密码仍读文件、
  两样都写 → 一个文件都不读、`server_enabled: false` → 那句人话、文件不存在 → 另一句人话、
  `server_enabled` 缺字段不算没开、坏 JSON 不 panic）
- `main.rs`（OBS 那条）：开播成功 → 顺手把**第一路 rtmp**（不是 srt 那一路）发到假 OBS，
  服务器 / 密钥逐字对得上，回来的那行字里说得出「填哪儿」且带「你自己按」；
  `obs_fill = false` → 一个字都不多说；这次开播没有 rtmp → 只多一行字、连都不连
- `ui/control.rs`（OBS 那行字）：`on_obs_note` 只往推流码栏末尾加一行、
  **不许**动顶栏那句「已开播」；换一场直播（`Stopped` → `Started`）之后上一场那行不许还挂着

改 B 站接口相关代码时顺手确认 `wbi.rs` 那三个黄金值测试还是对的 ——
它们存在的意义就是接口规则一变就报警。

### 怎么手工验真接口

`src/main.rs` 是纯 bin，没有 lib，所以**没法写一处 `examples/` 复用模块**。
这轮的做法是：临时往 `main.rs` 里塞一个 `--smoke` 分支（打印 nav/房间/榜单/getDanmuInfo，
再接 30~75 秒弹幕），跑完**删掉**。下一次要验还这么干，别把探针留在仓库里。

**发弹幕这条链路的探针**（只打一次，2026-10-03 用过）：用真 cookie 但把 `bili_jct`
换成一串 0、`msg` 传空串 —— 两道保险，**不可能真的发出一条弹幕** ——
只为看服务端的回话。结果见下面「实测过」。以后要真的发一条弹幕验回显，
也**只许一条**、发完把结果记进这份文件。

**扫码登录的探针**（2026-10-03 用过，这条完全只读）：往 `main.rs` 里塞一个
`--smoke-login` 分支 —— 用**空 cookie**（连自己那份配置都不碰）调
`login::generate` 打印 url/key，再 `login::poll` 三次、每次隔 2 秒，打印 `next_step`
和 `Set-Cookie` 条数。没人去扫那张码，它三分钟自己就过期了，账号上什么都不变。
探针要放在 `cli::Cli::parse` **之前** —— 放在后面的话 `--smoke-login` 会先被
参数解析器当成「不认识的参数」拒掉（这个坑刚踩过）。

**分区表的探针**（2026-10-03 用过，完全只读）：`--smoke-area` 分支 —— 空 cookie 调
`api::area::fetch_areas` 打印父分区 / 子分区个数和前三个分区的 id+名字；
顺手把 `area_tree::AreaTree::new(areas, 保存的 area_id)` 的可见行打出来（`>` 标光标），
好一眼看出「到底展开了谁」。同一个探针也给 `--smoke-area 371` 这样带一个 area_id 用。
放法同上：在 `cli::Cli::parse` **之前**。

**看响应头里有什么 cookie 的探针**（2026-10-03 用过，**不用改代码**）：
只读接口 + `curl -D -` 就够，空 cookie 也不会碰账号上任何东西（`-o /dev/null`
连 body 都不用要）。想知道「某个接口会不会给某个 cookie」时用它，
比塞一个 `--smoke` 分支快得多（第九轮量设备号就是这么干的，结果见「实测过」）：

```bash
UA='Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/96.0.4664.110 Safari/537.36'
curl -s -D - -o /dev/null 'https://api.live.bilibili.com/room/v1/Room/get_info?room_id=6' \
     -H "User-Agent: $UA" -H 'Accept: */*' \
     -H 'Origin: https://live.bilibili.com' -H 'Referer: https://live.bilibili.com/' | grep -i set-cookie
```

**OBS 联动的探针**（2026-10-03 用过，跑完已删）：`--smoke-obs`（只跑 `obs::resolve`，
纯读文件）和 `--smoke-obs-fill`（`resolve` + `fill`）。**用它必须先把
`XDG_CONFIG_HOME` 指到一个假目录**：

```bash
FAKE=$(mktemp -d)                       # 里面没有 obs-websocket/config.json
XDG_CONFIG_HOME=$FAKE cargo run -q -- --smoke-obs-fill   # 该得到「没找到…去打开它」
```

**绝不许**拿真 `XDG_CONFIG_HOME` 跑 `--smoke-obs-fill`：这台机器上 obs-websocket 是开着的，
`fill` 会真的改掉 tc191 的 OBS 推流设置（服务器 + 密钥）。只读的那半（`--smoke-obs`）
可以拿真 XDG 跑，但**别把读回来的密码打印出来**。

要验界面那一半（按键 -> 写回配置）就**别碰自己的配置**：用一份临时配置跑真 TUI——

```bash
# 临时配置里 room_id=0，不进任何房间；分区表是只读接口
printf '' > /tmp/smoke.toml
{ sleep 4; printf '\033[Z'; sleep 1; printf '\t'; sleep 5;   # 翻到配置页 -> 分区栏（自动拉表）
  printf '\033[B\033[C\033[B\r'; sleep 3; printf '\003'; } \
  | script -qec "stty rows 34 cols 110; cargo run -q -- -c /tmp/smoke.toml" /tmp/smoke.log
```

`script` 那条 `stty rows/cols` 不能省：不给窗口大小的 pty 会让 TUI 在 0 行 0 列上画，
日志里一个字都看不到（按键倒是照样管用）。跑完看 `/tmp/smoke.toml` 里
`area_id` / `area_name` 是不是选中的那个，就够证明「界面 -> 通道 -> 落盘」整条通了。

别做任何会改账号状态的操作（登录 / 开播 / 下播 / 改标题 / 改封面），
也别往真房间刷弹幕（要验就按上面那条规矩来一次）。

**改标题 / 换封面这两条谁来验、怎么验**：这是账号上**看得见**的改动，
所以只由 tc191 自己在真配置上按回车验（`F6` → `↑↓` 选 → 回车编辑 → 回车提交），
AI / 别人**不许**代为跑一遍真接口。他验的时候盯这三件事，顺手把结果记进「实测过」：

1. 顶栏那句话是「标题已提交，生效要等几秒」还是「改标题失败：…」（后者要抄下原文）
2. 换封面时先看顶栏有没有「换封面失败：传图床失败：…」——那说明卡在第一步
   （图床字段 / 分桶不对），跟封面接口没关系
3. 提交完看一眼直播间页面自己有没有变（服务端生效通常几秒）

**开播 / 下播谁来验、怎么验（比上面那两条还严）**：开播会让直播间**立刻对外可见**、
给粉丝**推开播推送**，所以：

1. AI / 别人**绝对不许**真开一次播（哪怕只开一秒）。整条链一律假服务器。
2. **只读的探针可以真调**：版本号接口
   `getHomePageLiveVersion?system_version=2&ts=<毫秒>` 带 app 签名，看服务端认不认
   我们的签名（认的话回 `code 0` + `curr_version` / `build`；签名错就是一句
   「签名校验失败」，那说明 appkey / appsec 或编码口径不对）。
   `stopLive` **不许**真调 —— 它跟开播一样是写操作，会把直播间的状态改掉。
3. 真要开播只能 tc191 自己按 F4（确认层那一下），而且他得知道粉丝会收到推送。
   验完把结果记进「实测过」（尤其是 `60024` / `60043` 这两种验证会不会出现）。
4. 界面那一半可以在**临时配置 + 空房间号**上真跑（`-c /tmp/smoke-live.toml`）：
   按 `F4` 只会得到「还没设置直播间号」这类话，出不了网，可以放心看确认层长什么样。

## 已完成 / 待办

### 已完成（2026-10-03，第一轮：读）

- `api/client.rs`：带 cookie 和浏览器头的 HTTP 客户端，`get_api` / `post_form`（写操作只用它，本轮无人调用）
- `api/room.rs`：房间信息、观众榜、30 秒轮询 + Ctrl+R 手动刷新、「拉成功才换」
- `api/danmaku.rs`：getDanmuInfo（WBI）-> wss 认证 -> 收包解压拆包 -> 六类消息 -> channel，
  30 秒心跳、指数退避重连、历史弹幕、失败只出系统弹幕
- `ui/mod.rs`：房间信息（框标题带最后刷新时间）、观众列表（前三奖牌）、
  弹幕列表（单行/多行、显示时间、上限 500 行）、推流状态、Ctrl+R / Ctrl+C
- `timefmt.rs`、39 个测试、假 HTTP 服务器

### 已完成（2026-10-03，第二轮：发弹幕）

- `api/client.rs`：从 cookie 里取 `bili_jct`（`cookie_value`，`BiliClient::csrf()`）；
  `post_form` 拆成 `post_form_raw`（不拆外壳，写操作自己看 code/message）+ `post_form`；
  `nav` 的 base 变成字段（`with_main_base`，只给测试，免得单测打真网络）
- `api/send.rs`：`split_segments`（纯函数）、`send_one`（签 URL + 七个表单字段 + 判 code）、
  `send_danmaku`（现取 nav 种子）、`send_loop`（界面 -> 发送的任务，段间 1 秒、
  失败变成系统弹幕、绝不 panic）
- `ui/mod.rs`：输入框真能打字（字符插入 / 退格 / Delete / 左右 / Home/End / Ctrl+U /
  ↑↓ 翻 10 条历史 / 回车发送，光标反色画出来），Ctrl+C 与 Ctrl+R 保持全局；
  obs 推流状态格改成「整段取舍 + 省略号」，长时长不再把「在线 N」截成半个词
- `main.rs`：多一条 `mpsc<String>`（容量 32）+ `send::send_loop` 任务
- 16 个新测试（切段边界、请求形状、输入框按键与历史、端到端回车发请求、状态格截断）

### 已完成（2026-10-03，第三轮：第二页 + 扫码登录）

- `api/client.rs`：凭据从「构造时定死」改成 `Mutex<Auth>`（`set_cookie` / `logged_in` /
  `csrf` 变异步），换凭据时清 nav 缓存；`Nav` 多带 `uname` / `is_login`；
  新增 `get_json_and_cookies`（把响应里的 `Set-Cookie` 一起带回来 —— `get_value` 只看 body）
- `api/login.rs`：`next_step` 状态机 + cookie 抠取/拼装（全是纯函数）+ generate/poll
  + `login_loop`（开局查一次登录态，之后每收到信号开一张码，2 秒一轮、最多 3 分钟）
- `ui/qr.rs`：半格字符 + 真彩色 + 自加静默区 + Low 纠错
- `ui/control.rs`：第二页 —— 页面 / 功能栏状态机、16 格左栏 + 带标题的右栏、
  按键提示栏 + 最近一条消息、账号栏（二维码居中）、直播间信息栏的选 / 编辑骨架、
  分区与推流码的占位
- `ui/mod.rs`：`Wiring` 收拢七条 channel；`event_loop` 先问配置页再决定要不要给输入框；
  `draw` 在第二页时整屏交给 `control::draw`；`Ctrl+R` 两页都能按
- `main.rs`：登录任务 + `apply_login` 会话任务（落盘 / 换凭据 / 通知）+ `supervise_danmaku`
- `config.rs`：`resolve_path` / `save_to`（`-c` 给的路径也要能写回去）
- 33 个新测试（90 个全绿），`cargo clippy --all-targets` 干净

### 已完成（2026-10-03，第四轮：分区栏）

- `api/area.rs`：`fetch_areas` + `parse_areas`（两级一次拿回来；子分区 id 是字符串也要收对；
  空表 / 缺 `list` / `list: null` 都站得住）+ 与界面之间那两条消息类型（`AreaRequest` / `AreaEvent`）
- `main.rs`：`area_task` —— 拉表（网络，`api::area`）和选定（落盘，`config::save_area`）
  都从一条 `mpsc<AreaRequest>`（容量 4）进来；分工跟 `apply_login` 一模一样
- `config.rs`：`Config::save_area` —— **先重读文件再写**，只覆盖 `area_id` / `area_name`
- `ui/area_tree.rs`：分区树的纯逻辑（可见行 / 光标 / 展开 / 窗口）+ 两个坑的行为钉死
- `ui/control.rs`：分区栏从占位变成真树（`↑↓` 走可见行、`←→` 收起展开、回车选定或展开、
  进来没表自动拉一次、拉失败顶栏写「回车 重试」、选中行整行反色、窗口自己滚）
- `ui/mod.rs`：`Wiring` 多两条 channel；开局把 `cfg.area_id` 喂给配置页
- 31 个新测试（121 个全绿），`cargo clippy --all-targets` 干净

### 已完成（2026-10-03，第五轮：直播间信息栏）

- `api/info.rs`（新）：`check_title`（40 字符）、`update_title`、`upload_image`（multipart）、
  `update_cover`、`pick_image_url`、`normalize_image_url` / `absolute_image_url`、
  `expand_home` + `InfoRequest` / `InfoEvent` 两条消息类型
- `api/client.rs`：`post_multipart`（边界交给 reqwest）、`get_bytes`（封面图，8M 上限、
  **不带 cookie**）、`main_base()`（图床在主站，测试要能顶掉）
- `api/room.rs`：`RoomInfo.cover`（`user_cover` 字段）、`room_id` 以服务端回的为准
  （配置里可能是短号）
- `main.rs`：`info_task` —— 拉 meta + 抓封面图（同址不重抓）、改标题、
  **两步换封面**（本地图先上图床）、成功顺手刷新房间信息；短号换成规范号再写
- `ui/control.rs`：信息栏从骨架变真（提交带 `Action` 出去、40 字按键拦截、
  `on_info_event` 只动显示状态、进栏自动拉一次 meta）、下半格接上封面预览
- `ui/cover.rs`（新）：解码 + 区域平均缩图 + 按控件尺寸现采样成半格字符
- `Cargo.toml`：显式加 `image`（`--no-default-features --features jpeg,png`，
  它本来就在 qrcode 的依赖树里）
- 31 个新测试（153 个全绿），`cargo clippy --all-targets` 干净

### 已完成（2026-10-03，第六轮：开播 / 下播 / 推流码）

- `api/appsign.rs`（新）：直播姬那套 app 签名 —— 写死的 appkey/appsec、
  **Go `url.Values.Encode()` 口径**的表单编码（`~` 不转义、`*` 转义、空格 `+`）、
  `sign = md5(query + appsec)`。三组 **python 独立算的黄金值**钉着算法
  （外加跟 WHATWG 编码的对照——那正是不能直接用 `url` crate 那套的原因）
- `api/live.rs`（新）：`live_version`（app 签名 GET）、`start_live`（app 签名 POST +
  `StartOutcome::Started / Verify`）、`stop_live`（**不带签名**）、
  `assemble_streams`（rtmp + protocols 多路、`?` 开头 / `/` 结尾那两个怪情况）、
  `face_auth_url`（60043 的地址模板）、`LiveRequest` / `LiveEvent` 两条消息类型
- `api/client.rs`：`post_body` —— 发一段**已经拼好**的表单体。
  签名是对着拼好的串算的，交给 `post_form_raw` 重编码（WHATWG 口径）就对不上了
- `main.rs`：`live_task` + `canonical_room` —— 开播 / 下播都先 `get_info` 拿**规范房间号**
  （配置里那个可能是短号），成功顺手让房间信息那条只读链重拉一次
- `ui/control.rs`：`F4` 开播（先在 ratatui 里自搭的确认层里确认，默认选中「取消」）、
  `F5` 下播（不用确认），两个键都是全局的；推流码栏从占位变成真的
  （进栏查一次状态、开播成功后自动切过去、地址 / 密钥 / 完整 URL 一行一个不折行、
  两种验证的二维码画到账号栏并说「扫完再按 F4」、失败只写顶栏那一句）；
  两个键的提示补回 `Tab::Stream::hint`
- 37 个新测试（190 个全绿），`cargo clippy --all-targets` 干净

### 已完成（2026-10-03，第七轮：OBS 联动）

- `obs.rs`（新）：obs-websocket 5.x 的 JSON 客户端 —— `Hello / Identify / Identified /
  Request / Response` 五个操作码、`auth_string`（黄金值钉住）、
  `set_stream_request`（纯函数拼 `SetStreamServiceSettings`）、
  `resolve` / `resolve_at`（读 OBS 那份 `config.json`，端口 / 密码留空才读，
  `server_enabled: false` 和「没文件」各给一句人话）、`fill` / `fill_resolved`
  （连接 3 秒、每步读写 5 秒的超时；`result: false` 当失败）。
  非测试部分约 320 行（超了 400 才需要拆目录，现在一个文件够）—— 测试跟别处一样内联
- `main.rs`：`ObsFill`（配置 + 那行字的通道）+ `spawn_obs_fill` —— 开播成功后才动手、
  取第一路 rtmp、**另起一条任务**跑（不拖慢开播那条链的结果）、
  结果只发进 `mpsc<String>`
- `ui/mod.rs`：`Wiring` 多一条 `obs_notes`（**不并进 `LiveEvent`**：它不是开播的结果）
- `ui/control.rs`：`obs_note` 字段 + `on_obs_note` —— 那一行字摆在推流码栏末尾，
  动不了顶栏那句「已开播」；`Started` / `Stopped` 时清掉
- 15 个新测试（205 个全绿），`cargo clippy --all-targets` 干净
- **真机探针**（临时 `--smoke-obs` 分支，跑完已删）：拿假 `XDG_CONFIG_HOME` 跑了
  四组，见下面「实测过」

### 已完成（2026-10-03，第八轮：退出登录）

补的是账号栏那个洞：**cookie 还有效时回车不出码**（`login_loop` 回「已登录，无需重复扫码」，
这句提示照 Go 版留着），于是想换账号 / cookie 脏了想重扫只能拿临时配置绕
（`-c /tmp/xxx.toml`）—— tc191 自己刚踩过。

- `config.rs`：`Config::clear_cookie(path)` —— 形状照 `save_area`（**先重读再写**），
  只把 `cookie` 清成空串，别的字段一个都不动
- `main.rs`：`apply_login` 拆进新的 `session_task`（扫码登录和退出登录**同一处落点**，
  两条通道 `creds` / `logout` 一起 `select`），新增 `apply_logout` 做四件事：
  清内存凭据（`client.set_cookie("")`，连 nav 缓存一起）→ 只清配置里的 cookie →
  房间信息补一次手刷 + 弹幕那条链路整条重启 → 界面回「未登录（回车扫码）」
  并顺手再要一张新码
- `ui/mod.rs`：`Wiring` 多一条 `logout`（**新开一条通道**，不拿空串去挤 `creds`）；
  `Action::Logout` 落成 `try_send`，送不出去就 `logout_dropped()` 放掉「正在退出」
- `ui/control.rs`：账号栏变成两个可选项（`▸ 重新扫码` / `退出登录`，`↑↓` 选行、
  回车执行）；确认层从「开播专用」抽成 `ConfirmKind::{StartLive, Logout}` 共用一个框
  （默认仍落在「取消」）；退出登录的文案把后果说清楚（清掉之后到重新扫码之前弹幕会断）；
  `Tab::Account::hint` 改成「↑↓ 选一项 回车 执行」
- 11 个新测试（216 个全绿），`cargo clippy --all-targets` 干净
- **顺手修掉一个自锁**：`auth.send_replace(auth.borrow().wrapping_add(1))`（原来
  `apply_login` 里就是这么写的）读借用活到语句结束、`send_replace` 又要拿写权，
  同一个线程把自己锁死 —— 扫码登录**成功那一刻**进程会一声不响地卡住。
  现在收进 `signal_credential_change`（用 `send_modify`），退出登录那条测试把它钉住

### 已完成（2026-10-03，第九轮：cookie 整罐 + 弹幕滚动）

两件事，行为参照物都是 Go 版，但**没有抄代码**。

- **cookie 整罐存**（见第 18 条）：`api/login.rs` 的 `cookies_from_set_cookie` /
  `cookies_from_redirect` / `merge_cookie` 从「白名单挑五个字段」改成「服务端给什么
  就存什么」；新增 `split_cookie`（拆罐，控制字符照旧掐掉）和 `has_cookie`（判登录成功
  只看这一趟新拿到的）；删掉 `COOKIE_NAMES`，换成只挡路由参数的 `NON_COOKIE_QUERY`。
  `api/client.rs` 新增 `raw_cookie()`（登录合并时当底，这一趟没重发的字段不丢）。
  配置文件格式**没变**，还是 `cookie = "k=v; k=v"`。
- **弹幕滚动**（见第 19 条）：`ui/mod.rs` 新增 `Viewport { top, follow }` +
  `view_top` / `hidden_below` / `scroll_by` / `on_mouse`；`main_layout()` 把主页面
  各块的矩形抽出来（画图和滚轮热区共用一份）；弹幕框右边界内侧画滚动条
  （`thumb()` 算位置，`█` 滑块 / `│` 轨道，不满一屏不画）；上翻时标题写
  「已上翻 N 行 · 滚到底恢复跟随」。
- **鼠标捕获**：`setup(cfg.mouse)` 开 `EnableMouseCapture`，`restore()` 第一件事是
  `release_mouse()`，并且装了 panic 钩子（走同一个 `restore`）—— 退出和 panic 都不会
  把终端留在捕获状态。「还回去了没有」有可查的状态（`MOUSE_CAPTURE`）。
  事件循环改成**一次把积压的事件收干净再重画**（鼠标移动事件是 1003 一起开的）。
- `config.rs` 新增 `mouse`（默认 `true`），注释里写清楚默认开的代价（要选中文得
  **按住 Shift 拖**）。
- 15 个新测试（231 个全绿），`cargo build` / `cargo clippy --all-targets` 干净。
- `api/test_http.rs` 新增 `start_with_headers`（原来那套 `(状态码, body)` 表达不出
  `Set-Cookie`，而登录那条链的凭据正藏在响应头里）；`start` 收成它的一个薄壳，
  几十处老调用一处都没动。
- **真终端验过滚轮**（临时探针 + python pty，探针跑完已删）：滚三格 -> 标题变成
  「已上翻 9 行」、屏幕上是 80 行里的第 50…70 行、右侧 `█` 滑块正好落在算出来的
  那 5 格上；指针挪到左半边再滚、在配置页上滚，视口都不动；`Ctrl+C` 之后字节流里
  能看到 `?1000l` / `?1006l`（鼠标还回去了）和 `?1049l`，退出码 0。
- **顺手补掉一个自己写出来的 panic**：滚动条那个 `area.right() - 2` 在**一列宽**的
  终端（高度够、弹幕又满一屏）上会下溢 —— debug 下直接整屏消失。现在框窄到 3 格
  以下就不画，`tiny_terminal_does_not_panic` 多钉了 `(1,20) (2,20) (3,20) (4,20) (120,14)`
  这几组「窄而高」的尺寸（原来那几组高度都不够，滚动条那段代码根本没走到）。
- **顺带看到的（不是这一轮碰出来的）**：上面那次 pty 验证一开始拿真房间跑，
  房间 6（短号 `6` 和规范号 `7734200` 都试了）**45 秒一条弹幕都没来、
  也没有任何系统提示** —— 弹幕那条链路既不投递也不报错（`pump` 不等服务端那句
  认证回话，`connect_ws` 本身也没有超时），所以只能临时塞一屏假弹幕来验滚轮。
  `api/danmaku.rs` 这一轮**一行都没动**，但下一轮要验真弹幕前先知道这件事：
  上面「实测过」那节里记着今天早些时候还收得到真弹幕（`op=8 {"code":0}` + 真报文），
  而现在这个出口 IP 上 `getDanmuInfo` 不带签名是
  `-352`（见「实测过」那节），ss 里能看到连接是 ESTABLISHED 的（本机走 `198.18.0.x`
  那种代理网段）—— 多半是风控把内容扣住了，不是代码。

### 实测过（真接口）

**cookie 里的设备号到底从哪来（2026-10-03，第九轮，全程只读：空 cookie + curl）**。
这一轮把 cookie 改成整罐存取（见第 18 条），顺手拿几个只读接口看了响应头，
想知道「整罐存能不能真拿到 `buvid3` / `buvid4` / `b_nut`」：

```bash
UA='Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/96.0.4664.110 Safari/537.36'
curl -s -D - -o /dev/null '<接口地址>' -H "User-Agent: $UA" -H 'Accept: */*' \
     -H 'Origin: https://live.bilibili.com' -H 'Referer: https://live.bilibili.com/'
```

```text
GET api.bilibili.com/x/web-interface/nav                        -> 200，**没有任何 Set-Cookie**
GET api.live.bilibili.com/room/v1/Room/get_info?room_id=6       -> Set-Cookie: LIVE_BUVID=AUTO1790…（直播侧的设备号）
GET api.live.bilibili.com/xlive/…/getDanmuInfo?id=6&type=0      -> 不带 WBI 签名时 code=-352，无 Set-Cookie
GET passport.bilibili.com/x/passport-login/web/qrcode/generate  -> 200，**没有 Set-Cookie**
GET api.bilibili.com/x/frontend/finger/spi                      -> body 里给 b_3 / b_4（就是 buvid3 / buvid4 那对）
GET www.bilibili.com/                                           -> Set-Cookie: buvid3=BA59A1E3-…; Set-Cookie: b_nut=1790981010
```

结论（**「没有」也得记下来**，免得下一轮又指望整罐存能把它们捞回来）：

1. **`buvid3` / `buvid4` / `b_nut` 三个，程序真会调的那些只读接口一个都不返。**
   `buvid3` / `b_nut` 是 `www.bilibili.com` 那个页面发的；`buvid3` / `buvid4`
   还能从 `x/frontend/finger/spi` **主动要**（body 里的 `b_3` / `b_4`）。
2. 整罐存真正能拿到、以前会丢掉的，是 `Room/get_info` 那种响应头里的 `LIVE_BUVID`
   （直播侧设备号）—— 但**这一轮的整罐只作用于扫码登录那一下**：房间信息是 30 秒
   一轮的只读轮询，响应头谁也不存，所以它现在也落不了盘。
3. ws 认证包里那个 `buvid` 字段（`danmaku.rs`）**跟 Go 版一样是空的**，
   不是这一轮漏的（Go 版 `getter.go` 里那个 `Buvid` 也从来没赋过值）。
4. **要不要主动去要一个设备号**（`x/frontend/finger/spi`，或者把浏览器里的
   `buvid3` 抄进 `config.toml`）**由 tc191 定** —— 这一轮只把「整罐不再丢字段」
   做完，没有加任何新的接口调用。

nav、房间信息（在播/未播两种）、观众榜（3 人 / 50 人两种）、getDanmuInfo（没 -352）、
wss 认证（`op=8 {"code":0}`）、**真实弹幕解析**（含 `[dog]` 这类表情标签）、
历史弹幕、75 秒长连接不中断（心跳正常）。

发弹幕（2026-10-03，**就打了一次**）：真 cookie + **故意换成假 `bili_jct`** + 空 `msg` 的
`POST /msg/send`（URL 带 WBI 签名），服务端回
`{"code":-111,"message":"csrf 校验失败"}` —— 说明签名和浏览器头过了风控（不是 -352）、
表单也被认了，只有被我们改坏的那个字段不认。

**2026-10-03 真机补验（tc191 自己在房间 6 发的）**：发弹幕正常，`code == 0`，
而且**自己发的那条从弹幕 websocket 回显回来了** —— 这条链路到此为止全部走过真的。
`rnd` 用毫秒（跟 `bili-live-hime` 一致、跟 biligo 的秒不一样）也确认没问题，不用再去试秒。

扫码登录（2026-10-03，**只读**：匿名 cookie，generate + poll 三次，没人去扫那张码）：

```
generate OK  url=https://account.bilibili.com/h5/account-h5/auth/scan-web?navhide=1&callback=close&qrcode_key=16886d…&from=
poll#0  code=86101  message="未扫码"  set-cookie=[]
poll#1  code=86101  message="未扫码"  set-cookie=[]
poll#2  code=86101  message="未扫码"  set-cookie=[]
```

也就是说：`generate` 给的 `url` 和 `qrcode_key` 都在（而且两处的 key 是同一个），
`poll` 的外壳 `code` 恒为 0、真正的状态在 `data.code` 里 —— 上面这条实测就是
「只看外壳 `code` 会一直以为还没扫码」的直接证据。
顺带量了真地址的码有多大：Low 41 模块（49 列 × 25 行），Medium 49 模块（57 列 × 29 行）。
**没有真的扫过**：`86090` / `0` / `Set-Cookie` 这三条路只有假服务器上的单测覆盖。

分区表（2026-10-03，**只读**：空 cookie 的 `--smoke-area` 探针）：

```
parents=12   subs=450   parents_without_children=0
网游 -> [(86, "英雄联盟"), (329, "无畏契约"), (878, "三角洲行动")]
手游 -> [(35, "王者荣耀"), (1034, "王者万象棋"), (292, "火影忍者手游")]
单机游戏 -> [(236, "主机游戏"), (235, "其他单机"), (216, "我的世界")]
```

- 不带参数、不用登录就返回（`code=0`，响应 125KB）。**子分区的 `id` 是字符串**
  （`"id":"86"`），父分区的 `id` 是数字 —— `int_of` 两种都收，这条就是它的证据。
- 树的默认状态在真表上也看了一遍（探针顺手打印可见行，`>` 标光标）：
  - `area_id = 0`：13 行（「全部分区」+ 12 个父分区，都是 `▸`），光标在「全部分区」上
    —— 没有任何一个父分区被顺手展开
  - `area_id = 371`：只有「虚拟主播」（第 6 个父分区，**不是第一个**）是 `▾`，
    光标停在「虚拟日常」上，`select()` 给 `(371, "虚拟主播/虚拟日常")`
    —— Go 版那两个坑在这儿一个都看不到
- 界面那一半在**临时配置**上真跑过一遍（`script` + pty，`-c /tmp/smoke-pick.toml`，
  自己的配置一个字没动）：Shift+Tab → Tab 进分区栏，右栏出现
  「▾ 全部分区 ▸ 网游 ▸ 手游 …」，顶栏写「分区表已加载：12 个父分区，回车 选定」；
  按 `↓`（网游）`→`（展开，画面上变成 `▾ 网游`）`↓`（英雄联盟）回车，
  右栏就多出「英雄联盟」、提示「开播分区已设为 网游/英雄联盟」，
  临时配置里多出 `area_id = 86` / `area_name = "网游/英雄联盟"`，别的字段原样
  —— 「界面 -> 通道 -> 落盘」整条通了。

**封面相关的两个只读探针（2026-10-03，没碰账号上任何东西）**：

```
GET room/v1/Room/get_info?room_id=6   （空 cookie）
  title      = '【预告】10月3日德玛西亚杯'
  user_cover = 'https://i0.hdslb.com/bfs/live/55adc2ec0e24a7f329bf35742472205492b4526b.png'
  cover      = 字段不存在（None）
  room_id    = 7734200   ← 配置里那个 6 是短号
```

也就是说：封面字段确实是 `user_cover`（老文档里的 `cover` 真的不返了），这次拿到的是
https 的绝对地址（协议相对的形状是 Go 版见过的，这里没见过）；房间号那条也能确认
「配置里可能写短号」这件事是真的。

```
GET <上面那个封面地址>  带 BROWSER_HEADERS 那一套（UA / Origin / Referer / accept）
  → HTTP 200  image/png  206565 字节  470x264 PNG
```

拿真图跑过一遍 `ui::cover` 的解码 → 缩放 → 采样（临时探针，跑完删了）：
470×264 → 96×53，画进 60×12 的框是 12 行、每行 51 格 —— 图床不挑我们的请求头，
`image` 的 png 解码也没问题。

**界面那一半也在临时配置上真跑过**（`-c /tmp/smoke-info2.toml`，`room_id=0`，
所以整条链一个请求都没发）：`F6` 直接跳到直播间信息栏，右栏画的是
「▸ 标题 （空）/ 封面 （空）/ 两行说明 / ┌ 当前封面 ┐ 还没有封面」，
顶栏那条消息是「读不到当前标题 / 封面，可以直接输入：还没设置直播间号（config.toml 里的 room_id）」
—— 「进栏 -> 要 meta -> 失败只写一句话」这条通着。另外跑过一遍
「进编辑打字 -> `Esc`」：那一行先跟着按键变、`Esc` 之后回到「（空）」，
顶栏写「已取消，没有改动」。

**第六轮（开播 / 下播 / 推流码）的界面在 pty 里真跑过一遍**（2026-10-03，
`script` + `stty rows 34 cols 110`，`-c /tmp/smoke-live.toml`，**`room_id = 0`**，
所以开播 / 下播那两条链一个请求都发不出去；顺手真调的只有分区表那次只读 GET）：

- `Shift+Tab` → `Tab`×3 进推流码栏：右栏先写「正在查开播状态…」，
  紧接着变成「读不到开播状态：还没设置直播间号（config.toml 里的 room_id）/
  回车 重试 / 别的都不受影响：开播 / 下播照样能按」，顶栏换成
  「开播状态没查到：回车 重试    F4 开播    F5 下播    Tab 换功能」
  —— 「进栏自动查一次 + 失败只影响那一处 + 提示补回 F4/F5」这三件都通着。
- `F4`（没登录时）：顶栏写「还没登录：先到「账号」栏扫码登录（回车 重新扫码）」，
  **不弹确认层、不发任何请求** —— 正是设计要的那个顺序。
- 整屏没 panic、`Ctrl+C` 干净退出。
- **没验到的**：确认层本身（要 `logged_in` 为真才会出现，而那需要一份真 cookie）
  只在 `TestBackend` 上画过；真终端里那个 44 格宽的框长什么样、长密钥被截断那一下
  好不好看，得人眼看过才算。

**OBS 联动的四组探针（2026-10-03，第七轮，`--smoke-obs` / `--smoke-obs-fill`，
跑完已把探针删掉）**。探针只跑 `obs::resolve` 和 `obs::fill` 的**报错那几条路**，
一次都没往真 OBS 发请求（那会改掉 tc191 的推流设置）：

```text
1) 真 XDG（这台机器上那份 ~/.config/obs-studio/.../config.json）：
   resolve OK host=127.0.0.1 port=4455 密码长度=16     ← 读对了（密码没打印，别打印）
2) 假 XDG：目录里没有 obs-websocket/config.json
   resolve/fill ERR 没找到 OBS 的 WebSocket 配置（<路径>），
                    先去 OBS 里 工具 → WebSocket 服务器设置 打开它
3) 假 XDG：server_enabled=false
   resolve/fill ERR OBS 里的 WebSocket 服务器没开：工具 → WebSocket 服务器设置
                    → 勾上「启用 WebSocket 服务器」
4) 假 XDG：server_enabled=true + server_port=1（没人听）
   fill ERR 连不上 OBS 的 WebSocket（ws://127.0.0.1:1/）：IO error: Connection refused
```

四组都是「一句人话」，没有一个 panic。**顺带发现一件跟任务前提相反的事**：
这台机器的 `~/.config/obs-studio/plugin_config/obs-websocket/config.json` 里
`server_enabled` 是 **true**、端口 4455，而且 `ss -ltnp` 显示 `/usr/bin/obs`（pid 676241）
正听着 4455 —— 也就是说 OBS 这边的 WebSocket 服务器**现在是开着的**
（Go 版那轮说「没开」，情况已经变了）。所以「真连一次只会得到去打开它的提示」
这条**没法照原样验**：真要连是连得上的，而连上之后 `fill` 就会**真的改掉他的 OBS 推流设置**，
没人授权这么干，就没连。

退出登录（2026-10-03，**全离线、pseudo-tty 里真跑**，第八轮）：
拿 `-c /tmp/bililive-logout-smoke.toml`（临时配置，cookie 是假的、`room_id = 0`）
起真二进制，用 pty 喂键，逐帧读回屏幕文字核对：

```
Shift+Tab      -> 账号栏：▸ 重新扫码 / 退出登录 都在，▸ 在「重新扫码」上
↓              -> ▸ 挪到「退出登录」
回车           -> 确认层：「退出登录会清掉本地保存的 Cookie，在你重新扫码
                  登录之前，弹幕会断开。」+ [ 退出登录 ]  [ 取消 ]（默认「取消」）
回车（默认）    -> 顶栏「已取消退出登录」，临时配置里的 cookie **一个字没动**
回车 → ← → 回车 -> 真退出：临时配置里 `cookie` 变成空串、别的字段原样，
                  账号行回到「未登录（回车扫码）」，顶栏「已退出登录，弹幕已断开；
                  正在生成新的二维码」，右栏自动出现一张新二维码
```

**这条探针顺带把 `signal_credential_change` 那个自锁坐实了**：用修之前编出来的二进制
跑同一条链，现象是「配置里的 cookie 已经清掉了、界面上却什么都没变」——
会话任务卡在 `auth.send_replace(auth.borrow()…)` 上（同一个线程读锁没还就要写锁），
`set_cookie` / 落盘都做完了，后面通知链路和界面那两句永远发不出去。
**没有拿 tc191 真在用的那份配置跑过**（那份现在是好的）。

### 风控/网络：这台机器的 B 站流量在绕代理（重要）

2026-10-03 实测：`api.live.bilibili.com` / `api.bilibili.com` / `passport.bilibili.com` 在这台机器上
解析出来是 `2001:2::1f` 这种 **Clash 的 fake-ip 段**（`ip route` 里也有 `198.18.0.0/30 dev Meta`）。
绕开 fake-ip、用真实 IP 直连同一个接口 → HTTP 200。

也就是说：**程序对 B 站的请求其实是从机场出口出去的**，B 站看到的 IP 在国外机房。现象上对得上两件事：

- 弹幕 websocket 连上了（ESTABLISHED、认证 op=8 code 0），但**一条内容都不来、也不报错**（风控掐着内容）
- 开播频繁要求人脸认证（IP 归属地飘 = 账号异常信号）

排查这类「连上了但没数据」「莫名其妙要验证」的问题时，**先看 DNS 解析到哪一段**，别一上来怀疑代码。
修法是 Clash 规则里给 `bilibili.com` / `hdslb.com` / `bilivideo.com` 这些后缀配 DIRECT —— 那是 tc191
的网络配置，改之前先看他配置怎么组织的。

### 还没验过

**2026-10-03 全部验过了**：tc191 自己按回车改了一次标题、换了一次封面，都成功。

- `room/v1/Room/update` ✅ —— 表单、csrf、返回码都真的走通了。写操作用的是**服务端回的规范号**
  （配置里那个 `6` 是短号，`get_info` 回的 `7734200` 才是能写的那个），这条选对了。
- `x/upload/web/image` ✅ —— 真传了一张图。`image_url` 与 `location` 谁先谁后仍然没细分
  （`pick_image_url` 两个都收，哪个先算哪个），但**图床这条路是通的了**。
- `xlive/app-blink/v1/preLive/UpdatePreLiveInfo` ✅ —— **不需要 app 签名**，光带 csrf 就过。
  第 14 条写的依据到此从「Go 版的经验」升级成「实测」。
- 封面预览 ✅ —— 真网络下从进栏到画出来走过了（我在 pty 里跑过：`F6` 进栏，67 格宽 × 20 行）。

**留给下一轮的注意点**：开播那套要用直播姬的 `appkey` + `appsec`（`live/client.go` 那对），
跟这三个接口用的 web + csrf 是**两套签名**，别串。

**第六轮（开播 / 下播 / 推流码）—— 一条都没在真账号上验过**，全绿的是假服务器上的单测：

- **app 签名没有真调过一次**。它只在单测里对着 python 算的黄金值走通；
  服务端到底认不认（`code 0`）还是回一句「签名校验失败」，没验过。
  **唯一允许的探针**是版本号那个 GET（只读，见上面「怎么手工验真接口」第 4 条）——
  值得验一次：签名规则错的话，错误只会出现在服务端那句含糊的话里，
  本地单测是发现不了的。
- `startLive` / `stopLive` **一次都没真调过**（开播会让直播间立刻对外可见、给粉丝推推送，
  AI 一律不许碰）。所以下面这些全是**假服务器上的形状**，真机上可能还有坑：
  - `protocols` 数组在真响应里到底有几路、`protocol` 的取值是不是 `rtmp` / `srt`；
  - `ts` / `version` / `build` 这三个字段服务端会不会挑（比如版本号过期）；
  - `stopLive` 不带签名这个结论**来自 Go 版**（它实测过），我们没验；
  - 下播之后推流码是不是立刻就失效了（OBS 那边会不会被踢），没验。
- **两种验证（60024 / 60043）只有假服务器上那两条**：
  - `60024` 的地址真在 `data.qr` 里吗？
  - `60043` 的响应里真的没有二维码、真的要靠 nav 的 mid 拼吗？
    拼出来的地址能不能扫、扫完是不是就能开播，都没验过。
  - 这两种是不是每次开播都会出现（还是只有换设备 / 异地登录才出现）也不清楚。
- **开播成功后推流凭据的真实形状**：`rtmp.code` 是不是真的以 `?` 开头、
  `addr` 是不是真的可能以 `/` 结尾 —— 这两个怪情况是照 Go 版的注释写的，
  没见过真的响应。
- 界面那一半全是在 `TestBackend` 上画出来读回来的（确认层、推流码栏、验证码画到账号栏），
  真终端里的观感（尤其是 44 格宽的确认框、长密钥被截断那一下）没看过。
- 封面预览只验过**假服务器给的**图 + 一张手工抓下来的真图（见上面「实测过」），
  「从进栏到画出来」这条端到端没在真网络下走过。

**第七轮（OBS 联动）——2026-10-03 真 OBS 上验过了**：

我当时手搓了一个最小 obs-websocket 客户端（python，没依赖）打本机 OBS，全程只读 + 一次
「原样写回」，结论：

- 握手 → `Hello(op 0)` → `Identify(op 1)` → `Identified(op 2)` → `Request/Response` 全程通，
  obs-websocket **5.7.4**、OBS **32.2.2 (CachyOS)**
- `GetVersion` ✅、`GetStreamServiceSettings` ✅
- **`SetStreamServiceSettings` ✅** —— 把读到的值**原样写回去**（净改动为零），
  OBS 回 `requestStatus.result = true`，写完再读回来跟写之前逐字节一致。写这条路通了。
- 顺带一条硬证据：**tc191 自己那组能用的推流设置，形状跟我们填的一模一样** ——
  `rtmp_custom` + `rtmp://live-push.bilivideo.com/live-bvc/` + 一串 94 字符、以
  `?streamname=` 开头的密钥。也就是说「密钥整串原样塞进 key、不自己拼」这条规则，
  他的实际配置就是活证据。
- 他那台是 `auth_required = false`，所以**鉴权那条分支真机上没走到**（黄金值只对过 python
  和假 OBS）；哪天他把鉴权打开，这条才算真验过。
- **现在这台机器上是连得上的**（OBS 正跑着、`server_enabled: true`、4455 在听，见上面
  「实测过」那段），所以验一次的门槛很低 —— 但 `fill` 会**真的改掉 OBS 的推流设置**，
  得 tc191 自己愿意的时候跑（跑之前先看清原来那组 server / key，跑完能对回来）。
- 「OBS 拒绝了这次填写」（`requestStatus.result = false`）只有假 OBS 造出来过；
  真 OBS 什么情况下会拒（比如请求里有它不认的字段）没见过。
- 超时那两个常量（连接 3 秒 / 每步 5 秒）是拍的，没在真机上量过。
- 界面那一行字只在 `TestBackend` 上画出来读过；真终端里那行长文本的观感没看过。

- ~~真发一条弹幕~~ —— 2026-10-03 验过了，见上面「实测过」。
- ~~扫码登录的落地那半段~~ —— **2026-10-03 真机扫过了**（tc191 拿空 cookie 的临时配置扫的）：
  `86090`（已扫码待确认）→ `0`（成功）整条走通，**cookie 真的写进了配置文件**
  （`SESSDATA` + `bili_jct` 都在）。至于是 `Set-Cookie` 那条还是跳转 URL 的 query 那条送来的
  （两条都实现着，后者是兜底），这次没细分 —— 结果是好的。
- 顺带记住这个验证办法：**cookie 还有效时账号栏按回车不会出码**（「已登录，无需重复扫码」，
  跟 Go 版一致）。要验登录就用空 cookie 的临时配置：`bililive -c /tmp/某个.toml 6`，
  扫描结果写进那个文件，**不碰真配置**。
- **没有「退出登录」**：想换账号 / cookie 脏了想重扫，现在只能靠临时配置绕。待办。
  `code` 为 0 的那条路、以及自己发的弹幕从 websocket 回显回来，都还没走过
  （要验就按「怎么手工验真接口」那一节的规矩，一条，验完记结果）
- **protover 2 的 zlib 真实数据**：实测那几个房间发的都是 `ver=0` 明文单包，
  解压路径目前只有单测覆盖（自造包），没吃过服务端的真包
- `SEND_GIFT` / `COMBO_SEND` / `GUARD_BUY` / `NOTICE_MSG` / `USER_TOAST_MSG` 的真实报文
  （凌晨的房间只有普通弹幕），字段名对着 Go 版抄的，第一次真收到时要盯一眼
- **断线重连**：只走了代码和退避逻辑，没有真拔过网
- **扫码登录的成功那一半**：真接口只验到 `86101 未扫码`（没人去扫）。
  真正扫码之后的 `86090` / `0`、以及凭据到底从 `Set-Cookie` 来还是从跳转 URL 来，
  都只有假服务器上的单测覆盖。要真验一次得用手机扫，扫完记得把结果记进来
  （只读，不改账号上任何东西）
- **登录成功后弹幕链路真的重连了没有**：`supervise_danmaku` 的 abort + respawn
  没有在真终端里跑过（这个沙箱起不了 TUI），只有代码路径
- TUI 本体没在真终端里跑过：这个沙箱的 pty 下 `crossterm::enable_raw_mode()` 会**挂住**
  （原骨架一样，不是本轮的改动），界面是靠 `TestBackend` 测试验的
- **第二页在真终端里的观感**：布局是拿 `TestBackend` 画进内存再读回来核对的
  （左栏 16 格、▸ 只出现一次、右栏标题跟着栏走都对上了），但真终端里
  `││` 这道双竖线（左栏和右栏各有一条边框）好不好看，得人眼了才作数

### 已知缺口 / 下一步

1. **`INTERACT_WORD_V2` 收不到**：现在房间发的是 V2，名字在 protobuf 的 `pb` 字段里，
   不是 JSON 的 `uname`。所以「XXX 进入了房间」这类提示目前**不会显示**。
   要显示得手写一小段 protobuf varint/length-delimited 解码（Go 版同样没处理 V2）。
2. ~~写操作还差一条：OBS 联动~~ —— 2026-10-03 第七轮接完了（见上面第 16 条）。
   留下的口子见下面 2.4。
   （发弹幕第二轮接完，扫码登录第三轮，分区栏第四轮，直播间信息栏第五轮，
   开播 / 下播 / 推流码第六轮，OBS 联动第七轮。）
2.4 **第七轮（OBS 联动）留下的口子**：
   - ~~跟真 OBS 一次都没连过~~ —— 2026-10-03 验过了（见「还没验过」那节开头）。
     剩下没走到的只有 `auth_required = true` 那条鉴权分支。
   - 连真 OBS 的**代价**跟别的写操作不一样：它改的是本机 OBS 的推流设置
     （服务器 + 密钥），不改 B 站账号。所以「谁来验」得说清楚：只能 tc191 自己在
     真要开播的时候顺手看（填完看一眼 OBS「设置 → 推流」是不是换成了 B 站那组），
     AI **不许**代为连一次（那会把他的推流设置改成测试值）。
   - 只填了**第一路 rtmp**：多路 rtmp（`rtmp-2`）和 srt 那几路都没填。B 站主推流
     就是第一路，暂时够用；要支持「OBS 里选哪一路」得先在界面上让他选。
   - OBS 的 `server_enabled: false` 只在**配置里没写端口 / 密码**时才会看出来
     （两样都写了就不读文件，也就看不到这个开关）。这是照 Go 版的行为抄的：
     写死了就是用户自己负责。
   - 失败提示只写在推流码栏末尾；那一刻用户要是在别的栏，得多按一次 `Tab` 才看得到
     （顶栏那句「已开播」有意没被顶掉）。要不要顺手也写一句顶栏，看下一轮。
2.0 **第五轮留下的三个口子**（都不影响用，按需接）：
   - 信息栏没有「重新拉一次当前标题 / 封面」的键：一次运行只自动问一次
     （拉失败后切走再切回来会重试）。要补就照分区栏那套（回车重试 / F6 重来）。
   - 封面预览的解码跑在界面的事件循环里：几百 KB 的图几十毫秒，
     真碰到超大图会卡一帧。要治就把它挪到信息任务里（解完只把缩好的图传过来）。
   - 预览那张图**抓失败之后不会再自动重抓**（任务那边按地址去重，失败也算「抓过了」，
     跟 Go 版 `ui/cover` 的 `loaded` 一个口径）：网络抖一下之后要等地址变了
     （改一次封面 / 重启）才会再试。要治就在失败时不记那个地址。
   - ~~「退出登录」还是没有~~ —— 2026-10-03 第八轮接完了（见上面第 17 条）。
     改标题 / 换封面在没登录时会直接在顶栏说「Cookie 里没有 bili_jct…」，
     这时先到账号栏「退出登录」清干净再扫码重登（不用再去动临时配置）。
2.1 **第二页还差两栏**：
   - ~~分区栏~~ —— 2026-10-03 第四轮接完了（见上面第 13 条）。剩一个小口子：
     分区表是**每次运行拉一次**（切进这一栏时自动拉，失败了回车重试），
     中途没有「刷新分区表」的键 —— Go 版的 F3 是「手动重来一次」（会重拉，
     代价是丢掉手工展开的状态），要不要补看下一轮怎么定。
   - ~~直播间信息栏~~ —— 2026-10-03 第五轮接完了（见上面第 14 条）：改标题、
     换封面（本地图先传图床）、下半格画当前封面都做了。留下的口子见上面 2.0，
     以及**这三个写接口一个都没在真账号上验过**（见「还没验过」那一节）。
   - ~~推流码栏~~ —— 2026-10-03 第六轮接完了（见上面第 15 条）：开播 / 下播、推流凭据、
     两种验证都做了，`Tab::Stream::hint` 里的 F4 / F5 也补回去了。
     留下的口子见下面 2.3。
2.3 **第六轮留下的口子**：
   - 开播成功后那一栏**只在手上摆着凭据时**有内容；重启程序（或者在别处开的播）
     就只能看到「正在直播中，凭据只有开播那一下返回一次」。要从服务端再要一组，
     只能 F5 → F4（Go 版也一样）。
   - 开播 / 下播**没有超时保护**：`startLive` 慢的时候界面停在「正在开播…」，
     这期间 F4 / F5 都被挡住（不会重复发，但也没有「取消」）。要治就给任务加个
     `tokio::time::timeout`。
   - 确认层只认 `Tab` / `←→` / `回车` / `Esc`：`h` / `l` 这些键没接（别的 TUI 有）。
   - 推流码栏窄终端下密钥会被**截断**（不折行那条的死角）：终端比
     密钥长时不丢，短了就丢尾巴 —— 要治得自己做横向滚动。
2.2 ~~**第二页的账号栏只有「扫码登录」，没有「退出登录」**~~ —— 2026-10-03 第八轮接完了
   （见上面第 17 条）。留下这几个口子：
   - 退出登录之后**不会自动把弹幕列表清掉**，房间信息 / 观众榜也还留着上一版的：
     看着像「还在连着」，其实那条 wss 已经在用空凭据重连了。要治就在
     `LoggedOut` 那条事件里顺手把这些显示状态也清一遍。
   - 退出登录**只清 `config.toml` 里的 cookie**，别处的痕迹一概不管
     （Go 版也没有别的地方）。
   - 退出时如果扫码任务**正在轮询一张旧码**，新码是排在它后面生成的
     （`login_start` 只有一格，退出那下 `try_send` 会被丢掉）；
     屏幕上那张旧码这时仍然有效，扫了照样能登回来。要治得让
     `login_loop` 在轮询中间也能被新信号打断（Go 版靠 `qrGen` 计数作废旧那一轮）。
   - 没有「退出登录之后再自动退出发送链路」：`send_loop` 每次现取 `csrf()`，
     没凭据时就往弹幕框里说「Cookie 里没有 bili_jct…」，行为是对的，不用管。
3. ~~弹幕列表不能滚动/翻页，只能看最后几行~~ —— 2026-10-03 第九轮接完了
   （滚轮，见第 19 条）。留下的口子见下面 2.5。
2.5 **第九轮（cookie 整罐 + 弹幕滚动）留下的口子**：
   - **滚动只有滚轮，没有一个键盘键位**（这是 tc191 定的）：终端里没有鼠标、
     或者鼠标事件被别的东西吃掉（比如 ssh 里没开会话的鼠标转发）时，就**看不了旧的弹幕**。
     要补得先跟他确认键位（`PgUp`/`PgDn` 是最自然的候选，`no_keyboard_key_scrolls_the_danmaku`
     那条测试会先红在哪儿，改的时候顺手一起改）。
   - **设备号那件事只做了一半**：整罐存不再丢字段了，但我们调的只读接口**不返**
     `buvid3` / `buvid4` / `b_nut`（见「实测过」里那节），所以「每次重启都是新设备」
     这件事**还没真治**。三条路可选（都由 tc191 定，AI 别自己加）：
     ① 他把自己浏览器里的 `buvid3` 抄进 `config.toml`（零代码，但要手工）；
     ② 加一次 `x/frontend/finger/spi`（只读，拿到 `b_3` / `b_4` 并并进罐里）；
     ③ 把**响应头里的 `Set-Cookie` 也并进客户端那罐**（`Room/get_info` 会给
     `LIVE_BUVID`），但那要再想清楚「什么时候落盘」—— 现在只有扫码登录 / 选分区 /
     退出登录会写配置，轮询响应头攒下来的东西没人写。
   - **ws 认证包里的 `buvid` 还是空的**（跟 Go 版一致）。真要填，得先有上面那条 ②/③
     拿到的设备号 —— 别凭空编一个。
   - **`thumb()` 的滑块长度取的是「一屏行数 / 总行数」**，跟浏览器那种按像素比例算的
     手感略有出入（一行内容的显示高度可能不止一行）。现在一条弹幕一行，够用。
   - 滚轮**不能横向滚**（长弹幕不折行被截断，还是没救）；`single_line = false`
     的多行模式下「一行」指的是排版后的那一行，视口按行算没错，但一屏能放几条弹幕
     会随着内容长度变，上翻的手感跟单行模式不一样。
4. 没有「弹幕关键词过滤」「屏蔽用户」这类开关。
5. 主播侧数据（观众榜分数）拿到了但没显示在界面上，只显示了名字。
6. 发送端没有前端节流：连按回车会排队发（频道容量 32，满了会在弹幕框里提示这条没发出去）。
   真被风控时至少能看到「发送过于频繁」，暂时够用。
7. 输入框没有「↑↓ 翻历史时按别的键」这种细节（翻历史会直接覆盖当前内容，跟 Go 版一致），
   也没有多行编辑 / 光标闪烁。

## 别做的事

- 不要 `git push`；不要动 `../bilibili_live_tui+`（那里是行为参照物）
- 不要把 Cookie 写进日志或打印出来；`config.toml` 永远不进 git
- 不要在没验证的情况下改 `wbi.rs` 的置换表 / `!'()*` 过滤 / `client.rs` 的请求头
- 不要在 `ui/` 里直接发请求，也不要在 `api/` 里画东西
- 不要把 `ui/qr.rs` 的颜色改成 `Color::Black` / `Color::White`（浅色主题下扫不出来）
- 不要让第二页抢走弹幕页的 `Tab`，也不要把 `Esc` 接成退出（退出只有 `Ctrl+C`）
- 不要在 AI 手里跑真的改标题 / 传封面 / 开播（那是账号上看得见的改动）；
  界面这一半用临时配置 + 空房间号验（见「怎么手工验真接口」最后一节）
- **绝对不要真的开播 / 下播**（`startLive` / `stopLive`）：开播会让 tc191 的直播间
  立刻对外可见、给粉丝推开播推送。测试一律假服务器；唯一能真调的只读探针是
  开播版本号那个 GET（见「怎么手工验真接口」里那一条）
- **不要顺手连一次真 OBS**（`obs::fill` / `SetStreamServiceSettings`）：这台机器上
  obs-websocket 现在是开着的（4455 在听），一连上就会**真的改掉 tc191 的 OBS 推流设置**。
  OBS 那边一律用假服务器（`accept_async`）验；真要连只能他自己来
- **不要拿 tc191 真在用的 `~/.config/bililive/config.toml` 试「退出登录」**：
  它清的就是那串 cookie，试一次他现在能用的登录态就废了（得重新扫码才能回来）。
  要试一律 `-c /tmp/某个.toml`（测试里是临时路径）
- 不要给 `stopLive` 顺手补一个 app 签名（Go 版实测它不带签名）；
  也不要把 app 签名（`appsign`）加到改标题 / 换封面 / 分区表那些接口上
- 不要为了「跟 web 端统一」把 `appsign` 的编码换成 `url::form_urlencoded` 那套
  （`~` / `*` 的口径相反，签名会对不上）
- 不要在登录成功后去动弹幕那条**已经在跑的**连接 —— 换不掉，只能整条重来
- 不要为了登录把 reqwest 的 `cookie_store` 打开：那跟我们自己管的 Cookie 头是两套账
- **不要把 cookie 再挑成白名单**（第 18 条）：整罐是治「每次重启都是新设备」的那一步，
  谁想「配置里干净点」顺手 filter 一下，等于把这个病请回来。要验证就加自己的浏览器
  `buvid3`，别在代码里筛。也别为了这个自己在 `api/` 里加「申请设备号」的接口调用 ——
  由 tc191 定（见「实测过」那节）。
- **不要给弹幕滚动加键盘键位**：`↑↓` 是发送历史、`Home`/`End` 是行首行尾、
  `PgUp`/`PgDn` 也一并留着（tc191 定的）。`no_keyboard_key_scrolls_the_danmaku`
  那条测试就是拦这个的；要改先问人，别「顺手补一个」。
- **开了鼠标捕获就必须还回去**：一律走 `set_mouse_capture` / `release_mouse`
  （状态记账在那儿），`restore()` 和 panic 钩子都别再自己写一串 `execute!` ——
  TUI panic 之后终端还在捕获状态里，用户连选文字都做不到。
- 不要在临时探针里把 cookie 或响应头整串打印出来（要打印就只打印**字段名**）。
