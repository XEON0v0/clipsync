import Foundation

private final class CallbackRecorder: FfiSpikeCallback, @unchecked Sendable {
    private let lock = NSLock()
    private var recordedMessage: String?

    func receive(message: String) {
        lock.withLock {
            recordedMessage = message
        }
    }

    func received() -> String? {
        lock.withLock { recordedMessage }
    }
}

@main
private enum CallbackHost {
    static func main() {
        let expected = "swift-to-rust-to-swift"
        let recorder = CallbackRecorder()
        let returned = ffiSpikeRoundTrip(callback: recorder, message: expected)
        guard returned == expected, recorder.received() == expected else {
            fatalError("UniFFI callback round trip returned unexpected values")
        }
        print("PASS: Swift host received Rust-to-foreign callback: \(expected)")
    }
}
