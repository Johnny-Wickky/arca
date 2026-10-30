// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import Foundation
import os
import ArcaCore

/// Configuration limits applied to the isolated WebAssembly sandbox.
public struct EngineLimits: Sendable {
    /// Maximum allowed input document size in MiB.
    public let maxInputMiB: Int32
    /// Guest linear memory ceiling in MiB.
    public let maxMemoryMiB: Int32
    /// Wall-clock timeout for X-ray threat detection in seconds.
    public let scanTimeoutSecs: Int32
    /// Wall-clock timeout for preview generation in seconds.
    public let previewTimeoutSecs: Int32

    public init(
        maxInputMiB: Int32 = 256,
        maxMemoryMiB: Int32 = 768,
        scanTimeoutSecs: Int32 = 120,
        previewTimeoutSecs: Int32 = 300
    ) {
        self.maxInputMiB = maxInputMiB
        self.maxMemoryMiB = maxMemoryMiB
        self.scanTimeoutSecs = scanTimeoutSecs
        self.previewTimeoutSecs = previewTimeoutSecs
    }

    public static let `default` = EngineLimits()
}

/// An active document session managed by Arca.
///
/// Wraps an opaque 64-bit handle. Automatically releases and zeroizes the document
/// when deallocated (RAII).
public final class DocumentSession: Sendable {
    public let handle: Int64
    private let isClosed = OSAllocatedUnfairLock(initialState: false)

    init(handle: Int64) {
        self.handle = handle
    }

    deinit {
        close()
    }

    /// Releases the document handle and wipes its bytes immediately.
    public func close() {
        isClosed.withLock { closed in
            if !closed {
                closed = true
                arca_doc_free(handle)
            }
        }
    }
}

/// The main entry point to the Arca Zero-Trust Document Engine.
///
/// An actor that coordinates all interactions with the native Rust core,
/// ensuring thread safety, structured cancellation, and automatic memory zeroization.
public actor ArcaEngine {
    public static let shared = ArcaEngine()

    private var isInitialized = false

    public init() {}

    /// Initializes the engine with specified device limits.
    ///
    /// Idempotent: safe to call multiple times.
    public func initialize(limits: EngineLimits = .default) throws {
        if isInitialized || arca_is_init() != 0 {
            isInitialized = true
            return
        }
        let status = arca_init(
            limits.maxInputMiB,
            limits.maxMemoryMiB,
            limits.scanTimeoutSecs,
            limits.previewTimeoutSecs
        )
        guard status == ArcaStatusCode.ok else {
            throw ArcaError(code: status)
        }
        isInitialized = true
    }

    /// Returns true if the WebAssembly guest parser module was embedded at build time.
    public var hasGuest: Bool {
        arca_has_guest() != 0
    }

    // MARK: - Document Ingestion

    /// Ingests a document from an in-memory byte buffer.
    public func openDocument(data: Data, maxBytes: Int64 = 256 << 20) throws -> DocumentSession {
        try ensureInitialized()
        let docHandle = arca_doc_new(maxBytes)
        guard docHandle > 0 else {
            throw ArcaError(handleOrCode: docHandle)
        }

        let session = DocumentSession(handle: docHandle)
        let appendStatus = data.withUnsafeBytes { ptr -> Int32 in
            guard let base = ptr.baseAddress?.assumingMemoryBound(to: UInt8.self) else {
                return ArcaStatusCode.badArgument
            }
            return arca_doc_append(docHandle, base, ptr.count)
        }

        guard appendStatus == ArcaStatusCode.ok else {
            throw ArcaError(code: appendStatus)
        }
        return session
    }

    /// Ingests a document from a security-scoped or local filesystem URL.
    public func openDocument(at fileURL: URL, maxBytes: Int64 = 256 << 20) throws -> DocumentSession {
        try ensureInitialized()
        let docHandle = arca_doc_new(maxBytes)
        guard docHandle > 0 else {
            throw ArcaError(handleOrCode: docHandle)
        }

        let session = DocumentSession(handle: docHandle)
        let didStartAccessing = fileURL.startAccessingSecurityScopedResource()
        defer {
            if didStartAccessing {
                fileURL.stopAccessingSecurityScopedResource()
            }
        }

        let readCount = fileURL.path.withCString { cPath in
            arca_doc_read_file(docHandle, cPath)
        }

        guard readCount >= 0 else {
            throw ArcaError(handleOrCode: readCount)
        }
        return session
    }

    /// Ingests a document from a POSIX file descriptor, taking ownership and closing it.
    public func openDocument(fd: Int32, maxBytes: Int64 = 256 << 20) throws -> DocumentSession {
        try ensureInitialized()
        let docHandle = arca_doc_new(maxBytes)
        guard docHandle > 0 else {
            throw ArcaError(handleOrCode: docHandle)
        }

        let session = DocumentSession(handle: docHandle)
        let readCount = arca_doc_read_fd(docHandle, fd)
        guard readCount >= 0 else {
            throw ArcaError(handleOrCode: readCount)
        }
        return session
    }

    // MARK: - Threat X-Ray Scan

    /// Performs an X-ray threat inspection in the Wasm sandbox.
    public func scan(
        document: DocumentSession,
        password: String? = nil
    ) async throws -> (report: Report, diagnostics: RunDiagnostics) {
        try applyPassword(password, to: document)
        let outcome = try await runOperation(op: ARCA_OP_SCAN, flags: 0, document: document)
        defer { arca_outcome_free(outcome) }

        let reportData = try fetchBufferData { arca_outcome_report(outcome, $0) }
        let diagData = try fetchBufferData { arca_outcome_info(outcome, $0) }

        let decoder = JSONDecoder()
        let report = try decoder.decode(Report.self, from: reportData)
        let diag = try decoder.decode(RunDiagnostics.self, from: diagData)

        return (report, diag)
    }

    // MARK: - Safe Preview

    /// Fetches preview metadata (sections, notices, total items).
    public func previewMeta(
        document: DocumentSession,
        password: String? = nil
    ) async throws -> PreviewMeta {
        try applyPassword(password, to: document)
        let outcome = try await runOperation(op: ARCA_OP_PREVIEW, flags: ArcaFlag.previewAll, document: document)
        defer { arca_outcome_free(outcome) }

        let metaData = try fetchBufferData { arca_outcome_preview_meta(outcome, $0) }
        return try JSONDecoder().decode(PreviewMeta.self, from: metaData)
    }

    /// Fetches a paginated page of preview items.
    public func previewItems(
        document: DocumentSession,
        section: UInt32,
        from: UInt64,
        count: UInt32
    ) async throws -> [PreviewItem] {
        let outcome = try await runOperation(op: ARCA_OP_PREVIEW, flags: ArcaFlag.previewAll, document: document)
        defer { arca_outcome_free(outcome) }

        let itemsData = try fetchBufferData {
            arca_outcome_preview_items(outcome, section, from, count, $0)
        }

        guard let jsonArray = try JSONSerialization.jsonObject(with: itemsData) as? [[String: Any]] else {
            return []
        }
        return jsonArray.compactMap(PreviewItem.parse)
    }

    // MARK: - One-Tap Sanitize

    /// Produces a sterile copy of the document with active content and trackers removed.
    public func sanitize(
        document: DocumentSession,
        flags: Int32 = ArcaFlag.sanitizeAll,
        password: String? = nil
    ) async throws -> SanitizedDocument {
        try applyPassword(password, to: document)
        let outcome = try await runOperation(op: ARCA_OP_SANITIZE, flags: flags, document: document)
        defer { arca_outcome_free(outcome) }

        let infoData = try fetchBufferData { arca_outcome_sanitized_info(outcome, $0) }
        let info = try JSONDecoder().decode(SanitizedInfo.self, from: infoData)

        let cleanBytes = try fetchBufferData { arca_outcome_sanitized_bytes(outcome, $0) }
        return SanitizedDocument(info: info, bytes: cleanBytes)
    }

    // MARK: - Page Render (PDF/Vector Primitives)

    /// Renders vector drawing primitives of a single page as JSON.
    public func renderPage(document: DocumentSession, pageNumber: UInt32) async throws -> String {
        let flags = Int32(pageNumber & ArcaFlag.pageMask)
        let outcome = try await runOperation(op: ARCA_OP_RENDER, flags: flags, document: document)
        defer { arca_outcome_free(outcome) }

        let pageData = try fetchBufferData { arca_outcome_page(outcome, $0) }
        return String(decoding: pageData, as: UTF8.self)
    }

    // MARK: - Sandboxed Image Decoding

    /// Decodes an embedded image inside the memory-safe sandbox into RGBA pixels.
    public func decodeImage(document: DocumentSession, imageId: Int) async throws -> DecodedImage {
        let outcome = try await runOperation(op: ARCA_OP_IMAGE, flags: Int32(imageId), document: document)
        defer { arca_outcome_free(outcome) }

        let factsData = try fetchBufferData { arca_outcome_image_info(outcome, $0) }
        let facts = try JSONDecoder().decode(ImageFacts.self, from: factsData)

        let pixelsData = try fetchBufferData { arca_outcome_image_pixels(outcome, $0) }
        return DecodedImage(facts: facts, rgbaPixels: pixelsData)
    }

    // MARK: - Attribution & Diagnostics

    /// Generates the "About & Support" information per AGPLv3 section 7(b).
    public func about(isRussian: Bool = false) throws -> AboutInfo {
        try ensureInitialized()
        let data = try fetchBufferData { arca_about(isRussian ? 0 : 1, $0) }
        return try JSONDecoder().decode(AboutInfo.self, from: data)
    }

    /// Generates the Guardian's Passport information for the given 36-byte record.
    public func passport(record: Data, isRussian: Bool = false) throws -> PassportInfo {
        try ensureInitialized()
        let today = Int32(Date().timeIntervalSince1970 / 86400.0)
        let data = try record.withUnsafeBytes { ptr -> Data in
            let base = ptr.baseAddress?.assumingMemoryBound(to: UInt8.self)
            return try fetchBufferData {
                arca_passport(isRussian ? 0 : 1, base, ptr.count, today, $0)
            }
        }
        return try JSONDecoder().decode(PassportInfo.self, from: data)
    }

    /// Runs the built-in self-test diagnostics.
    public func selfTest() throws -> SelfTestReport {
        try ensureInitialized()
        let data = try fetchBufferData { arca_self_test($0) }
        return try JSONDecoder().decode(SelfTestReport.self, from: data)
    }

    /// Returns static facts about the engine build.
    public func engineInfo() throws -> EngineFacts {
        try ensureInitialized()
        let data = try fetchBufferData { arca_info($0) }
        return try JSONDecoder().decode(EngineFacts.self, from: data)
    }

    /// Retrieves verbatim license text strings.
    public func licenseText(which: Int32) throws -> String {
        let data = try fetchBufferData { arca_license_text(which, $0) }
        return String(decoding: data, as: UTF8.self)
    }

    // MARK: - Private Execution Helpers

    private func ensureInitialized() throws {
        if !isInitialized && arca_is_init() == 0 {
            try initialize()
        }
    }

    private func applyPassword(_ password: String?, to document: DocumentSession) throws {
        guard let pw = password, !pw.isEmpty else { return }
        let pwData = Data(pw.utf8)
        let status = pwData.withUnsafeBytes { ptr -> Int32 in
            guard let base = ptr.baseAddress?.assumingMemoryBound(to: UInt8.self) else { return 0 }
            return arca_doc_set_password(document.handle, base, ptr.count)
        }
        guard status == ArcaStatusCode.ok else {
            throw ArcaError(code: status)
        }
    }

    private func runOperation(
        op: ArcaOp,
        flags: Int32,
        document: DocumentSession
    ) async throws -> Int64 {
        try ensureInitialized()
        let jobHandle = arca_job_new()
        guard jobHandle > 0 else {
            throw ArcaError(handleOrCode: jobHandle)
        }

        defer { arca_job_free(jobHandle) }

        return try await withTaskCancellationHandler {
            let docHandle = document.handle
            let opRaw = op.rawValue
            return try await Task.detached(priority: .userInitiated) {
                let outcome = arca_run(jobHandle, docHandle, Int32(opRaw), flags)
                guard outcome > 0 else {
                    throw ArcaError(handleOrCode: outcome)
                }
                let outcomeStatus = arca_outcome_status(outcome)
                guard outcomeStatus == ArcaStatusCode.ok else {
                    arca_outcome_free(outcome)
                    throw ArcaError(code: outcomeStatus)
                }
                return outcome
            }.value
        } onCancel: {
            arca_job_cancel(jobHandle)
        }
    }

    private func fetchBufferData(
        _ call: (UnsafeMutablePointer<ArcaBuffer>) -> Int32
    ) throws -> Data {
        var buf = ArcaBuffer(data: nil, len: 0)
        let status = call(&buf)
        defer { arca_buffer_free(buf) }

        guard status == ArcaStatusCode.ok else {
            throw ArcaError(code: status)
        }
        guard let ptr = buf.data, buf.len > 0 else {
            return Data()
        }
        return Data(bytes: ptr, count: buf.len)
    }
}
