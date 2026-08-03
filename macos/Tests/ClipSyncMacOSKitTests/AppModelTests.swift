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

    private func makeModel(
        core: FakeCoreService = FakeCoreService(),
        clipboard: ModelFakeClipboard = ModelFakeClipboard(),
        loginItem: FakeLoginItemController = FakeLoginItemController(status: .disabled)
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
            startMonitoring: false
        )
    }

}

private final class FakeCoreService: CoreServicing, @unchecked Sendable {
    var beginResult: Result<String, CoreExecutionFailure> = .failure(.init(message: "not configured"))
    var snapshot: HostPairingSnapshot = .unpaired
    var historyResult: Result<[ClipboardHistoryEntry], CoreExecutionFailure> = .success([])
    private(set) var confirmedSAS: [String] = []
    private(set) var cancelCount = 0
    private(set) var appliedHistoryIDs: [String] = []
    private(set) var clearHistoryCount = 0

    func initialize(completion: @escaping @MainActor @Sendable (Result<Bool, CoreExecutionFailure>) -> Void) {
        MainActor.assumeIsolated { completion(.success(false)) }
    }

    func pairBegin(serverURL: String, completion: @escaping @MainActor @Sendable (Result<String, CoreExecutionFailure>) -> Void) {
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

    func send(_ payload: ClipboardPayload, completion: @escaping @MainActor @Sendable (Result<UInt64, CoreExecutionFailure>) -> Void) {
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

    func readPayload() throws -> ClipboardPayload? {
        payload
    }

    func write(_ payload: ClipboardPayload, ifUnchanged expected: Int?) throws -> Int? {
        if let expected, expected != changeCount {
            return nil
        }
        self.payload = payload
        changeCount += 1
        return changeCount
    }
}
