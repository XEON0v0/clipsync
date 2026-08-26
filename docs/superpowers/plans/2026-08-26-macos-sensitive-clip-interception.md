# macOS 敏感内容拦截 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 macOS 发送端拦截密码/密钥等敏感剪贴板内容，使其不进入同步管道（不发送、不落盘、不进历史），并通过系统通知告知用户。

**Architecture:** 全部改动在 `macos/` SwiftPM 包内。新增纯逻辑判定组件 `SensitiveContentFilter`（暂停态 → 密码管理器 pasteboard 标记 → 内置规则 → 自定义规则，短路求值）和通知组件 `SystemInterceptNotifier`；`PasteboardMonitor` 透传敏感标记，`AppModel` 在发送入口收口策略。零协议/Rust/FFI/Android 改动。

**Tech Stack:** Swift 6（严格并发，@MainActor 模式沿用现有代码）、SwiftUI（macOS 13+）、UserNotifications、XCTest。

**Spec:** `docs/superpowers/specs/2026-08-26-macos-sensitive-clip-interception-design.md`

## Global Constraints

- **外部存储**：所有 swift test/build 必须先 `source scripts/require-external-dev.sh && clipsync_require_external_dev "$PWD"`，然后传 `--scratch-path "$CLIPSYNC_SWIFTPM_SCRATCH" --cache-path "$CLIPSYNC_SWIFTPM_CACHE"`。就绪检查失败时停下报告，绝不使用仓库内 `.build/`。本计划所有 `swift test` 命令均为：
  ```zsh
  source scripts/require-external-dev.sh && clipsync_require_external_dev "$PWD" \
    && (cd macos && swift test --scratch-path "$CLIPSYNC_SWIFTPM_SCRATCH" --cache-path "$CLIPSYNC_SWIFTPM_CACHE" --filter <TestClass>)
  ```
- **只改 `macos/`**：不碰 Rust、FFI、协议、Android、desktop crate。
- **通知绝不携带剪贴板内容**：标题/正文只能由 `InterceptReason` 派生（该枚举不含 payload）。
- **UI 文案用英文**（与现有 App 文案一致：`"Pair Device"`、`"Connected"` 等）。
- **Swift 6 语言模式**：新类型遵循现有 `@MainActor` 风格；静态存储不放非-Sendable 对象（内置规则存正则字符串，匹配时编译）。
- **仓库有其他未提交改动**：提交时只 `git add` 本任务列出的文件，禁止 `git add -A`/`git add .`。
- **TDD**：每任务先写失败测试、跑出失败、最小实现、跑过、提交。
- 最终 `swift build -Xswiftc -warnings-as-errors` 必须零警告（README 构建门禁）。

**Spec 实现细化说明**（不改变语义）：spec 中 `syncPaused` 放在 AppModel；实现时持久化状态统一放 `SettingsStore`（单一数据源，`@Published` + `didSet` 持久化），`AppModel` 读取 `settings.syncPaused` 参与判定，UI 直接绑定 `settings`。通知"5 秒内合并为一条"实现为窗口内静默抑制（已展示的那条即为合并结果）。

---

### Task 1: SensitiveContentFilter 纯逻辑组件

**Files:**
- Create: `macos/Sources/ClipSyncMacOSKit/Services/SensitiveContentFilter.swift`
- Test: `macos/Tests/ClipSyncMacOSKitTests/FilterTests.swift`

**Interfaces:**
- Consumes: `ClipboardPayload`（已存在于 `macos/Sources/ClipSyncMacOSKit/Models/ClipboardPayload.swift`）
- Produces（后续任务依赖的精确签名）:
  ```swift
  public struct SensitiveRule: Codable, Equatable, Identifiable
      // 属性: id: UUID, pattern: String, isRegex: Bool
  public enum InterceptReason: Equatable
      // case paused / pasteboardMarker / builtinRule(String) / userRule(Int)
  public enum FilterDecision: Equatable   // case allow / block(InterceptReason)
  public struct SensitiveContentFilter    // static func evaluate(paused:markedSensitive:payload:rules:) -> FilterDecision
  public struct BuiltinSensitiveRule: Identifiable, Sendable
      // 属性: id: String, description: String（内部另有 substrings/regexPattern）
  public enum BuiltinSensitiveRules       // static let all: [BuiltinSensitiveRule]; static func match(_ text: String) -> String?
  ```

- [ ] **Step 1: 写失败测试**

创建 `macos/Tests/ClipSyncMacOSKitTests/FilterTests.swift`：

```swift
import XCTest
@testable import ClipSyncMacOSKit

final class FilterTests: XCTestCase {
    private func evaluate(
        paused: Bool = false,
        markedSensitive: Bool = false,
        payload: ClipboardPayload,
        rules: [SensitiveRule] = []
    ) -> FilterDecision {
        SensitiveContentFilter.evaluate(
            paused: paused,
            markedSensitive: markedSensitive,
            payload: payload,
            rules: rules
        )
    }

    func testPausedBlocksEverything() {
        XCTAssertEqual(
            evaluate(paused: true, payload: .text(" grocery list ")),
            .block(.paused)
        )
    }

    func testPasteboardMarkerBlocksTextAndImage() {
        XCTAssertEqual(
            evaluate(markedSensitive: true, payload: .text("pw")),
            .block(.pasteboardMarker)
        )
        XCTAssertEqual(
            evaluate(markedSensitive: true, payload: .image(png: Data([1]), semanticDigest: "d")),
            .block(.pasteboardMarker)
        )
    }

    func testOrdinaryTextIsAllowed() {
        XCTAssertEqual(evaluate(payload: .text("hello world")), .allow)
        XCTAssertEqual(
            evaluate(payload: .image(png: Data([1, 2]), semanticDigest: "d")),
            .allow
        )
    }

    func testBuiltinPemPrivateKeyBlockedAndPublicBlocksAllowed() {
        XCTAssertEqual(
            evaluate(payload: .text("-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----")),
            .block(.builtinRule("pem-private-key"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("-----BEGIN OPENSSH PRIVATE KEY-----\nabc")),
            .block(.builtinRule("pem-private-key"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("-----BEGIN PUBLIC KEY-----\nabc\n-----END PUBLIC KEY-----")),
            .allow
        )
        XCTAssertEqual(
            evaluate(payload: .text("-----BEGIN CERTIFICATE-----\nabc")),
            .allow
        )
    }

    func testBuiltinTokenPrefixesBlocked() {
        XCTAssertEqual(
            evaluate(payload: .text("otpauth://totp/Example:alice?secret=JBSW")),
            .block(.builtinRule("otpauth"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("token: ghp_16C7e42F292c6912E7710c838347Ae178B4a")),
            .block(.builtinRule("github-token"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG")),
            .block(.builtinRule("github-token"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("sk-proj4abcdefghij0123456789")),
            .block(.builtinRule("openai-key"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("sk-short")),
            .allow
        )
        XCTAssertEqual(
            evaluate(payload: .text("AKIAIOSFODNN7EXAMPLE")),
            .block(.builtinRule("aws-key"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("AKIAIOSFODNN7EXAMPL")),
            .allow
        )
        XCTAssertEqual(
            evaluate(payload: .text("AIzaSyA1bC2dE3fG4hI5jK6lM7nO8pQ9rS0tU1v")),
            .block(.builtinRule("google-api-key"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("xoxb-1234567890abcdefXYZ")),
            .block(.builtinRule("slack-token"))
        )
    }

    func testUserSubstringRuleBlocks() {
        XCTAssertEqual(
            evaluate(payload: .text("server password is hunter2!"), rules: [
                SensitiveRule(pattern: "hunter2", isRegex: false)
            ]),
            .block(.userRule(0))
        )
        XCTAssertEqual(
            evaluate(payload: .text("nothing here"), rules: [
                SensitiveRule(pattern: "hunter2", isRegex: false)
            ]),
            .allow
        )
    }

    func testUserRegexRuleBlocksAndInvalidRegexDoesNotCrash() {
        XCTAssertEqual(
            evaluate(payload: .text("password: trunk8-OK"), rules: [
                SensitiveRule(pattern: "trunk\\d-OK", isRegex: true)
            ]),
            .block(.userRule(0))
        )
        XCTAssertEqual(
            evaluate(payload: .text("password: trunk8-OK"), rules: [
                SensitiveRule(pattern: "trunk(\\d-OK", isRegex: true)
            ]),
            .allow
        )
    }

    func testEmptyUserPatternNeverMatches() {
        XCTAssertEqual(
            evaluate(payload: .text("anything"), rules: [SensitiveRule(pattern: "", isRegex: false)]),
            .allow
        )
    }

    func testUserRulesDoNotApplyToImages() {
        XCTAssertEqual(
            evaluate(payload: .image(png: Data([9]), semanticDigest: "d"), rules: [
                SensitiveRule(pattern: "anything", isRegex: false)
            ]),
            .allow
        )
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

按 Global Constraints 的命令模板运行 `--filter FilterTests`。
Expected: FAIL，编译错误 `cannot find 'SensitiveContentFilter' in scope`。

- [ ] **Step 3: 最小实现**

创建 `macos/Sources/ClipSyncMacOSKit/Services/SensitiveContentFilter.swift`：

```swift
import Foundation

/// 用户自定义排除规则：子串或正则，仅存本机。
public struct SensitiveRule: Codable, Equatable, Identifiable {
    public var id: UUID
    public var pattern: String
    public var isRegex: Bool

    public init(id: UUID = UUID(), pattern: String, isRegex: Bool) {
        self.id = id
        self.pattern = pattern
        self.isRegex = isRegex
    }
}

public enum InterceptReason: Equatable {
    case paused
    case pasteboardMarker
    case builtinRule(String)
    case userRule(Int)
}

public enum FilterDecision: Equatable {
    case allow
    case block(InterceptReason)
}

/// 内置敏感格式规则。存正则字符串而非编译对象，避免 Swift 6 静态
/// 非-Sendable 存储；每次匹配至多编译 7 个正则，成本可忽略。
public struct BuiltinSensitiveRule: Identifiable, Sendable {
    public let id: String
    public let description: String
    let substrings: [String]
    let regexPattern: String?
}

public enum BuiltinSensitiveRules {
    public static let all: [BuiltinSensitiveRule] = [
        BuiltinSensitiveRule(
            id: "pem-private-key",
            description: "PEM private key block",
            substrings: [],
            regexPattern: "-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----"
        ),
        BuiltinSensitiveRule(
            id: "otpauth",
            description: "2FA seed URI (otpauth://)",
            substrings: ["otpauth://"],
            regexPattern: nil
        ),
        BuiltinSensitiveRule(
            id: "github-token",
            description: "GitHub token (ghp_/github_pat_)",
            substrings: ["ghp_", "github_pat_"],
            regexPattern: nil
        ),
        BuiltinSensitiveRule(
            id: "openai-key",
            description: "OpenAI-style key (sk-…)",
            substrings: [],
            regexPattern: "sk-[A-Za-z0-9_-]{20,}"
        ),
        BuiltinSensitiveRule(
            id: "aws-key",
            description: "AWS access key (AKIA…)",
            substrings: [],
            regexPattern: "AKIA[0-9A-Z]{16}"
        ),
        BuiltinSensitiveRule(
            id: "google-api-key",
            description: "Google API key (AIza…)",
            substrings: [],
            regexPattern: "AIza[0-9A-Za-z_-]{35}"
        ),
        BuiltinSensitiveRule(
            id: "slack-token",
            description: "Slack token (xox…)",
            substrings: [],
            regexPattern: "xox[baprs]-[A-Za-z0-9-]{10,}"
        ),
    ]

    /// 返回首个命中的内置规则 id；未命中返回 nil。
    static func match(_ text: String) -> String? {
        for rule in all {
            if rule.substrings.contains(where: { text.range(of: $0) != nil }) {
                return rule.id
            }
            if let pattern = rule.regexPattern,
               let regex = try? NSRegularExpression(pattern: pattern),
               regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)) != nil {
                return rule.id
            }
        }
        return nil
    }
}

/// 发送入口的敏感内容判定。纯逻辑、无依赖、不抛错；
/// 判定异常（如非法正则）按不匹配处理，放行优先保同步可用性。
public struct SensitiveContentFilter {
    public static func evaluate(
        paused: Bool,
        markedSensitive: Bool,
        payload: ClipboardPayload,
        rules: [SensitiveRule]
    ) -> FilterDecision {
        if paused {
            return .block(.paused)
        }
        if markedSensitive {
            return .block(.pasteboardMarker)
        }
        guard case let .text(text) = payload else {
            return .allow
        }
        if let ruleID = BuiltinSensitiveRules.match(text) {
            return .block(.builtinRule(ruleID))
        }
        for (index, rule) in rules.enumerated() where Self.ruleMatches(rule, text: text) {
            return .block(.userRule(index))
        }
        return .allow
    }

    static func ruleMatches(_ rule: SensitiveRule, text: String) -> Bool {
        guard !rule.pattern.isEmpty else {
            return false
        }
        if !rule.isRegex {
            return text.range(of: rule.pattern) != nil
        }
        guard let regex = try? NSRegularExpression(pattern: rule.pattern) else {
            return false
        }
        return regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)) != nil
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

运行 `--filter FilterTests`。Expected: PASS（全部用例）。

- [ ] **Step 5: 提交**

```zsh
git add macos/Sources/ClipSyncMacOSKit/Services/SensitiveContentFilter.swift macos/Tests/ClipSyncMacOSKitTests/FilterTests.swift
git commit -m "feat(macos): add sensitive content filter with builtin and user rules"
```

---

### Task 2: SettingsStore 持久化 syncPaused 与 sensitiveRules

**Files:**
- Modify: `macos/Sources/ClipSyncMacOSKit/Stores/SettingsStore.swift`
- Test: `macos/Tests/ClipSyncMacOSKitTests/SettingsStoreTests.swift`

**Interfaces:**
- Consumes: Task 1 的 `SensitiveRule`
- Produces:
  ```swift
  // SettingsStore 上新增（均 @Published）：
  var syncPaused: Bool                     // UserDefaults "syncPaused"，默认 false
  var sensitiveRules: [SensitiveRule]      // UserDefaults "sensitiveRules" JSON，默认 []
  ```

- [ ] **Step 1: 写失败测试**

在 `SettingsStoreTests.swift` 的类内（`isolatedDefaults()` 之前）追加：

```swift
    func testSyncPausedDefaultsFalseAndPersists() {
        let defaults = isolatedDefaults()
        let store = SettingsStore(defaults: defaults)
        XCTAssertFalse(store.syncPaused)

        store.syncPaused = true

        XCTAssertTrue(SettingsStore(defaults: defaults).syncPaused)
    }

    func testSensitiveRulesDefaultEmptyAndRoundTrip() {
        let defaults = isolatedDefaults()
        let store = SettingsStore(defaults: defaults)
        XCTAssertEqual(store.sensitiveRules, [])

        store.sensitiveRules = [
            SensitiveRule(pattern: "hunter2", isRegex: false),
            SensitiveRule(pattern: "corp-\\w+-token", isRegex: true)
        ]

        let reloaded = SettingsStore(defaults: defaults).sensitiveRules
        XCTAssertEqual(reloaded.map(\.pattern), ["hunter2", "corp-\\w+-token"])
        XCTAssertEqual(reloaded.map(\.isRegex), [false, true])
    }

    func testCorruptSensitiveRulesDataFallsBackToEmpty() {
        let defaults = isolatedDefaults()
        defaults.set(Data("not json".utf8), forKey: "sensitiveRules")

        XCTAssertEqual(SettingsStore(defaults: defaults).sensitiveRules, [])
    }
```

- [ ] **Step 2: 跑测试确认失败**

运行 `--filter SettingsStoreTests`。Expected: FAIL，编译错误 `value of type 'SettingsStore' has no member 'syncPaused'`。

- [ ] **Step 3: 最小实现**

在 `SettingsStore.swift` 中：

1. `Keys` 枚举追加两行：
```swift
        static let syncPaused = "syncPaused"
        static let sensitiveRules = "sensitiveRules"
```

2. `serverURL` 属性声明之后追加两个属性：
```swift
    @Published public var syncPaused: Bool {
        didSet {
            defaults.set(syncPaused, forKey: Keys.syncPaused)
        }
    }

    @Published public var sensitiveRules: [SensitiveRule] {
        didSet {
            if let data = try? JSONEncoder().encode(sensitiveRules) {
                defaults.set(data, forKey: Keys.sensitiveRules)
            }
        }
    }
```

3. `init` 中 `serverURL = ...` 之后追加：
```swift
        syncPaused = defaults.bool(forKey: Keys.syncPaused)
        if let data = defaults.data(forKey: Keys.sensitiveRules),
           let decoded = try? JSONDecoder().decode([SensitiveRule].self, from: data) {
            sensitiveRules = decoded
        } else {
            sensitiveRules = []
        }
```

- [ ] **Step 4: 跑测试确认通过**

运行 `--filter SettingsStoreTests`（含原有用例）。Expected: PASS。

- [ ] **Step 5: 提交**

```zsh
git add macos/Sources/ClipSyncMacOSKit/Stores/SettingsStore.swift macos/Tests/ClipSyncMacOSKitTests/SettingsStoreTests.swift
git commit -m "feat(macos): persist sync pause state and sensitive rules"
```

---

### Task 3: ClipboardAccess 返回 ReadResult，PasteboardMonitor 透传标记

**Files:**
- Modify: `macos/Sources/ClipSyncMacOSKit/Services/ClipboardAccess.swift`
- Modify: `macos/Sources/ClipSyncMacOSKit/Services/PasteboardMonitor.swift`
- Test: `macos/Tests/ClipSyncMacOSKitTests/PasteboardMonitorTests.swift`（改两个 Fake + 新增用例）
- Test: `macos/Tests/ClipSyncMacOSKitTests/AppModelTests.swift`（仅改 `ModelFakeClipboard`，行为断言不变）

**Interfaces:**
- Consumes: 无（独立于 Task 1/2）
- Produces:
  ```swift
  public struct ReadResult: Equatable, Sendable   // payload: ClipboardPayload, markedSensitive: Bool
  // ClipboardAccess 协议方法签名变更：
  func readPayload() throws -> ReadResult?
  // PasteboardMonitor 回调签名变更：
  public var onLocalChange: ((ClipboardPayload, _ markedSensitive: Bool) -> Void)?
  ```

- [ ] **Step 1: 改测试（先让编译失败）**

1. `PasteboardMonitorTests.swift`：

   - `FakeClipboard`（文件末尾的 private 类）替换为：
```swift
@MainActor
private final class FakeClipboard: ClipboardAccess {
    private(set) var changeCount = 0
    private(set) var payload: ClipboardPayload?
    var markedSensitive = false
    var overwriteBeforeConditionalWrite: ClipboardPayload?

    func readPayload() throws -> ReadResult? {
        payload.map { ReadResult(payload: $0, markedSensitive: markedSensitive) }
    }

    func write(_ payload: ClipboardPayload, ifUnchanged expected: Int?) throws -> Int? {
        if let racing = overwriteBeforeConditionalWrite {
            overwriteBeforeConditionalWrite = nil
            simulateUserCopy(racing)
        }
        if let expected, expected != changeCount {
            return nil
        }
        self.payload = payload
        changeCount += 1
        return changeCount
    }

    func simulateUserCopy(_ payload: ClipboardPayload) {
        self.payload = payload
        changeCount += 1
    }
}
```

   - 现有四处 `monitor.onLocalChange = { sent.append($0) }` 全部改为：
```swift
        monitor.onLocalChange = { payload, _ in sent.append(payload) }
```

   - 类内追加两个新用例：
```swift
    func testMarkedSensitiveFlagIsPassedThroughToLocalChange() throws {
        let clipboard = FakeClipboard()
        let monitor = PasteboardMonitor(clipboard: clipboard)
        var received: [(payload: ClipboardPayload, markedSensitive: Bool)] = []
        monitor.onLocalChange = { payload, marked in received.append((payload, marked)) }

        clipboard.markedSensitive = true
        clipboard.simulateUserCopy(.text("pw"))
        try monitor.poll()

        XCTAssertEqual(received.count, 1)
        XCTAssertEqual(received.first?.payload, .text("pw"))
        XCTAssertEqual(received.first?.markedSensitive, true)
    }

    func testSystemClipboardReportsConcealedTypeMarker() throws {
        let pasteboard = NSPasteboard.withUniqueName()
        defer { pasteboard.releaseGlobally() }
        let clipboard = SystemClipboard(pasteboard: pasteboard)

        pasteboard.clearContents()
        XCTAssertTrue(pasteboard.setString("secret", forType: .string))
        XCTAssertTrue(pasteboard.setData(Data([0]), forType: NSPasteboard.PasteboardType("org.nspasteboard.ConcealedType")))
        let concealed = try clipboard.readPayload()
        XCTAssertEqual(concealed?.payload, .text("secret"))
        XCTAssertTrue(concealed?.markedSensitive ?? false)

        pasteboard.clearContents()
        XCTAssertTrue(pasteboard.setString("plain", forType: .string))
        let plain = try clipboard.readPayload()
        XCTAssertEqual(plain?.payload, .text("plain"))
        XCTAssertFalse(plain?.markedSensitive ?? true)
    }
```

2. `AppModelTests.swift` 的 `ModelFakeClipboard` 替换 readPayload 与 simulateUserCopy：
```swift
@MainActor
private final class ModelFakeClipboard: ClipboardAccess {
    private(set) var changeCount = 0
    private(set) var payload: ClipboardPayload?
    var markedSensitive = false

    func readPayload() throws -> ReadResult? {
        payload.map { ReadResult(payload: $0, markedSensitive: markedSensitive) }
    }

    func write(_ payload: ClipboardPayload, ifUnchanged expected: Int?) throws -> Int? {
        if let expected, expected != changeCount {
            return nil
        }
        self.payload = payload
        changeCount += 1
        return changeCount
    }

    func simulateUserCopy(_ payload: ClipboardPayload) {
        self.payload = payload
        changeCount += 1
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

运行 `--filter PasteboardMonitorTests`。Expected: FAIL，编译错误（`ReadResult` 未定义、协议不符合）。

- [ ] **Step 3: 实现**

1. `ClipboardAccess.swift`：协议前追加类型；协议方法改签名；`SystemClipboard.readPayload` 重写：

```swift
/// 单轮剪贴板读取结果：内容 + 与内容同轮取得的敏感标记。
public struct ReadResult: Equatable, Sendable {
    public let payload: ClipboardPayload
    public let markedSensitive: Bool

    public init(payload: ClipboardPayload, markedSensitive: Bool) {
        self.payload = payload
        self.markedSensitive = markedSensitive
    }
}
```

协议内 `func readPayload() throws -> ClipboardPayload?` 改为 `func readPayload() throws -> ReadResult?`。

`SystemClipboard` 追加标记常量并重写 `readPayload`（读取顺序与原实现一致，marker 与内容同轮取得）：

```swift
    private static let sensitiveMarkerTypes: Set<String> = [
        "org.nspasteboard.ConcealedType",
        "org.nspasteboard.TransientType",
    ]

    public func readPayload() throws -> ReadResult? {
        let markedSensitive = (pasteboard.types ?? []).contains {
            Self.sensitiveMarkerTypes.contains($0.rawValue)
        }
        if let png = pasteboard.data(forType: .png) {
            return ReadResult(
                payload: try ClipboardImageCodec.payload(fromEncodedImage: png, enforceEncodedLimit: true),
                markedSensitive: markedSensitive
            )
        }
        if let tiff = pasteboard.data(forType: .tiff) {
            return ReadResult(
                payload: try ClipboardImageCodec.payload(fromEncodedImage: tiff, enforceEncodedLimit: false),
                markedSensitive: markedSensitive
            )
        }
        if let text = pasteboard.string(forType: .string) {
            return ReadResult(payload: .text(text), markedSensitive: markedSensitive)
        }
        return nil
    }
```

2. `PasteboardMonitor.swift`：

   - 回调声明改为：
```swift
    public var onLocalChange: ((ClipboardPayload, _ markedSensitive: Bool) -> Void)?
```
   - `poll()` 中 `guard let payload = try clipboard.readPayload()` 起的整段改为：
```swift
        guard let result = try clipboard.readPayload() else {
            ownershipToken = nil
            return
        }
        let payload = result.payload
        if ownershipToken == OwnershipToken(changeCount: changeCount, semanticDigest: payload.semanticDigest) {
            ownershipToken = nil
            return
        }
        ownershipToken = nil
        onLocalChange?(payload, result.markedSensitive)
```

- [ ] **Step 4: 跑测试确认通过**

运行 `--filter PasteboardMonitorTests` 和 `--filter AppModelTests`（后者验证既有行为未回归）。Expected: 全部 PASS。

- [ ] **Step 5: 提交**

```zsh
git add macos/Sources/ClipSyncMacOSKit/Services/ClipboardAccess.swift macos/Sources/ClipSyncMacOSKit/Services/PasteboardMonitor.swift macos/Tests/ClipSyncMacOSKitTests/PasteboardMonitorTests.swift macos/Tests/ClipSyncMacOSKitTests/AppModelTests.swift
git commit -m "feat(macos): surface pasteboard sensitive markers through monitor"
```

---

### Task 4: SystemInterceptNotifier 拦截通知

**Files:**
- Create: `macos/Sources/ClipSyncMacOSKit/Services/InterceptNotifier.swift`
- Test: `macos/Tests/ClipSyncMacOSKitTests/InterceptNotifierTests.swift`

**Interfaces:**
- Consumes: Task 1 的 `InterceptReason`、`BuiltinSensitiveRules`
- Produces:
  ```swift
  public protocol NotificationCentering: AnyObject   // 非 MainActor：UNUserNotificationCenter 才能一致性合成
  @MainActor public protocol InterceptNotifying: AnyObject { func notify(reason: InterceptReason) }
  @MainActor public final class SystemInterceptNotifier: InterceptNotifying
      // init(center: NotificationCentering = UNUserNotificationCenter.current(),
      //      now: @escaping () -> Date = Date.init, dedupWindow: TimeInterval = 5)
      // static let notificationIdentifier = "clipsync.sensitive-intercept"
  enum InterceptCopy   // static let title: String; static func body(for: InterceptReason) -> String
  ```

- [ ] **Step 1: 写失败测试**

创建 `macos/Tests/ClipSyncMacOSKitTests/InterceptNotifierTests.swift`：

```swift
import UserNotifications
import XCTest
@testable import ClipSyncMacOSKit

@MainActor
final class InterceptNotifierTests: XCTestCase {
    func testNotifyPostsNotificationWithFixedIdentifier() {
        let center = FakeNotificationCenter(granted: true)
        let notifier = SystemInterceptNotifier(center: center, now: { Date(timeIntervalSince1970: 100) })

        notifier.notify(reason: .pasteboardMarker)

        XCTAssertEqual(center.addedRequests.map(\.identifier), [SystemInterceptNotifier.notificationIdentifier])
        let content = center.addedRequests[0].content
        XCTAssertEqual(content.title, InterceptCopy.title)
        XCTAssertTrue(content.body.contains("not synced or stored"))
    }

    func testAuthorizationDeniedPostsNothing() {
        let center = FakeNotificationCenter(granted: false)
        let notifier = SystemInterceptNotifier(center: center, now: { Date(timeIntervalSince1970: 100) })

        notifier.notify(reason: .builtinRule("otpauth"))

        XCTAssertTrue(center.addedRequests.isEmpty)
    }

    func testRepeatsWithinDedupWindowAreSuppressed() {
        let center = FakeNotificationCenter(granted: true)
        var current = Date(timeIntervalSince1970: 100)
        let notifier = SystemInterceptNotifier(center: center, now: { current })

        notifier.notify(reason: .pasteboardMarker)
        current = current.addingTimeInterval(2)
        notifier.notify(reason: .builtinRule("otpauth"))
        XCTAssertEqual(center.addedRequests.count, 1)

        current = current.addingTimeInterval(4)
        notifier.notify(reason: .builtinRule("otpauth"))
        XCTAssertEqual(center.addedRequests.count, 2)
    }

    func testBodyNeverContainsClipboardContent() {
        // body 只能由 reason 派生；reason 枚举不携带 payload，这里对全部 case 断言。
        for reason in [InterceptReason.paused, .pasteboardMarker, .builtinRule("otpauth"), .userRule(0)] {
            let body = InterceptCopy.body(for: reason)
            XCTAssertFalse(body.contains("secret"))
            XCTAssertFalse(body.contains("hunter2"))
        }
    }

    func testBuiltinRuleBodyUsesDisplayName() {
        XCTAssertTrue(InterceptCopy.body(for: .builtinRule("otpauth")).contains("otpauth"))
    }
}

@MainActor
private final class FakeNotificationCenter: NotificationCentering {
    let granted: Bool
    private(set) var addedRequests: [UNNotificationRequest] = []

    init(granted: Bool) {
        self.granted = granted
    }

    func requestAuthorization(
        _ options: UNAuthorizationOptions,
        completionHandler: @escaping (Bool, (any Error)?) -> Void
    ) {
        completionHandler(granted, nil)
    }

    func add(
        _ request: UNNotificationRequest,
        withCompletionHandler completionHandler: @escaping ((any Error)?) -> Void
    ) {
        addedRequests.append(request)
        completionHandler(nil)
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

运行 `--filter InterceptNotifierTests`。Expected: FAIL，编译错误 `cannot find 'SystemInterceptNotifier'`。

- [ ] **Step 3: 实现**

创建 `macos/Sources/ClipSyncMacOSKit/Services/InterceptNotifier.swift`：

```swift
import Foundation
import UserNotifications

/// UNUserNotificationCenter 的可注入抽象，仅供测试替身实现。
/// 刻意不标 @MainActor：SDK 中的 UNUserNotificationCenter 非 MainActor 隔离。
public protocol NotificationCentering: AnyObject {
    func requestAuthorization(
        _ options: UNAuthorizationOptions,
        completionHandler: @escaping (Bool, (any Error)?) -> Void
    )
    func add(
        _ request: UNNotificationRequest,
        withCompletionHandler completionHandler: @escaping ((any Error)?) -> Void
    )
}

extension UNUserNotificationCenter: NotificationCentering {}

@MainActor
public protocol InterceptNotifying: AnyObject {
    func notify(reason: InterceptReason)
}

/// 拦截通知文案。标题/正文只由 reason 派生，绝不携带剪贴板内容。
enum InterceptCopy {
    static let title = "Sensitive content blocked"

    static func body(for reason: InterceptReason) -> String {
        let source: String
        switch reason {
        case .paused:
            source = "Sync is paused"
        case .pasteboardMarker:
            source = "Password manager marked the clipboard as sensitive"
        case let .builtinRule(id):
            let name = BuiltinSensitiveRules.all.first { $0.id == id }?.description ?? id
            source = "Matched built-in rule: \(name)"
        case let .userRule(index):
            source = "Matched custom rule #\(index + 1)"
        }
        return "\(source). Content was not synced or stored."
    }
}

/// 首次拦截时才请求通知授权；授权拒绝或发送失败一律静默降级，
/// 绝不影响同步主链路。去重窗口内重复拦截静默抑制（合并为已展示的一条）。
@MainActor
public final class SystemInterceptNotifier: InterceptNotifying {
    public static let notificationIdentifier = "clipsync.sensitive-intercept"

    private let center: any NotificationCentering
    private let now: () -> Date
    private let dedupWindow: TimeInterval
    private var authorizationRequested = false
    private var lastNotifiedAt: Date?

    public init(
        center: any NotificationCentering = UNUserNotificationCenter.current(),
        now: @escaping () -> Date = Date.init,
        dedupWindow: TimeInterval = 5
    ) {
        self.center = center
        self.now = now
        self.dedupWindow = dedupWindow
    }

    public func notify(reason: InterceptReason) {
        let timestamp = now()
        if let last = lastNotifiedAt, timestamp.timeIntervalSince(last) < dedupWindow {
            return
        }
        lastNotifiedAt = timestamp

        let content = UNMutableNotificationContent()
        content.title = InterceptCopy.title
        content.body = InterceptCopy.body(for: reason)
        let request = UNNotificationRequest(
            identifier: Self.notificationIdentifier,
            content: content,
            trigger: nil
        )

        if authorizationRequested {
            center.add(request) { _ in }
            return
        }
        authorizationRequested = true
        center.requestAuthorization(options: [.alert]) { [center] granted, _ in
            guard granted else { return }
            center.add(request) { _ in }
        }
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

运行 `--filter InterceptNotifierTests`。Expected: PASS。

若 Step 3 的授权回调在 Swift 6 隔离检查下报闭包捕获错误（`center`/`request` 非 Sendable），
把回调体改为派发回 MainActor 再投递：

```swift
        center.requestAuthorization(options: [.alert]) { [weak self] granted, _ in
            guard granted else { return }
            Task { @MainActor [weak self] in
                self?.postAuthorized(request)
            }
        }

    // 类内拆一个私有方法：
    private func postAuthorized(_ request: UNNotificationRequest) {
        center.add(request) { _ in }
    }
```

`FakeNotificationCenter` 是 @MainActor 类且回调同步执行，此改法不影响测试语义。

- [ ] **Step 5: 提交**

```zsh
git add macos/Sources/ClipSyncMacOSKit/Services/InterceptNotifier.swift macos/Tests/ClipSyncMacOSKitTests/InterceptNotifierTests.swift
git commit -m "feat(macos): add intercept notification with dedup and silent fallback"
```

---

### Task 5: AppModel 发送入口收口拦截策略

**Files:**
- Modify: `macos/Sources/ClipSyncMacOSKit/Stores/AppModel.swift`
- Test: `macos/Tests/ClipSyncMacOSKitTests/AppModelTests.swift`

**Interfaces:**
- Consumes: Task 1 `SensitiveContentFilter.evaluate(paused:markedSensitive:payload:rules:)`；Task 2 `settings.syncPaused/sensitiveRules`；Task 3 `onLocalChange` 新签名；Task 4 `InterceptNotifying`
- Produces:
  ```swift
  // AppModel 新增：
  @Published public private(set) var lastInterceptAt: Date?
  // 两个 convenience init 新增参数（带默认值，现有调用点不动）：
  //     interceptNotifier: any InterceptNotifying = SystemInterceptNotifier()
  ```

- [ ] **Step 1: 写失败测试**

在 `AppModelTests.swift`：

1. `FakeCoreService.send` 改为记录 payload：
```swift
    private(set) var sentPayloads: [ClipboardPayload] = []

    func send(_ payload: ClipboardPayload, completion: @escaping @MainActor @Sendable (Result<UInt64, CoreExecutionFailure>) -> Void) {
        sentPayloads.append(payload)
        MainActor.assumeIsolated { completion(.success(1)) }
    }
```

2. 文件末尾（`TestFailure` 枚举之后）追加：
```swift
@MainActor
private final class SpyInterceptNotifier: InterceptNotifying {
    private(set) var reasons: [InterceptReason] = []

    func notify(reason: InterceptReason) {
        reasons.append(reason)
    }
}
```

3. `makeModel` 增加参数并传给 `AppModel`：
```swift
    private func makeModel(
        core: FakeCoreService = FakeCoreService(),
        clipboard: ModelFakeClipboard = ModelFakeClipboard(),
        loginItem: FakeLoginItemController = FakeLoginItemController(status: .disabled),
        interceptNotifier: SpyInterceptNotifier = SpyInterceptNotifier()
    ) -> AppModel {
        // …前略，原样保留…
        return AppModel(
            settings: settings,
            clipboard: clipboard,
            core: core,
            loginItemController: loginItem,
            interceptNotifier: interceptNotifier,
            startMonitoring: false
        )
    }
```

4. 类内追加用例：
```swift
    func testOrdinaryLocalCopyIsStillSent() throws {
        let core = FakeCoreService()
        let clipboard = ModelFakeClipboard()
        let spy = SpyInterceptNotifier()
        let model = makeModel(core: core, clipboard: clipboard, interceptNotifier: spy)

        clipboard.simulateUserCopy(.text("meeting notes"))
        try model.monitor.poll()

        XCTAssertEqual(core.sentPayloads, [.text("meeting notes")])
        XCTAssertTrue(spy.reasons.isEmpty)
        XCTAssertNil(model.lastInterceptAt)
    }

    func testMarkedSensitiveCopyIsBlockedAndNotified() throws {
        let core = FakeCoreService()
        let clipboard = ModelFakeClipboard()
        let spy = SpyInterceptNotifier()
        let model = makeModel(core: core, clipboard: clipboard, interceptNotifier: spy)

        clipboard.markedSensitive = true
        clipboard.simulateUserCopy(.text("site password"))
        try model.monitor.poll()

        XCTAssertTrue(core.sentPayloads.isEmpty)
        XCTAssertEqual(spy.reasons, [.pasteboardMarker])
        XCTAssertNotNil(model.lastInterceptAt)
    }

    func testBuiltinRuleCopyIsBlockedWithoutPasteboardMarker() throws {
        let core = FakeCoreService()
        let clipboard = ModelFakeClipboard()
        let spy = SpyInterceptNotifier()
        let model = makeModel(core: core, clipboard: clipboard, interceptNotifier: spy)

        clipboard.simulateUserCopy(.text("otpauth://totp/Example?secret=JBSW"))
        try model.monitor.poll()

        XCTAssertTrue(core.sentPayloads.isEmpty)
        XCTAssertEqual(spy.reasons, [.builtinRule("otpauth")])
    }

    func testUserRuleFromSettingsBlocks() throws {
        let core = FakeCoreService()
        let clipboard = ModelFakeClipboard()
        let spy = SpyInterceptNotifier()
        let model = makeModel(core: core, clipboard: clipboard, interceptNotifier: spy)
        model.settings.sensitiveRules = [SensitiveRule(pattern: "hunter2", isRegex: false)]

        clipboard.simulateUserCopy(.text("password is hunter2"))
        try model.monitor.poll()

        XCTAssertTrue(core.sentPayloads.isEmpty)
        XCTAssertEqual(spy.reasons, [.userRule(0)])
    }

    func testPausedSyncDropsSilentlyWithoutNotification() throws {
        let core = FakeCoreService()
        let clipboard = ModelFakeClipboard()
        let spy = SpyInterceptNotifier()
        let model = makeModel(core: core, clipboard: clipboard, interceptNotifier: spy)
        model.settings.syncPaused = true

        clipboard.simulateUserCopy(.text("secret while paused"))
        try model.monitor.poll()

        XCTAssertTrue(core.sentPayloads.isEmpty)
        XCTAssertTrue(spy.reasons.isEmpty)
        XCTAssertNil(model.lastInterceptAt)
    }
```

- [ ] **Step 2: 跑测试确认失败**

运行 `--filter AppModelTests`。Expected: FAIL，编译错误（`interceptNotifier` 参数不存在、`markedSensitive`/`sentPayloads` 未定义——后者由本任务 Step 1 补齐后为参数错误）。

- [ ] **Step 3: 实现**

`AppModel.swift`：

1. 属性区（`historyError` 之后）追加：
```swift
    @Published public private(set) var lastInterceptAt: Date?
```

2. `private let loginItemController` 之后追加：
```swift
    private let interceptNotifier: any InterceptNotifying
```

3. 第一个 convenience init（`CoreService` 那个）参数列表加 `interceptNotifier: any InterceptNotifying = SystemInterceptNotifier(),`（放在 `dataDirectory` 之前），并传入私有 init。第二个 convenience init（测试用的）同样加该参数（无默认值也行，保持带默认值一致）并传入。

4. 私有 init 参数列表加 `interceptNotifier: any InterceptNotifying`，存储属性赋值区加：
```swift
        self.interceptNotifier = interceptNotifier
```

5. `monitor.onLocalChange` 闭包改为：
```swift
        monitor.onLocalChange = { [weak self] payload, markedSensitive in
            self?.handleLocalChange(payload, markedSensitive: markedSensitive)
        }
```

6. `send(_:)` 方法之前追加：
```swift
    /// 发送入口的拦截收口：blocked 内容不调 core.send、不落任何盘；
    /// paused 是用户显式操作，静默丢弃，其余原因发通知。
    private func handleLocalChange(_ payload: ClipboardPayload, markedSensitive: Bool) {
        switch SensitiveContentFilter.evaluate(
            paused: settings.syncPaused,
            markedSensitive: markedSensitive,
            payload: payload,
            rules: settings.sensitiveRules
        ) {
        case .allow:
            send(payload)
        case .block(.paused):
            break
        case .block(let reason):
            lastInterceptAt = Date()
            interceptNotifier.notify(reason: reason)
        }
    }
```

- [ ] **Step 4: 跑测试确认通过**

运行 `--filter AppModelTests`。Expected: 全部 PASS（含原有 12 个用例无回归）。

- [ ] **Step 5: 提交**

```zsh
git add macos/Sources/ClipSyncMacOSKit/Stores/AppModel.swift macos/Tests/ClipSyncMacOSKitTests/AppModelTests.swift
git commit -m "feat(macos): intercept sensitive clips at send entry"
```

---

### Task 6: UI — 暂停开关、拦截状态行、排除规则页

**Files:**
- Modify: `macos/Sources/ClipSyncMacOSKit/Views/MenuBarContent.swift`
- Modify: `macos/Sources/ClipSyncMacOSKit/Views/SettingsView.swift`
- Create: `macos/Sources/ClipSyncMacOSKit/Views/ExclusionRulesView.swift`

**Interfaces:**
- Consumes: Task 2 `settings.syncPaused/sensitiveRules`、Task 5 `model.lastInterceptAt`、Task 1 `BuiltinSensitiveRules.all`/`SensitiveRule`
- Produces: 无（叶子 UI 层）

本仓库不写 View 单测；以编译 + Task 7 手动清单验证。

- [ ] **Step 1: 实现 ExclusionRulesView**

创建 `macos/Sources/ClipSyncMacOSKit/Views/ExclusionRulesView.swift`：

```swift
import SwiftUI

public struct ExclusionRulesView: View {
    @ObservedObject private var settings: SettingsStore

    public init(settings: SettingsStore) {
        self.settings = settings
    }

    public var body: some View {
        Section("Exclusions") {
            Text("Built-in rules always apply.")
                .font(.footnote)
                .foregroundStyle(.secondary)
            ForEach(BuiltinSensitiveRules.all) { rule in
                Label(rule.description, systemImage: "checkmark.shield")
            }
            ForEach(settings.sensitiveRules) { rule in
                ruleRow(rule)
            }
            .onDelete { offsets in
                settings.sensitiveRules.remove(atOffsets: offsets)
            }
            Button("Add Rule...") {
                settings.sensitiveRules.append(SensitiveRule(pattern: "", isRegex: false))
            }
        }
    }

    private func ruleRow(_ rule: SensitiveRule) -> some View {
        HStack {
            TextField("Keyword or regex", text: binding(for: rule))
                .textFieldStyle(.roundedBorder)
            Toggle("Regex", isOn: regexBinding(for: rule))
                .toggleStyle(.checkbox)
        }
    }

    private func binding(for rule: SensitiveRule) -> Binding<String> {
        Binding(
            get: { rule.pattern },
            set: { newValue in
                if let index = settings.sensitiveRules.firstIndex(where: { $0.id == rule.id }) {
                    settings.sensitiveRules[index].pattern = newValue
                }
            }
        )
    }

    private func regexBinding(for rule: SensitiveRule) -> Binding<Bool> {
        Binding(
            get: { rule.isRegex },
            set: { newValue in
                if let index = settings.sensitiveRules.firstIndex(where: { $0.id == rule.id }) {
                    settings.sensitiveRules[index].isRegex = newValue
                }
            }
        )
    }
}
```

- [ ] **Step 2: 挂进 SettingsView**

`SettingsView.swift` 的 Form 中，`Section("Startup")` 结束之后、`if let error` 之前插入：

```swift
            ExclusionRulesView(settings: settings)
```

- [ ] **Step 3: MenuBarContent 加暂停开关与拦截状态行**

`MenuBarContent.swift`：

1. `@ObservedObject private var model: AppModel` 之后追加：
```swift
    @ObservedObject private var settings: SettingsStore
```
2. `init` 内 `self.model = model` 之后追加 `self.settings = model.settings`。
3. `Button("Settings...", ...)` 与 `Button("Quit", ...)` 之间插入：
```swift
        Divider()
        Toggle("Pause Sync", isOn: $settings.syncPaused)
        if let intercepted = model.lastInterceptAt {
            Text("Sensitive content blocked at \(intercepted.formatted(date: .omitted, time: .shortened))")
                .foregroundStyle(.secondary)
        }
```

- [ ] **Step 4: 编译并跑全量测试**

```zsh
source scripts/require-external-dev.sh && clipsync_require_external_dev "$PWD" \
  && (cd macos && swift test --scratch-path "$CLIPSYNC_SWIFTPM_SCRATCH" --cache-path "$CLIPSYNC_SWIFTPM_CACHE")
```
Expected: BUILD SUCCEEDED，全部测试 PASS（View 代码参与编译即验证类型正确）。

- [ ] **Step 5: 提交**

```zsh
git add macos/Sources/ClipSyncMacOSKit/Views/ExclusionRulesView.swift macos/Sources/ClipSyncMacOSKit/Views/SettingsView.swift macos/Sources/ClipSyncMacOSKit/Views/MenuBarContent.swift
git commit -m "feat(macos): add pause toggle, intercept status line, and rules UI"
```

---

### Task 7: 全量构建门禁 + 手动验证清单

**Files:**
- Modify: `docs/desktop-verification-checklist.md`（文末追加一节）

**Interfaces:**
- Consumes: 前六个任务的全部产物
- Produces: 验收通过的构建与文档

- [ ] **Step 1: warnings-as-errors 构建**

```zsh
source scripts/require-external-dev.sh && clipsync_require_external_dev "$PWD" \
  && (cd macos && swift build --scratch-path "$CLIPSYNC_SWIFTPM_SCRATCH" --cache-path "$CLIPSYNC_SWIFTPM_CACHE" -Xswiftc -warnings-as-errors)
```
Expected: BUILD SUCCEEDED 零警告。有警告必须修复后重跑，不得绕过。

- [ ] **Step 2: 追加手动验证清单**

在 `docs/desktop-verification-checklist.md` 文件末尾追加（保持现有小节风格）：

```markdown
## Sensitive content interception (2026-08)

- Copy a password from 1Password / Keychain (marks `org.nspasteboard.ConcealedType`):
  - no sync to Android, no history entry, macOS notification "Sensitive content blocked".
- `printf -- '-----BEGIN RSA PRIVATE KEY-----' | pbcopy`: blocked by built-in rule.
- `printf 'otpauth://totp/x?secret=ABC' | pbcopy`: blocked by built-in rule.
- `printf 'normal text' | pbcopy`: syncs to Android as before.
- `printf 'hunter2' | pbcopy` with custom rule `hunter2` added in Settings → Exclusions: blocked.
- Menu bar → "Pause Sync" checked: any copy stays local silently (no notification); Android receiving still works.
- Two sensitive copies within 5 seconds: only one notification.
- Deny notification permission on first intercept: no crash; menu bar still shows the "Sensitive content blocked at ..." line.
```

- [ ] **Step 3: 提交**

```zsh
git add docs/desktop-verification-checklist.md
git commit -m "docs: add sensitive interception verification checklist"
```

- [ ] **Step 4: 汇总**

向用户报告：已完成任务列表、全量测试结果、构建结果，并提示按清单做真机手动验证（1Password/钥匙串实测）。
