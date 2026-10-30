// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import XCTest
@testable import ArcaKit

/// The real Rust core and its Wasm guest (libarca_ffi, linked in by `tools/ios/check-ios.sh`)
/// running on an iOS Simulator: the first proof that the engine starts, scans, opens a protected file
/// and writes a sterile copy where the app will run. The documents are the repository's benign
/// fixtures (`tests/fixtures`), read from the host by path.
final class ArcaEngineTests: XCTestCase {

    /// `<repository>/tests/fixtures`: this file is `<repository>/app/Tests/ArcaKitTests/ArcaEngineTests.swift`.
    private static let fixtures: URL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()  // ArcaKitTests
        .deletingLastPathComponent()  // Tests
        .deletingLastPathComponent()  // app
        .deletingLastPathComponent()  // repository root
        .appendingPathComponent("tests", isDirectory: true)
        .appendingPathComponent("fixtures", isDirectory: true)

    private func fixture(_ name: String) throws -> URL {
        let url = Self.fixtures.appendingPathComponent(name)
        try XCTSkipUnless(
            FileManager.default.fileExists(atPath: url.path),
            "fixture \(name) is not reachable from the test process at \(url.path)")
        return url
    }

    private func scan(_ name: String, password: String? = nil) async throws -> Report {
        let engine = ArcaEngine.shared
        let session = try await engine.openDocument(at: try fixture(name))
        defer { session.close() }
        return try await engine.scan(document: session, password: password).report
    }

    // MARK: - The engine itself

    func testEngineStartsWithTheEmbeddedGuest() async throws {
        let engine = ArcaEngine.shared
        try await engine.initialize()
        let hasGuest = await engine.hasGuest
        XCTAssertTrue(hasGuest, "libarca_ffi was built without the Wasm guest (ARCA_GUEST_WASM)")

        let facts = try await engine.engineInfo()
        XCTAssertFalse(facts.guestSha256.isEmpty)
        XCTAssertEqual(facts.guestSha256.count, 64)
        print("arca engine: abi=\(facts.abi) arch=\(facts.arch) os=\(facts.os) guest=\(facts.guestSha256)")
    }

    func testBuiltInSelfTestPasses() async throws {
        let report = try await ArcaEngine.shared.selfTest()
        for step in report.steps where !step.isOk {
            XCTFail("self-test step \(step.name) failed: \(step.detail)")
        }
        XCTAssertTrue(report.isOk)
        XCTAssertFalse(report.steps.isEmpty)
    }

    func testAboutScreenCarriesTheUpstreamCardFirst() async throws {
        // AGPLv3 section 7(b): the card of the authors of the original core is always the first.
        for russian in [true, false] {
            let about = try await ArcaEngine.shared.about(isRussian: russian)
            XCTAssertFalse(about.screen.cards.isEmpty)
            XCTAssertTrue(about.screen.cards[0].isUpstream, "russian=\(russian)")
        }
    }

    // MARK: - Documents

    func testOpensOfficeDocumentsAndNeverCallsThemRejected() async throws {
        for name in ["report.docx", "sheet.xlsx", "slides.pptx", "report.pdf", "report.rtf", "report.odt"] {
            let report = try await scan(name)
            XCTAssertNotEqual(report.verdict, .rejected, name)
            XCTAssertFalse(report.sha256.isEmpty, name)
            XCTAssertFalse(report.needsPassword, name)
        }
    }

    func testPreviewOfADocumentHasContent() async throws {
        let engine = ArcaEngine.shared
        let session = try await engine.openDocument(at: try fixture("report.docx"))
        defer { session.close() }
        let meta = try await engine.previewMeta(document: session)
        XCTAssertGreaterThan(meta.totalItems, 0)
        XCTAssertFalse(meta.sections.isEmpty)
        let items = try await engine.previewItems(document: session, section: 0, from: 0, count: 20)
        XCTAssertFalse(items.isEmpty)
    }

    func testEncryptedDocumentAsksForAPasswordAndOpensWithIt() async throws {
        let locked = try await scan("enc-agile.docx")
        XCTAssertTrue(locked.needsPassword, "the app must ask for a password, not show a report")

        let engine = ArcaEngine.shared
        let session = try await engine.openDocument(at: try fixture("enc-agile.docx"))
        defer { session.close() }
        // A wrong password does not open it: refused as `wrongPassword`, or the file is reported locked again.
        do {
            let wrong = try await engine.scan(document: session, password: "wrong").report
            XCTAssertTrue(wrong.needsPassword, "a wrong password must not open the file")
        } catch let error as ArcaError {
            XCTAssertEqual(error, .wrongPassword)
        }
        let opened = try await engine.scan(document: session, password: "пароль-123").report
        XCTAssertFalse(opened.needsPassword)
        XCTAssertEqual(opened.format, "Docx")
    }

    func testSterileCopyIsVerifiedClean() async throws {
        let engine = ArcaEngine.shared
        let session = try await engine.openDocument(at: try fixture("report.docx"))
        defer { session.close() }
        let copy = try await engine.sanitize(document: session)
        XCTAssertEqual(copy.info.verification.verdict, .clean)
        XCTAssertFalse(copy.bytes.isEmpty)
        // A new OOXML package: a zip.
        XCTAssertEqual(Array(copy.bytes.prefix(2)), [0x50, 0x4B])
    }

    func testAGarbageFileIsRefusedNotCrashed() async throws {
        let engine = ArcaEngine.shared
        let junk = Data((0..<4096).map { UInt8(truncatingIfNeeded: $0 &* 31 &+ 7) })
        let session = try await engine.openDocument(data: junk)
        defer { session.close() }
        do {
            let report = try await engine.scan(document: session).report
            XCTAssertNotEqual(report.verdict, .clean, "bytes that are no document are never reported clean")
        } catch is ArcaError {
            // Refused with a status: also right. What must not happen is a crash or a "clean".
        }
    }

    func testAClosedSessionIsABadHandleNotAFault() async throws {
        let engine = ArcaEngine.shared
        let session = try await engine.openDocument(data: Data("x".utf8))
        session.close()
        do {
            _ = try await engine.scan(document: session)
            XCTFail("a closed session cannot be scanned")
        } catch let error as ArcaError {
            XCTAssertEqual(error, .badHandle)
        }
    }
}
