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
