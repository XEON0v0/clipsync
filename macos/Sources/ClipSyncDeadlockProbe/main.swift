import Dispatch
import Foundation

private final class DeadlockHarness: @unchecked Sendable {
    private let callbackQueued = DispatchSemaphore(value: 0)
    private let callbackRan = DispatchSemaphore(value: 0)
    private let resetRequested = DispatchSemaphore(value: 0)
    private let shutdownCompleted = DispatchSemaphore(value: 0)
    private let worker = DispatchQueue(label: "com.clipsync.spike.worker")

    func queueCallbackBeforeReset() {
        worker.async {
            DispatchQueue.main.async {
                self.callbackRan.signal()
            }
            self.callbackQueued.signal()
            if self.resetRequested.wait(timeout: .now() + 2) == .success {
                self.shutdownCompleted.signal()
            }
        }
    }

    func waitUntilCallbackQueued() -> Bool {
        callbackQueued.wait(timeout: .now() + 2) == .success
    }

    func resetFromMainThread() {
        dispatchPrecondition(condition: .onQueue(.main))
        resetRequested.signal()
    }

    func awaitCompletion() -> Bool {
        let callbackFinished = callbackRan.wait(timeout: .now() + 2) == .success
        let shutdownFinished = shutdownCompleted.wait(timeout: .now() + 2) == .success
        return callbackFinished && shutdownFinished
    }
}

@main
private enum ClipSyncDeadlockProbe {
    static func main() {
        let harness = DeadlockHarness()
        harness.queueCallbackBeforeReset()
        guard harness.waitUntilCallbackQueued() else {
            fatalError("FAIL: callback was not queued before the reset deadline")
        }

        harness.resetFromMainThread()
        DispatchQueue.global().async {
            guard harness.awaitCompletion() else {
                fatalError("FAIL: queued callback or background shutdown exceeded the deadline")
            }
            let message = "PASS: queued callback and background shutdown completed without blocking main thread\n"
            FileHandle.standardOutput.write(Data(message.utf8))
            exit(EXIT_SUCCESS)
        }
        dispatchMain()
    }
}
