import Foundation
import XCTest
@testable import ClipSyncMacOSKit

@MainActor
final class SettingsStoreTests: XCTestCase {
    func testServerURLDefaultsToEmpty() {
        let defaults = isolatedDefaults()
        XCTAssertEqual(SettingsStore(defaults: defaults).serverURL, "")
    }

    func testOnlyWSSRelayURLsAreAccepted() throws {
        let defaults = isolatedDefaults()
        let store = SettingsStore(defaults: defaults)

        for invalid in ["", "https://sync.example/ws", "ws://sync.example/ws", "not a url"] {
            store.serverURL = invalid
            XCTAssertThrowsError(try store.validatedServerURL(), "accepted \(invalid)")
        }

        store.serverURL = "wss://sync.example/ws"
        XCTAssertEqual(try store.validatedServerURL().absoluteString, "wss://sync.example/ws")
    }

    private func isolatedDefaults() -> UserDefaults {
        let suite = "SettingsStoreTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        return defaults
    }
}
