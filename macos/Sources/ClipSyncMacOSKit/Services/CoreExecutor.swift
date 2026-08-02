@preconcurrency import Dispatch
import Foundation

public struct CoreExecutionFailure: Error, LocalizedError, Sendable {
    public let message: String

    public var errorDescription: String? { message }
}

public final class CoreExecutor: @unchecked Sendable {
    private let queue: DispatchQueue

    public init(label: String = "com.clipsync.macos.core") {
        queue = DispatchQueue(label: label, qos: .userInitiated)
    }

    public func submit(_ operation: @escaping @Sendable () -> Void) {
        queue.async(execute: operation)
    }

    public func submit<T: Sendable>(
        _ operation: @escaping @Sendable () throws -> T,
        completion: @escaping @MainActor @Sendable (Result<T, CoreExecutionFailure>) -> Void
    ) {
        queue.async {
            let result: Result<T, CoreExecutionFailure>
            do {
                result = .success(try operation())
            } catch {
                result = .failure(CoreExecutionFailure(message: error.localizedDescription))
            }
            Task { @MainActor in
                completion(result)
            }
        }
    }
}
