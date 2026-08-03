@preconcurrency import Dispatch
import ClipboardCoreBindings
import Foundation

@MainActor
public protocol CoreCallbackSink: AnyObject {
    func receiveLive(_ payload: ClipboardPayload) throws
    func receiveMailbox(_ payload: ClipboardPayload) throws -> RemoteApplyResult
    func receiveStatus(_ status: CoreStatus)
}

public final class CoreCallbackBridge: CoreCallbacks, @unchecked Sendable {
    @MainActor public weak var sink: (any CoreCallbackSink)?

    public init() {}

    public func onClip(item: FfiClipItem) throws {
        let payload = try item.clipboardPayload()
        try DispatchQueue.main.sync {
            try MainActor.assumeIsolated {
                try sink?.receiveLive(payload)
            }
        }
    }

    public func onMailboxClip(item: FfiClipItem) throws -> MailboxDisposition {
        let payload = try item.clipboardPayload()
        return try DispatchQueue.main.sync {
            try MainActor.assumeIsolated {
                try sink?.receiveMailbox(payload) == .applied ? .applied : .deferred
            }
        }
    }

    public func onStatus(status: CoreStatus) {
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated {
                self?.sink?.receiveStatus(status)
            }
        }
    }
}

public protocol CoreServicing: AnyObject, Sendable {
    func initialize(completion: @escaping @MainActor @Sendable (Result<Bool, CoreExecutionFailure>) -> Void)
    func pairBegin(serverURL: String, completion: @escaping @MainActor @Sendable (Result<String, CoreExecutionFailure>) -> Void)
    func pairPoll(completion: @escaping @MainActor @Sendable (HostPairingSnapshot) -> Void)
    func pairConfirm(sas: String, completion: @escaping @MainActor @Sendable (Result<Void, CoreExecutionFailure>) -> Void)
    func pairCancel(completion: @escaping @MainActor @Sendable (Result<Void, CoreExecutionFailure>) -> Void)
    func history(completion: @escaping @MainActor @Sendable (Result<[ClipboardHistoryEntry], CoreExecutionFailure>) -> Void)
    func historyImage(id: String, completion: @escaping @MainActor @Sendable (Result<Data, CoreExecutionFailure>) -> Void)
    func historyApply(id: String, completion: @escaping @MainActor @Sendable (Result<Void, CoreExecutionFailure>) -> Void)
    func historyClear(completion: @escaping @MainActor @Sendable (Result<Void, CoreExecutionFailure>) -> Void)
    func send(_ payload: ClipboardPayload, completion: @escaping @MainActor @Sendable (Result<UInt64, CoreExecutionFailure>) -> Void)
    func shutdown(completion: @escaping @MainActor @Sendable () -> Void)
}

public final class CoreService: CoreServicing, @unchecked Sendable {
    private let executor: CoreExecutor
    private let dataDirectory: String
    private let callbacks: CoreCallbackBridge
    private var handle: CoreHandle?

    public init(dataDirectory: String, callbacks: CoreCallbackBridge, executor: CoreExecutor = CoreExecutor()) {
        self.dataDirectory = dataDirectory
        self.callbacks = callbacks
        self.executor = executor
    }

    public func initialize(completion: @escaping @MainActor @Sendable (Result<Bool, CoreExecutionFailure>) -> Void) {
        executor.submit({ [self] in
            let handle = try CoreHandle(dataDir: dataDirectory, callbacks: callbacks)
            self.handle = handle
            let paired = try handle.pairLoad()
            if paired {
                try handle.start()
            }
            return paired
        }, completion: completion)
    }

    public func pairBegin(serverURL: String, completion: @escaping @MainActor @Sendable (Result<String, CoreExecutionFailure>) -> Void) {
        executor.submit({ [self] in
            try requireHandle().pairBegin(serverUrl: serverURL)
        }, completion: completion)
    }

    public func pairPoll(completion: @escaping @MainActor @Sendable (HostPairingSnapshot) -> Void) {
        executor.submit({ [self] in
            switch try requireHandle().pairPoll() {
            case .unpaired:
                return .unpaired
            case let .offering(qrJson):
                return .offering(qrJSON: qrJson)
            case let .sasReady(sas):
                return .sasReady(sas: sas)
            case let .paired(roomId):
                return .paired(roomID: roomId)
            }
        }) { result in
            completion((try? result.get()) ?? .unpaired)
        }
    }

    public func pairConfirm(sas: String, completion: @escaping @MainActor @Sendable (Result<Void, CoreExecutionFailure>) -> Void) {
        executor.submit({ [self] in
            try requireHandle().pairConfirm(sas: sas)
        }, completion: completion)
    }

    public func pairCancel(completion: @escaping @MainActor @Sendable (Result<Void, CoreExecutionFailure>) -> Void) {
        executor.submit({ [self] in
            try requireHandle().pairCancel()
        }, completion: completion)
    }

    public func history(completion: @escaping @MainActor @Sendable (Result<[ClipboardHistoryEntry], CoreExecutionFailure>) -> Void) {
        executor.submit({ [self] in
            try requireHandle().history().map(ClipboardHistoryEntry.init)
        }, completion: completion)
    }

    public func historyImage(id: String, completion: @escaping @MainActor @Sendable (Result<Data, CoreExecutionFailure>) -> Void) {
        executor.submit({ [self] in
            try requireHandle().historyImageBytes(id: id)
        }, completion: completion)
    }

    public func historyApply(id: String, completion: @escaping @MainActor @Sendable (Result<Void, CoreExecutionFailure>) -> Void) {
        executor.submit({ [self] in
            try requireHandle().historyApply(id: id)
        }, completion: completion)
    }

    public func historyClear(completion: @escaping @MainActor @Sendable (Result<Void, CoreExecutionFailure>) -> Void) {
        executor.submit({ [self] in
            try requireHandle().historyClear()
        }, completion: completion)
    }

    public func send(_ payload: ClipboardPayload, completion: @escaping @MainActor @Sendable (Result<UInt64, CoreExecutionFailure>) -> Void) {
        executor.submit({ [self] in
            switch payload {
            case let .text(text):
                return try requireHandle().sendText(text: text)
            case let .image(png, _):
                return try requireHandle().sendImage(bytes: png)
            }
        }, completion: completion)
    }

    public func shutdown(completion: @escaping @MainActor @Sendable () -> Void) {
        executor.submit({ [self] in
            if let handle {
                try handle.shutdown()
                self.handle = nil
            }
        }) { (_: Result<Void, CoreExecutionFailure>) in
            completion()
        }
    }

    private func requireHandle() throws -> CoreHandle {
        guard let handle else {
            throw CoreExecutionFailure(message: "Core is not ready.")
        }
        return handle
    }
}

private extension ClipboardHistoryEntry {
    init(_ item: FfiHistoryItem) {
        let content: ClipboardHistoryContent = switch item.kind {
        case let .text(content): .text(content)
        case .image: .image
        }
        let source: ClipboardHistorySource = switch item.source {
        case .local: .local
        case .remote: .remote
        case .remoteDeferred: .remoteDeferred
        }
        self.init(id: item.id, tsMs: item.tsMs, content: content, source: source)
    }
}

private extension FfiClipItem {
    func clipboardPayload() throws -> ClipboardPayload {
        switch content {
        case let .text(text):
            return .text(text)
        case let .image(bytes):
            return try ClipboardImageCodec.payload(fromEncodedImage: bytes, enforceEncodedLimit: true)
        }
    }
}
