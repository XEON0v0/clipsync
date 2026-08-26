# macOS 发送端敏感内容拦截（密码/密钥不同步）— 设计

日期：2026-08-26
状态：已与用户对齐，待实施

## 背景与目标

安全评审结论：传输链路（XChaCha20-Poly1305 E2E 加密、中继结构性不可解密、wss:// 强制）已经扎实，
但没有敏感内容过滤。用户复制密码/密钥后，内容会被无条件同步到安卓端，并以明文进入两端
`history.jsonl`（50 条上限、无时间上限）、安卓系统剪贴板。

用户在三个候选方案（源头拦截 / 阅后即焚 / 存储加固）中选定第一阶段做**源头拦截**：
敏感内容根本不进入同步管道。全部改动集中在 macOS 发送端，零协议改动，Android 不动。

已确认的产品决策：

- 拦截时发系统通知，通知**绝不含剪贴板内容本身**。
- 排除规则 = 内置常见密钥格式 + 用户自定义关键字/正则，仅存本机，不同步到对端。
- 新增"暂停同步"开关，只停发送，接收不受影响。
- 范围外（后续阶段另行立项）：阅后即焚模式（协议加 sensitive 标志）、历史 TTL/落盘加密/
  Keychain 迁移、desktop(egui/arboard) crate 支持。

## 现状链路（实施前提）

同步是 Mac → Android 单向自动发送：`AppModel` 把 `monitor.onLocalChange` 直连
`core.send`（`macos/Sources/ClipSyncMacOSKit/Stores/AppModel.swift:85-87`）。Android 端没有
剪贴板监听（`ClipboardSyncService.kt` 无 `PrimaryClipChangedListener`），只有手动
`sendText/sendImage` FFI 入口，因此拦截只需覆盖 macOS 发送路径。

macOS 读取侧 `ClipboardAccess.readPayload`（`macos/Sources/ClipSyncMacOSKit/Services/ClipboardAccess.swift:23-34`）
只读 `.png`/`.tiff`/`.string` 三种类型，不检查 `org.nspasteboard.ConcealedType` /
`org.nspasteboard.TransientType`（nspasteboard.org 约定：前者表示"不应出现在剪贴板历史"，
1Password/Bitwarden/钥匙串等复制密码时使用；后者表示一次性内容，密码管理器同样大量使用）。

`PasteboardMonitor.poll`（`PasteboardMonitor.swift:44-58`）以 changeCount + ownership token
判定本地变更，是事实上报层，当前直接透传 payload。

测试基础：`macos/Tests/ClipSyncMacOSKitTests/` 已有 AppModel/PasteboardMonitor/SettingsStore
测试，沿用其注入风格。

## 组件设计

### SensitiveContentFilter（新增，纯逻辑，无依赖）

```swift
enum InterceptReason: Equatable {
    case paused
    case pasteboardMarker   // ConcealedType 或 TransientType
    case builtinRule(String)   // 内置规则标识，如 "pem-private-key"
    case userRule(Int)         // 自定义规则下标
}
enum FilterDecision: Equatable {
    case allow
    case block(InterceptReason)
}
```

`evaluate(paused:markedSensitive:payload:rules:) -> FilterDecision` 按序短路：
`paused` → `markedSensitive`（对文本和图片都拦截）→ 内置规则（仅文本）→ 自定义规则（仅文本）。

内置规则保守清单（要求高置信、低误伤；均按"整段子串或行首匹配"实现）：

| 标识 | 模式 | 说明 |
| --- | --- | --- |
| `pem-private-key` | `-----BEGIN X PRIVATE KEY-----`（X 任意词） | `PUBLIC KEY`/`CERTIFICATE` 不拦 |
| `otpauth` | `otpauth://` | 2FA 种子 |
| `github-token` | `ghp_`、`github_pat_` | |
| `openai-key` | `sk-` 后跟 ≥20 个字符 | 裸 `sk-` 前缀不拦，避免误伤普通词 |
| `aws-key` | `AKIA[0-9A-Z]{16}` | |
| `google-api-key` | `AIza[0-9A-Za-z_-]{35}` | |
| `slack-token` | `xox[baprs]-` 后跟 ≥10 字符 | |

显式不纳入：JWT（`eyJ…` 误伤率高）。

用户规则模型：`struct SensitiveRule: Codable, Equatable { var pattern: String; var isRegex: Bool }`
（`isRegex == false` 为子串匹配）。非法正则按"不匹配"处理并 `Log.w`，保存时另做校验提示。

### ClipboardAccess 扩展（标记与内容同轮读取）

新增返回结构，避免标记读取与 changeCount 竞态（必须与内容同一次 poll 内取得）：

```swift
struct ReadResult {
    let payload: ClipboardPayload
    let markedSensitive: Bool   // pasteboard.types 含任一 marker UTI
}
```

`ClipboardAccess.readPayload() throws -> ClipboardPayload?` 改为返回 `ReadResult?`；
`SystemClipboard` 实现一次读取 `pasteboard.types` 判定。测试用 Fake 同步调整。

### PasteboardMonitor 透传

`onLocalChange: ((ClipboardPayload, _ markedSensitive: Bool) -> Void)?`。监控层只报事实，
不引用 filter（策略归 AppModel）。

### AppModel 接线（策略收口点）

- `send(_:markedSensitive:)` 入口先过 `SensitiveContentFilter.evaluate`：
  - `.block(.paused)`：静默丢弃（用户显式操作，不打扰）；
  - `.block(其余)`：不调 `core.send`、不进任何历史或磁盘，仅经 `InterceptNotifier` 发通知；
  - `.allow`：走现有发送路径不变。
- `@Published var syncPaused: Bool`，didSet 持久化到 SettingsStore；暂停时 `PasteboardMonitor`
  照常轮询（恢复暂停后能立即正常工作），仅在发送入口丢弃。

### InterceptNotifier（新增，协议化便于测试）

```swift
protocol InterceptNotifying {
    func notify(reason: InterceptReason)
}
```

- `UNUserNotificationCenter` 实现（LSUIElement 应用可用）。首次拦截时才请求
  `.alert` 授权（不在启动时打扰）；拒绝或发送失败 → 静默降级，绝不影响主链路。
- 通知文案固定：标题"ClipSync 已拦截敏感内容"，正文"检测到{原因描述}，未同步、未存储"。
  **任何情况下不携带剪贴板内容。**
- 去重：5 秒内重复拦截更新同一条通知（identifier 固定为拦截类通知），不刷屏。

### UI

- `MenuBarContent` 加 checkable 菜单项"暂停同步"，绑定 `syncPaused`。
- 设置窗口加"排除规则"页：内置规则只读展示（说明不可关闭的原因），自定义规则列表增删。
- 拦截通知被拒绝授权时，菜单栏状态区显示上次拦截时间作为降级反馈（不存内容，仅时间戳）。

### SettingsStore 扩展

沿用 UserDefaults Keys 模式，新增：`syncPaused: Bool`（默认 false）、`sensitiveRules`（
`[SensitiveRule]` 的 JSON 编码，默认空数组）。

## 错误处理

- filter 纯逻辑不抛错；通知失败静默降级；规则正则非法按不匹配处理。
- 拦截判定异常时按放行处理并 `Log.w`：同步可用性优先，误拦（同步失效、难排查）的
  代价高于漏拦单条（即现状既有行为）。

## 测试

`swift test`（按仓库惯例走外部存储 scratch path）：

- FilterTests：每个拦截来源的正反例（PUBLIC KEY 块放行、裸 `sk-` 放行、普通文本放行、
  marker 拦截、图片 + marker 拦截、paused 拦截、用户子串/正则规则、非法正则不匹配不崩溃）。
- PasteboardMonitorTests：FakeClipboardAccess 返回的 markedSensitive 透传到回调。
- AppModelTests：拦截时不调 core.send（spy core）；allow 路径回归不变；paused 丢弃且无通知。
- InterceptNotifierTests：去重窗口内合并；权限拒绝不抛。
- SettingsStoreTests：两个新键 round-trip、默认值。

手动验证清单（并入 `docs/desktop-verification-checklist.md` 风格）：1Password/钥匙串复制
密码 → 不同步 + 收到通知；`pbcopy` 普通文本/私钥文本/`otpauth://` → 对应放行或拦截；
暂停开关生效且恢复后立即可用。

## 实施涉及文件

| 文件 | 改动 |
| --- | --- |
| `macos/Sources/ClipSyncMacOSKit/Services/SensitiveContentFilter.swift` | 新增 |
| `macos/Sources/ClipSyncMacOSKit/Services/InterceptNotifier.swift` | 新增 |
| `macos/Sources/ClipSyncMacOSKit/Services/ClipboardAccess.swift` | readPayload 返回 ReadResult |
| `macos/Sources/ClipSyncMacOSKit/Services/PasteboardMonitor.swift` | 回调透传标记 |
| `macos/Sources/ClipSyncMacOSKit/Stores/AppModel.swift` | 发送入口过滤 + syncPaused |
| `macos/Sources/ClipSyncMacOSKit/Stores/SettingsStore.swift` | 新键 |
| `macos/Sources/ClipSyncMacOSKit/Views/MenuBarContent.swift` | 暂停菜单项 |
| `macos/Sources/ClipSyncMacOSKit/Views/ExclusionRulesView.swift` | 新增：排除规则管理页 |
| `macos/Tests/ClipSyncMacOSKitTests/` | 上述各测试 |
