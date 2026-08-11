# ClipSync Desktop（Windows + macOS 统一 Rust 桌面客户端）设计

日期：2026-08-11
状态：已获用户逐节确认（架构 / UI / 平台适配与构建）

## 背景与目标

现有客户端：macOS（SwiftUI 菜单栏 agent + 三个按需小窗口）、Android（Compose 单 Activity 三 Tab）。Windows 端完全不存在。用户要求：

1. 新增 Windows 客户端。
2. Windows 与 macOS 使用**同一套完整、美观的窗口应用**，取代"藏在菜单栏里的简陋操作条"。

已确认的决策：

- UI 技术栈：**eframe/egui**（Rust 原生 GUI，方案 A）。
- macOS 端同样由这套 Rust 桌面应用覆盖（Swift 版不再扩展窗口功能）。
- 形态：窗口 + 托盘/菜单栏后台驻留（关窗不退出，同步持续运行）。
- v1 功能范围：对齐现有能力（配对 / 历史 / 设置 / 连接状态），不新增需要改协议的功能。
- Windows 构建测试环境：Parallels Windows VM，原生 MSVC 工具链构建。

## 总体架构

新增 `crates/desktop` workspace crate（binary + library target），**直接以库形式链接 `clipboard-core`**（`--no-default-features --features full`），不走 UniFFI FFI。复用 `CoreHandle` + `CoreCallbacks`（crates/core/src/ffi.rs:251、161）——与 macOS/Android 完全相同的、经过 loopback 闭环测试的代码路径，包括配对、崩溃安全投递、50 条历史、自动重连（1s–30s ±20% jitter）。

分层（模块即 crates/desktop 内的模块，各自单一职责、可独立测试）：

```
crates/desktop/
├── src/main.rs            # 入口：初始化日志、数据目录、启动 eframe
├── src/app.rs             # eframe::App 实现：顶层状态机、页面路由、渲染循环
├── src/core_bridge.rs     # CoreHandle 生命周期 + CoreCallbacks 实现
│                          #   回调（core 线程）→ 跨线程消息 → egui 上下文 request_repaint
├── src/messages.rs        # UiEvent 枚举：Clip / Status / PairingSnapshot 等跨线程消息
├── src/clipboard_monitor.rs # arboard 读写 + 300ms 轮询 + ownership token 防回环
├── src/tray.rs            # tray-icon 托盘/菜单栏 + 菜单事件 → UiEvent
├── src/autostart.rs       # auto-launch 开机自启动
├── src/settings.rs        # 设置持久化（relay URL、自启开关），独立于 core 数据
├── src/ui/
│   ├── mod.rs             # 主题（深浅色跟随系统、强调色、圆角卡片样式）
│   ├── nav.rs             # 左侧导航栏 + 连接状态指示
│   ├── pairing.rs         # 配对页
│   ├── history.rs         # 历史页
│   └── settings.rs        # 设置页
└── src/image_util.rs      # QR 渲染（core qrcode → egui 纹理）、图片缩略图、PNG 编解码
```

数据流：

```
core 线程 (CoreCallbacks) ──channel──> app.rs (egui 主线程) ──> UI 渲染
UI 操作 ──> core_bridge ──> CoreHandle 命令队列（有界 32，QueueFull 背压）
clipboard_monitor (轮询线程) ──> core_bridge.send_text/send_image
tray 事件 ──channel──> app.rs（打开窗口/发送剪贴板/退出）
```

协议、加密、配对角色（桌面=offerer 显示 QR，手机=claimer 扫码）全部不变，服务端零改动。

## UI 设计

单窗口 + 左侧导航栏（Android 三 Tab 的桌面化）：

- **窗口**：默认 900×600，最小 720×480，可调整大小。关闭按钮 = 隐藏窗口到托盘（不退出进程）。托盘菜单「打开窗口」或单击托盘图标恢复。
- **左侧导航**（约 200px）：顶部 App 图标 + 名称；导航项「配对 / 历史 / 设置」；底部连接状态指示（彩色圆点 + 文字），映射 `CoreStatus` 8 态（未配对/连接中/已连接/离线等）。
- **配对页**：
  - 未配对：QR 大图（`pair_begin` 返回的 qr_json 用 core 自带 qrcode crate 渲染为 egui 纹理）+ 配对码文本 + 「取消配对」；收到 `PairingSnapshot` 后显示 SAS 大字号核对区（确认/取消）。
  - 已配对：房间/对端信息 + 「发送当前剪贴板」+ 换绑、解绑（二次确认）。
- **历史页**：卡片列表，文本摘要 / 图片缩略图（懒加载 `history_image_bytes`）；每张卡片带来源标签（本机/对方/离线补投）+ 相对时间 + 点击应用（`history_apply`）；顶部刷新、清空（二次确认）。上限 50 条（core 约束）。
- **设置页**：Relay 服务器地址（URL 校验，重启会话生效）、开机自启开关、关于（版本号）。
- **视觉**：跟随系统深浅色；圆角卡片 + 统一强调色；中文文案；操作反馈用 toast/短暂状态条。风格参照现代桌面应用 sidebar 布局（1Password 设置密度），不做花哨动画。

## 平台适配层

UI 代码不直接触碰平台 API，全部封装在上述模块中：

- **剪贴板**：`arboard` crate 读写；监控为独立线程 300ms 轮询（对齐 macOS PasteboardMonitor 策略），ownership token 防回环（本机写入不回发）；图片走 PNG 编解码，10MiB 上限（协议常量）。
- **托盘**：`tray-icon` crate —— Windows 托盘 / macOS 菜单栏 status item。菜单：打开窗口 / 发送当前剪贴板 / 退出。
- **Dock/任务栏策略（macOS）**：窗口打开时显示 Dock 图标，窗口隐藏后切 accessory（`NSApplication.setActivationPolicy`），保持菜单栏 agent 体验。
- **自启动**：`auto-launch` crate（Windows 注册表 Run 键；macOS Login Item）。
- **数据目录**：`directories` crate —— Windows `%APPDATA%/clipsync`，macOS `~/Library/Application Support/clipsync`。identity.json / pairing.json / history 由 core 在该目录下管理（CoreHandle::new(data_dir, callbacks)）。

## crates/core 移植改动（最小化）

- `#[cfg(unix)]` 的 0600 文件权限代码（pairing.rs、session.rs、history.rs 中设置身份/配对/历史文件权限处）增加 `#[cfg(windows)]` 分支：Windows 上跳过 POSIX 权限设置（NTFS ACL 默认仅当前用户可写，可接受）。
- 目标：使 `--no-default-features --features full` 能编译 `x86_64-pc-windows-msvc`。
- **不改**任何协议、加密、投递逻辑；现有全部测试与 loopback fixture 必须保持绿色。

## 构建与分发

- macOS：本机 `cargo build --release -p clipsync-desktop`（CARGO_TARGET_DIR 已按 AGENTS.md 路由到外部卷）；后续可打 .app bundle（复用 scripts/package-macos.sh 思路，独立脚本，不在 v1 范围）。
- Windows：Parallels VM 内安装 Rust MSVC 工具链，`cargo build --release`，产出单 exe + 数据目录，无安装器。
- 验证环境遵循外部卷策略（AGENTS.md）：构建、缓存、产物全部在 `/Volumes/ClipSyncDev` 下。

## 错误处理

- `CoreError`（含 QueueFull/SasMismatch/Shutdown）与 `CoreStatus` 全量映射为 UI 状态与文案；网络断线由 core 自动重连，UI 仅展示状态，不自行重试。
- 剪贴板读取失败（如 Windows 剪贴板被占用）：跳过该轮并记录日志，不阻断轮询。
- 配对码过期（TTL 300s）：UI 提供「刷新配对码」。

## 测试

- desktop crate 单元测试：消息映射（messages.rs）、历史模型、设置校验（settings.rs）等非 UI 逻辑。
- core 回归：现有测试 + build-ffi.sh 的 loopback fixture 必须保持通过（core 仅加 cfg 分支）。
- 手工验证清单：双端配对（桌面 QR ↔ Android 扫码）→ 文本互发 → 图片互发 → 离线补投 → 断网重连 → 托盘驻留/关窗恢复 → 自启动。
- Windows 端在 Parallels VM 中重复上述清单。

## 明确不做（YAGNI）

- 不改协议、不加局域网发现、不多设备（>2）管理。
- 不做本地（非同步）剪贴板历史、快捷键全局呼出。
- v1 不做 Windows 安装器、macOS .app 打包脚本、自动更新。
- 不删除现有 Swift macOS 应用（保留仓库现状，desktop 应用先行验证）。
