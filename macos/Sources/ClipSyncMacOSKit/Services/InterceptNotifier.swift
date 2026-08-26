import Foundation
import UserNotifications

/// UNUserNotificationCenter 的可注入抽象，仅供测试替身实现。
/// 标 @MainActor：Swift 6.3 一致性隔离模型下，非隔离协议无法由
/// @MainActor 测试替身满足同步要求（#ConformanceIsolation）；而 SDK 的
/// UNUserNotificationCenter 在本 SDK 上本就无法自动合成一致性
/// （见下方显式桥接），故无需为合成而保持非 MainActor。
@MainActor
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

/// 协议闭包为非 Sendable，而本 SDK 中 UNUserNotificationCenter 的两个回调参数
/// 均为 @Sendable（add 的还是 Optional），Swift 无法自动合成一致性；
/// 以下显式转发桥接。非 Sendable 闭包值不能直接转成 @Sendable，
/// 用 @unchecked Sendable 盒子过桥：回调至多被 SDK 调用一次，无并发复用，
/// 不存在盒子内值的数据竞争。
private final class SendableCompletionBox<T>: @unchecked Sendable {
    let value: T
    init(value: T) { self.value = value }
}

extension UNUserNotificationCenter: NotificationCentering {
    @MainActor
    public func requestAuthorization(
        _ options: UNAuthorizationOptions,
        completionHandler: @escaping (Bool, (any Error)?) -> Void
    ) {
        let box = SendableCompletionBox(value: completionHandler)
        self.requestAuthorization(options: options) { granted, error in
            box.value(granted, error)
        }
    }

    @MainActor
    public func add(
        _ request: UNNotificationRequest,
        withCompletionHandler completionHandler: @escaping ((any Error)?) -> Void
    ) {
        let box = SendableCompletionBox(value: completionHandler)
        // 参数显式声明为 SDK 的 Optional @Sendable 类型，使重载解析选中
        // SDK 方法而非本转发方法，避免递归。
        let completion: (@Sendable ((any Error)?) -> Void)? = { error in
            box.value(error)
        }
        self.add(request, withCompletionHandler: completion)
    }
}

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
        center.requestAuthorization([.alert]) { [center] granted, _ in
            guard granted else { return }
            center.add(request) { _ in }
        }
    }
}
