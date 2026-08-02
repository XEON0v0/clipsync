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

    public let settings: SettingsStore
    public let monitor: PasteboardMonitor
    private let core: CoreService

    public init(
        settings: SettingsStore = SettingsStore(),
        clipboard: ClipboardAccess = SystemClipboard(),
        dataDirectory: String? = nil
    ) {
        self.settings = settings
        monitor = PasteboardMonitor(clipboard: clipboard)
        let bridge = CoreCallbackBridge()
        let directory = dataDirectory ?? Self.defaultDataDirectory()
        core = CoreService(dataDirectory: directory, callbacks: bridge)
        bridge.sink = self

        monitor.onLocalChange = { [weak self] payload in
            self?.send(payload)
        }
        monitor.onError = { [weak self] error in
            self?.showError(error.localizedDescription)
        }
        monitor.start()
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

    public func beginPairing() {
        do {
            let url = try settings.validatedServerURL()
            lastError = nil
            core.pairBegin(serverURL: url.absoluteString) { [weak self] result in
                guard let self else { return }
                switch result {
                case let .success(qr):
                    pairingQRJSON = qr
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

    public func shutdown(completion: @escaping @MainActor @Sendable () -> Void) {
        monitor.stop()
        core.shutdown(completion: completion)
    }

    public func receiveLive(_ payload: ClipboardPayload) throws {
        _ = try monitor.applyRemote(payload, mailbox: false)
    }

    public func receiveMailbox(_ payload: ClipboardPayload) throws -> RemoteApplyResult {
        try monitor.applyRemote(payload, mailbox: true)
    }

    public func receiveStatus(_ status: CoreStatus) {
        switch status {
        case .readyUnpaired:
            connectionState = .unpaired
            statusText = "Not paired"
        case .offering:
            connectionState = .waitingForPeer
            statusText = "Waiting for phone"
        case .sasReady:
            connectionState = .sasReady
            statusText = "Confirm security code"
        case .connecting:
            connectionState = .connecting
            statusText = "Connecting..."
        case .connected:
            monitor.markConnected()
            connectionState = .connected
            statusText = "Connected"
            lastError = nil
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

    private func send(_ payload: ClipboardPayload) {
        core.send(payload) { [weak self] result in
            if case let .failure(error) = result {
                self?.showError(error.message)
            }
        }
    }

    private func showError(_ message: String) {
        lastError = message
        connectionState = .failed
        statusText = "Unavailable"
    }

    private static func defaultDataDirectory() -> String {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        let directory = base.appendingPathComponent("ClipSync", isDirectory: true)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory.path
    }
}
