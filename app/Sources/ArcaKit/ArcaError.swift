// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import Foundation
import ArcaCore

/// Strongly-typed error states emitted by the Arca security engine.
///
/// Mirrors the core `ArcaStatus` / `arca_engine::Status` enum.
public enum ArcaError: Int32, Error, Equatable, Sendable, LocalizedError {
    /// Operation succeeded (not an error).
    case ok = 0
    /// The user explicitly cancelled the ongoing operation.
    case cancelled = 1
    /// Wall-clock timeout exceeded.
    case timeout = 2
    /// Input document exceeds the device or configured limit.
    case tooLarge = 3
    /// Document is hostile, malformed, or rejected by sandbox policy.
    case rejected = 4
    /// Requested operation is unsupported for this format.
    case unsupported = 5
    /// Document is encrypted and requires a password.
    case encrypted = 6
    /// Operation failed after acceptance (e.g. sanitization verification failed).
    case failed = 7
    /// WebAssembly guest exhausted its CPU fuel, stack, or memory ceiling.
    case resourceLimit = 8
    /// Internal engine or sandbox fault.
    case fault = 9
    /// Handle does not correspond to an active document, job, or outcome.
    case badHandle = 10
    /// I/O error reading from or writing to the filesystem or stream.
    case io = 11
    /// Invalid argument supplied to the engine.
    case badArgument = 12
    /// Engine has not been initialized.
    case notReady = 13
    /// Provided password failed to decrypt the document.
    case wrongPassword = 14
    /// Unknown error status code.
    case unknown = -1

    /// Creates an `ArcaError` from a raw status integer.
    public init(code: Int32) {
        if let err = ArcaError(rawValue: abs(code)) {
            self = err
        } else {
            self = .unknown
        }
    }

    /// Creates an `ArcaError` from an `int64_t` handle or negative status return.
    public init(handleOrCode: Int64) {
        if handleOrCode < 0 {
            self.init(code: Int32(-handleOrCode))
        } else {
            self = .ok
        }
    }

    /// Human-readable localized description.
    public var errorDescription: String? {
        switch self {
        case .ok:
            return "Operation successful"
        case .cancelled:
            return "Operation was cancelled by the user"
        case .timeout:
            return "Processing timed out"
        case .tooLarge:
            return "Document exceeds size limit"
        case .rejected:
            return "Document rejected as hostile or corrupted"
        case .unsupported:
            return "Unsupported document format"
        case .encrypted:
            return "Document is password protected"
        case .failed:
            return "Operation failed during processing"
        case .resourceLimit:
            return "Resource limit exceeded (possible decompression bomb)"
        case .fault:
            return "Internal sandbox fault"
        case .badHandle:
            return "Invalid or expired document session"
        case .io:
            return "File access error"
        case .badArgument:
            return "Invalid argument"
        case .notReady:
            return "Security engine is not ready"
        case .wrongPassword:
            return "Incorrect password"
        case .unknown:
            return "Unknown security engine error"
        }
    }
}
