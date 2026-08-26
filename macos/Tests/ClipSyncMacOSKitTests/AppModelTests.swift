import ClipboardCoreBindings
import Foundation
import XCTest
@testable import ClipSyncMacOSKit

@MainActor
final class AppModelTests: XCTestCase {
    func testPairingRequiresDisplayedSASBeforeConfirmation() {
        let core = FakeCoreService()
        let model = makeModel(core: core)
        core.beginResult = .success(#"{"server":"wss://sync.example/ws","code":"ABC234"}"#)

        model.beginPairing()

        XCTAssertEqual(model.connectionState, .waitingForPeer)
        XCTAssertEqual(model.pairingCode, "ABC234")
        XCTAssertNotNil(model.pairingQRJSON)

        core.snapshot = .sasReady(sas: "482901")
        model.receiveStatus(.sasReady)

        XCTAssertEqual(model.connectionState, .sasReady)
        XCTAssertEqual(model.pairingSAS, "482901")
        XCTAssertTrue(core.confirmedSAS.isEmpty)

        model.confirmPairing()

        XCTAssertEqual(core.confirmedSAS, ["482901"])
    }

    func testCancelPairingNeverConfirmsCandidate() {
        let core = FakeCoreService()
        let model = makeModel(core: core)
        core.beginResult = .success(#"{"server":"wss://sync.example/ws","code":"ABC234"}"#)
        model.beginPairing()

        model.cancelPairing()

        XCTAssertEqual(core.cancelCount, 1)
        XCTAssertTrue(core.confirmedSAS.isEmpty)
        XCTAssertEqual(model.connectionState, .unpaired)
        XCTAssertNil(model.pairingQRJSON)
    }

    func testUnpairResetsDurablePairingAndReturnsToUnpaired() {
        let core = FakeCoreService()
        core.initializeResult = .success(true)
        let model = makeModel(core: core)
        model.receiveStatus(.connected)

        model.unpair()

        XCTAssertEqual(core.resetPairingCount, 1)
        XCTAssertFalse(model.isPaired)
        XCTAssertEqual(model.connectionState, .unpaired)
        XCTAssertEqual(model.statusText, "Not paired")
        XCTAssertNil(model.pairingQRJSON)
        XCTAssertNil(model.pairingSAS)
    }

    func testReplacePairedDeviceResetsThenStartsFreshPairingOffer() {
        let core = FakeCoreService()
        core.initializeResult = .success(true)
        core.beginResult = .success(#"{"server":"wss://sync.example/ws","code":"NEW234"}"#)
        let model = makeModel(core: core)
        model.receiveStatus(.connected)

        model.replacePairedDevice()

        XCTAssertEqual(core.resetPairingCount, 1)
        XCTAssertEqual(core.begunServerURLs, ["wss://sync.example/ws"])
        XCTAssertFalse(model.isPaired)
        XCTAssertEqual(model.connectionState, .waitingForPeer)
        XCTAssertEqual(model.pairingCode, "NEW234")
        XCTAssertNotNil(model.pairingQRJSON)
    }

    func testPairedDeviceRemainsManageableWhenInitialRelayConnectionFails() {
        let core = FakeCoreService()
        core.initializeResult = .failure(.init(message: "relay unavailable"))
        let model = makeModel(core: core)

        model.receiveStatus(.connecting)
        model.receiveStatus(.error(message: "relay unavailable"))

        XCTAssertTrue(model.isPaired)
        XCTAssertEqual(model.connectionState, .failed)
        XCTAssertEqual(model.lastError, "relay unavailable")
    }

    func testReplacePairedDeviceDoesNotStartPairingWhenResetFails() {
        let core = FakeCoreService()
        core.initializeResult = .success(true)
        core.resetPairingResult = .failure(.init(message: "reset failed"))
        core.beginResult = .success(#"{"server":"wss://sync.example/ws","code":"NEW234"}"#)
        let model = makeModel(core: core)
        model.receiveStatus(.connected)

        model.replacePairedDevice()

        XCTAssertEqual(core.resetPairingCount, 1)
        XCTAssertTrue(core.begunServerURLs.isEmpty)
        XCTAssertTrue(model.isPaired)
        XCTAssertEqual(model.connectionState, .failed)
        XCTAssertEqual(model.lastError, "reset failed")
    }

    func testDelayedReadyUnpairedDoesNotDismissFreshReplacementOffer() {
        let core = FakeCoreService()
        core.initializeResult = .success(true)
        core.beginResult = .success(#"{"server":"wss://sync.example/ws","code":"NEW234"}"#)
        let model = makeModel(core: core)
        model.receiveStatus(.connected)
        model.replacePairedDevice()

        model.receiveStatus(.readyUnpaired)

        XCTAssertEqual(model.connectionState, .waitingForPeer)
        XCTAssertEqual(model.pairingCode, "NEW234")
        XCTAssertNotNil(model.pairingQRJSON)
    }

    func testHistorySelectionWritesClipboardAndPromotesOnlyDeferredItem() {
        let core = FakeCoreService()
        let clipboard = ModelFakeClipboard()
        let model = makeModel(core: core, clipboard: clipboard)
        let deferred = ClipboardHistoryEntry(
            id: "deferred-id",
            tsMs: 1,
            content: .text("offline text"),
            source: .remoteDeferred
        )
        let local = ClipboardHistoryEntry(
            id: "local-id",
            tsMs: 2,
            content: .text("local text"),
            source: .local
        )
        core.historyResult = .success([deferred, local])
        model.refreshHistory()

        XCTAssertEqual(model.history, [local, deferred])

        model.applyHistory(deferred)
        XCTAssertEqual(clipboard.payload, .text("offline text"))
        XCTAssertEqual(core.appliedHistoryIDs, ["deferred-id"])

        model.applyHistory(local)
        XCTAssertEqual(clipboard.payload, .text("local text"))
        XCTAssertEqual(core.appliedHistoryIDs, ["deferred-id"])
    }

    func testClearHistoryUsesCoreAndLeavesStableEmptyState() {
        let core = FakeCoreService()
        let model = makeModel(core: core)
        core.historyResult = .success([
            ClipboardHistoryEntry(id: "one", tsMs: 1, content: .text("one"), source: .local)
        ])
        model.refreshHistory()
        XCTAssertEqual(model.history.count, 1)

        model.clearHistory()

        XCTAssertEqual(core.clearHistoryCount, 1)
        XCTAssertTrue(model.history.isEmpty)
    }

    func testHistoryReadFailureKeepsLastDurableSnapshotVisible() {
        let core = FakeCoreService()
        let model = makeModel(core: core)
        let saved = ClipboardHistoryEntry(
            id: "saved",
            tsMs: 1,
            content: .text("saved"),
            source: .local
        )
        core.historyResult = .success([saved])
        model.refreshHistory()

        core.historyResult = .failure(.init(message: "relay restarting"))
        model.refreshHistory()

        XCTAssertEqual(model.history, [saved])
        XCTAssertEqual(model.historyError, "relay restarting")
    }

    func testLoginItemApprovalStateIsVisibleAndOpensSystemSettings() {
        let loginItem = FakeLoginItemController(status: .requiresApproval)
        let model = makeModel(loginItem: loginItem)

        XCTAssertEqual(model.loginItemStatus, .requiresApproval)
        XCTAssertEqual(model.loginItemStatusText, "Requires approval in System Settings")

        model.openLoginItemSettings()

        XCTAssertEqual(loginItem.openSettingsCount, 1)
    }

    func testRejectedLoginItemRegistrationKeepsSystemSettingsGuidanceAvailable() {
        let loginItem = FakeLoginItemController(status: .disabled)
        loginItem.setError = TestFailure.denied
        let model = makeModel(loginItem: loginItem)

        model.setLoginItemEnabled(true)

        XCTAssertTrue(model.showsLoginItemSettingsLink)
        XCTAssertEqual(model.loginItemStatusText, "Approval required in System Settings")
        model.openLoginItemSettings()
        XCTAssertEqual(loginItem.openSettingsCount, 1)

        loginItem.status = .enabled
        model.refreshLoginItemStatus()
        XCTAssertFalse(model.showsLoginItemSettingsLink)
        XCTAssertEqual(model.loginItemStatusText, "On")
    }

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

    private func makeModel(
        core: FakeCoreService = FakeCoreService(),
        clipboard: ModelFakeClipboard = ModelFakeClipboard(),
        loginItem: FakeLoginItemController = FakeLoginItemController(status: .disabled),
        interceptNotifier: SpyInterceptNotifier = SpyInterceptNotifier()
    ) -> AppModel {
        let suite = "AppModelTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        let settings = SettingsStore(defaults: defaults)
        settings.serverURL = "wss://sync.example/ws"
        return AppModel(
            settings: settings,
            clipboard: clipboard,
            core: core,
            loginItemController: loginItem,
            interceptNotifier: interceptNotifier,
            startMonitoring: false
        )
    }

}

private final class FakeCoreService: CoreServicing, @unchecked Sendable {
    var initializeResult: Result<Bool, CoreExecutionFailure> = .success(false)
    var beginResult: Result<String, CoreExecutionFailure> = .failure(.init(message: "not configured"))
    var resetPairingResult: Result<Void, CoreExecutionFailure> = .success(())
    var snapshot: HostPairingSnapshot = .unpaired
    var historyResult: Result<[ClipboardHistoryEntry], CoreExecutionFailure> = .success([])
    private(set) var confirmedSAS: [String] = []
    private(set) var begunServerURLs: [String] = []
    private(set) var cancelCount = 0
    private(set) var appliedHistoryIDs: [String] = []
    private(set) var clearHistoryCount = 0
    private(set) var resetPairingCount = 0
    private(set) var sentPayloads: [ClipboardPayload] = []

    func initialize(completion: @escaping @MainActor @Sendable (Result<Bool, CoreExecutionFailure>) -> Void) {
        MainActor.assumeIsolated { completion(initializeResult) }
    }

    func pairBegin(serverURL: String, completion: @escaping @MainActor @Sendable (Result<String, CoreExecutionFailure>) -> Void) {
        begunServerURLs.append(serverURL)
        MainActor.assumeIsolated { completion(beginResult) }
    }

    func pairPoll(completion: @escaping @MainActor @Sendable (HostPairingSnapshot) -> Void) {
        MainActor.assumeIsolated { completion(snapshot) }
    }

    func pairConfirm(sas: String, completion: @escaping @MainActor @Sendable (Result<Void, CoreExecutionFailure>) -> Void) {
        confirmedSAS.append(sas)
        MainActor.assumeIsolated { completion(.success(())) }
    }

    func pairCancel(completion: @escaping @MainActor @Sendable (Result<Void, CoreExecutionFailure>) -> Void) {
        cancelCount += 1
        MainActor.assumeIsolated { completion(.success(())) }
    }

    func history(completion: @escaping @MainActor @Sendable (Result<[ClipboardHistoryEntry], CoreExecutionFailure>) -> Void) {
        MainActor.assumeIsolated { completion(historyResult) }
    }

    func historyImage(id: String, completion: @escaping @MainActor @Sendable (Result<Data, CoreExecutionFailure>) -> Void) {
        MainActor.assumeIsolated { completion(.failure(.init(message: "no image"))) }
    }

    func historyApply(id: String, completion: @escaping @MainActor @Sendable (Result<Void, CoreExecutionFailure>) -> Void) {
        appliedHistoryIDs.append(id)
        MainActor.assumeIsolated { completion(.success(())) }
    }

    func historyClear(completion: @escaping @MainActor @Sendable (Result<Void, CoreExecutionFailure>) -> Void) {
        clearHistoryCount += 1
        MainActor.assumeIsolated { completion(.success(())) }
    }

    func resetPairing(completion: @escaping @MainActor @Sendable (Result<Void, CoreExecutionFailure>) -> Void) {
        resetPairingCount += 1
        MainActor.assumeIsolated { completion(resetPairingResult) }
    }

    func send(_ payload: ClipboardPayload, completion: @escaping @MainActor @Sendable (Result<UInt64, CoreExecutionFailure>) -> Void) {
        sentPayloads.append(payload)
        MainActor.assumeIsolated { completion(.success(1)) }
    }

    func shutdown(completion: @escaping @MainActor @Sendable () -> Void) {
        MainActor.assumeIsolated { completion() }
    }
}

@MainActor
private final class FakeLoginItemController: LoginItemControlling {
    var status: LoginItemStatus
    var setError: Error?
    private(set) var openSettingsCount = 0

    init(status: LoginItemStatus) {
        self.status = status
    }

    func setEnabled(_ enabled: Bool) throws -> LoginItemStatus {
        if let setError { throw setError }
        status = enabled ? .enabled : .disabled
        return status
    }

    func openSystemSettings() {
        openSettingsCount += 1
    }
}

private enum TestFailure: Error {
    case denied
}

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

@MainActor
private final class SpyInterceptNotifier: InterceptNotifying {
    private(set) var reasons: [InterceptReason] = []

    func notify(reason: InterceptReason) {
        reasons.append(reason)
    }
}
