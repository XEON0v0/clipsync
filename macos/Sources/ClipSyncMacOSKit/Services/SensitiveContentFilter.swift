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
