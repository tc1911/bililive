# bililive

哔哩哔哩直播弹幕 TUI 客户端。Rust 写的：界面 ratatui、异步 tokio、HTTP 与 TLS 全在进程里
（reqwest + rustls），不依赖外部的 curl / openssl。

行为参照自作者的 Go 版 `bilibili-live-tui-plus`（上游 [yaocccc/bilibili_live_tui](https://github.com/yaocccc/bilibili_live_tui)），
这一份是从零重写的。

## 有什么

**第一页（弹幕）**

- 弹幕流：websocket 认证 + 30 秒心跳 + 指数退避重连，最多留 500 行
- 鼠标滚轮翻历史（一格 3 行，带滚动条；滚到底自动恢复跟随）
- 直播间信息：标题 / 分区 / 在线人数 / 粉丝数，30 秒自刷，`Ctrl+R` 立刻刷
- 观众列表：前三名带奖牌
- 发弹幕：输入框直接打字回车，超 20 字按字符切段
- 弹幕屏蔽：`config.toml` 里配类型 / 关键词 / 用户名，默认挡房间广播与连击送礼

**第二页（`Shift+Tab` 翻开）**

- 账号：扫码登录、退出登录
- 分区：12 个父分区 450 个子分区，选定后写回配置
- 直播间信息：改标题、换封面（本地图先传图床）
- 推流码：`F4` 开播 / `F5` 下播 / 看推流地址与密钥；开播后顺手填进 OBS

## 装

Arch 用作者的个人源：

```ini
[tc191]
SigLevel = Optional TrustAll
Server = https://tc1911.github.io/tc191-pkgs/
```

```bash
sudo pacman -Syu && sudo pacman -S bililive
```

自己编：

```bash
git clone https://github.com/tc1911/bililive
cd bililive && cargo build --release
./target/release/bililive -r 6      # 6 号是官方赛事间，弹幕够多
```

需要 Rust（edition 2024）和 git；运行时只要 glibc / gcc-libs。

## 用

```bash
bililive                 # 看配置里那个房间
bililive 6               # 指定房间号（不改配置）
bililive -c /tmp/a.toml  # 换一份配置
bililive -h              # 用法
```

配置在 `~/.config/bililive/config.toml`，第一次跑自动生成；cookie 存在里面，别把那文件
提交到任何地方。`Ctrl+C` 是唯一的退出键。

**`F4` 开播是这个程序唯一会对外发生的动作** —— 直播间立刻可见、给粉丝推开播推送，
所以它永远先弹一个确认框，默认落在「取消」。

按键表、接口口径（三套签名各用在哪）、以及哪些地方还没在真机上验过，都写在
[`AGENTS.md`](AGENTS.md) 里。

## 许可

GPL-2.0-only，跟参照的 Go 版同一个许可。
