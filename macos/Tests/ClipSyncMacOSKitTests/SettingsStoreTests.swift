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

    func testSyncPausedDefaultsFalseAndPersists() {
        let defaults = isolatedDefaults()
        let store = SettingsStore(defaults: defaults)
        XCTAssertFalse(store.syncPaused)

        store.syncPaused = true

        XCTAssertTrue(SettingsStore(defaults: defaults).syncPaused)
    }

    func testSensitiveRulesDefaultEmptyAndRoundTrip() {
        let defaults = isolatedDefaults()
        let store = SettingsStore(defaults: defaults)
        XCTAssertEqual(store.sensitiveRules, [])

        store.sensitiveRules = [
            SensitiveRule(pattern: "hunter2", isRegex: false),
            SensitiveRule(pattern: "corp-\\w+-token", isRegex: true)
        ]

        let reloaded = SettingsStore(defaults: defaults).sensitiveRules
        XCTAssertEqual(reloaded.map(\.pattern), ["hunter2", "corp-\\w+-token"])
        XCTAssertEqual(reloaded.map(\.isRegex), [false, true])
    }

    func testCorruptSensitiveRulesDataFallsBackToEmpty() {
        let defaults = isolatedDefaults()
        defaults.set(Data("not json".utf8), forKey: "sensitiveRules")

        XCTAssertEqual(SettingsStore(defaults: defaults).sensitiveRules, [])
    }

    private func isolatedDefaults() -> UserDefaults {
        let suite = "SettingsStoreTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        return defaults
    }
}
