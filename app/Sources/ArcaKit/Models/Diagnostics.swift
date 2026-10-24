// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import Foundation

/// Technical diagnostics and resource utilization metrics for an executed run.
public struct RunDiagnostics: Codable, Sendable, Equatable {
    public let status: Int
    public let statusName: String
    public let message: String
    public let fuel: Int64
    public let fuelLimit: Int64
    public let peakMemory: Int64
    public let slices: Int64
    public let wallMs: Int64
    /// 0=not encrypted, 1=unlocked with password, 2=unlocked without password.
    public let unlock: Int

    private enum CodingKeys: String, CodingKey {
        case status, statusName, message, unlock, stats
    }

    private struct Stats: Codable {
        let fuel: Int64
        let fuelLimit: Int64
        let peakMemory: Int64
        let slices: Int64
        let wallMs: Int64
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        status = try c.decodeIfPresent(Int.self, forKey: .status) ?? 0
        statusName = try c.decodeIfPresent(String.self, forKey: .statusName) ?? ""
        message = try c.decodeIfPresent(String.self, forKey: .message) ?? ""
        unlock = try c.decodeIfPresent(Int.self, forKey: .unlock) ?? 0

        let st = try c.decodeIfPresent(Stats.self, forKey: .stats)
        fuel = st?.fuel ?? 0
        fuelLimit = st?.fuelLimit ?? 0
        peakMemory = st?.peakMemory ?? 0
        slices = st?.slices ?? 0
        wallMs = st?.wallMs ?? 0
    }
}

/// One step in the engine self-test suite.
public struct SelfTestStep: Codable, Sendable, Equatable, Identifiable {
    public var id: String { name }

    public let name: String
    public let isOk: Bool
    public let durationMs: Int64
    public let detail: String

    enum CodingKeys: String, CodingKey {
        case name
        case isOk = "ok"
        case durationMs = "ms"
        case detail
    }
}

/// Complete diagnostic report of the built-in self-test run.
public struct SelfTestReport: Codable, Sendable, Equatable {
    public let isOk: Bool
    public let guestSha256: String
    public let steps: [SelfTestStep]

    enum CodingKeys: String, CodingKey {
        case isOk = "ok"
        case guestSha256
        case steps
    }
}

/// Static facts about the engine build and host platform.
public struct EngineFacts: Codable, Sendable, Equatable {
    public let engine: String
    public let abi: Int
    public let guestSha256: String
    public let arch: String
    public let os: String
    public let appName: String
    public let maxInputBytes: Int64
    public let maxMemoryBytes: Int64
}

/// Camera EXIF metadata extracted safely by the parser (never passed to iOS CoreImage).
public struct ExifFacts: Codable, Sendable, Equatable {
    public let make: String
    public let model: String
    public let software: String
    public let datetime: String
    public let artist: String
    public let copyright: String
    public let owner: String
    public let orientation: Int
    public let hasSerial: Bool
    public let hasMakerNote: Bool
    public let hasThumbnail: Bool
    public let hasGps: Bool
    public let position: String?

    enum CodingKeys: String, CodingKey {
        case make, model, software, datetime, artist, copyright, owner, orientation
        case hasSerial = "serial"
        case hasMakerNote = "makerNote"
        case hasThumbnail = "thumbnail"
        case hasGps = "gps"
        case position
    }
}

/// Metadata describing an image rasterized by the sandbox.
public struct ImageFacts: Codable, Sendable, Equatable {
    public let format: String
    public let width: Int
    public let height: Int
    public let srcWidth: Int
    public let srcHeight: Int
    public let bits: Int
    public let flags: Int
    public let frames: Int
    public let trailingBytes: Int64
    public let metadataBytes: Int64
    public let exif: ExifFacts?

    enum CodingKeys: String, CodingKey {
        case format, width, height, srcWidth, srcHeight, bits, flags, frames
        case trailingBytes = "trailing"
        case metadataBytes
        case exif
    }
}

/// Fully decoded, sandboxed image raster (premultiplied RGBA).
public struct DecodedImage: Sendable {
    public let facts: ImageFacts
    /// Premultiplied RGBA pixels (4 bytes per pixel: [R, G, B, A]).
    public let rgbaPixels: Data

    public init(facts: ImageFacts, rgbaPixels: Data) {
        self.facts = facts
        self.rgbaPixels = rgbaPixels
    }
}
