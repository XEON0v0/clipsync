import Foundation

public enum RemoteApplyResult: Equatable, Sendable {
    case applied
    case deferred
}

private struct OwnershipToken: Equatable {
    let changeCount: Int
    let semanticDigest: String
}

@MainActor
public final class PasteboardMonitor: NSObject {
    public static let pollInterval: TimeInterval = 0.3

    private let clipboard: ClipboardAccess
    private var observedChangeCount: Int
    private var ownershipToken: OwnershipToken?
    private var disconnectBaseline: Int?
    private var timer: Timer?

    public var onLocalChange: ((ClipboardPayload) -> Void)?
    public var onError: ((Error) -> Void)?

    public init(clipboard: ClipboardAccess) {
        self.clipboard = clipboard
        observedChangeCount = clipboard.changeCount
        super.init()
    }

    public func start() {
        guard timer == nil else { return }
        let timer = Timer(timeInterval: Self.pollInterval, target: self, selector: #selector(timerFired), userInfo: nil, repeats: true)
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    public func stop() {
        timer?.invalidate()
        timer = nil
    }

    public func poll() throws {
        let changeCount = clipboard.changeCount
        guard changeCount != observedChangeCount else { return }
        observedChangeCount = changeCount
        guard let payload = try clipboard.readPayload() else {
            ownershipToken = nil
            return
        }
        if ownershipToken == OwnershipToken(changeCount: changeCount, semanticDigest: payload.semanticDigest) {
            ownershipToken = nil
            return
        }
        ownershipToken = nil
        onLocalChange?(payload)
    }

    public func markDisconnected() {
        disconnectBaseline = clipboard.changeCount
    }

    public func markConnected() {
        disconnectBaseline = nil
    }

    public func applyRemote(_ payload: ClipboardPayload, mailbox: Bool) throws -> RemoteApplyResult {
        let expected: Int?
        if mailbox {
            guard let baseline = disconnectBaseline, clipboard.changeCount == baseline else {
                return .deferred
            }
            expected = baseline
        } else {
            expected = nil
        }
        guard let writtenChangeCount = try clipboard.write(payload, ifUnchanged: expected) else {
            return .deferred
        }
        ownershipToken = OwnershipToken(
            changeCount: writtenChangeCount,
            semanticDigest: payload.semanticDigest
        )
        if mailbox {
            disconnectBaseline = writtenChangeCount
        }
        return .applied
    }

    @objc private func timerFired() {
        do {
            try poll()
        } catch {
            onError?(error)
        }
    }
}
