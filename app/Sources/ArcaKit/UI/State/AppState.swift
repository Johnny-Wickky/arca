// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import SwiftUI
import Combine

/// High-level navigation state of the application.
public enum AppScreen: Sendable, Equatable {
    case idle
    case scanning(fileName: String)
    case report(report: Report, diagnostics: RunDiagnostics)
    case viewer(report: Report, meta: PreviewMeta)
    case about
}

/// Central state manager driving the entire iOS user interface.
@MainActor
public final class AppState: ObservableObject {
    @Published public var currentScreen: AppScreen = .idle
    @Published public var theme: ArcaTheme = .obsidian
    @Published public var isPasswordPromptVisible: Bool = false
    @Published public var passwordInput: String = ""
    @Published public var isSanitizeInProgress: Bool = false
    @Published public var sanitizedDocument: SanitizedDocument?
    @Published public var inspectedMacro: MacroModule?
    @Published public var inspectedImage: DecodedImage?
    @Published public var activeError: ArcaError?

    public let engine: ArcaEngine
    public let patronStore: PatronStore

    private var activeDocument: DocumentSession?
    private var activeDocumentName: String = ""
    private var pendingDocumentData: Data?

    public init(engine: ArcaEngine = .shared, patronStore: PatronStore = PatronStore()) {
        self.engine = engine
        self.patronStore = patronStore
    }

    // MARK: - Document Intake

    /// Opens and scans a document from a file URL.
    public func openDocument(from url: URL) {
        let name = url.lastPathComponent
        currentScreen = .scanning(fileName: name)
        activeDocumentName = name

        Task {
            do {
                let session = try await engine.openDocument(at: url)
                self.activeDocument = session
                try await startScan(session: session)
            } catch let err as ArcaError {
                handleError(err)
            } catch {
                handleError(.fault)
            }
        }
    }

    /// Opens and scans a document from in-memory Data.
    public func openDocument(data: Data, name: String) {
        currentScreen = .scanning(fileName: name)
        activeDocumentName = name
        pendingDocumentData = data

        Task {
            do {
                let session = try await engine.openDocument(data: data)
                self.activeDocument = session
                try await startScan(session: session)
            } catch let err as ArcaError {
                handleError(err)
            } catch {
                handleError(.fault)
            }
        }
    }

    // MARK: - Scanning & Unlocking

    private func startScan(session: DocumentSession, password: String? = nil) async throws {
        do {
            let result = try await engine.scan(document: session, password: password)
            ArcaHaptics.shieldVerdict(result.report.verdict)

            // Update patron passport statistics
            await patronStore.recordDocumentOpened(
                trackers: Int(result.report.trackers),
                active: Int(result.report.active)
            )

            currentScreen = .report(report: result.report, diagnostics: result.diagnostics)
        } catch ArcaError.encrypted, ArcaError.wrongPassword {
            isPasswordPromptVisible = true
        }
    }

    /// Submits a password for an encrypted document.
    public func submitPassword() {
        guard let session = activeDocument, !passwordInput.isEmpty else { return }
        let pw = passwordInput
        passwordInput = ""
        isPasswordPromptVisible = false

        currentScreen = .scanning(fileName: activeDocumentName)
        Task {
            do {
                try await startScan(session: session, password: pw)
            } catch let err as ArcaError {
                handleError(err)
            } catch {
                handleError(.fault)
            }
        }
    }

    // MARK: - Viewer Navigation

    /// Transitions to the safe read-only preview viewer.
    public func proceedToViewer(report: Report) {
        guard let session = activeDocument else { return }
        Task {
            do {
                let meta = try await engine.previewMeta(document: session)
                currentScreen = .viewer(report: report, meta: meta)
                ArcaHaptics.selection()
            } catch let err as ArcaError {
                handleError(err)
            } catch {
                handleError(.fault)
            }
        }
    }

    // MARK: - One-Tap Sanitize

    /// Triggers one-tap sterilization of the currently active document.
    public func sanitizeCurrentDocument() {
        guard let session = activeDocument else { return }
        isSanitizeInProgress = true

        Task {
            do {
                let cleanDoc = try await engine.sanitize(document: session)
                self.sanitizedDocument = cleanDoc
                self.isSanitizeInProgress = false
                ArcaHaptics.sanitizeSuccess()
            } catch let err as ArcaError {
                self.isSanitizeInProgress = false
                handleError(err)
            } catch {
                self.isSanitizeInProgress = false
                handleError(.fault)
            }
        }
    }

    // MARK: - Image Inspection

    /// Fetches and opens an embedded sandboxed image raster in full screen.
    public func inspectImage(imageId: Int) {
        guard let session = activeDocument else { return }
        Task {
            do {
                let decoded = try await engine.decodeImage(document: session, imageId: imageId)
                self.inspectedImage = decoded
                ArcaHaptics.selection()
            } catch let err as ArcaError {
                handleError(err)
            } catch {
                handleError(.fault)
            }
        }
    }

    // MARK: - Close / Reset

    /// Closes the current document, wipes session memory, and returns to the home screen.
    public func closeDocument() {
        activeDocument?.close()
        activeDocument = nil
        activeDocumentName = ""
        pendingDocumentData = nil
        sanitizedDocument = nil
        inspectedImage = nil
        inspectedMacro = nil
        currentScreen = .idle
        ArcaHaptics.selection()
    }

    private func handleError(_ error: ArcaError) {
        self.activeError = error
        if case .scanning = currentScreen {
            currentScreen = .idle
        }
    }
}
