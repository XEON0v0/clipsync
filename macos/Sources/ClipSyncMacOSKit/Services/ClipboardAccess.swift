import AppKit
import Foundation

/// 单轮剪贴板读取结果：内容 + 与内容同轮取得的敏感标记。
public struct ReadResult: Equatable, Sendable {
    public let payload: ClipboardPayload
    public let markedSensitive: Bool

    public init(payload: ClipboardPayload, markedSensitive: Bool) {
        self.payload = payload
        self.markedSensitive = markedSensitive
    }
}

@MainActor
public protocol ClipboardAccess: AnyObject {
    var changeCount: Int { get }
    func readPayload() throws -> ReadResult?
    func write(_ payload: ClipboardPayload, ifUnchanged expected: Int?) throws -> Int?
}

@MainActor
public final class SystemClipboard: ClipboardAccess {
    private let pasteboard: NSPasteboard

    public init(pasteboard: NSPasteboard = .general) {
        self.pasteboard = pasteboard
    }

    public var changeCount: Int {
        pasteboard.changeCount
    }

    private static let sensitiveMarkerTypes: Set<String> = [
        "org.nspasteboard.ConcealedType",
        "org.nspasteboard.TransientType",
    ]

    public func readPayload() throws -> ReadResult? {
        let markedSensitive = (pasteboard.types ?? []).contains {
            Self.sensitiveMarkerTypes.contains($0.rawValue)
        }
        if let png = pasteboard.data(forType: .png) {
            return ReadResult(
                payload: try ClipboardImageCodec.payload(fromEncodedImage: png, enforceEncodedLimit: true),
                markedSensitive: markedSensitive
            )
        }
        if let tiff = pasteboard.data(forType: .tiff) {
            return ReadResult(
                payload: try ClipboardImageCodec.payload(fromEncodedImage: tiff, enforceEncodedLimit: false),
                markedSensitive: markedSensitive
            )
        }
        if let text = pasteboard.string(forType: .string) {
            return ReadResult(payload: .text(text), markedSensitive: markedSensitive)
        }
        return nil
    }

    public func write(_ payload: ClipboardPayload, ifUnchanged expected: Int?) throws -> Int? {
        if let expected, expected != pasteboard.changeCount {
            return nil
        }
        pasteboard.clearContents()
        switch payload {
        case let .text(text):
            guard pasteboard.setString(text, forType: .string) else {
                return nil
            }
        case let .image(png, _):
            let tiff = try ClipboardImageCodec.tiffData(fromPNG: png)
            guard pasteboard.setData(png, forType: .png),
                  pasteboard.setData(tiff, forType: .tiff)
            else {
                return nil
            }
        }
        return pasteboard.changeCount
    }
}
