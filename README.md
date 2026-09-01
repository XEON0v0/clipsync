# ClipSync

端到端加密的跨设备剪贴板同步：在 macOS 与 Android 之间安全地同步文本与图片。中继服务器只转发密文——它读不到你的剪贴板内容，也无法篡改或伪造消息。

## 特性一览

**安全**

- 端到端加密：XChaCha20-Poly1305 AEAD + 规范化 AAD，中继零知识，防窃听、防篡改、防伪造
- X25519 身份 + Ed25519 签名 + HKDF-SHA256 密钥派生
- 二维码带外配对，双端人工确认六位安全码（SAS），抵御中间人攻击
- 发送端敏感内容拦截（macOS）：内置 7 类高敏凭据规则（PEM 私钥、`otpauth://` 2FA 种子、GitHub / OpenAI / AWS / Google / Slack 凭据），支持自定义子串 / 正则规则（仅存本机），命中即拦截并通知，绝不外发
- 一键暂停同步（状态持久化），暂停期间任何内容不出本机
- 回显抑制：每设备独立序列号 + 去重环，自己刚发出去的内容不会被再次广播
- 自建中继自带防护：按客户端 IP 限速（配对次数与入站流量）、房间配额、过期清理、容器加固

**体验**

- 文本与图片双向同步（图片上限 10 MiB）
- 剪贴板历史（最近 50 条），菜单栏随时取用
- macOS：纯菜单栏应用（无 Dock 图标），支持开机自启
- Android：ML Kit 扫码配对、前台服务常驻接收、厂商自启动引导；可选将同步到的图片自动存入相册
- 一套 Rust 核心（UniFFI）同时驱动 Swift 与 Kotlin 客户端

## 安全模型

### 端到端加密与防篡改

剪贴板内容在发送端封装为加密信封，只有配对设备的会话密钥能打开：

- 密钥协商 X25519；加密 XChaCha20-Poly1305；签名 Ed25519；派生 HKDF-SHA256（info 前缀 `clipboard-sync-v1`）
- 每条消息带规范化 AAD 与设备签名，恶意中继无法篡改、重放或伪造来源
- 每个房间恰好两个身份：配对即点对点，不存在第三方成员

### 配对与防中间人

1. 发起端展示二维码，包含：中继地址、六字符配对码（5 分钟有效）、公钥束，以及只存在于二维码中、永不上行中继的一次性 16 字节 nonce
2. 对端扫码加入后，两端同时显示六位安全码（SAS）
3. 只有两端人工确认数字一致后，配对才写入磁盘生效——中间人无法让两端显示一致的 SAS

### 零知识中继

中继（`clipboard-server`）只见到密文信封，编译时不链接任何客户端加密实现；每个房间仅保留最新一条待投递密文（100 房间 × 24 MiB 配额，7 天 TTL 自动清理），并按客户端 IP 对配对请求与入站字节限速。

### 发送端敏感内容拦截（macOS）

内容在离开本机之前先过过滤器：命中内置规则（PEM 私钥块、`otpauth://`、`ghp_` / `github_pat_`、`sk-…`、`AKIA…`、`AIza…`、`xox…`）或你自定义的规则即拦截并弹出通知（带去重）。规则界面可随时增删；自定义规则只保存在本机。

### 已知安全边界

ClipSync 使用静态 X25519 身份，**不提供前向保密（PFS）与 KCI 抗性**：若某端 DH 私钥泄露，攻击者可解密既往记录的流量，并向该端伪造消息。若怀疑密钥泄露，请在两端执行重新配对（生成全新身份与房间）。完整说明见 `crates/core/src/crypto.rs`。

## 仓库结构

| 目录 | 内容 |
| --- | --- |
| `crates/core` | `clipboard-core`：共享 Rust 协议数据与完整客户端核心，经 UniFFI 暴露 FFI；默认 feature 仅数据模型，`verify` / `full` 按需启用 |
| `crates/server` | `clipboard-server`：Axum 中继服务，只启用核心 `verify` feature，不链接客户端加密、二维码、网络与 UniFFI 依赖 |
| `tools/uniffi-bindgen` | `clipsync-uniffi-bindgen`：锁定版本的 UniFFI 绑定生成器封装 |
| `macos` | SwiftUI macOS 13 菜单栏代理（`com.clipsync.macos`，`LSUIElement` 应用） |
| `android` | Kotlin + Jetpack Compose 应用（`com.clipsync.app`），内置 ML Kit 二维码扫描 |
| `deploy` | Docker Compose + Caddy 部署与运维手册（RUNBOOK） |
| `scripts`、`dist` | 仓库自动化脚本与被忽略的构建产物 |

## 协议常量

| 常量 | 值 |
| --- | --- |
| WebSocket 单帧上限 | 24 MiB |
| WebSocket 消息上限 | 24 MiB |
| 编码后图片上限 | 10 MiB |
| 配对码有效期 | 300 秒 |
| 邮箱保留时间 | 7 天 |
| 每房间身份数 | 2 |
| 剪贴板轮询间隔 | 300 ms |
| 重连退避 | 1–30 秒，±20% 抖动 |
| 历史上限 | 50 条 |
| 去重环 | 64 条 |
| 信封版本 | v1 |
| HKDF info 前缀 | `clipboard-sync-v1` |
| UniFFI 版本 | `=0.29.4` |

## 构建

```sh
cargo build --locked --workspace
(cd macos && swift build -Xswiftc -warnings-as-errors)
(cd android && ./gradlew assembleDebug)
```

UniFFI 绑定只能通过工作区封装生成：

```sh
cargo run --locked -p clipsync-uniffi-bindgen -- <args>
```

## 安装与配对

### macOS

1. 运行 `scripts/package-macos.sh`，将 `dist/ClipboardSync.app` 移入 `/Applications`
2. 首次启动时 Control-点击应用并选择**打开**，批准 ad-hoc 签名构建
3. 填写中继 `wss://` 地址，选择 **Pair Device**，用 Android 扫码，并在两端确认相同的六位安全码
4. 需要授权时可从 ClipSync 设置一键跳转登录项设置页；ClipSync 是纯菜单栏应用（`LSUIElement`），没有 Dock 图标，配对与历史按需弹出窗口

### Android

1. 运行 `scripts/android-release.sh` 生成 `dist/clipboard-sync.apk`，安装到设备
2. 打开 ClipSync 扫描 Mac 上的二维码，并在两端确认相同的六位安全码；确认之后才会保存配对
3. 允许通知并豁免电池优化，让前台数据同步服务在应用未打开时也能接收。小米、华为、荣耀、OPPO、一加、realme、vivo 或 iQOO 设备还需按应用内引导在厂商的自启动设置中放行 ClipSync
4. 首次 release 构建会生成 `android/keystore/release.jks` 与 `android/keystore/keystore.properties`（权限 `0600`，均被 Git 忽略）。请将两者一起妥善备份：后续升级包必须使用同一签名。不要把内容粘贴到 issue 或构建日志里

## 自建中继

中继是一台只转发密文的小服务，用 Docker Compose + Caddy 部署：

```sh
cd deploy
cp .env.example .env   # 设置 DOMAIN=中继域名
chmod 600 .env
docker compose up --build -d
```

- Caddy 自动签发并续期公共证书（80/443，443/udp 可选启用 HTTP/3），镜像按摘要锁定
- 中继容器不发布端口，只接受反代流量，且仅信任 `X-Forwarded-For` 中单一可信代理
- 容器以只读根文件系统、`cap_drop: ALL`、`no-new-privileges` 运行
- 完整步骤与验证脚本见 `deploy/RUNBOOK.md`；中国大陆公开域名需确认 ICP 备案要求

## 许可

工作区声明为 `MIT OR Apache-2.0`。
