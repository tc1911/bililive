# AGENTS.md

给在这个仓库里干活的 AI / 人看的项目说明。命令、结构、硬约束、踩过的坑都在这儿。

## 这是什么

哔哩哔哩**直播弹幕 TUI 客户端**，Rust + ratatui 重写版。
参照物是本机的 Go 版 `../bilibili_live_tui+`（读它的代码摸行为，**不抄代码**，
也**不要改那个仓库**）。两边共用同一份账号思路，但配置文件和字段名是不同的两套。

**当前进度：读 + 发弹幕 + 扫码登录** —— 看弹幕、看房间信息、看观众榜，
底部输入框能打字、回车发弹幕（超 20 字自动切段连发）；
第二页（配置页）能翻了，账号栏能扫码登录并把 cookie 写回配置。
开播 / 下播 / 改标题 / 改封面 / OBS 联动**一行都还没写**。

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
| `↑↓` | 翻输入历史 | 直播间信息栏里选一项；**账号 / 推流码**栏没东西可选，就顺手拿来换栏；分区栏这轮不动 |
| `回车` | 发弹幕 | 账号栏重新扫码；直播间信息栏进编辑 / 提交；分区、推流码栏只说一句「下一步接」 |
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
  client.rs      HTTP 客户端：cookie + 浏览器头 + get_api/post_form + nav（WBI 种子的缓存）
  wbi.rs         WBI 签名（三个黄金值测试钉住算法，别动置换表和 !'()* 过滤）
  room.rs        房间信息 + 观众榜 + 30 秒轮询
  danmaku.rs     弹幕：上半是**纯函数**（拆包/解压/解析），下半才是 wss 和重连
  send.rs        发弹幕：切段纯函数 + POST /msg/send + 发送任务（唯一的写链路）
  login.rs       扫码登录：poll 的 code → 下一步（纯函数）+ cookie 拼装（纯函数）
                 + generate/poll 两个接口 + 扫码任务
  test_http.rs   只给测试用的假 HTTP 服务器（不引 wiremock）
ui/mod.rs        第一页（弹幕）：画图 + 收键 + 输入框状态；只碰 channel，不碰网络
ui/control.rs    第二页（配置）：页面 / 功能栏状态机 + 布局 + 编辑骨架，同样不碰网络
ui/qr.rs         二维码：半格字符 + 真彩色（纯函数）
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
```

**写在两个地方的东西只有两个**：`login_loop` 只跟接口说话（不碰磁盘、不碰界面），
`apply_login` 只管落盘和重启链路（不碰网络请求的构造）。分界别混。

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
  登录事件只动显示状态、提示语里不许有换行、各种小终端尺寸下画一页不 panic
- `ui/mod.rs`（配置页）：整屏被第二页接管（主页面那几块一个字都不剩）、
  左栏 16 格里是那四个功能名、全屏只有一个 ▸、右栏标题跟着当前栏换、
  拿到二维码就画出来且登录后收掉

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

别做任何会改账号状态的操作（登录 / 开播 / 下播 / 改标题 / 改封面），
也别往真房间刷弹幕（要验就按上面那条规矩来一次）。

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

### 实测过（真接口）

nav、房间信息（在播/未播两种）、观众榜（3 人 / 50 人两种）、getDanmuInfo（没 -352）、
wss 认证（`op=8 {"code":0}`）、**真实弹幕解析**（含 `[dog]` 这类表情标签）、
历史弹幕、75 秒长连接不中断（心跳正常）。

发弹幕（2026-10-03，**就打了一次**）：真 cookie + **故意换成假 `bili_jct`** + 空 `msg` 的
`POST /msg/send`（URL 带 WBI 签名），服务端回
`{"code":-111,"message":"csrf 校验失败"}` —— 说明签名和浏览器头过了风控（不是 -352）、
表单也被认了，只有被我们改坏的那个字段不认。**没有真的发出过一条弹幕，回显这条路没验过。**

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

### 还没验过

- **真发一条弹幕**：上面那次是「假 csrf + 空 msg」的安全探针，只证明了请求能被服务端收下。
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
2. 写操作还差几条：**开播 / 下播 / 改标题 / 改封面 / OBS 联动**。
   Go 版的对应实现在 `live/client.go`（app 签名那套 appKey/appSec 是直播姬的，
   和 web 端 WBI 是两码事，别混）。
   （发弹幕第二轮接完，扫码登录第三轮接完。）
2.1 **第二页那三栏还是占位**：
   - 分区栏：要接 `/room/v1/area/getList`（两级分区表），做成一棵可展开的树；
     `↑↓` 已经在 `Control::handle_key` 里给这一栏留了口子，直接往里填就行。
     注意 Go 版踩过的两个坑：父分区不可选 + 默认全收起时整棵树点不动；
     以及「默认展开列表第一项」——判据得看配置里的分区在哪个父节点下，不是看下标 0。
   - 直播间信息栏：选 / 编辑 / `Esc` 还原这套骨架是真的，但**提交是假的**
     （只把值记在内存里，提示也这么说）。接的时候做两件事：
     `POST` 改标题（上限 40 字，超了服务端只回一句看不懂的错，界面上先拦）、
     封面要么给 `.hdslb.com` 链接要么先传图床（别处的地址服务端一律 100402 拒）。
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
- 不要在登录成功后去动弹幕那条**已经在跑的**连接 —— 换不掉，只能整条重来
- 不要为了登录把 reqwest 的 `cookie_store` 打开：那跟我们自己管的 Cookie 头是两套账
