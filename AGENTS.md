# AGENTS.md

给在这个仓库里干活的 AI / 人看的项目说明。命令、结构、硬约束、踩过的坑都在这儿。

## 这是什么

哔哩哔哩**直播弹幕 TUI 客户端**，Rust + ratatui 重写版。
参照物是本机的 Go 版 `../bilibili_live_tui+`（读它的代码摸行为，**不抄代码**，
也**不要改那个仓库**）。两边共用同一份账号思路，但配置文件和字段名是不同的两套。

**当前进度：读 + 发弹幕** —— 看弹幕、看房间信息、看观众榜，
底部输入框能打字、回车发弹幕（超 20 字自动切段连发）。
登录 / 开播 / 下播 / 改标题 / 改封面 / OBS 联动**一行都还没写**。

## 常用命令

```bash
cargo build              # 构建
cargo test               # 全部测试，纯离线，不碰网络
cargo clippy --all-targets
cargo run                # 跑 TUI（Ctrl+R 手动刷房间信息，Ctrl+C 退出）
```

配置在 `~/.config/bililive/config.toml`（跟 Go 版的 `~/.config/bili/config.toml` 是两个文件）。
第一次跑会生成一份默认的。crates 走 rsproxy 镜像，**不要改** `~/.cargo/config.toml`。

## 代码结构

```text
main.rs          出入口：读配置 -> 建 client -> 起两条后台任务 -> ui::run
config.rs        config.toml 读写
timefmt.rs       时间：本地 HH:MM、已播时长、「北京时间的墙上时间」还原（纯函数为主）
api/
  client.rs      HTTP 客户端：cookie + 浏览器头 + get_api/post_form + nav（WBI 种子的缓存）
  wbi.rs         WBI 签名（三个黄金值测试钉住算法，别动置换表和 !'()* 过滤）
  room.rs        房间信息 + 观众榜 + 30 秒轮询
  danmaku.rs     弹幕：上半是**纯函数**（拆包/解压/解析），下半才是 wss 和重连
  send.rs        发弹幕：切段纯函数 + POST /msg/send + 发送任务（唯一的写链路）
  test_http.rs   只给测试用的假 HTTP 服务器（不引 wiremock）
ui/mod.rs        画图 + 收键 + 输入框状态；只碰 channel，不碰网络
```

数据流是单向的，别绕开：

```text
danmaku::supervisor --(mpsc<DanmuMsg>)--> ui 的弹幕列表（系统提示也走这条）
room::sync_loop      --(mpsc<RoomInfo>)--> ui 的房间信息 / 观众列表 / 推流状态
ui 的弹幕输入框       --(mpsc<String>, 容量 32, try_send)--> send::send_loop
                                                              └─ 失败时变成系统弹幕回弹幕列表
ui 的 Ctrl+R         --(mpsc<()>, 容量 1)--> room::sync_loop 立刻重拉
```

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

### 实测过（真接口）

nav、房间信息（在播/未播两种）、观众榜（3 人 / 50 人两种）、getDanmuInfo（没 -352）、
wss 认证（`op=8 {"code":0}`）、**真实弹幕解析**（含 `[dog]` 这类表情标签）、
历史弹幕、75 秒长连接不中断（心跳正常）。

发弹幕（2026-10-03，**就打了一次**）：真 cookie + **故意换成假 `bili_jct`** + 空 `msg` 的
`POST /msg/send`（URL 带 WBI 签名），服务端回
`{"code":-111,"message":"csrf 校验失败"}` —— 说明签名和浏览器头过了风控（不是 -352）、
表单也被认了，只有被我们改坏的那个字段不认。**没有真的发出过一条弹幕，回显这条路没验过。**

### 还没验过

- **真发一条弹幕**：上面那次是「假 csrf + 空 msg」的安全探针，只证明了请求能被服务端收下。
  `code` 为 0 的那条路、以及自己发的弹幕从 websocket 回显回来，都还没走过
  （要验就按「怎么手工验真接口」那一节的规矩，一条，验完记结果）
- **protover 2 的 zlib 真实数据**：实测那几个房间发的都是 `ver=0` 明文单包，
  解压路径目前只有单测覆盖（自造包），没吃过服务端的真包
- `SEND_GIFT` / `COMBO_SEND` / `GUARD_BUY` / `NOTICE_MSG` / `USER_TOAST_MSG` 的真实报文
  （凌晨的房间只有普通弹幕），字段名对着 Go 版抄的，第一次真收到时要盯一眼
- **断线重连**：只走了代码和退避逻辑，没有真拔过网
- TUI 本体没在真终端里跑过：这个沙箱的 pty 下 `crossterm::enable_raw_mode()` 会**挂住**
  （原骨架一样，不是本轮的改动），界面是靠 `TestBackend` 测试验的

### 已知缺口 / 下一步

1. **`INTERACT_WORD_V2` 收不到**：现在房间发的是 V2，名字在 protobuf 的 `pb` 字段里，
   不是 JSON 的 `uname`。所以「XXX 进入了房间」这类提示目前**不会显示**。
   要显示得手写一小段 protobuf varint/length-delimited 解码（Go 版同样没处理 V2）。
2. 写操作一个都没接：登录（扫码/密码）、开播、下播、改标题/封面、发弹幕、OBS 联动。
   Go 版的对应实现在 `live/client.go`（app 签名那套 appKey/appSec 是直播姬的，
  和 web 端 WBI 是两码事，别混）。
   （发弹幕这一条本轮接完了，剩下登录 / 开播 / 下播 / 改标题 / 改封面 / OBS。）
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
