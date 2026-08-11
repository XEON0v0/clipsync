# ClipSync Desktop（crates/desktop，Windows + macOS egui 客户端）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增 `crates/desktop` —— 一套 eframe/egui Rust 桌面应用，同时作为 Windows 与 macOS 的完整窗口客户端（窗口 + 托盘驻留），直接以库形式链接 `clipboard-core`。

**Architecture:** `clipboard-core`（`--no-default-features --features full`）作为普通 Rust 依赖链接，复用 `CoreHandle`/`CoreCallbacks`（与 macOS/Android 同一闭环测试路径）。桌面壳分层：专线程「core executor」（所有 `CoreHandle` 阻塞调用离开 UI 线程）、剪贴板轮询线程（300ms + digest ownership token）、`tray-icon` 托盘、egui 单窗口三页（配对/历史/设置）。UI 线程只收发 channel 消息，永不阻塞。

**Tech Stack:** eframe/egui（UI）、arboard（剪贴板）、tray-icon（托盘）、auto-launch（自启动）、directories（数据目录）、clipboard-core workspace 内的 image/qrcode/sha2、objc2-app-kit（仅 macOS activation policy）。

**Spec:** `docs/superpowers/specs/2026-08-11-desktop-app-design.md`（已获批并提交）。

**与 spec 文件结构的两处刻意差异（更简单，不改变架构）：**
- 不设 `messages.rs` 统一 `UiEvent` 枚举：core 事件、托盘命令、剪贴板变更各走一条类型化 channel（`CoreEvent`/`TrayCommand`/`ClipboardPayload`），UI 在 `drain_events` 里依次排空。
- 不设独立 `image_util.rs`/`nav.rs`：QR 渲染在 `ui/pairing.rs`，PNG 编解码在 `clipboard_monitor.rs`，导航栏在 `app.rs` 的 `show_nav`。

## Global Constraints

- 外部卷 preflight（每个会构建/测试的任务开头必须执行）：
  ```zsh
  source scripts/external-dev-env.zsh
  [[ "$EXTERNAL_DEV_READY" == 1 && "$CLIPSYNC_EXTERNAL_READY" == 1 && "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
  ```
  任一检查失败即停止并报告，不得回退到内置磁盘。
- workspace 统一 `edition = "2024"`、`rust-version = "1.90"`（继承 workspace）。
- workspace 已有依赖一律 `xxx.workspace = true`（image/qrcode/sha2/serde/serde_json/tokio/tempfile 精确版本见根 `Cargo.toml`）；新增外部依赖用 `cargo add` 解析后在 `Cargo.toml` 里改为 `=x.y.z` 精确钉版。
- `clipboard-core` 只允许为 Windows 编译追加 `#[cfg(windows)]` 平台分支；协议、加密、投递逻辑一行不改；`build-ffi.sh` 的全部断言与 loopback fixture 必须保持绿色。
- 协议常量不变：图片 ≤ 10 MiB、历史 50 条、帧 24 MiB；不新增协议帧、不改配对角色（桌面 = offerer 显示 QR）。
- UI 文案一律中文。
- 生产 relay 无默认值：`relay_url` 默认空串，由用户在设置页填写（与 macOS 现状一致）。
- 每个 Task 结束一次 `git commit`；提交信息格式 `feat(desktop): ...` / `fix(core): ...`。
- 数据目录：`BaseDirs::new().data_dir().join("clipsync")`（Windows → `%APPDATA%/clipsync`，macOS → `~/Library/Application Support/clipsync`）。
- 窗口图标 v1 不做（`assets/icon/` 只有 SVG，egui 不解码 SVG；不引入 resvg 依赖）。

---

### Task 1: crates/desktop 脚手架与依赖钉版

**Files:**
- Create: `crates/desktop/Cargo.toml`
- Create: `crates/desktop/src/main.rs`
- Modify: `Cargo.toml`（根 workspace members）

**Interfaces:**
- Produces: workspace member `clipsync-desktop`（binary 名 `clipsync-desktop`）；后续所有 Task 在本 crate 内加模块。

- [ ] **Step 1: 外部卷 preflight**

```zsh
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 && "$CLIPSYNC_EXTERNAL_READY" == 1 && "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]] && echo READY
```
Expected: 输出 `READY`。否则停止并报告外部存储不可用。

- [ ] **Step 2: 注册 workspace member**

根 `Cargo.toml` 的 `members` 改为：

```toml
members = [
    "crates/core",
    "crates/desktop",
    "crates/server",
    "tools/uniffi-bindgen",
]
```

- [ ] **Step 3: 创建 `crates/desktop/Cargo.toml`**

```toml
[package]
name = "clipsync-desktop"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
clipboard-core = { path = "../core", default-features = false, features = ["full"] }
image.workspace = true
qrcode.workspace = true
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true

[target.'cfg(target_os = "macos")'.dependencies]
# objc2 / objc2-app-kit —— 由 Step 4 的 cargo add 填实际版本

[dev-dependencies]
clipboard-server = { path = "../server" }
tempfile = "=3.27.0"
tokio.workspace = true
```

- [ ] **Step 4: cargo add 外部依赖并精确钉版**

```zsh
cargo add eframe --package clipsync-desktop
cargo add arboard --package clipsync-desktop
cargo add tray-icon --package clipsync-desktop
cargo add auto-launch --package clipsync-desktop
cargo add directories --package clipsync-desktop
cargo add log --package clipsync-desktop
cargo add env_logger --package clipsync-desktop
cargo add objc2 --package clipsync-desktop --target 'cfg(target_os = "macos")'
cargo add objc2-app-kit --package clipsync-desktop --target 'cfg(target_os = "macos")'
```

然后打开 `crates/desktop/Cargo.toml`，把上列每个依赖的版本号加上 `=` 前缀（如 `eframe = "0.32.1"` → `eframe = "=0.32.1"`），与 workspace 钉版风格一致。

- [ ] **Step 5: 最小 `main.rs` 并编译**

`crates/desktop/src/main.rs`：

```rust
fn main() -> eframe::Result<()> {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ClipSync")
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "ClipSync",
        options,
        Box::new(|_cc| Ok(Box::new(HelloApp))),
    )
}

struct HelloApp;

impl eframe::App for HelloApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("ClipSync Desktop 脚手架");
        });
    }
}
```

- [ ] **Step 6: 编译验证**

```zsh
cargo check -p clipsync-desktop
```
Expected: 编译通过（首次解析依赖耗时较长）。若 eframe 与 objc2 版本解析冲突，降低 objc2 到与 eframe 传递依赖兼容的版本后重试。

- [ ] **Step 7: 运行冒烟（人工）**

```zsh
cargo run -p clipsync-desktop
```
Expected: 弹出 900×600 窗口，标题 ClipSync，显示「ClipSync Desktop 脚手架」。关闭窗口后进程退出。

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/desktop
git commit -m "feat(desktop): scaffold clipsync-desktop crate with eframe window"
```

---

### Task 2: clipboard-core Windows target 编译验证

**Files:**
- Modify: `crates/core/src/pairing.rs` / `session.rs` / `crypto.rs`（仅在编译报错处追加 `#[cfg(windows)]` 分支）

**Interfaces:**
- Consumes: 无。
- Produces: `clipboard-core` 在 `x86_64-pc-windows-msvc` 下 `--no-default-features --features full` 编译通过；既有行为不变。

- [ ] **Step 1: 安装 Windows target 并 check**

```zsh
source scripts/external-dev-env.zsh
rustup target add x86_64-pc-windows-msvc
cargo check -p clipboard-core --no-default-features --features full --target x86_64-pc-windows-msvc
```
Expected: 通过。已知 POSIX-only 代码均有 `#[cfg(unix)]` 门控（`pairing.rs:10,392,476`、`crypto.rs:408`、`session.rs:36,688`），`cargo check` 不链接，无需 MSVC 链接器。

- [ ] **Step 2: 若有报错，补 `#[cfg(windows)]` 分支**

规则：只在报错处追加平台分支；POSIX 分支逻辑原样保留；Windows 分支跳过 POSIX 权限设置（NTFS ACL 默认仅当前用户可写，可接受）。典型形态：

```rust
let mut options = OpenOptions::new();
options.write(true).create_new(true);
#[cfg(unix)]
options.mode(0o600);
// Windows: 无等价 mode()，跳过即可
```

- [ ] **Step 3: 回归既有测试**

```zsh
cargo test -p clipboard-core --features full
```
Expected: 全部通过（含 loopback fixture）。

- [ ] **Step 4: Commit（若有改动）**

```bash
git add crates/core
git commit -m "fix(core): cfg(windows) shims for msvc target compilation"
```

---

### Task 3: settings.rs —— 设置持久化与 relay URL 校验（TDD）

**Files:**
- Create: `crates/desktop/src/settings.rs`
- Modify: `crates/desktop/src/main.rs`（声明模块）

**Interfaces:**
- Consumes: 无。
- Produces（后续 Task 依赖的精确签名）：
  ```rust
  pub struct Settings { pub relay_url: String, pub autostart: bool }
  impl Settings {
      pub fn load(path: &Path) -> Settings;          // 读失败/缺文件 → default
      pub fn save(&self, path: &Path) -> Result<(), String>;
  }
  impl Default for Settings { /* relay_url: "", autostart: false */ }
  pub fn validate_relay_url(url: &str) -> Result<(), &'static str>;
  ```

- [ ] **Step 1: 写失败测试**

`crates/desktop/src/settings.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_wss_url() {
        assert!(validate_relay_url("wss://sync.example.com/ws").is_ok());
    }

    #[test]
    fn rejects_non_wss_in_release_rules() {
        // 校验规则与 core validate_server 对齐：非 wss 一律拒绝
        // （core 中 ws:// 仅 debug 放行；本函数按 release 规则拒绝，
        //  本地联调用 ws:// 时走 cfg!(debug_assertions) 分支）
        assert!(validate_relay_url("https://sync.example.com").is_err());
        assert!(validate_relay_url("sync.example.com").is_err());
        assert!(validate_relay_url("").is_err());
    }

    #[test]
    fn rejects_whitespace_and_userinfo() {
        assert!(validate_relay_url("wss://user@sync.example.com").is_err());
        assert!(validate_relay_url("wss://sync.example.com/w s").is_err());
    }

    #[test]
    fn rejects_fragment() {
        assert!(validate_relay_url("wss://sync.example.com/ws#x").is_err());
    }

    #[test]
    fn ws_allowed_only_in_debug() {
        assert_eq!(
            validate_relay_url("ws://127.0.0.1:8787/ws").is_ok(),
            cfg!(debug_assertions)
        );
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let settings = Settings {
            relay_url: "wss://sync.example.com/ws".to_owned(),
            autostart: true,
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
    }

    #[test]
    fn load_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Settings::load(&dir.path().join("nope.json")),
            Settings::default()
        );
    }

    #[test]
    fn load_corrupt_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
    }
}
```

- [ ] **Step 2: 运行确认失败**

```zsh
cargo test -p clipsync-desktop settings
```
Expected: 编译失败（`validate_relay_url` 未定义）。

- [ ] **Step 3: 实现**

```rust
use std::path::Path;

use serde::{Deserialize, Serialize};

/// 桌面壳自身设置（独立于 core 数据）。relay_url 默认空串，
/// 由用户在设置页填写——与 macOS SettingsStore 现状一致。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub relay_url: String,
    #[serde(default)]
    pub autostart: bool,
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| e.to_string())
    }
}

/// 与 crates/core/src/pairing.rs 的 `validate_server`（pub(crate)，不可复用）
/// 规则镜像：wss:// 一律允许；ws:// 仅 debug 构建；拒绝空 authority、
/// userinfo、fragment、空白/控制字符。
pub fn validate_relay_url(url: &str) -> Result<(), &'static str> {
    let rest = if let Some(rest) = url.strip_prefix("wss://") {
        rest
    } else if cfg!(debug_assertions) {
        url.strip_prefix("ws://").ok_or("需要 wss:// 开头的 relay 地址")?
    } else {
        return Err("需要 wss:// 开头的 relay 地址");
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .ok_or("地址格式无效")?;
    let valid = !authority.is_empty()
        && !authority.contains('@')
        && !url.contains('#')
        && url
            .bytes()
            .all(|b| !b.is_ascii_whitespace() && !b.is_ascii_control());
    if valid { Ok(()) } else { Err("relay 地址无效") }
}
```

`main.rs` 顶部加 `mod settings;`（后续 Task 7 会重组为 lib/bin 分离，见 Task 5 Step 1）。

- [ ] **Step 4: 运行确认通过**

```zsh
cargo test -p clipsync-desktop settings
```
Expected: 8 个测试全绿。

- [ ] **Step 5: Commit**

```bash
git add crates/desktop
git commit -m "feat(desktop): settings persistence and relay URL validation"
```

---

### Task 4: clipboard_monitor.rs —— 轮询监控 + digest ownership token（TDD）

剪贴板无跨平台 change counter（arboard 不提供），用内容 digest 等价实现 macOS `PasteboardMonitor` 的语义：文本 digest = `clipboard_core::session::text_hash`，图片 = sha256(png bytes)。ownership token 防回环；`disconnect_baseline` 决定 mailbox clip 是 Applied 还是 Deferred。

**Files:**
- Create: `crates/desktop/src/clipboard_monitor.rs`
- Modify: `crates/desktop/src/main.rs`（声明模块）

**Interfaces:**
- Consumes: `clipboard_core::session::text_hash`；`sha2::{Digest, Sha256}`；`arboard`。
- Produces（后续 Task 依赖的精确签名）：
  ```rust
  #[derive(Clone, Debug, Eq, PartialEq)]
  pub enum ClipboardPayload { Text(String), ImagePng(Vec<u8>) }
  pub fn digest(payload: &ClipboardPayload) -> String;
  pub trait ClipboardIo: Send {
      fn read(&mut self) -> Option<ClipboardPayload>;   // None = 空/不支持/读失败
      fn write(&mut self, payload: &ClipboardPayload) -> bool;
  }
  #[derive(Default)]
  pub struct MonitorState { /* last_digest, ownership_digest, disconnect_baseline */ }
  pub enum PollDecision { Unchanged, OwnWrite, LocalChange }
  pub fn decide_poll(state: &mut MonitorState, current: Option<&ClipboardPayload>) -> PollDecision;
  pub fn decide_mailbox(state: &MonitorState, current: Option<&ClipboardPayload>) -> clipboard_core::ffi::MailboxDisposition;
  pub fn record_write(state: &mut MonitorState, payload: &ClipboardPayload, mailbox: bool);
  pub struct ArboardIo { /* 私有 */ }
  impl ArboardIo { pub fn new() -> Option<Self>; }
  #[derive(Clone)]
  pub struct MonitorHandle { /* Arc<Mutex<MonitorState>> + writer */ }
  impl MonitorHandle {
      pub fn new(state: Arc<Mutex<MonitorState>>) -> Self;
      pub fn apply_remote(&self, payload: &ClipboardPayload, mailbox: bool) -> bool;
      pub fn mark_disconnected(&self);
      pub fn mark_connected(&self);
      pub fn state(&self) -> Arc<Mutex<MonitorState>>;
  }
  pub fn spawn_poller(io: impl ClipboardIo + 'static, state: Arc<Mutex<MonitorState>>,
                      tx: std::sync::mpsc::Sender<ClipboardPayload>) -> std::thread::JoinHandle<()>;
  ```

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use clipboard_core::ffi::MailboxDisposition;

    fn text(s: &str) -> ClipboardPayload { ClipboardPayload::Text(s.to_owned()) }

    #[test]
    fn digest_is_stable_and_kind_sensitive() {
        assert_eq!(digest(&text("hello")), clipboard_core::session::text_hash("hello"));
        assert_ne!(digest(&text("a")), digest(&text("b")));
        assert_ne!(
            digest(&ClipboardPayload::ImagePng(vec![1, 2, 3])),
            digest(&ClipboardPayload::ImagePng(vec![1, 2, 4]))
        );
    }

    #[test]
    fn poll_unchanged_when_digest_matches() {
        let mut state = MonitorState::default();
        let payload = text("x");
        assert_eq!(decide_poll(&mut state, Some(&payload)), PollDecision::LocalChange);
        assert_eq!(decide_poll(&mut state, Some(&payload)), PollDecision::Unchanged);
    }

    #[test]
    fn poll_consumes_own_write_once() {
        let mut state = MonitorState::default();
        let payload = text("remote clip");
        record_write(&mut state, &payload, false);
        assert_eq!(decide_poll(&mut state, Some(&payload)), PollDecision::OwnWrite);
        // 之后再变化仍是本地变更
        let other = text("user typed");
        assert_eq!(decide_poll(&mut state, Some(&other)), PollDecision::LocalChange);
    }

    #[test]
    fn poll_empty_clipboard_is_unchanged() {
        let mut state = MonitorState::default();
        assert_eq!(decide_poll(&mut state, None), PollDecision::Unchanged);
        let payload = text("x");
        assert_eq!(decide_poll(&mut state, Some(&payload)), PollDecision::LocalChange);
        // 剪贴板被清空后再恢复同一内容，视为新的本地变更
        assert_eq!(decide_poll(&mut state, None), PollDecision::Unchanged);
        assert_eq!(decide_poll(&mut state, Some(&payload)), PollDecision::LocalChange);
    }

    #[test]
    fn mailbox_applied_when_baseline_intact() {
        let mut state = MonitorState::default();
        let payload = text("at disconnect");
        decide_poll(&mut state, Some(&payload));
        state.mark_disconnected();
        assert_eq!(
            decide_mailbox(&state, Some(&payload)),
            MailboxDisposition::Applied
        );
    }

    #[test]
    fn mailbox_deferred_when_clipboard_changed_while_disconnected() {
        let mut state = MonitorState::default();
        let payload = text("at disconnect");
        decide_poll(&mut state, Some(&payload));
        state.mark_disconnected();
        let changed = text("user copied during outage");
        assert_eq!(
            decide_mailbox(&state, Some(&changed)),
            MailboxDisposition::Deferred
        );
    }

    #[test]
    fn mailbox_applied_when_connected() {
        let state = MonitorState::default(); // 从未 disconnect
        assert_eq!(
            decide_mailbox(&state, Some(&text("anything"))),
            MailboxDisposition::Applied
        );
    }

    #[test]
    fn mailbox_apply_advances_baseline() {
        let mut state = MonitorState::default();
        let payload = text("at disconnect");
        decide_poll(&mut state, Some(&payload));
        state.mark_disconnected();
        let remote = text("mailbox payload");
        record_write(&mut state, &remote, true);
        // 应用后 baseline 指向新写入内容，下一封 mailbox 不再被误判
        assert_eq!(
            decide_mailbox(&state, Some(&remote)),
            MailboxDisposition::Applied
        );
    }

    #[derive(Clone)]
    struct FakeClipboard {
        content: Option<ClipboardPayload>,
        writes: Vec<ClipboardPayload>,
    }
    impl ClipboardIo for FakeClipboard {
        fn read(&mut self) -> Option<ClipboardPayload> { self.content.clone() }
        fn write(&mut self, payload: &ClipboardPayload) -> bool {
            self.content = Some(payload.clone());
            self.writes.push(payload.clone());
            true
        }
    }

    #[test]
    fn apply_remote_sets_ownership_and_returns_true() {
        let state = Arc::new(Mutex::new(MonitorState::default()));
        let fake = FakeClipboard { content: None, writes: vec![] };
        let handle = MonitorHandle::with_io(state.clone(), fake);
        let payload = text("from peer");
        assert!(handle.apply_remote(&payload, false));
        let guard = state.lock().unwrap();
        assert_eq!(guard.ownership_digest, Some(digest(&payload)));
    }
}
```

- [ ] **Step 2: 运行确认失败**

```zsh
cargo test -p clipsync-desktop clipboard_monitor
```
Expected: 编译失败（类型未定义）。

- [ ] **Step 3: 实现**

```rust
//! 剪贴板监控：300ms 轮询 + digest ownership token。
//! macOS PasteboardMonitor 用 NSPasteboard.changeCount；arboard 无 change
//! counter，这里用内容 digest 等价实现（text_hash / sha256(png)）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use clipboard_core::ffi::MailboxDisposition;
use sha2::{Digest, Sha256};

/// 轮询间隔，对齐 macOS PasteboardMonitor.pollInterval。
pub const POLL_INTERVAL: Duration = Duration::from_millis(300);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipboardPayload {
    Text(String),
    /// PNG 编码字节（协议线上格式，≤10MiB 由 core 约束）。
    ImagePng(Vec<u8>),
}

/// 内容指纹：文本走 core 的 text_hash 契约，图片走 sha256(png)。
pub fn digest(payload: &ClipboardPayload) -> String {
    match payload {
        ClipboardPayload::Text(text) => clipboard_core::session::text_hash(text),
        ClipboardPayload::ImagePng(bytes) => {
            let d = Sha256::digest(bytes);
            d.iter().map(|b| format!("{b:02x}")).collect()
        }
    }
}

/// 平台剪贴板读写抽象；测试用 FakeClipboard 注入。
pub trait ClipboardIo: Send {
    fn read(&mut self) -> Option<ClipboardPayload>;
    fn write(&mut self, payload: &ClipboardPayload) -> bool;
}

/// 监控共享状态（轮询线程 / core 回调线程 / UI 都可能触碰）。
#[derive(Default)]
pub struct MonitorState {
    last_digest: Option<String>,
    ownership_digest: Option<String>,
    disconnect_baseline: Option<String>,
}

impl MonitorState {
    pub fn mark_disconnected(&mut self) {
        self.disconnect_baseline = self.last_digest.clone();
    }
    pub fn mark_connected(&mut self) {
        self.disconnect_baseline = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PollDecision {
    Unchanged,
    OwnWrite,
    LocalChange,
}

/// 一轮轮询的纯决策：空剪贴板视为"无变化"并重置基线。
pub fn decide_poll(state: &mut MonitorState, current: Option<&ClipboardPayload>) -> PollDecision {
    let Some(payload) = current else {
        state.last_digest = None;
        return PollDecision::Unchanged;
    };
    let d = digest(payload);
    if state.ownership_digest.as_deref() == Some(d.as_str()) {
        state.ownership_digest = None;
        state.last_digest = Some(d);
        return PollDecision::OwnWrite;
    }
    if state.last_digest.as_deref() == Some(d.as_str()) {
        return PollDecision::Unchanged;
    }
    state.last_digest = Some(d);
    PollDecision::LocalChange
}

/// mailbox clip 的处置：断线期间剪贴板被改动过 → Deferred，否则 Applied。
pub fn decide_mailbox(
    state: &MonitorState,
    current: Option<&ClipboardPayload>,
) -> MailboxDisposition {
    match &state.disconnect_baseline {
        None => MailboxDisposition::Applied,
        Some(baseline) => {
            let now = current.map(digest);
            if now.as_ref() == Some(baseline) {
                MailboxDisposition::Applied
            } else {
                MailboxDisposition::Deferred
            }
        }
    }
}

/// 本壳写入剪贴板后记录 ownership（mailbox 应用同时推进 baseline）。
pub fn record_write(state: &mut MonitorState, payload: &ClipboardPayload, mailbox: bool) {
    let d = digest(payload);
    state.ownership_digest = Some(d.clone());
    state.last_digest = Some(d.clone());
    if mailbox {
        state.disconnect_baseline = Some(d);
    }
}

/// arboard 实现。Windows 上剪贴板被占用时读失败 → 当轮视为空（跳过）。
pub struct ArboardIo {
    inner: arboard::Clipboard,
}

impl ArboardIo {
    pub fn new() -> Option<Self> {
        arboard::Clipboard::new().ok().map(|inner| Self { inner })
    }
}

impl ClipboardIo for ArboardIo {
    fn read(&mut self) -> Option<ClipboardPayload> {
        if let Ok(text) = self.inner.get_text() {
            if !text.is_empty() {
                return Some(ClipboardPayload::Text(text));
            }
        }
        if let Ok(img) = self.inner.get_image() {
            return rgba_to_png(img.width as u32, img.height as u32, &img.bytes)
                .map(ClipboardPayload::ImagePng);
        }
        None
    }

    fn write(&mut self, payload: &ClipboardPayload) -> bool {
        match payload {
            ClipboardPayload::Text(text) => self.inner.set_text(text.clone()).is_ok(),
            ClipboardPayload::ImagePng(bytes) => {
                match png_to_rgba(bytes) {
                    Some((w, h, rgba)) => self
                        .inner
                        .set_image(arboard::ImageData {
                            width: w as usize,
                            height: h as usize,
                            bytes: rgba.into(),
                        })
                        .is_ok(),
                    None => false,
                }
            }
        }
    }
}

/// RGBA → PNG（image crate 只启 png feature，够用）。
pub fn rgba_to_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let buffer = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut out, image::ImageFormat::Png)
        .ok()?;
    Some(out.into_inner())
}

/// PNG → (width, height, RGBA)。
pub fn png_to_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some((w, h, img.into_raw()))
}

/// 给 core 回调线程使用的远程应用入口。每次操作新建 Clipboard 实例
/// （arboard 非 Sync，且回调线程与轮询线程各自持有实例）。
#[derive(Clone)]
pub struct MonitorHandle {
    state: Arc<Mutex<MonitorState>>,
    factory: Arc<dyn Fn() -> Option<Box<dyn ClipboardIo>> + Send + Sync>,
}

impl MonitorHandle {
    /// 生产实现：回调线程内即时构造 arboard Clipboard。
    pub fn new(state: Arc<Mutex<MonitorState>>) -> Self {
        Self {
            state,
            factory: Arc::new(|| {
                ArboardIo::new().map(|io| Box::new(io) as Box<dyn ClipboardIo>)
            }),
        }
    }

    /// 测试/集成测试注入假剪贴板；工厂每次克隆，可多次应用。
    #[doc(hidden)]
    pub fn with_io(state: Arc<Mutex<MonitorState>>, io: impl ClipboardIo + Clone + 'static) -> Self {
        Self {
            state,
            factory: Arc::new(move || {
                Some(Box::new(io.clone()) as Box<dyn ClipboardIo>)
            }),
        }
    }

    pub fn state(&self) -> Arc<Mutex<MonitorState>> {
        self.state.clone()
    }

    /// 把远端 clip 写入本机剪贴板。mailbox=true 时先按 baseline 决策；
    /// 返回是否真正写入（false = 写失败或应 Deferred，由调用方决定语义）。
    pub fn apply_remote(&self, payload: &ClipboardPayload, mailbox: bool) -> bool {
        let mut io = match (self.factory)() {
            Some(io) => io,
            None => return false,
        };
        if mailbox {
            let current = io.read();
            let decision = {
                let state = self.state.lock().unwrap();
                decide_mailbox(&state, current.as_ref())
            };
            if decision == MailboxDisposition::Deferred {
                return false;
            }
        }
        if !io.write(payload) {
            return false;
        }
        let mut state = self.state.lock().unwrap();
        record_write(&mut state, payload, mailbox);
        true
    }

    pub fn mark_disconnected(&self) {
        self.state.lock().unwrap().mark_disconnected();
    }

    pub fn mark_connected(&self) {
        self.state.lock().unwrap().mark_connected();
    }
}

/// 启动轮询线程；本地变更通过 tx 上报（决策由 decide_poll 完成）。
pub fn spawn_poller(
    mut io: impl ClipboardIo + 'static,
    state: Arc<Mutex<MonitorState>>,
    tx: std::sync::mpsc::Sender<ClipboardPayload>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("clipsync-clipboard-monitor".to_owned())
        .spawn(move || loop {
            let current = io.read();
            let decision = {
                let mut guard = state.lock().unwrap();
                decide_poll(&mut guard, current.as_ref())
            };
            if decision == PollDecision::LocalChange
                && let Some(payload) = current
                && tx.send(payload).is_err()
            {
                break; // UI 已退出
            }
            std::thread::sleep(POLL_INTERVAL);
        })
        .expect("clipboard monitor thread spawns")
}
```

`main.rs` 加 `mod clipboard_monitor;`。

- [ ] **Step 4: 运行确认通过**

```zsh
cargo test -p clipsync-desktop clipboard_monitor
```
Expected: 9 个测试全绿。

- [ ] **Step 5: Commit**

```bash
git add crates/desktop
git commit -m "feat(desktop): clipboard monitor with digest ownership token"
```

---

### Task 5: core_bridge.rs —— CoreHandle 包装 + 专线程 executor（TDD）

`CoreHandle` 的 `pair_begin/pair_confirm/start` 内部做最长 15s 的网络 I/O（`IO_TIMEOUT`），`send_text` 阻塞到帧写入 relay——都不能跑在 UI 线程。本任务引入「core executor」：单线程顺序执行 `BridgeCommand`，结果经 `CoreEvent` channel 回 UI（对应 macOS 的 `CoreExecutor`）。同时把 `main.rs` 重组为 lib + bin，让集成测试可链接。

**Files:**
- Create: `crates/desktop/src/lib.rs`
- Create: `crates/desktop/src/core_bridge.rs`
- Modify: `crates/desktop/src/main.rs`（改为调用 lib 的 `run()`，模块声明移入 lib.rs）
- Move: `mod settings; mod clipboard_monitor;` 声明 → `lib.rs`

**Interfaces:**
- Consumes: `clipboard_core::ffi::*`；`crate::clipboard_monitor::{MonitorHandle, ClipboardPayload}`；`crate::settings::Settings`。
- Produces（后续 Task 依赖的精确签名）：
  ```rust
  pub enum BridgeCommand {
      PairBegin(String), PairConfirm(String), PairCancel, LoadAndStart,
      SendText(String), SendImage(Vec<u8>),
      HistoryRefresh, HistoryImage(String), HistoryApply(String), HistoryClear,
      ResetPairing, Shutdown,
  }
  pub enum CoreEvent {
      Status(CoreStatus), LiveClip(FfiClipItem),
      QrReady(Result<String, CoreError>),
      PairConfirmed(Result<(), CoreError>),
      PairCancelled,
      PairedLoaded(bool),
      SessionStarted(Result<(), CoreError>),
      History(Result<Vec<FfiHistoryItem>, CoreError>),
      HistoryImage { id: String, result: Result<Vec<u8>, CoreError> },
      Applied(Result<(), CoreError>),
      Cleared(Result<(), CoreError>),
      ResetDone(Result<(), CoreError>),
  }
  pub struct CoreBridge { /* tx: Sender<BridgeCommand>, handle: Arc<CoreHandle> */ }
  impl CoreBridge {
      pub fn spawn(data_dir: &std::path::Path, monitor: MonitorHandle,
                   event_tx: std::sync::mpsc::Sender<CoreEvent>) -> Result<Self, CoreError>;
      pub fn send(&self, cmd: BridgeCommand);
      pub fn pair_poll(&self) -> PairingSnapshot;      // 非阻塞，UI 线程安全
      pub fn is_echo(&self, hash: String) -> bool;
      pub fn shutdown(self);
  }
  ```

- [ ] **Step 1: 重组为 lib + bin**

`crates/desktop/src/lib.rs`：

```rust
pub mod clipboard_monitor;
pub mod core_bridge;
pub mod settings;
```

本 Task 不动 `main.rs`：HelloApp 保持自包含、不引用 lib，单独编译通过（Task 7 才替换为 `clipsync_desktop::app::run()`）。`cargo test` 走 lib target，`cargo run` 走 bin target，互不干扰。

- [ ] **Step 2: 写失败测试（全部离线可跑）**

`core_bridge.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard_monitor::MonitorState;
    use std::sync::{Arc, Mutex};

    fn monitor() -> MonitorHandle {
        MonitorHandle::new(Arc::new(Mutex::new(MonitorState::default())))
    }

    fn recv_event(rx: &std::sync::mpsc::Receiver<CoreEvent>) -> CoreEvent {
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("event arrives")
    }

    #[test]
    fn load_and_start_reports_unpaired() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let bridge = CoreBridge::spawn(dir.path(), monitor(), tx).unwrap();
        bridge.send(BridgeCommand::LoadAndStart);
        match recv_event(&rx) {
            CoreEvent::PairedLoaded(false) => {}
            other => panic!("expected PairedLoaded(false), got {other:?}"),
        }
        bridge.shutdown();
    }

    #[test]
    fn history_is_empty_on_fresh_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let bridge = CoreBridge::spawn(dir.path(), monitor(), tx).unwrap();
        bridge.send(BridgeCommand::HistoryRefresh);
        match recv_event(&rx) {
            CoreEvent::History(Ok(items)) => assert!(items.is_empty()),
            other => panic!("expected empty History, got {other:?}"),
        }
        bridge.shutdown();
    }

    #[test]
    fn pair_begin_rejects_invalid_url() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let bridge = CoreBridge::spawn(dir.path(), monitor(), tx).unwrap();
        bridge.send(BridgeCommand::PairBegin("not-a-url".to_owned()));
        match recv_event(&rx) {
            CoreEvent::QrReady(Err(CoreError::InvalidInput(_))) => {}
            other => panic!("expected InvalidInput, got {other:?}"),
        }
        assert_eq!(bridge.pair_poll(), PairingSnapshot::Unpaired);
        bridge.shutdown();
    }

    #[test]
    fn is_echo_false_without_session() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let bridge = CoreBridge::spawn(dir.path(), monitor(), tx).unwrap();
        assert!(!bridge.is_echo("deadbeef".to_owned()));
        bridge.shutdown();
    }

    #[test]
    fn status_events_are_forwarded() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let bridge = CoreBridge::spawn(dir.path(), monitor(), tx).unwrap();
        // reset_pairing 在空目录上也会走 quiesce 流程并发 ReadyUnpaired
        bridge.send(BridgeCommand::ResetPairing);
        let mut saw_status = false;
        let mut saw_reset = false;
        while !(saw_status && saw_reset) {
            match recv_event(&rx) {
                CoreEvent::Status(CoreStatus::ReadyUnpaired) => saw_status = true,
                CoreEvent::ResetDone(Ok(())) => saw_reset = true,
                _ => {}
            }
        }
        bridge.shutdown();
    }
}
```

- [ ] **Step 3: 运行确认失败**

```zsh
cargo test -p clipsync-desktop core_bridge
```
Expected: 编译失败（`CoreBridge` 未定义）。

- [ ] **Step 4: 实现**

```rust
//! CoreHandle 的桌面壳包装：所有阻塞 FFI 调用在专用 executor 线程上顺序
//! 执行（对应 macOS CoreExecutor），UI 线程只收发 channel 消息。

use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use clipboard_core::ffi::{
    CoreCallbacks, CoreError, CoreHandle, CoreStatus, FfiClipContent, FfiClipItem,
    FfiHistoryItem, MailboxDisposition, PairingSnapshot,
};

use crate::clipboard_monitor::{ClipboardPayload, MonitorHandle, MonitorState};

#[derive(Debug)]
pub enum BridgeCommand {
    PairBegin(String),
    PairConfirm(String),
    PairCancel,
    LoadAndStart,
    SendText(String),
    SendImage(Vec<u8>),
    HistoryRefresh,
    HistoryImage(String),
    HistoryApply(String),
    HistoryClear,
    ResetPairing,
    Shutdown,
}

#[derive(Debug)]
pub enum CoreEvent {
    Status(CoreStatus),
    LiveClip(FfiClipItem),
    QrReady(Result<String, CoreError>),
    PairConfirmed(Result<(), CoreError>),
    PairCancelled,
    PairedLoaded(bool),
    SessionStarted(Result<(), CoreError>),
    History(Result<Vec<FfiHistoryItem>, CoreError>),
    HistoryImage { id: String, result: Result<Vec<u8>, CoreError> },
    Applied(Result<(), CoreError>),
    Cleared(Result<(), CoreError>),
    ResetDone(Result<(), CoreError>),
}

/// CoreCallbacks → 剪贴板应用 + 事件转发。回调在 core dispatcher 线程上
/// 触发，允许回调内再入 handle（core 契约保证不持锁回调）。
struct BridgeCallbacks {
    monitor: MonitorHandle,
    event_tx: Sender<CoreEvent>,
}

impl BridgeCallbacks {
    fn payload_of(item: &FfiClipItem) -> ClipboardPayload {
        match &item.content {
            FfiClipContent::Text { text } => ClipboardPayload::Text(text.clone()),
            FfiClipContent::Image { bytes } => ClipboardPayload::ImagePng(bytes.clone()),
        }
    }
}

impl CoreCallbacks for BridgeCallbacks {
    fn on_clip(&self, item: FfiClipItem) -> Result<(), CoreError> {
        let payload = Self::payload_of(&item);
        self.monitor.apply_remote(&payload, false);
        let _ = self.event_tx.send(CoreEvent::LiveClip(item));
        Ok(())
    }

    fn on_mailbox_clip(&self, item: FfiClipItem) -> Result<MailboxDisposition, CoreError> {
        let payload = Self::payload_of(&item);
        let applied = self.monitor.apply_remote(&payload, true);
        let _ = self.event_tx.send(CoreEvent::LiveClip(item));
        Ok(if applied {
            MailboxDisposition::Applied
        } else {
            MailboxDisposition::Deferred
        })
    }

    fn on_status(&self, status: CoreStatus) {
        match &status {
            CoreStatus::Connected => self.monitor.mark_connected(),
            CoreStatus::Disconnected => self.monitor.mark_disconnected(),
            _ => {}
        }
        let _ = self.event_tx.send(CoreEvent::Status(status));
    }
}

pub struct CoreBridge {
    tx: Mutex<Option<Sender<BridgeCommand>>>,
    handle: Arc<CoreHandle>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl CoreBridge {
    pub fn spawn(
        data_dir: &Path,
        monitor: MonitorHandle,
        event_tx: Sender<CoreEvent>,
    ) -> Result<Self, CoreError> {
        let callbacks = BridgeCallbacks {
            monitor,
            event_tx: event_tx.clone(),
        };
        let handle = CoreHandle::new(
            data_dir.to_string_lossy().into_owned(),
            Box::new(callbacks),
        )?;
        let (tx, rx) = std::sync::mpsc::channel::<BridgeCommand>();
        let executor = std::thread::Builder::new()
            .name("clipsync-core-executor".to_owned())
            .spawn({
                let handle = handle.clone();
                move || run_executor(&handle, rx, &event_tx)
            })
            .map_err(|e| CoreError::Internal(e.to_string()))?;
        Ok(Self {
            tx: Mutex::new(Some(tx)),
            handle,
            join: Mutex::new(Some(executor)),
        })
    }

    pub fn send(&self, cmd: BridgeCommand) {
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            let _ = tx.send(cmd);
        }
    }

    /// 非阻塞快照，UI 线程每帧调用安全。
    pub fn pair_poll(&self) -> PairingSnapshot {
        self.handle.pair_poll()
    }

    pub fn is_echo(&self, hash: String) -> bool {
        self.handle.is_echo(hash)
    }

    /// 发送 Shutdown、join executor 线程、关闭 core handle（幂等）。
    pub fn shutdown(self) {
        if let Some(tx) = self.tx.lock().unwrap().take() {
            let _ = tx.send(BridgeCommand::Shutdown);
            drop(tx);
        }
        if let Some(join) = self.join.lock().unwrap().take() {
            let _ = join.join();
        }
        let _ = self.handle.shutdown();
    }
}

fn run_executor(handle: &CoreHandle, rx: Receiver<BridgeCommand>, event_tx: &Sender<CoreEvent>) {
    while let Ok(cmd) = rx.recv() {
        let quit = matches!(cmd, BridgeCommand::Shutdown);
        match cmd {
            BridgeCommand::PairBegin(url) => {
                let _ = event_tx.send(CoreEvent::QrReady(handle.pair_begin(url)));
            }
            BridgeCommand::PairConfirm(sas) => {
                let _ = event_tx.send(CoreEvent::PairConfirmed(handle.pair_confirm(sas)));
            }
            BridgeCommand::PairCancel => {
                let _ = handle.pair_cancel();
                let _ = event_tx.send(CoreEvent::PairCancelled);
            }
            BridgeCommand::LoadAndStart => match handle.pair_load() {
                Ok(false) => {
                    let _ = event_tx.send(CoreEvent::PairedLoaded(false));
                }
                Ok(true) => {
                    let _ = event_tx.send(CoreEvent::PairedLoaded(true));
                    let result = handle.start();
                    let _ = event_tx.send(CoreEvent::SessionStarted(result));
                }
                Err(e) => {
                    let _ = event_tx.send(CoreEvent::SessionStarted(Err(e)));
                }
            },
            BridgeCommand::SendText(text) => {
                if let Err(e) = handle.send_text(text) {
                    log::warn!("send_text failed: {e}");
                }
            }
            BridgeCommand::SendImage(bytes) => {
                if let Err(e) = handle.send_image(bytes) {
                    log::warn!("send_image failed: {e}");
                }
            }
            BridgeCommand::HistoryRefresh => {
                let _ = event_tx.send(CoreEvent::History(handle.history()));
            }
            BridgeCommand::HistoryImage(id) => {
                let result = handle.history_image_bytes(id.clone());
                let _ = event_tx.send(CoreEvent::HistoryImage { id, result });
            }
            BridgeCommand::HistoryApply(id) => {
                let _ = event_tx.send(CoreEvent::Applied(handle.history_apply(id)));
            }
            BridgeCommand::HistoryClear => {
                let _ = event_tx.send(CoreEvent::Cleared(handle.history_clear()));
            }
            BridgeCommand::ResetPairing => {
                let _ = event_tx.send(CoreEvent::ResetDone(handle.reset_pairing()));
            }
            BridgeCommand::Shutdown => {}
        }
        if quit {
            break;
        }
    }
}
```

- [ ] **Step 5: 运行确认通过**

```zsh
cargo test -p clipsync-desktop core_bridge
```
Expected: 5 个测试全绿。

- [ ] **Step 6: Commit**

```bash
git add crates/desktop
git commit -m "feat(desktop): core bridge with dedicated executor thread"
```

---

### Task 6: tray.rs + autostart.rs —— 托盘与自启动

**Files:**
- Create: `crates/desktop/src/tray.rs`
- Create: `crates/desktop/src/autostart.rs`
- Modify: `crates/desktop/src/lib.rs`（加 `pub mod tray; pub mod autostart;`）

**Interfaces:**
- Consumes: `tray-icon`、`auto-launch` crate。
- Produces（精确签名）：
  ```rust
  // tray.rs
  #[derive(Clone, Copy, Debug, Eq, PartialEq)]
  pub enum TrayCommand { OpenWindow, SendCurrent, Quit }
  pub struct Tray { _tray: tray_icon::TrayIcon }
  impl Tray {
      pub fn spawn(tx: std::sync::mpsc::Sender<TrayCommand>) -> Result<Tray, String>;
  }
  // autostart.rs
  pub fn set_enabled(enabled: bool) -> Result<(), String>;
  pub fn is_enabled() -> bool;
  ```

- [ ] **Step 1: 实现 tray.rs**

托盘图标：无 PNG 资产，程序化生成 32×32 RGBA（圆角方块 + 两条白色"剪贴板横线"），v1 够用：

```rust
//! 系统托盘（Windows）/ 菜单栏 status item（macOS）。

use std::sync::mpsc::Sender;

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayCommand {
    OpenWindow,
    SendCurrent,
    Quit,
}

pub struct Tray {
    _tray: TrayIcon,
}

impl Tray {
    pub fn spawn(tx: Sender<TrayCommand>) -> Result<Tray, String> {
        let open = MenuItem::new("打开窗口", true, None);
        let send = MenuItem::new("发送当前剪贴板", true, None);
        let quit = MenuItem::new("退出", true, None);
        let ids = (open.id().clone(), send.id().clone(), quit.id().clone());
        let menu = Menu::with_items(&[&open, &send, &quit]).map_err(|e| e.to_string())?;
        let icon = Icon::from_rgba(icon_rgba(), 32, 32).map_err(|e| e.to_string())?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("ClipSync")
            .with_icon(icon)
            .build()
            .map_err(|e| e.to_string())?;

        MenuEvent::set_event_handler(Some(Box::new(move |event: MenuEvent| {
            let cmd = if event.id == ids.0 {
                TrayCommand::OpenWindow
            } else if event.id == ids.1 {
                TrayCommand::SendCurrent
            } else if event.id == ids.2 {
                TrayCommand::Quit
            } else {
                return;
            };
            let _ = tx.send(cmd);
        })));

        Ok(Tray { _tray: tray })
    }
}

/// 32×32 RGBA：圆角靛蓝方块 + 两条白色横线（剪贴板意象）。
fn icon_rgba() -> Vec<u8> {
    const N: usize = 32;
    let mut rgba = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let i = (y * N + x) * 4;
            let in_rounded_rect = x >= 2 && x < 30 && y >= 2 && y < 30;
            if !in_rounded_rect {
                continue; // 透明
            }
            // 白色横线（剪贴板纸张线条）
            let is_line = (y == 10 || y == 11 || y == 16 || y == 17) && x >= 8 && x < 24;
            if is_line {
                rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            } else {
                rgba[i..i + 4].copy_from_slice(&[99, 102, 241, 255]); // indigo-500
            }
        }
    }
    rgba
}
```

注意：`MenuEvent::set_event_handler` 的闭包签名若与所装 tray-icon 版本的 `EventHandler` 类型不符（如需要 `Send + Sync`），按编译器提示调整包装。`MenuId` 比较用 `event.id == ids.0`。

- [ ] **Step 2: 实现 autostart.rs**

```rust
//! 开机自启动：Windows 注册表 Run 键 / macOS Login Item。

use auto_launch::AutoLaunch;

fn launcher() -> Result<AutoLaunch, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().into_owned();
    Ok(auto_launch::AutoLaunchBuilder::new()
        .set_app_name("ClipSync")
        .set_app_path(&exe)
        .build()
        .map_err(|e| e.to_string())?)
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let launcher = launcher()?;
    let result = if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    };
    result.map_err(|e| e.to_string())
}

pub fn is_enabled() -> bool {
    launcher().and_then(|l| l.is_enabled().map_err(|e| e.to_string())).unwrap_or(false)
}
```

若所装 auto-launch 版本的 builder API 不同（如 `AutoLaunch::new(name, path, use_agent, args)`），按 crate docs 调整，签名保持 `set_enabled/is_enabled` 不变。

- [ ] **Step 3: 编译验证 + 手动冒烟**

```zsh
cargo check -p clipsync-desktop
```
Expected: 通过。托盘/自启动无单元测试（系统 API），冒烟合入 Task 7 的整窗验证。

- [ ] **Step 4: Commit**

```bash
git add crates/desktop
git commit -m "feat(desktop): tray icon and autostart wrappers"
```

---

### Task 7: app.rs —— 应用外壳：主题、CJK 字体、导航、关窗到托盘、macOS Dock 策略

UI 骨架。egui 默认字体不含 CJK，必须从系统加载（macOS `PingFang.ttc`、Windows `msyh.ttc`——底层 ttf-parser 取集合第 0 个 face，可用）。关窗 = 隐藏到托盘；macOS 上窗口隐藏时切 accessory（隐藏 Dock 图标），打开时切回 regular。

**Files:**
- Create: `crates/desktop/src/app.rs`
- Create: `crates/desktop/src/ui/mod.rs`（主题与字体）
- Create: `crates/desktop/src/platform.rs`（macOS activation policy + 非 macOS no-op）
- Modify: `crates/desktop/src/lib.rs`（`pub mod app; mod platform; pub mod ui;`）
- Modify: `crates/desktop/src/main.rs`（调用 `clipsync_desktop::app::run()`）

**Interfaces:**
- Consumes: 前面全部 Task 的类型（`CoreBridge/CoreEvent/BridgeCommand`、`MonitorHandle/spawn_poller/ArboardIo/ClipboardPayload`、`Tray/TrayCommand`、`Settings`）。
- Produces:
  ```rust
  // app.rs
  pub fn run() -> eframe::Result<()>;
  pub fn status_label(status: &CoreStatus) -> (&'static str, egui::Color32);
  // ui/mod.rs
  pub fn install_theme_and_fonts(ctx: &egui::Context);
  // platform.rs
  pub fn set_accessory_mode(accessory: bool);   // 非 macOS 为 no-op
  ```

- [ ] **Step 1: ui/mod.rs —— 主题与 CJK 字体**

```rust
use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

/// 统一强调色（indigo-500）。
pub const ACCENT: Color32 = Color32::from_rgb(99, 102, 241);

pub fn install_theme_and_fonts(ctx: &egui::Context) {
    ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::System);
    ctx.style_mut(|style| {
        let radius = CornerRadius::same(8);
        style.visuals.widgets.noninteractive.corner_radius = radius;
        style.visuals.widgets.inactive.corner_radius = radius;
        style.visuals.widgets.hovered.corner_radius = radius;
        style.visuals.widgets.active.corner_radius = radius;
        style.visuals.widgets.open.corner_radius = radius;
        style.visuals.selection.bg_fill = ACCENT.linear_multiply(0.4);
        style.visuals.hyperlink_color = ACCENT;
        style.text_styles.insert(TextStyle::Heading, FontId::proportional(22.0));
        style.text_styles.insert(TextStyle::Body, FontId::proportional(15.0));
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    });
    install_cjk_fonts(ctx);
}

/// egui 内置字体无 CJK；从系统字体目录取第一个可用候选。
fn install_cjk_fonts(ctx: &egui::Context) {
    #[cfg(target_os = "macos")]
    const CANDIDATES: &[&str] = &[
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
    ];
    #[cfg(target_os = "windows")]
    const CANDIDATES: &[&str] = &[
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\Deng.ttf",
        "C:\\Windows\\Fonts\\simhei.ttf",
        "C:\\Windows\\Fonts\\simsun.ttc",
    ];
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    const CANDIDATES: &[&str] = &[];

    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert("cjk".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push("cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        log::info!("CJK font loaded from {path}");
        return;
    }
    log::warn!("no CJK font found; Chinese text will render as boxes");
}

/// 导航/状态指示用的小圆点。
pub fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

/// 卡片容器（圆角浅底）。
pub fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .corner_radius(CornerRadius::same(10))
        .stroke(Stroke::NONE)
        .inner_margin(egui::Margin::same(12))
        .show(ui, add);
}
```

- [ ] **Step 2: platform.rs —— macOS Dock/accessory 切换**

```rust
//! 平台钩子：macOS 在窗口隐藏时切 accessory（隐藏 Dock 图标）。

#[cfg(target_os = "macos")]
pub fn set_accessory_mode(accessory: bool) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("set_accessory_mode called off main thread");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let policy = if accessory {
        NSApplicationActivationPolicy::Accessory
    } else {
        NSApplicationActivationPolicy::Regular
    };
    app.setActivationPolicy(policy);
}

#[cfg(not(target_os = "macos"))]
pub fn set_accessory_mode(_accessory: bool) {}
```

若 objc2-app-kit 的枚举路径不同（版本差异），按编译错误调整 import；调用语义不变。

- [ ] **Step 3: app.rs —— 外壳与事件循环**

```rust
//! eframe 应用外壳：事件泵 + 导航 + 关窗到托盘。
//! 规则：UI 线程不调用任何阻塞 CoreHandle 方法（一律走 BridgeCommand）。

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use clipboard_core::ffi::{CoreStatus, FfiHistoryItem, PairingSnapshot};
use egui::Color32;

use crate::clipboard_monitor::{
    self, ArboardIo, ClipboardPayload, MonitorHandle, MonitorState,
};
use crate::core_bridge::{BridgeCommand, CoreBridge, CoreEvent};
use crate::platform;
use crate::settings::Settings;
use crate::tray::{Tray, TrayCommand};
use crate::ui;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    Pairing,
    History,
    Settings,
}

pub struct DesktopApp {
    bridge: CoreBridge,
    core_rx: Receiver<CoreEvent>,
    tray_rx: Receiver<TrayCommand>,
    clip_rx: Receiver<ClipboardPayload>,
    monitor: MonitorHandle,
    _tray: Tray,
    settings: Settings,
    settings_path: PathBuf,
    page: Page,
    status: Option<CoreStatus>,
    history: Vec<FfiHistoryItem>,
    toast: Option<(String, Instant)>,
}

/// 状态文字 + 圆点颜色（CoreStatus 8 态 → UI 文案）。
pub fn status_label(status: &CoreStatus) -> (&'static str, Color32) {
    match status {
        CoreStatus::ReadyUnpaired => ("未配对", Color32::GRAY),
        CoreStatus::Offering => ("等待手机扫码…", Color32::GOLD),
        CoreStatus::SasReady => ("待确认安全码", Color32::GOLD),
        CoreStatus::Connecting => ("连接中…", Color32::GOLD),
        CoreStatus::Connected => ("已连接", Color32::from_rgb(34, 197, 94)),
        CoreStatus::Reconnecting => ("重连中…", Color32::GOLD),
        CoreStatus::Disconnected => ("已断开", Color32::from_rgb(239, 68, 68)),
        CoreStatus::Error { .. } => ("出错了", Color32::from_rgb(239, 68, 68)),
    }
}

impl DesktopApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
        ui::install_theme_and_fonts(&cc.egui_ctx);
        platform::set_accessory_mode(false);

        let data_dir = directories::BaseDirs::new()
            .ok_or("无法定位用户数据目录")?
            .data_dir()
            .join("clipsync");
        std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;

        let settings_path = data_dir.join("settings.json");
        let settings = Settings::load(&settings_path);

        let state = Arc::new(Mutex::new(MonitorState::default()));
        let monitor = MonitorHandle::new(state.clone());
        let (core_tx, core_rx) = std::sync::mpsc::channel();
        let bridge = CoreBridge::spawn(&data_dir, monitor.clone(), core_tx)
            .map_err(|e| format!("core 启动失败: {e}"))?;
        bridge.send(BridgeCommand::LoadAndStart);

        let (clip_tx, clip_rx) = std::sync::mpsc::channel();
        if let Some(io) = ArboardIo::new() {
            clipboard_monitor::spawn_poller(io, state, clip_tx);
        } else {
            log::error!("clipboard unavailable; sync of local changes disabled");
        }

        let (tray_tx, tray_rx) = std::sync::mpsc::channel();
        let tray = Tray::spawn(tray_tx).map_err(|e| format!("托盘创建失败: {e}"))?;

        Ok(Self {
            bridge,
            core_rx,
            tray_rx,
            clip_rx,
            monitor,
            _tray: tray,
            settings,
            settings_path,
            page: Page::Pairing,
            status: None,
            history: vec![],
            toast: None,
        })
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.core_rx.try_recv() {
            self.on_core_event(event);
        }
        while let Ok(cmd) = self.tray_rx.try_recv() {
            match cmd {
                TrayCommand::OpenWindow => {
                    platform::set_accessory_mode(false);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayCommand::SendCurrent => self.send_current_clipboard(),
                TrayCommand::Quit => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    std::process::exit(0);
                }
            }
        }
        while let Ok(payload) = self.clip_rx.try_recv() {
            // 仅在已连接时上报本地剪贴板；未连接直接丢弃（core 也会拒发）
            if matches!(&self.status, Some(CoreStatus::Connected)) {
                self.send_payload(payload);
            }
        }
    }

    fn on_core_event(&mut self, event: CoreEvent) {
        match event {
            CoreEvent::Status(status) => self.status = Some(status),
            CoreEvent::LiveClip(_) => {
                self.bridge.send(BridgeCommand::HistoryRefresh);
            }
            CoreEvent::History(Ok(items)) => self.history = items,
            CoreEvent::QrReady(Err(e)) => self.toast(format!("配对失败：{e}")),
            CoreEvent::PairConfirmed(Err(e)) => self.toast(format!("确认失败：{e}")),
            CoreEvent::SessionStarted(Err(e)) => self.toast(format!("连接失败：{e}")),
            CoreEvent::Applied(Err(e)) | CoreEvent::Cleared(Err(e)) | CoreEvent::ResetDone(Err(e)) => {
                self.toast(format!("操作失败：{e}"))
            }
            _ => {}
        }
    }

    fn send_current_clipboard(&mut self) {
        if let Some(mut io) = ArboardIo::new()
            && let Some(payload) = io.read()
        {
            self.send_payload(payload);
        }
    }

    fn send_payload(&self, payload: ClipboardPayload) {
        match payload {
            ClipboardPayload::Text(text) => {
                if !self.bridge.is_echo(clipboard_core::session::text_hash(&text)) {
                    self.bridge.send(BridgeCommand::SendText(text));
                }
            }
            ClipboardPayload::ImagePng(bytes) => {
                self.bridge.send(BridgeCommand::SendImage(bytes));
            }
        }
    }

    fn toast(&mut self, message: String) {
        self.toast = Some((message, Instant::now()));
    }

    fn show_nav(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("nav")
            .resizable(false)
            .exact_width(200.0)
            .show(ctx, |ui| {
                ui.add_space(16.0);
                ui.heading("ClipSync");
                ui.add_space(16.0);
                for (page, label) in [
                    (Page::Pairing, "配对"),
                    (Page::History, "历史"),
                    (Page::Settings, "设置"),
                ] {
                    if ui.selectable_label(self.page == page, label).clicked() {
                        if page == Page::History {
                            self.bridge.send(BridgeCommand::HistoryRefresh);
                        }
                        self.page = page;
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(12.0);
                    let (label, color) = self
                        .status
                        .as_ref()
                        .map(status_label)
                        .unwrap_or(("启动中…", Color32::GRAY));
                    ui.horizontal(|ui| {
                        ui::status_dot(ui, color);
                        ui.label(label);
                    });
                });
            });
    }
}

impl eframe::App for DesktopApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 关窗 = 隐藏到托盘（不退出）
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            platform::set_accessory_mode(true);
        }

        self.drain_events(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(300));

        self.show_nav(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(12.0);
            match self.page {
                // 页面实现在 Task 8-10 填充
                Page::Pairing => ui.label("（配对页 —— Task 8）"),
                Page::History => ui.label("（历史页 —— Task 9）"),
                Page::Settings => ui.label("（设置页 —— Task 10）"),
            }
        });

        // toast：右下角短暂提示
        if let Some((message, at)) = &self.toast {
            if at.elapsed().as_secs() >= 4 {
                self.toast = None;
            } else {
                egui::Area::new(egui::Id::new("toast"))
                    .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.label(message);
                        });
                    });
            }
        }
    }
}

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ClipSync")
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native("ClipSync", options, Box::new(|cc| {
        // Box<dyn Error + Send + Sync> 实现了 From<String>，into() 直接可用
        DesktopApp::new(cc)
            .map(|app| Box::new(app) as Box<dyn eframe::App>)
            .map_err(Into::into)
    }))
}
```

`main.rs`：

```rust
fn main() -> eframe::Result<()> {
    env_logger::init();
    clipsync_desktop::app::run()
}
```

`lib.rs`：

```rust
pub mod app;
pub mod clipboard_monitor;
pub mod core_bridge;
mod platform;
pub mod settings;
pub mod tray;
pub mod ui;
```

注意：`DesktopApp::new` 返回 `Result<Self, String>`；run_native 的 creator 需要 `Result<Box<dyn App>, Box<dyn Error + Send + Sync>>`，`From<String>` 已实现，`.map_err(Into::into)` 即可。

- [ ] **Step 4: 编译 + 手动验证**

```zsh
cargo run -p clipsync-desktop
```
Expected:
- 窗口打开，左侧导航「配对 / 历史 / 设置」，底部「启动中…」→「未配对」；
- 中文渲染正常（非方框）；
- 点关闭按钮窗口消失但**进程不退**（`ps aux | grep clipsync-desktop` 仍在）；
- 托盘/菜单栏出现靛蓝图标，菜单三项齐全；「打开窗口」恢复窗口；「退出」结束进程；
- macOS：窗口隐藏后 Dock 图标消失，重开恢复。

- [ ] **Step 5: Commit**

```bash
git add crates/desktop
git commit -m "feat(desktop): app shell with nav, tray, close-to-tray, CJK fonts"
```

---

### Task 8: 配对页（ui/pairing.rs）

**Files:**
- Create: `crates/desktop/src/ui/pairing.rs`
- Modify: `crates/desktop/src/app.rs`（配对页接入 + QR 纹理缓存字段）

**Interfaces:**
- Consumes: `DesktopApp`（`bridge`、`settings.relay_url`、`toast()`）；`crate::settings::validate_relay_url`。
- Produces: `pub fn show(app: &mut DesktopApp, ui: &mut egui::Ui)`（在 app.rs 中以 `crate::ui::pairing::show(self, ui)` 调用；为此把 `DesktopApp` 需要暴露给 ui 子模块的字段/方法设为 `pub(crate)`）。

- [ ] **Step 1: app.rs 结构调整**

- `DesktopApp` 增加字段：`qr_cache: Option<(String, egui::TextureHandle)>`、`confirm_reset: bool`、`pending_repair: bool`。
- 以下成员改 `pub(crate)`，供 `crate::ui::*` 子模块访问：字段 `bridge`、`settings`、`settings_path`、`monitor`、`history`、`qr_cache`、`confirm_reset`、`pending_repair`；方法 `toast()`、`send_current_clipboard()`。
- `Page::Pairing` 分支改为 `Page::Pairing => crate::ui::pairing::show(self, ui),`。
- `on_core_event` 增补「换绑」联动：`CoreEvent::ResetDone(Ok(()))` 时若 `self.pending_repair` 为真，则置回 `false` 并 `self.bridge.send(BridgeCommand::PairBegin(self.settings.relay_url.clone()))`（解绑成功后立即发起新配对）。

- [ ] **Step 2: 实现 ui/pairing.rs**

```rust
//! 配对页：桌面 = offerer（显示 QR，手机扫码），SAS 双向核对。

use clipboard_core::ffi::PairingSnapshot;
use egui::{ColorImage, RichText};

use crate::app::DesktopApp;
use crate::core_bridge::BridgeCommand;
use crate::settings::validate_relay_url;
use crate::ui;

pub fn show(app: &mut DesktopApp, ui: &mut egui::Ui) {
    let snapshot = app.bridge.pair_poll();
    match snapshot {
        PairingSnapshot::Unpaired => show_unpaired(app, ui),
        PairingSnapshot::Offering { qr_json } => show_offering(app, ui, &qr_json),
        PairingSnapshot::SasReady { sas } => show_sas(app, ui, &sas),
        PairingSnapshot::Paired { room_id } => show_paired(app, ui, &room_id),
    }
}

fn show_unpaired(app: &mut DesktopApp, ui: &mut egui::Ui) {
    ui.heading("配对设备");
    ui.add_space(8.0);
    ui.label("在手机上安装 ClipSync，扫描这里出现的二维码即可完成配对。");
    ui.add_space(12.0);
    if app.settings.relay_url.is_empty() {
        ui.colored_label(egui::Color32::from_rgb(239, 68, 68), "请先在「设置」页填写 relay 服务器地址");
        return;
    }
    if ui.button("开始配对").clicked() {
        app.bridge.send(BridgeCommand::PairBegin(app.settings.relay_url.clone()));
    }
}

fn show_offering(app: &mut DesktopApp, ui: &mut egui::Ui, qr_json: &str) {
    ui.heading("用手机扫码配对");
    ui.add_space(8.0);
    if let Some(texture) = qr_texture(app, ui, qr_json) {
        let size = egui::vec2(280.0, 280.0);
        ui.image(egui::load::SizedTexture::new(texture.id(), size));
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("取消").clicked() {
            app.bridge.send(BridgeCommand::PairCancel);
        }
        // 配对码 TTL 300s（服务端），过期后一键换新
        if ui.button("刷新配对码").clicked() {
            app.bridge.send(BridgeCommand::PairCancel);
            app.bridge
                .send(BridgeCommand::PairBegin(app.settings.relay_url.clone()));
        }
    });
}

fn show_sas(app: &mut DesktopApp, ui: &mut egui::Ui, sas: &str) {
    ui.heading("核对安全码");
    ui.add_space(8.0);
    ui.label("请确认手机上显示的安全码与下方一致：");
    ui.add_space(12.0);
    ui.label(
        RichText::new(sas)
            .size(48.0)
            .family(egui::FontFamily::Monospace)
            .strong(),
    );
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if ui.button("一致，确认配对").clicked() {
            app.bridge.send(BridgeCommand::PairConfirm(sas.to_owned()));
        }
        if ui.button("取消").clicked() {
            app.bridge.send(BridgeCommand::PairCancel);
        }
    });
}

fn show_paired(app: &mut DesktopApp, ui: &mut egui::Ui, room_id: &str) {
    ui.heading("已配对");
    ui.add_space(8.0);
    ui::card(ui, |ui| {
        ui.label(format!("房间号：{room_id}"));
    });
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if ui.button("发送当前剪贴板").clicked() {
            app.send_current_clipboard();
        }
        let reset = if app.confirm_reset {
            "再次点击确认解绑"
        } else {
            "解除配对"
        };
        if ui.button(reset).clicked() {
            if app.confirm_reset {
                app.confirm_reset = false;
                app.bridge.send(BridgeCommand::ResetPairing);
            } else {
                app.confirm_reset = true;
            }
        }
        // 换绑 = 解绑 + 立即发起新配对（ResetDone 事件里联动 PairBegin）
        if ui.button("换绑（重新配对）").clicked() {
            app.pending_repair = true;
            app.bridge.send(BridgeCommand::ResetPairing);
        }
    });
}

/// QR 纹理缓存：同一 qr_json 只渲染一次。
fn qr_texture(
    app: &mut DesktopApp,
    ui: &mut egui::Ui,
    qr_json: &str,
) -> Option<egui::TextureHandle> {
    if let Some((cached, texture)) = &app.qr_cache
        && cached == qr_json
    {
        return Some(texture.clone());
    }
    let code = qrcode::QrCode::new(qr_json.as_bytes()).ok()?;
    let image = code
        .render::<qrcode::image::Luma<u8>>()
        .min_dimensions(320, 320)
        .build();
    let (w, h) = (image.width() as usize, image.height() as usize);
    let color = ColorImage::from_gray([w, h], &image.into_raw());
    let texture = ui.ctx().load_texture("pairing-qr", color, egui::TextureOptions::NEAREST);
    app.qr_cache = Some((qr_json.to_owned(), texture.clone()));
    Some(texture)
}
```

- [ ] **Step 3: 编译 + 手动验证**

```zsh
cargo run -p clipsync-desktop
```
再按 Task 11 的 loopback 思路或用本地 relay（`deploy/docker-compose.local.yml` 或 `cargo run -p clipboard-server`）+ Android 真机/模拟器验证：
- relay 为空时提示先填设置；填入本地 `ws://127.0.0.1:8787/ws`（debug 构建放行）→ 「开始配对」→ QR 显示；
- Android 扫码 → 双方 SAS 一致 → 确认 → 状态变「已连接」，配对页显示房间号；
- 「解除配对」二次确认生效，回到未配对页。

- [ ] **Step 4: Commit**

```bash
git add crates/desktop
git commit -m "feat(desktop): pairing page with QR and SAS confirmation"
```

---

### Task 9: 历史页（ui/history.rs）

**Files:**
- Create: `crates/desktop/src/ui/history.rs`
- Modify: `crates/desktop/src/app.rs`（缩略图缓存字段 + 历史事件处理 + 页面接入）

**Interfaces:**
- Consumes: `CoreBridge`（HistoryRefresh/HistoryImage/HistoryApply/HistoryClear）、`MonitorHandle::apply_remote`。
- Produces:
  ```rust
  pub fn show(app: &mut DesktopApp, ui: &mut egui::Ui);
  pub fn format_relative_time(ts_ms: i64, now_ms: i64) -> String;  // 纯函数，带测试
  pub fn source_label(source: FfiHistorySource) -> &'static str;   // 纯函数，带测试
  ```

- [ ] **Step 1: 写纯函数失败测试**

`ui/history.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_buckets() {
        let now = 1_000_000_000i64;
        assert_eq!(format_relative_time(now - 5_000, now), "刚刚");
        assert_eq!(format_relative_time(now - 90_000, now), "1 分钟前");
        assert_eq!(format_relative_time(now - 3_600_000, now), "1 小时前");
        assert_eq!(format_relative_time(now - 5 * 3_600_000, now), "5 小时前");
        assert_eq!(format_relative_time(now - 2 * 86_400_000, now), "2 天前");
    }

    #[test]
    fn source_labels() {
        assert_eq!(source_label(FfiHistorySource::Local), "本机");
        assert_eq!(source_label(FfiHistorySource::Remote), "对方");
        assert_eq!(source_label(FfiHistorySource::RemoteDeferred), "离线补投");
    }
}
```

- [ ] **Step 2: 运行确认失败**

```zsh
cargo test -p clipsync-desktop history
```
Expected: 编译失败（函数未定义）。

- [ ] **Step 3: 实现**

app.rs 增加（以下字段/枚举均 `pub(crate)`，供 `crate::ui::history` 访问）：

```rust
// DesktopApp 字段
thumbs: std::collections::HashMap<String, ThumbState>,
pending_thumbs: std::collections::HashMap<String, Vec<u8>>,
pending_apply: Option<String>,
confirm_clear: bool,
// 枚举
pub(crate) enum ThumbState { Loading, Ready(egui::TextureHandle), Failed }
```

`on_core_event` 增补：

```rust
CoreEvent::HistoryImage { id, result } => match result {
    Ok(bytes) => {
        // 若是「应用」触发的取图，先写剪贴板（ownership），再走缩略图缓存
        if self.pending_apply.as_deref() == Some(id.as_str()) {
            self.monitor
                .apply_remote(&ClipboardPayload::ImagePng(bytes.clone()), false);
            self.pending_apply = None;
        }
        self.pending_thumbs.insert(id, bytes); // 纹理在渲染期懒建
        self.bridge.send(BridgeCommand::HistoryRefresh); // Deferred→Remote 可能已变
    }
    Err(_) => { self.thumbs.insert(id, ThumbState::Failed); }
},
CoreEvent::Applied(Ok(())) => { self.toast("已应用到剪贴板".to_owned()); self.bridge.send(BridgeCommand::HistoryRefresh); }
CoreEvent::Cleared(Ok(())) => { self.history.clear(); self.thumbs.clear(); }
```
（`pending_thumbs`/`pending_apply` 字段已在上方字段清单中。）

`ui/history.rs`：

```rust
//! 历史页：50 条卡片（core 约束），文本摘要 / 图片缩略图懒加载。

use clipboard_core::ffi::{FfiHistoryItem, FfiHistoryKind, FfiHistorySource};
use egui::ColorImage;

use crate::app::{DesktopApp, ThumbState};
use crate::clipboard_monitor::ClipboardPayload;
use crate::core_bridge::BridgeCommand;
use crate::ui;

pub fn show(app: &mut DesktopApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.heading("历史");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if app.confirm_clear { "再次点击确认清空" } else { "清空" };
            if ui.button(label).clicked() {
                if app.confirm_clear {
                    app.confirm_clear = false;
                    app.bridge.send(BridgeCommand::HistoryClear);
                } else {
                    app.confirm_clear = true;
                }
            }
            if ui.button("刷新").clicked() {
                app.bridge.send(BridgeCommand::HistoryRefresh);
            }
        });
    });
    ui.add_space(8.0);
    if app.history.is_empty() {
        ui.label("暂无历史记录");
        return;
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        // core 返回"最旧在前"，展示时倒序（最新在上）
        let now = now_ms();
        for item in app.history.clone().iter().rev() {
            show_item(app, ui, item, now);
            ui.add_space(6.0);
        }
    });
}

fn show_item(app: &mut DesktopApp, ui: &mut egui::Ui, item: &FfiHistoryItem, now: i64) {
    ui::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                match &item.kind {
                    FfiHistoryKind::Text { content } => {
                        let summary: String = content.chars().take(200).collect();
                        ui.label(summary);
                    }
                    FfiHistoryKind::Image => {
                        show_thumbnail(app, ui, &item.id);
                    }
                }
                ui.horizontal(|ui| {
                    ui.weak(format!(
                        "{} · {}",
                        source_label(item.source),
                        format_relative_time(item.ts_ms, now)
                    ));
                    if item.source == FfiHistorySource::RemoteDeferred {
                        ui.weak("（未应用）");
                    }
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("应用").clicked() {
                    apply_item(app, item);
                }
            });
        });
    });
}

fn show_thumbnail(app: &mut DesktopApp, ui: &mut egui::Ui, id: &str) {
    match app.thumbs.get(id) {
        Some(ThumbState::Ready(texture)) => {
            let size = texture.size_vec2();
            let scale = (120.0 / size.x.max(size.y)).min(1.0);
            ui.image(egui::load::SizedTexture::new(texture.id(), size * scale));
        }
        Some(ThumbState::Loading) => { ui.spinner(); }
        Some(ThumbState::Failed) => { ui.label("[图片加载失败]"); }
        None => {
            app.thumbs.insert(id.to_owned(), ThumbState::Loading);
            app.bridge.send(BridgeCommand::HistoryImage(id.to_owned()));
            ui.label("[图片]");
        }
    }
    // pending bytes → 建纹理
    if let Some(bytes) = app.pending_thumbs.remove(id) {
        match image::load_from_memory(&bytes) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = (rgba.width() as usize, rgba.height() as usize);
                let color = ColorImage::from_rgba_unmultiplied([w, h], &rgba.into_raw());
                let tex = ui.ctx().load_texture(
                    format!("thumb-{id}"), color, egui::TextureOptions::LINEAR);
                app.thumbs.insert(id.to_owned(), ThumbState::Ready(tex));
            }
            Err(_) => { app.thumbs.insert(id.to_owned(), ThumbState::Failed); }
        }
    }
}

fn apply_item(app: &mut DesktopApp, item: &FfiHistoryItem) {
    // 先写剪贴板（记录 ownership），再通知 core promote（Deferred→Remote）
    match &item.kind {
        FfiHistoryKind::Text { content } => {
            app.monitor
                .apply_remote(&ClipboardPayload::Text(content.clone()), false);
            app.bridge.send(BridgeCommand::HistoryApply(item.id.clone()));
        }
        FfiHistoryKind::Image => {
            // 二段式：先取回图片字节，on_core_event 的 HistoryImage 分支里
            // 命中 pending_apply 时写剪贴板；同时通知 core promote
            app.pending_apply = Some(item.id.clone());
            app.bridge.send(BridgeCommand::HistoryImage(item.id.clone()));
            app.bridge.send(BridgeCommand::HistoryApply(item.id.clone()));
        }
    }
}

pub fn source_label(source: FfiHistorySource) -> &'static str {
    match source {
        FfiHistorySource::Local => "本机",
        FfiHistorySource::Remote => "对方",
        FfiHistorySource::RemoteDeferred => "离线补投",
    }
}

/// 相对时间：刚刚 / N 分钟前 / N 小时前 / N 天前。
pub fn format_relative_time(ts_ms: i64, now_ms: i64) -> String {
    let delta = (now_ms - ts_ms).max(0);
    const MIN: i64 = 60_000;
    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 86_400_000;
    if delta < MIN {
        "刚刚".to_owned()
    } else if delta < HOUR {
        format!("{} 分钟前", delta / MIN)
    } else if delta < DAY {
        format!("{} 小时前", delta / HOUR)
    } else {
        format!("{} 天前", delta / DAY)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
```

- [ ] **Step 4: 运行确认通过 + 手动验证**

```zsh
cargo test -p clipsync-desktop history
cargo run -p clipsync-desktop
```
Expected: 2 个纯函数测试绿；与手机互发文本/图片后历史页出现卡片（来源/相对时间正确），图片缩略图加载，「应用」写回剪贴板，「清空」二次确认生效。

- [ ] **Step 5: Commit**

```bash
git add crates/desktop
git commit -m "feat(desktop): history page with lazy thumbnails"
```

---

### Task 10: 设置页（ui/settings.rs）

**Files:**
- Create: `crates/desktop/src/ui/settings.rs`
- Modify: `crates/desktop/src/app.rs`（页面接入）

**Interfaces:**
- Consumes: `Settings/validate_relay_url`、`crate::autostart`、`PairingSnapshot`（已配对时禁止改 relay）。

- [ ] **Step 1: 实现 ui/settings.rs**

```rust
//! 设置页：relay 地址（未配对时可改）、开机自启、关于。

use clipboard_core::ffi::PairingSnapshot;

use crate::app::DesktopApp;
use crate::autostart;
use crate::settings::validate_relay_url;

pub fn show(app: &mut DesktopApp, ui: &mut egui::Ui) {
    ui.heading("设置");
    ui.add_space(12.0);

    ui.label("Relay 服务器地址");
    let paired = matches!(app.bridge.pair_poll(), PairingSnapshot::Paired { .. });
    let edit = egui::TextEdit::singleline(&mut app.settings.relay_url)
        .hint_text("wss://sync.example.com/ws")
        .desired_width(420.0);
    ui.add_enabled_ui(!paired, |ui| {
        ui.add(edit);
    });
    if paired {
        ui.weak("解除配对后可修改");
    } else if !app.settings.relay_url.is_empty() {
        match validate_relay_url(&app.settings.relay_url) {
            Ok(()) => {}
            Err(message) => { ui.colored_label(egui::Color32::from_rgb(239, 68, 68), message); }
        }
    }
    if ui.button("保存").clicked() {
        match app.settings.save(&app.settings_path) {
            Ok(()) => app.toast("已保存".to_owned()),
            Err(e) => app.toast(format!("保存失败：{e}")),
        }
    }

    ui.add_space(16.0);
    if ui.checkbox(&mut app.settings.autostart, "开机自动启动").changed() {
        if let Err(e) = autostart::set_enabled(app.settings.autostart) {
            app.toast(format!("自启动设置失败：{e}"));
            app.settings.autostart = autostart::is_enabled();
        } else {
            let _ = app.settings.save(&app.settings_path);
        }
    }

    ui.add_space(16.0);
    ui.weak(format!("ClipSync Desktop v{}", env!("CARGO_PKG_VERSION")));
}
```

app.rs：`settings` 与 `settings_path` 字段改 `pub(crate)`（Task 8 已要求）；`Page::Settings => crate::ui::settings::show(self, ui),`。

- [ ] **Step 2: 编译 + 手动验证**

```zsh
cargo run -p clipsync-desktop
```
Expected: 非法地址红字提示；保存后重启应用设置仍在；勾选自启动后（Windows）注册表 `HKCU\...\Run` 出现 ClipSync /（macOS）登录项生效。

- [ ] **Step 3: Commit**

```bash
git add crates/desktop
git commit -m "feat(desktop): settings page with relay URL and autostart"
```

---

### Task 11: loopback 集成测试 —— 双 bridge 全链路（文本互发 + ownership 防回环）

复用 core `tests/ffi_smoke.rs` 的 Relay 模式，在 desktop crate 层验证：配对 → A 发文本 → B 的假剪贴板被写入且 ownership 生效（B 轮询不再回发）→ B 发文本 → A 收到。

**Files:**
- Create: `crates/desktop/tests/bridge_loopback.rs`

**Interfaces:**
- Consumes: `CoreBridge/CoreEvent/BridgeCommand`、`MonitorHandle::with_io`（`#[doc(hidden)]`，Task 4 已提供，可注入 FakeClipboard）、`clipboard_server::{start, ServerConfig, Limits, InMemoryRegistry, PairingRelay, PairingConfig, NoopMailboxSink}`。

- [ ] **Step 1: 写集成测试**

```rust
//! 桌面壳全链路：两个 CoreBridge 经真实 relay 配对、文本互发、
//! 远端应用写入假剪贴板且 ownership 生效。

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clipboard_server::{
    InMemoryRegistry, Limits, PairingConfig, PairingRelay, ServerConfig, ServerHandle, start,
};
use clipsync_desktop::clipboard_monitor::{
    ClipboardIo, ClipboardPayload, MonitorHandle, MonitorState,
};
use clipsync_desktop::core_bridge::{BridgeCommand, CoreBridge, CoreEvent};

const WAIT: Duration = Duration::from_secs(15);

#[derive(Clone, Default)]
struct FakeClipboard {
    inner: Arc<Mutex<FakeInner>>,
}

#[derive(Default)]
struct FakeInner {
    content: Option<ClipboardPayload>,
    writes: Vec<ClipboardPayload>,
}

impl ClipboardIo for FakeClipboard {
    fn read(&mut self) -> Option<ClipboardPayload> {
        self.inner.lock().unwrap().content.clone()
    }
    fn write(&mut self, payload: &ClipboardPayload) -> bool {
        let mut g = self.inner.lock().unwrap();
        g.content = Some(payload.clone());
        g.writes.push(payload.clone());
        true
    }
}

impl FakeClipboard {
    fn writes(&self) -> Vec<ClipboardPayload> {
        self.inner.lock().unwrap().writes.clone()
    }
}

struct Relay {
    _runtime: tokio::runtime::Runtime,
    handle: ServerHandle,
}

impl Relay {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("relay runtime builds");
        let registry = Arc::new(InMemoryRegistry::new());
        let pairing = Arc::new(PairingRelay::new(
            registry.clone(),
            PairingConfig { attempts_per_window: 100, ..PairingConfig::default() },
        ));
        let mailbox = Arc::new(clipboard_server::NoopMailboxSink);
        let handle = runtime
            .block_on(start(
                "127.0.0.1:0".parse().unwrap(),
                ServerConfig {
                    limits: Limits { join_attempts_per_minute: 100, ..Limits::default() },
                    ..ServerConfig::default()
                },
                registry,
                pairing,
                mailbox,
            ))
            .expect("relay binds");
        Relay { _runtime: runtime, handle }
    }

    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}/ws", self.handle.addr().port())
    }
}

fn wait_event(rx: &Receiver<CoreEvent>, matches: impl Fn(&CoreEvent) -> bool) -> CoreEvent {
    let started = Instant::now();
    loop {
        let event = rx.recv_timeout(Duration::from_millis(100));
        if let Ok(event) = event
            && matches(&event)
        {
            return event;
        }
        assert!(started.elapsed() < WAIT, "timed out waiting for event");
    }
}

fn wait_until(cond: impl Fn() -> bool) {
    let started = Instant::now();
    while !cond() {
        assert!(started.elapsed() < WAIT, "timed out waiting for condition");
        std::thread::sleep(Duration::from_millis(25));
    }
}

struct Side {
    bridge: CoreBridge,
    rx: Receiver<CoreEvent>,
    clipboard: FakeClipboard,
    _dir: tempfile::TempDir,
}

impl Side {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let clipboard = FakeClipboard::default();
        let state = Arc::new(Mutex::new(MonitorState::default()));
        let monitor = MonitorHandle::with_io(state, clipboard.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let bridge = CoreBridge::spawn(dir.path(), monitor, tx).unwrap();
        Side { bridge, rx, clipboard, _dir: dir }
    }
}

#[test]
fn desktop_bridge_pairing_and_live_text_roundtrip() {
    let relay = Relay::start();
    let a = Side::new();
    let b = Side::new();

    // A offer（桌面产品形态是 offerer-only）；B 走 pair_claim_for_test 扮演手机端
    a.bridge.send(BridgeCommand::PairBegin(relay.url()));
    let qr = match wait_event(&a.rx, |e| matches!(e, CoreEvent::QrReady(Ok(_)))) {
        CoreEvent::QrReady(Ok(qr)) => qr,
        _ => unreachable!(),
    };

    // B claim + 双方确认
    let sas_b = b.bridge.pair_claim_for_test(&qr).expect("claim succeeds");
    wait_until(|| matches!(
        a.bridge.pair_poll(),
        clipboard_core::ffi::PairingSnapshot::SasReady { .. }
    ));
    let sas_a = match a.bridge.pair_poll() {
        clipboard_core::ffi::PairingSnapshot::SasReady { sas } => sas,
        _ => unreachable!(),
    };
    assert_eq!(sas_a, sas_b);
    a.bridge.send(BridgeCommand::PairConfirm(sas_a));
    wait_event(&a.rx, |e| matches!(e, CoreEvent::PairConfirmed(Ok(()))));
    b.bridge.send(BridgeCommand::PairConfirm(sas_b));
    wait_event(&b.rx, |e| matches!(e, CoreEvent::PairConfirmed(Ok(()))));

    // 等双方都 Connected
    wait_event(&a.rx, |e| matches!(e, CoreEvent::Status(clipboard_core::ffi::CoreStatus::Connected)));
    wait_event(&b.rx, |e| matches!(e, CoreEvent::Status(clipboard_core::ffi::CoreStatus::Connected)));

    // A 发文本 → B 的假剪贴板被写入
    a.bridge.send(BridgeCommand::SendText("hello desktop".to_owned()));
    wait_until(|| {
        b.clipboard.writes().iter().any(|p| matches!(
            p,
            ClipboardPayload::Text(t) if t == "hello desktop"
        ))
    });

    // ownership：B 端轮询读到该内容时判定为 OwnWrite（不回发）
    let payload = ClipboardPayload::Text("hello desktop".to_owned());
    let decision = {
        let state = b.bridge_monitor_state();
        let mut guard = state.lock().unwrap();
        clipsync_desktop::clipboard_monitor::decide_poll(&mut guard, Some(&payload))
    };
    assert_eq!(
        decision,
        clipsync_desktop::clipboard_monitor::PollDecision::OwnWrite
    );

    // B 反向发文本 → A 收到
    b.bridge.send(BridgeCommand::SendText("reply".to_owned()));
    wait_until(|| {
        a.clipboard.writes().iter().any(|p| matches!(
            p,
            ClipboardPayload::Text(t) if t == "reply"
        ))
    });

    a.bridge.shutdown();
    b.bridge.shutdown();
}
```

- [ ] **Step 2: 运行确认失败并补齐接口**

```zsh
cargo test -p clipsync-desktop --test bridge_loopback
```
Expected: 编译失败——`pair_claim_for_test` 与 `bridge_monitor_state` 不存在。在 `core_bridge.rs` 补：

```rust
impl CoreBridge {
    /// 桌面产品形态只做 offerer；claim 仅供 loopback 集成测试驱动对端。
    #[doc(hidden)]
    pub fn pair_claim_for_test(&self, qr_payload: &str) -> Result<String, CoreError> {
        self.handle.pair_claim(qr_payload.to_owned())
    }

    #[doc(hidden)]
    pub fn bridge_monitor_state(&self) -> Arc<Mutex<MonitorState>> {
        self.monitor_state.clone()
    }
}
```
（`CoreBridge` 增加字段 `monitor_state: Arc<Mutex<MonitorState>>`，`spawn` 时从 `monitor.state()` 取得并存入。）

- [ ] **Step 3: 运行确认通过**

```zsh
cargo test -p clipsync-desktop --test bridge_loopback
```
Expected: 1 个测试通过（双 bridge 配对、双向文本、ownership 防回环）。

- [ ] **Step 4: 全量回归**

```zsh
cargo test -p clipsync-desktop
cargo test -p clipboard-core --features full
```
Expected: 全绿。

- [ ] **Step 5: Commit**

```bash
git add crates/desktop
git commit -m "test(desktop): loopback integration test with dual bridges"
```

---

### Task 12: scripts/build-desktop.sh + 手工验证清单

**Files:**
- Create: `scripts/build-desktop.sh`
- Create: `docs/desktop-verification-checklist.md`

**Interfaces:**
- Consumes: 全部前置 Task。
- Produces: release 构建脚本（外部卷路由 + 双平台指引）；手工验证清单文档。

- [ ] **Step 1: 编写 scripts/build-desktop.sh**

```bash
#!/usr/bin/env zsh
# 构建 clipsync-desktop release 二进制。
# macOS：本机直接构建。Windows：在 Parallels VM 内安装 Rust MSVC 工具链后
# 执行同一命令（本脚本仅 macOS 侧）。产物与缓存按 AGENTS.md 走外部卷
# （CARGO_TARGET_DIR 由 scripts/external-dev-env.zsh 配置）。
set -euo pipefail

cd "$(dirname "$0")/.."
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]

cargo build --release -p clipsync-desktop
echo "==> built: ${CARGO_TARGET_DIR:-target}/release/clipsync-desktop"
```

`chmod +x scripts/build-desktop.sh`。

- [ ] **Step 2: 编写 docs/desktop-verification-checklist.md**

```markdown
# ClipSync Desktop 手工验证清单

前置：本地 relay 运行（`cargo run -p clipboard-server`，debug 下 ws:// 可用），
设置页填 `ws://127.0.0.1:8787/ws`；Android 端指向同一 relay。

## 配对
- [ ] relay 为空时配对页提示先填设置
- [ ] 开始配对 → QR 显示，手机扫码后双方 SAS 一致
- [ ] SAS 确认 → 状态变「已连接」，配对页显示房间号
- [ ] 取消配对（Offering 状态）→ 回到未配对页
- [ ] 解除配对（二次确认）→ 回到未配对页，可立即重新配对

## 同步
- [ ] 桌面复制文本 → 手机收到并写入剪贴板
- [ ] 手机发文本 → 桌面剪贴板被写入（ownership：桌面不再回发）
- [ ] 双向图片互发（≤10MiB）
- [ ] 手机断网发消息 → 恢复后桌面收到 mailbox 补投；
      断线期间桌面剪贴板未动 → 自动应用；被动过 → Deferred（历史中「离线补投（未应用）」）
- [ ] 杀掉 relay → 状态「重连中…」；重启 relay → 自动恢复「已连接」

## 历史
- [ ] 50 条上限，最新在上；来源标签（本机/对方/离线补投）与相对时间正确
- [ ] 图片缩略图懒加载；「应用」写回剪贴板
- [ ] 「清空」二次确认后列表清空

## 窗口与托盘
- [ ] 关窗不退进程；托盘菜单「打开窗口」恢复；「退出」结束进程
- [ ] macOS：窗口隐藏后 Dock 图标消失（accessory），重开恢复
- [ ] 中文渲染正常；跟随系统深浅色

## 设置
- [ ] 非法 relay 地址红字提示；保存后重启仍在
- [ ] 开机自启动开关生效（注册表 Run 键 / macOS 登录项）

## Windows（Parallels VM）
- [ ] VM 内 `cargo build --release -p clipsync-desktop`（MSVC 工具链）通过
- [ ] 重复上述全部清单（至少配对、双向文本、图片、托盘、自启动）
```

- [ ] **Step 3: 运行构建脚本验证**

```zsh
scripts/build-desktop.sh
```
Expected: release 构建通过，输出产物路径（外部卷）。

- [ ] **Step 4: Commit**

```bash
git add scripts/build-desktop.sh docs/desktop-verification-checklist.md
git commit -m "feat(desktop): release build script and manual verification checklist"
```

---

## 附：范围外（v1 不做，来自 spec YAGNI 清单）

- 窗口图标 PNG（assets 只有 SVG，不引入 resvg）
- Windows 安装器、macOS .app 打包、自动更新
- 局域网发现、多设备（>2）、本地非同步剪贴板历史、全局快捷键
- 删除现有 Swift macOS 应用
