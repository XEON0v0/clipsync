import AppKit
import Foundation

@MainActor
public protocol ClipboardAccess: AnyObject {
    var changeCount: Int { get }
    func readPayload() throws -> ClipboardPayload?
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

    public func readPayload() throws -> ClipboardPayload? {
        if let png = pasteboard.data(forType: .png) {
            return try ClipboardImageCodec.payload(fromEncodedImage: png, enforceEncodedLimit: true)
        }
        if let tiff = pasteboard.data(forType: .tiff) {
            return try ClipboardImageCodec.payload(fromEncodedImage: tiff, enforceEncodedLimit: false)
        }
        if let text = pasteboard.string(forType: .string) {
            return .text(text)
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
