import ClipboardCoreBindings
import Foundation

public enum AppConnectionState: Equatable, Sendable {
    case starting
    case unpaired
    case waitingForPeer
    case sasReady
    case connecting
    case connected
    case reconnecting
    case disconnected
    case failed
}

@MainActor
public final class AppModel: ObservableObject, CoreCallbackSink {
    @Published public private(set) var connectionState: AppConnectionState = .starting
    @Published public private(set) var statusText = "Starting..."
    @Published public private(set) var lastError: String?
    @Published public private(set) var pairingQRJSON: String?
    @Published public private(set) var pairingCode: String?
    @Published public private(set) var pairingSAS: String?
    @Published public private(set) var history: [ClipboardHistoryEntry] = []
    @Published public private(set) var historyError: String?
    @Published public private(set) var loginItemStatus: LoginItemStatus
    @Published public private(set) var loginItemError: String?

    public let settings: SettingsStore
    public let monitor: PasteboardMonitor
    private let core: any CoreServicing
    private let loginItemController: any LoginItemControlling

    public convenience init(
        settings: SettingsStore = SettingsStore(),
        clipboard: ClipboardAccess = SystemClipboard(),
        dataDirectory: String? = nil
    ) {
        let bridge = CoreCallbackBridge()
        let directory = dataDirectory ?? Self.defaultDataDirectory()
        self.init(
            settings: settings,
            clipboard: clipboard,
            core: CoreService(dataDirectory: directory, callbacks: bridge),
            loginItemController: LoginItemController(),
            callbackBridge: bridge,
            startMonitoring: true
        )
    }

    public convenience init(
        settings: SettingsStore,
        clipboard: ClipboardAccess,
        core: any CoreServicing,
        loginItemController: any LoginItemControlling,
        startMonitoring: Bool
    ) {
        self.init(
            settings: settings,
            clipboard: clipboard,
            core: core,
            loginItemController: loginItemController,
            callbackBridge: nil,
            startMonitoring: startMonitoring
        )
    }

    private init(
        settings: SettingsStore,
        clipboard: ClipboardAccess,
        core: any CoreServicing,
        loginItemController: any LoginItemControlling,
        callbackBridge: CoreCallbackBridge?,
        startMonitoring: Bool
    ) {
        self.settings = settings
        monitor = PasteboardMonitor(clipboard: clipboard)
        self.core = core
        self.loginItemController = loginItemController
        loginItemStatus = loginItemController.status
        callbackBridge?.sink = self

        monitor.onLocalChange = { [weak self] payload in
            self?.send(payload)
        }
        monitor.onError = { [weak self] error in
            self?.showError(error.localizedDescription)
        }
        if startMonitoring {
            monitor.start()
        }
        core.initialize { [weak self] result in
            guard let self else { return }
            switch result {
            case let .success(paired):
                if !paired {
                    connectionState = .unpaired
                    statusText = "Not paired"
                }
            case let .failure(error):
                showError(error.message)
            }
        }
    }

    public var loginItemEnabled: Bool {
        loginItemStatus == .enabled
    }

    public var loginItemStatusText: String {
        if loginItemError != nil {
            return "Approval required in System Settings"
        }
        return switch loginItemStatus {
        case .disabled: "Off"
        case .enabled: "On"
        case .requiresApproval: "Requires approval in System Settings"
        case .unavailable: "Unavailable for this app"
        }
    }

    public var showsLoginItemSettingsLink: Bool {
        loginItemStatus == .requiresApproval || loginItemError != nil
    }

    public func beginPairing() {
        do {
            let url = try settings.validatedServerURL()
            lastError = nil
            core.pairBegin(serverURL: url.absoluteString) { [weak self] result in
                guard let self else { return }
                switch result {
                case let .success(qr):
                    pairingQRJSON = qr
                    pairingCode = Self.decodePairingCode(qr)
                    pairingSAS = nil
                    connectionState = .waitingForPeer
                    statusText = "Waiting for phone"
                case let .failure(error):
                    showError(error.message)
                }
            }
        } catch {
            showError(error.localizedDescription)
        }
    }

    public func confirmPairing() {
        guard let pairingSAS else { return }
        core.pairConfirm(sas: pairingSAS) { [weak self] result in
            guard let self else { return }
            switch result {
            case .success:
                connectionState = .connecting
                statusText = "Connecting..."
            case let .failure(error):
                showError(error.message)
            }
        }
    }

    public func cancelPairing() {
        core.pairCancel { [weak self] result in
            guard let self else { return }
            switch result {
            case .success:
                clearPairingPresentation()
                connectionState = .unpaired
                statusText = "Not paired"
            case let .failure(error):
                showError(error.message)
            }
        }
    }

    public func refreshHistory() {
        core.history { [weak self] result in
            guard let self else { return }
            switch result {
            case let .success(items):
                history = items.sorted { $0.tsMs > $1.tsMs }
                historyError = nil
            case let .failure(error):
                historyError = error.message
            }
        }
    }

    public func applyHistory(_ entry: ClipboardHistoryEntry) {
        switch entry.content {
        case let .text(text):
            applyHistoryPayload(.text(text), entry: entry)
        case .image:
            core.historyImage(id: entry.id) { [weak self] result in
                guard let self else { return }
                switch result {
                case let .success(bytes):
                    do {
                        let payload = try ClipboardImageCodec.payload(fromEncodedImage: bytes, enforceEncodedLimit: true)
                        applyHistoryPayload(payload, entry: entry)
                    } catch {
                        historyError = error.localizedDescription
                    }
                case let .failure(error):
                    historyError = error.message
                }
            }
        }
    }

    public func clearHistory() {
        core.historyClear { [weak self] result in
            guard let self else { return }
            switch result {
            case .success:
                history = []
                historyError = nil
            case let .failure(error):
                historyError = error.message
            }
        }
    }

    public func setLoginItemEnabled(_ enabled: Bool) {
        do {
            loginItemStatus = try loginItemController.setEnabled(enabled)
            loginItemError = nil
            lastError = nil
        } catch {
            loginItemStatus = loginItemController.status
            loginItemError = error.localizedDescription
            lastError = error.localizedDescription
        }
    }

    public func refreshLoginItemStatus() {
        loginItemStatus = loginItemController.status
        if loginItemStatus == .enabled {
            loginItemError = nil
        }
    }

    public func openLoginItemSettings() {
        loginItemController.openSystemSettings()
    }

    public func shutdown(completion: @escaping @MainActor @Sendable () -> Void) {
        monitor.stop()
        core.shutdown(completion: completion)
    }

    public func receiveLive(_ payload: ClipboardPayload) throws {
        _ = try monitor.applyRemote(payload, mailbox: false)
        refreshHistory()
    }

    public func receiveMailbox(_ payload: ClipboardPayload) throws -> RemoteApplyResult {
        let result = try monitor.applyRemote(payload, mailbox: true)
        refreshHistory()
        return result
    }

    public func receiveStatus(_ status: CoreStatus) {
        switch status {
        case .readyUnpaired:
            clearPairingPresentation()
            connectionState = .unpaired
            statusText = "Not paired"
        case .offering:
            connectionState = .waitingForPeer
            statusText = "Waiting for phone"
        case .sasReady:
            connectionState = .sasReady
            statusText = "Confirm security code"
            refreshPairingSnapshot()
        case .connecting:
            connectionState = .connecting
            statusText = "Connecting..."
        case .connected:
            monitor.markConnected()
            clearPairingPresentation()
            connectionState = .connected
            statusText = "Connected"
            lastError = nil
            refreshHistory()
        case .reconnecting:
            monitor.markDisconnected()
            connectionState = .reconnecting
            statusText = "Reconnecting..."
        case .disconnected:
            monitor.markDisconnected()
            connectionState = .disconnected
            statusText = "Disconnected"
        case let .error(message):
            showError(message)
        }
    }

    private func refreshPairingSnapshot() {
        core.pairPoll { [weak self] snapshot in
            guard let self else { return }
            switch snapshot {
            case let .sasReady(sas):
                pairingSAS = sas
                connectionState = .sasReady
                statusText = "Confirm security code"
            case let .offering(qrJSON):
                pairingQRJSON = qrJSON
                pairingCode = Self.decodePairingCode(qrJSON)
            case .unpaired:
                clearPairingPresentation()
                connectionState = .unpaired
                statusText = "Not paired"
            case .paired:
                connectionState = .connecting
                statusText = "Connecting..."
            }
        }
    }

    private func applyHistoryPayload(_ payload: ClipboardPayload, entry: ClipboardHistoryEntry) {
        do {
            _ = try monitor.applyRemote(payload, mailbox: false)
            historyError = nil
        } catch {
            historyError = error.localizedDescription
            return
        }
        guard entry.source == .remoteDeferred else { return }
        core.historyApply(id: entry.id) { [weak self] result in
            guard let self else { return }
            switch result {
            case .success:
                refreshHistory()
            case let .failure(error):
                historyError = error.message
            }
        }
    }

    private func send(_ payload: ClipboardPayload) {
        core.send(payload) { [weak self] result in
            if case let .failure(error) = result {
                self?.showError(error.message)
            } else {
                self?.refreshHistory()
            }
        }
    }

    private func clearPairingPresentation() {
        pairingQRJSON = nil
        pairingCode = nil
        pairingSAS = nil
    }

    private func showError(_ message: String) {
        lastError = message
        connectionState = .failed
        statusText = "Unavailable"
    }

    private static func decodePairingCode(_ json: String) -> String? {
        struct Payload: Decodable { let code: String }
        return try? JSONDecoder().decode(Payload.self, from: Data(json.utf8)).code
    }

    private static func defaultDataDirectory() -> String {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        let directory = base.appendingPathComponent("ClipSync", isDirectory: true)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory.path
    }
}
