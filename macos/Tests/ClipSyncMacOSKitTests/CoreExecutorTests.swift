import Dispatch
import XCTest
@testable import ClipSyncMacOSKit

final class CoreExecutorTests: XCTestCase {
    @MainActor
    func testQueuedMainCallbackDoesNotDeadlockBackgroundReset() async {
        let executor = CoreExecutor(label: "CoreExecutorTests")
        let callbackReachedMain = expectation(description: "callback reached main")
        let resetFinished = expectation(description: "reset finished")

        executor.submit {
            let callbackFinished = DispatchSemaphore(value: 0)
            DispatchQueue.global().async {
                DispatchQueue.main.sync {
                    callbackReachedMain.fulfill()
                }
                callbackFinished.signal()
            }
            XCTAssertEqual(callbackFinished.wait(timeout: .now() + 2), .success)
            resetFinished.fulfill()
        }

        await fulfillment(of: [callbackReachedMain, resetFinished], timeout: 3)
    }
}
