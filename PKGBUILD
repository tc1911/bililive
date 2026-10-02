# Maintainer: tc1911 <171405782+tc1911@users.noreply.github.com>
# 源码包：从 GitHub 的 tag 拉源码，用 cargo 编译。
# 仓库已公开，source 走 https；本机 github.com 直连不通，所以 makepkg 取源码那一步
# 得靠 https_proxy（见同目录的 make_bililive_pkg.sh）。crates 走
# ~/.cargo/config.toml 里那个 rsproxy 镜像 —— 打包**不要**去动那个文件。
pkgname=bililive
pkgver=0.1.0
pkgrel=1
pkgdesc='Bilibili 直播弹幕 TUI 客户端（Rust 版：弹幕 / 扫码登录 / 分区 / 开播推流码 / OBS 联动）'
arch=('x86_64')
url='https://github.com/tc1911/bililive'
license=('GPL-2.0-only')
# 纯 Rust：只要 glibc 和 libgcc_s。TLS 走 rustls + 内置根证书，所以
# **不需要** openssl / ca-certificates —— 别照抄 Go 版那两行。
depends=('glibc' 'gcc-libs')
makedepends=('cargo' 'rust' 'git')
# `!lto` 不是随手加的：makepkg 的 OPTIONS 默认开 lto，会往 CFLAGS 里塞
# `-flto=auto`，aws-lc-sys 那个 C 静态库（reqwest → rustls → aws-lc-rs 拉进来的）
# 就被编成 LTO 目标文件，而最后 rustc 那次链接不带 `-flto`，
# 于是 `aws_lc_0_45_0_*` 全成了未定义符号、链接期报一屏。
options=('!debug' '!lto')
source=("$pkgname::git+https://github.com/tc1911/bililive.git#tag=v$pkgver")
# git 源没有稳定的校验和，按 Arch 的规矩写 SKIP（tag 就是那根钉子）
sha256sums=('SKIP')

# makepkg 把 git 源 clone 成 $srcdir/bililive
_srcname="$pkgname"

prepare() {
	cd "$_srcname"
	# 不设 CARGO_HOME：用本机那份缓存与镜像配置（理由见文件头）
	cargo fetch --locked
}

build() {
	cd "$_srcname"
	cargo build --release --frozen
}

# 测试是纯离线的（假 HTTP 服务器），跑一遍不吃网络；release profile 复用
# 上面那次编译的依赖，代价只有本 crate 自己那点。
check() {
	cd "$_srcname"
	cargo test --release --frozen
}

package() {
	cd "$_srcname"
	install -Dm755 target/release/bililive "$pkgdir/usr/bin/bililive"
	install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
