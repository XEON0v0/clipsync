import Foundation

// Swift host closed loop (T10): two CoreHandles pair through the real relay
// over the generated UniFFI bindings; a live text clip round-trips through
// the typed CoreCallbacks surface.
//
// Usage: SwiftClosedLoop <relay-ws-url>

private enum HostError: Error {
    case timeout(String)
}

private final class HostCallbacks: CoreCallbacks, @unchecked Sendable {
    private let lock = NSLock()
    private var clips: [FfiClipItem] = []

    func onClip(item: FfiClipItem) throws {
        lock.withLock { clips.append(item) }
    }

    func onMailboxClip(item _: FfiClipItem) throws -> MailboxDisposition {
        .applied
    }

    func onStatus(status _: CoreStatus) {}

    func receivedTexts() -> [String] {
        lock.withLock {
            clips.compactMap { item in
                if case let .text(text) = item.content { return text }
                return nil
            }
        }
    }
}

private func waitSas(_ handle: CoreHandle) throws -> String {
    let deadline = Date().addingTimeInterval(10)
    while Date() < deadline {
        if case let .sasReady(sas) = handle.pairPoll() { return sas }
        Thread.sleep(forTimeInterval: 0.025)
    }
    throw HostError.timeout("peer SAS did not arrive")
}

private func waitTexts(_ callbacks: HostCallbacks, count: Int) throws -> [String] {
    let deadline = Date().addingTimeInterval(10)
    while Date() < deadline {
        let texts = callbacks.receivedTexts()
        if texts.count >= count { return texts }
        Thread.sleep(forTimeInterval: 0.025)
    }
    throw HostError.timeout("live clip callback did not arrive")
}

@main
private enum SwiftClosedLoop {
    static func main() throws {
        let arguments = CommandLine.arguments
        guard arguments.count == 2 else {
            fatalError("usage: SwiftClosedLoop <relay-ws-url>")
        }
        let relayURL = arguments[1]
        let scratch = NSTemporaryDirectory()
        let dirA = scratch + "clipsync-swift-a-\(UUID().uuidString)"
        let dirB = scratch + "clipsync-swift-b-\(UUID().uuidString)"
        defer { try? FileManager.default.removeItem(atPath: dirA); try? FileManager.default.removeItem(atPath: dirB) }

        let callbacksA = HostCallbacks()
        let callbacksB = HostCallbacks()
        let handleA = try CoreHandle(dataDir: dirA, callbacks: callbacksA)
        let handleB = try CoreHandle(dataDir: dirB, callbacks: callbacksB)

        let qr = try handleA.pairBegin(serverUrl: relayURL)
        let sasB = try handleB.pairClaim(qrPayload: qr)
        let sasA = try waitSas(handleA)
        guard sasA == sasB else {
            fatalError("FAIL: SAS mismatch between hosts")
        }
        try handleA.pairConfirm(sas: sasA)
        try handleB.pairConfirm(sas: sasB)

        let seq = try handleA.sendText(text: "swift closed loop")
        guard seq == 1 else {
            fatalError("FAIL: unexpected first sequence number \(seq)")
        }
        let texts = try waitTexts(callbacksB, count: 1)
        guard texts == ["swift closed loop"] else {
            fatalError("FAIL: unexpected callback payload \(texts)")
        }

        try handleA.shutdown()
        try handleB.shutdown()
        print("PASS: Swift host closed loop delivered live clip over UniFFI FFI")
    }
}
