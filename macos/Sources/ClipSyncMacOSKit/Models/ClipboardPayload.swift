import CryptoKit
import Foundation

public enum ClipboardPayload: Equatable, Sendable {
    case text(String)
    case image(png: Data, semanticDigest: String)

    public var semanticDigest: String {
        switch self {
        case let .text(text):
            return SHA256.hash(data: Data(text.utf8)).hexString
        case let .image(_, semanticDigest):
            return semanticDigest
        }
    }
}

extension Digest {
    fileprivate var hexString: String {
        map { String(format: "%02x", $0) }.joined()
    }
}
