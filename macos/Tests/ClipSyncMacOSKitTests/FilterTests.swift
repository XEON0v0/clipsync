import XCTest
@testable import ClipSyncMacOSKit

final class FilterTests: XCTestCase {
    private func evaluate(
        paused: Bool = false,
        markedSensitive: Bool = false,
        payload: ClipboardPayload,
        rules: [SensitiveRule] = []
    ) -> FilterDecision {
        SensitiveContentFilter.evaluate(
            paused: paused,
            markedSensitive: markedSensitive,
            payload: payload,
            rules: rules
        )
    }

    func testPausedBlocksEverything() {
        XCTAssertEqual(
            evaluate(paused: true, payload: .text(" grocery list ")),
            .block(.paused)
        )
    }

    func testPasteboardMarkerBlocksTextAndImage() {
        XCTAssertEqual(
            evaluate(markedSensitive: true, payload: .text("pw")),
            .block(.pasteboardMarker)
        )
        XCTAssertEqual(
            evaluate(markedSensitive: true, payload: .image(png: Data([1]), semanticDigest: "d")),
            .block(.pasteboardMarker)
        )
    }

    func testOrdinaryTextIsAllowed() {
        XCTAssertEqual(evaluate(payload: .text("hello world")), .allow)
        XCTAssertEqual(
            evaluate(payload: .image(png: Data([1, 2]), semanticDigest: "d")),
            .allow
        )
    }

    func testBuiltinPemPrivateKeyBlockedAndPublicBlocksAllowed() {
        XCTAssertEqual(
            evaluate(payload: .text("-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----")),
            .block(.builtinRule("pem-private-key"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("-----BEGIN OPENSSH PRIVATE KEY-----\nabc")),
            .block(.builtinRule("pem-private-key"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("-----BEGIN PUBLIC KEY-----\nabc\n-----END PUBLIC KEY-----")),
            .allow
        )
        XCTAssertEqual(
            evaluate(payload: .text("-----BEGIN CERTIFICATE-----\nabc")),
            .allow
        )
    }

    func testBuiltinTokenPrefixesBlocked() {
        XCTAssertEqual(
            evaluate(payload: .text("otpauth://totp/Example:alice?secret=JBSW")),
            .block(.builtinRule("otpauth"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("token: ghp_16C7e42F292c6912E7710c838347Ae178B4a")),
            .block(.builtinRule("github-token"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG")),
            .block(.builtinRule("github-token"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("sk-proj4abcdefghij0123456789")),
            .block(.builtinRule("openai-key"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("sk-short")),
            .allow
        )
        XCTAssertEqual(
            evaluate(payload: .text("AKIAIOSFODNN7EXAMPLE")),
            .block(.builtinRule("aws-key"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("AKIAIOSFODNN7EXAMPL")),
            .allow
        )
        XCTAssertEqual(
            evaluate(payload: .text("AIzaSyA1bC2dE3fG4hI5jK6lM7nO8pQ9rS0tU1v")),
            .block(.builtinRule("google-api-key"))
        )
        XCTAssertEqual(
            evaluate(payload: .text("xoxb-1234567890abcdefXYZ")),
            .block(.builtinRule("slack-token"))
        )
    }

    func testUserSubstringRuleBlocks() {
        XCTAssertEqual(
            evaluate(payload: .text("server password is hunter2!"), rules: [
                SensitiveRule(pattern: "hunter2", isRegex: false)
            ]),
            .block(.userRule(0))
        )
        XCTAssertEqual(
            evaluate(payload: .text("nothing here"), rules: [
                SensitiveRule(pattern: "hunter2", isRegex: false)
            ]),
            .allow
        )
    }

    func testUserRegexRuleBlocksAndInvalidRegexDoesNotCrash() {
        XCTAssertEqual(
            evaluate(payload: .text("password: trunk8-OK"), rules: [
                SensitiveRule(pattern: "trunk\\d-OK", isRegex: true)
            ]),
            .block(.userRule(0))
        )
        XCTAssertEqual(
            evaluate(payload: .text("password: trunk8-OK"), rules: [
                SensitiveRule(pattern: "trunk(\\d-OK", isRegex: true)
            ]),
            .allow
        )
    }

    func testIsValidRegexClassification() {
        XCTAssertTrue(SensitiveContentFilter.isValidRegex("trunk\\d-OK"))
        XCTAssertFalse(SensitiveContentFilter.isValidRegex("trunk(\\d-OK"))
        XCTAssertTrue(SensitiveContentFilter.isValidRegex(""))
    }

    func testEmptyUserPatternNeverMatches() {
        XCTAssertEqual(
            evaluate(payload: .text("anything"), rules: [SensitiveRule(pattern: "", isRegex: false)]),
            .allow
        )
    }

    func testUserRulesDoNotApplyToImages() {
        XCTAssertEqual(
            evaluate(payload: .image(png: Data([9]), semanticDigest: "d"), rules: [
                SensitiveRule(pattern: "anything", isRegex: false)
            ]),
            .allow
        )
    }
}
