# AGENTS.md

给在这个仓库里干活的 AI / 人看的项目说明。命令、结构、硬约束、踩过的坑都在这儿。

## 这是什么

哔哩哔哩**直播弹幕 TUI 客户端**，Rust + ratatui 重写版。
参照物是本机的 Go 版 `../bilibili_live_tui+`（读它的代码摸行为，**不抄代码**，
也**不要改那个仓库**）。两边共用同一份账号思路，但配置文件和字段名是不同的两套。

**当前进度：读 + 发弹幕 + 扫码登录 + 选分区 + 改标题 / 换封面** —— 看弹幕、看房间信息、
看观众榜，底部输入框能打字、回车发弹幕（超 20 字自动切段连发）；
第二页（配置页）能翻了，账号栏能扫码登录并把 cookie 写回配置，
分区栏是一棵真分区树（拉分区表 / 选分区 / 只把 area_id + area_name 写回配置），
直播间信息栏能改标题、换封面（本地图先传 B 站图床），下半格还画当前封面。
**开播 / 下播 / 推流码 / OBS 联动一行都还没写**（appkey 那套签名也没接）。

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
| `↑↓` | 翻输入历史 | 直播间信息栏里选一项；分区栏在**可见的行**里走（父分区也是可停的一行）；**账号 / 推流码**栏没东西可选，就顺手拿来换栏 |
| `←→` | 什么都不做 | 只在分区栏有意义：收起 / 展开光标那一行（叶子 / 没有子分区的父分区上不吞键） |
| `回车` | 发弹幕 | 账号栏重新扫码；直播间信息栏进编辑 / 提交；分区栏停在子分区上 = **选定它**（写回配置），停在父分区 / 「全部分区」上 = 展开 / 收起；推流码栏只说一句「下一步接」 |
| `Esc` | 什么都不做（**不是退出**） | 取消编辑 → 收起配置页；退到弹幕页就停住 |
| `F2` / `F3` / `F6` | 翻开配置页并跳到账号 / 分区 / 直播间信息 | 同上（这几栏之间直接跳） |
| `Ctrl+R` | 立刻刷房间信息 | 同上（两页都能按） |
| `Ctrl+C` | 退出（唯一出口） | 退出 |

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
api/
  area.rs        分区表（两级一次拿回来）+ 与界面之间那两条消息类型
  client.rs      HTTP 客户端：cookie + 浏览器头 + get_api/post_form + nav（WBI 种子的缓存）
  wbi.rs         WBI 签名（三个黄金值测试钉住算法，别动置换表和 !'()* 过滤）
  room.rs        房间信息 + 观众榜 + 30 秒轮询
  danmaku.rs     弹幕：上半是**纯函数**（拆包/解压/解析），下半才是 wss 和重连
  send.rs        发弹幕：切段纯函数 + POST /msg/send + 发送任务（唯一的写链路）
  info.rs        直播间信息：改标题 / 传 B 站图床 / 写封面（三个接口 + 两条消息类型）
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
login::login_loop    --(mpsc<String>)------> main::apply_login（拼好的 Cookie 串）
main::apply_login    --(watch<u64>)--------> main::supervise_danmaku 整条重连
main::apply_login    --(mpsc<()>)----------> room::sync_loop 立刻用新凭据重拉
main::apply_login    --(写盘)--------------> config.toml
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
```

**写在两个地方的东西只有两个**：`login_loop` 只跟接口说话（不碰磁盘、不碰界面），
`apply_login` 只管落盘和重启链路（不碰网络请求的构造）。分界别混。
分区那条照抄这个分工：`api/area.rs` 只管接口，`main::area_task` 才是「拉表 + 落盘」的落点
（界面两件事都不许自己干，所以两种请求都从同一条 `mpsc<AreaRequest>` 进去）。
直播间信息那条同理：`api/info.rs` 只管接口，`main::info_task` 才是「先传图床再写封面」
这个**顺序**的落点（界面不许自己发请求，所以三条请求都从 `mpsc<InfoRequest>` 进去）。

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

`nav` 的 img_key/sub_key 和 uid 一起缓存 30 分钟（key 每天轮换，但也别每个请求都问一遍）。

### 7. 断线重连用指数退避，别写死

1 秒起步翻倍、上限 60 秒；连接活过 30 秒才把退避归位（被风控时别拿重连去撞墙）。
Go 版是写死 30 秒，那是历史包袱，不要照抄。

### 8. 界面上的死规矩

- 弹幕列表**有上限**（`MAX_LINES = 500` 行），只显示最后几行 —— 开一整天不能无限长
- 单行 / 多行、显不显示时间，跟 `config.toml` 的 `single_line` / `show_time` 走
- 多行模式下同一个人在同一分钟连着说话不重打名字（跟 Go 版行为一致）
- 观众榜前三名 👑🥈🥉
- 退出只有 Ctrl+C；Esc 是「返回上一层」的语义，**别接成退出**
- 顶部的艺术字和四宫格布局是定稿，别动

### 9. 本地时间只能问 libc

std 不提供本地时区（要自己解 TZif）。`timefmt::hhmm` 走 `libc::localtime_r`
（用 `_r` 那版：tokio 是多线程运行时，`localtime` 返回共享静态缓冲区，两个线程一起格式化会串）。
`libc` 本来就是间接依赖，不是新引入的一棵树。

### 10. 发弹幕（目前唯一的写链路）

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

## 测试

`cargo test` 必须全绿，且**不许引入真实网络请求**（真接口只许手工验，见下）。

- `api/danmaku.rs`：自己造二进制包喂给 `unpack` —— 拆包、protover 2 解压、坏长度不越界、
  压缩炸弹有上限、`DANMU_MSG` 带后缀、缺昵称退回 uid、认证包/心跳包的形状
- `api/room.rs`：假 HTTP 服务器断言**请求路径 + 参数形状 + 浏览器头**，
  外加 `sync_loop` 的「拉失败保留上一版」和 Ctrl+R 那条刷新路
- `api/client.rs`：`post_form` 的表单编码；脏 Cookie 不能把进程带走；
  `cookie_value` 取 `bili_jct`（缺键/空值/带换行）
- `api/send.rs`：`split_segments` 的边界（空串、正好 20、21、41 个中文、`limit=0` 不 panic）；
  假服务器上断言**路径 + WBI 参数（逐字节等于 `wbi::sign` 的结果）+ 七个表单字段 +
  cookie/浏览器头**；`code != 0` 带上服务端的 message；没有 `bili_jct` 时一个请求都不发；
  21 个字切两段且真有间隔；一段失败要变成系统弹幕
- `ui/mod.rs`：用 `TestBackend` 把一帧画进内存再读回来，验弹幕上限、框标题的刷新时间与
  「没刷上」、前三名奖牌、推流状态、超小终端不 panic；
  另外验输入框（按字符编辑 / 历史 10 条与翻到头翻到底 / Ctrl+C 和 Ctrl+R 不被当文字收 /
  空回车不发）、推流状态格的整段截断，以及**打字 -> 回车 -> 假服务器上真的出现
  `/msg/send` 请求**这条端到端的路
  （注意：缓冲区里宽字符会多占一格留下空格，断言前两边都 `flat()` 掉空白再比）
- `api/login.rs`：`next_step` 四种 code 各一条（外加没见过的 code 不能硬猜成成功/过期）；
  cookie 从 `Set-Cookie`（属性里的 `=`、`deleted`、旁路 cookie）和跳转 URL 的 query
  （`%2C` 要解码、字段顺序乱、只有一部分）里抠得对不对；拼装的顺序固定不固定；
  整条流程在假服务器上打一遍（generate → poll 三次不同 code → 成功），
  断言请求路径、`qrcode_key` 参数、轮询次数，以及最后交接出来的那串 Cookie
  （`LoginCtx` 的 `gap` / `attempts` 做成字段就是为了这里能传 0，别让单测真睡 2 秒）
- `ui/qr.rs`：行数是列数的一半、只有那四种半格字符、静默区四边都是白的
  （按**半格**验：压在边界上的那一行只有一半属于静默区）、真彩色不是调色板、
  编不出来的内容只给一行说明、以及**反解回模块矩阵的往返测试**
- `ui/control.rs`：按键语义（Shift+Tab 开合两页、弹幕页上 Tab/Esc 不归它管、
  Tab 换栏绕圈、↑↓ 在四栏里各是什么、Esc 一层层往回走然后停住、F2/F3/F6 直接跳栏、
  没登录进账号栏才要码）、编辑骨架（按字符编辑、Tab 跑掉要还原、提交要说清楚没真发）、
  登录事件只动显示状态、提示语里不许有换行、各种小终端尺寸下画一页不 panic；
  分区栏另有一组：切进来只自动拉一次表、没配过停在「全部分区」且父分区都收起、
  配过的展开它所在的那一个、`←→` 只在父分区上有动作、回车在子分区上给
  `Action::PickArea { id, name }`（在父分区上只是展开收起）、拉失败后回车重试、
  空表也能回车再拉、顶栏提示跟着状态换、选中的行反色且整屏只有它一条、
  长列表走到底选中行还在屏幕上
- `ui/mod.rs`（配置页）：整屏被第二页接管（主页面那几块一个字都不剩）、
  左栏 16 格里是那四个功能名、全屏只有一个 ▸、右栏标题跟着当前栏换、
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
  写之前必须重读一遍文件（模拟扫码登录刚写进新 cookie）
- `main.rs`：`area_task` 按真通道打一遍 —— `Load` 走假服务器拿回分区表，
  `Pick` 把两个字段写进临时配置文件，别的不许动

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

### 实测过（真接口）

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
- 封面预览只验过**假服务器给的**图 + 一张手工抓下来的真图（见上面「实测过」），
  「从进栏到画出来」这条端到端没在真网络下走过。

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
2. 写操作还差几条：**开播 / 下播 / 推流码 / OBS 联动**（改标题和换封面第五轮接完了）。
   Go 版的对应实现在 `live/client.go`（app 签名那套 appKey/appSec 是直播姬的，
   和 web 端 WBI 是两码事，别混）。
   （发弹幕第二轮接完，扫码登录第三轮接完，分区栏第四轮，直播间信息栏第五轮。）
2.0 **第五轮留下的三个口子**（都不影响用，按需接）：
   - 信息栏没有「重新拉一次当前标题 / 封面」的键：一次运行只自动问一次
     （拉失败后切走再切回来会重试）。要补就照分区栏那套（回车重试 / F6 重来）。
   - 封面预览的解码跑在界面的事件循环里：几百 KB 的图几十毫秒，
     真碰到超大图会卡一帧。要治就把它挪到信息任务里（解完只把缩好的图传过来）。
   - 预览那张图**抓失败之后不会再自动重抓**（任务那边按地址去重，失败也算「抓过了」，
     跟 Go 版 `ui/cover` 的 `loaded` 一个口径）：网络抖一下之后要等地址变了
     （改一次封面 / 重启）才会再试。要治就在失败时不记那个地址。
   - 「退出登录」还是没有（见 2.2），改标题 / 换封面在没登录时会直接在顶栏说
     「Cookie 里没有 bili_jct…」，这时得靠临时配置或手工清 cookie 换账号。
2.1 **第二页还差两栏**：
   - ~~分区栏~~ —— 2026-10-03 第四轮接完了（见上面第 13 条）。剩一个小口子：
     分区表是**每次运行拉一次**（切进这一栏时自动拉，失败了回车重试），
     中途没有「刷新分区表」的键 —— Go 版的 F3 是「手动重来一次」（会重拉，
     代价是丢掉手工展开的状态），要不要补看下一轮怎么定。
   - ~~直播间信息栏~~ —— 2026-10-03 第五轮接完了（见上面第 14 条）：改标题、
     换封面（本地图先传图床）、下半格画当前封面都做了。留下的口子见上面 2.0，
     以及**这三个写接口一个都没在真账号上验过**（见「还没验过」那一节）。
   - 推流码栏：等开播接完，`F4` 开播 / `F5` 下播的提示再补回 `Tab::Stream::hint`
     （现在特意没写，提示里挂一个按了没反应的键比不写更坑人）。
2.2 **第二页的账号栏只有「扫码登录」，没有「退出登录」**：想换账号得手工清
   `config.toml` 里的 `cookie`。Go 版也没有，但既然 cookie 已经能写进去，
   加一个「清掉 cookie + 重启链路」是顺手的事。
3. 弹幕列表不能滚动/翻页，只能看最后几行。
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
- 不要在登录成功后去动弹幕那条**已经在跑的**连接 —— 换不掉，只能整条重来
- 不要为了登录把 reqwest 的 `cookie_store` 打开：那跟我们自己管的 Cookie 头是两套账
