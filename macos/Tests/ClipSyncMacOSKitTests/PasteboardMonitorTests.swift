import AppKit
import Foundation
import XCTest
@testable import ClipSyncMacOSKit

@MainActor
final class PasteboardMonitorTests: XCTestCase {
    func testObservedLocalTextIsSent() throws {
        let clipboard = FakeClipboard()
        let monitor = PasteboardMonitor(clipboard: clipboard)
        var sent: [ClipboardPayload] = []
        monitor.onLocalChange = { sent.append($0) }

        clipboard.simulateUserCopy(.text("hello"))
        try monitor.poll()

        XCTAssertEqual(sent, [.text("hello")])
    }

    func testRemoteImageOwnershipTokenSuppressesExactlyOneEcho() throws {
        let clipboard = FakeClipboard()
        let monitor = PasteboardMonitor(clipboard: clipboard)
        let image = ClipboardPayload.image(png: Data([1, 2, 3]), semanticDigest: "image-digest")
        var sent: [ClipboardPayload] = []
        monitor.onLocalChange = { sent.append($0) }

        XCTAssertEqual(try monitor.applyRemote(image, mailbox: false), .applied)
        try monitor.poll()
        XCTAssertTrue(sent.isEmpty)

        clipboard.simulateUserCopy(image)
        try monitor.poll()
        XCTAssertEqual(sent, [image])
    }

    func testRemoteImageWrittenToSystemPasteboardDoesNotLoopBack() throws {
        let pasteboard = NSPasteboard.withUniqueName()
        defer { pasteboard.releaseGlobally() }
        let clipboard = SystemClipboard(pasteboard: pasteboard)
        let monitor = PasteboardMonitor(clipboard: clipboard)
        let payload = try ClipboardImageCodec.payload(
            fromEncodedImage: makePNG(),
            enforceEncodedLimit: true
        )
        var sent: [ClipboardPayload] = []
        monitor.onLocalChange = { sent.append($0) }

        XCTAssertEqual(try monitor.applyRemote(payload, mailbox: false), .applied)
        XCTAssertNotNil(pasteboard.data(forType: .png))
        XCTAssertNotNil(pasteboard.data(forType: .tiff))

        try monitor.poll()
        XCTAssertTrue(sent.isEmpty)
    }

    func testMailboxAppliesOnlyWhenClipboardStayedCleanSinceDisconnect() throws {
        let clipboard = FakeClipboard()
        let monitor = PasteboardMonitor(clipboard: clipboard)
        monitor.markDisconnected()

        XCTAssertEqual(try monitor.applyRemote(.text("clean"), mailbox: true), .applied)

        monitor.markDisconnected()
        clipboard.simulateUserCopy(.text("local edit"))
        XCTAssertEqual(try monitor.applyRemote(.text("deferred"), mailbox: true), .deferred)
        XCTAssertEqual(clipboard.payload, .text("local edit"))
    }

    func testMailboxDefersWhenClipboardIsOverwrittenDuringConditionalWrite() throws {
        let clipboard = FakeClipboard()
        let monitor = PasteboardMonitor(clipboard: clipboard)
        monitor.markDisconnected()
        clipboard.overwriteBeforeConditionalWrite = .text("racing local copy")

        XCTAssertEqual(try monitor.applyRemote(.text("mailbox"), mailbox: true), .deferred)
        XCTAssertEqual(clipboard.payload, .text("racing local copy"))
    }

    private func makePNG() throws -> Data {
        guard let bitmap = NSBitmapImageRep(
            bitmapDataPlanes: nil,
            pixelsWide: 2,
            pixelsHigh: 2,
            bitsPerSample: 8,
            samplesPerPixel: 4,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: .deviceRGB,
            bytesPerRow: 8,
            bitsPerPixel: 32
        ) else {
            XCTFail("failed to allocate test image")
            return Data()
        }
        bitmap.setColor(NSColor(deviceRed: 1, green: 0, blue: 0, alpha: 1), atX: 0, y: 0)
        bitmap.setColor(NSColor(deviceRed: 0, green: 1, blue: 0, alpha: 1), atX: 1, y: 0)
        bitmap.setColor(NSColor(deviceRed: 0, green: 0, blue: 1, alpha: 1), atX: 0, y: 1)
        bitmap.setColor(NSColor(deviceRed: 1, green: 1, blue: 1, alpha: 1), atX: 1, y: 1)
        guard let png = bitmap.representation(using: .png, properties: [:]) else {
            XCTFail("failed to encode test PNG")
            return Data()
        }
        return png
    }
}

@MainActor
private final class FakeClipboard: ClipboardAccess {
    private(set) var changeCount = 0
    private(set) var payload: ClipboardPayload?
    var overwriteBeforeConditionalWrite: ClipboardPayload?

    func readPayload() throws -> ClipboardPayload? {
        payload
    }

    func write(_ payload: ClipboardPayload, ifUnchanged expected: Int?) throws -> Int? {
        if let racing = overwriteBeforeConditionalWrite {
            overwriteBeforeConditionalWrite = nil
            simulateUserCopy(racing)
        }
        if let expected, expected != changeCount {
            return nil
        }
        self.payload = payload
        changeCount += 1
        return changeCount
    }

    func simulateUserCopy(_ payload: ClipboardPayload) {
        self.payload = payload
        changeCount += 1
    }
}
