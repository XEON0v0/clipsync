import Dispatch
import Foundation

// Swift-host deadlock regression (T10): a background CoreExecutor performs
// reset_pairing while a clipboard callback awaits the main queue
// (MainActor). reset_pairing stops and joins the session/dispatcher using
// only blocking std synchronization and never needs the main thread, so the
// callback returns as soon as the free main queue runs and the whole reset
// completes well inside the timeout. Any design that takes the handle lock or
// joins on the main thread times out here and fails.
//
// Usage: SwiftDeadlockRegression <relay-ws-url>

private final class BlockingCallbacks: CoreCallbacks, @unchecked Sendable {
    let callbackArrived = DispatchSemaphore(value: 0)
    let callbackRanOnMain = DispatchSemaphore(value: 0)

    func onClip(item _: FfiClipItem) throws {
        callbackArrived.signal()
        // Awaiting the MainActor: the callback only returns after the main
        // queue executes this block.
        _ = DispatchQueue.main.sync {
            callbackRanOnMain.signal()
        }
    }

    func onMailboxClip(item _: FfiClipItem) throws -> MailboxDisposition {
        .applied
    }

    func onStatus(status _: CoreStatus) {}
}

private func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("FAIL: \(message)\n".utf8))
    exit(EXIT_FAILURE)
}

private func waitSas(_ handle: CoreHandle) -> String {
    let deadline = Date().addingTimeInterval(10)
    while Date() < deadline {
        if case let .sasReady(sas) = handle.pairPoll() { return sas }
        Thread.sleep(forTimeInterval: 0.025)
    }
    fail("peer SAS did not arrive in time")
}

@main
private enum SwiftDeadlockRegression {
    static func main() {
        let arguments = CommandLine.arguments
        guard arguments.count == 2 else {
            fail("usage: SwiftDeadlockRegression <relay-ws-url>")
        }
        let relayURL = arguments[1]
        let scratch = NSTemporaryDirectory()
        let dirA = scratch + "clipsync-deadlock-a-\(UUID().uuidString)"
        let dirB = scratch + "clipsync-deadlock-b-\(UUID().uuidString)"

        let coordinator = DispatchQueue(label: "com.clipsync.ffi.coordinator")
        coordinator.async {
            do {
                let callbacksB = BlockingCallbacks()
                let handleA = try CoreHandle(dataDir: dirA, callbacks: BlockingCallbacks())
                let handleB = try CoreHandle(dataDir: dirB, callbacks: callbacksB)

                let qr = try handleA.pairBegin(serverUrl: relayURL)
                let sasB = try handleB.pairClaim(qrPayload: qr)
                let sasA = waitSas(handleA)
                guard sasA == sasB else { fail("SAS mismatch between hosts") }
                try handleA.pairConfirm(sas: sasA)
                try handleB.pairConfirm(sas: sasB)

                _ = try handleA.sendText(text: "deadlock probe")
                guard callbacksB.callbackArrived.wait(timeout: .now() + 10) == .success else {
                    fail("callback did not arrive before the reset deadline")
                }
                // The callback is now parked awaiting the main queue.

                let resetFinished = DispatchSemaphore(value: 0)
                DispatchQueue(label: "com.clipsync.ffi.core-executor").async {
                    do {
                        try handleB.resetPairing()
                    } catch {
                        fail("reset_pairing failed: \(error)")
                    }
                    resetFinished.signal()
                }
                guard resetFinished.wait(timeout: .now() + 10) == .success else {
                    fail("background reset_pairing deadlocked behind a MainActor-awaiting callback")
                }
                guard callbacksB.callbackRanOnMain.wait(timeout: .now() + 10) == .success else {
                    fail("callback never reached the main queue")
                }

                try handleA.shutdown()
                try handleB.shutdown()
                try? FileManager.default.removeItem(atPath: dirA)
                try? FileManager.default.removeItem(atPath: dirB)
                print("PASS: background reset_pairing joined while callback awaited MainActor; no deadlock")
                exit(EXIT_SUCCESS)
            } catch {
                fail("unexpected error: \(error)")
            }
        }
        dispatchMain()
    }
}
