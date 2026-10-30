// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import Foundation
import ArcaCore

/// Manages the locally persisted 36-byte Patron Passport record.
///
/// Under AGPLv3 section 7(b), the Guardian's Passport tracks statistics
/// (documents opened, trackers blocked, active content neutralized).
/// This store persists the exact 36-byte binary payload using iOS hardware
/// file protection (`.completeFileProtection`).
public actor PatronStore {
    private let fileURL: URL
    private var cachedRecord: Data?

    public init(fileManager: FileManager = .default) {
        let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? fileManager.temporaryDirectory
        let dir = appSupport.appendingPathComponent("Arca", isDirectory: true)
        try? fileManager.createDirectory(at: dir, withIntermediateDirectories: true)
        self.fileURL = dir.appendingPathComponent("patron_record.bin")
    }

    /// Loads the current 36-byte record, or returns an empty `Data` for a fresh start.
    public func loadRecord() -> Data {
        if let cached = cachedRecord {
            return cached
        }
        guard let data = try? Data(contentsOf: fileURL), data.count == 36 else {
            return Data()
        }
        cachedRecord = data
        return data
    }

    /// Updates the record after opening a document and saves it securely to disk.
    public func recordDocumentOpened(trackers: Int, active: Int) {
        let current = loadRecord()
        let today = Int32(Date().timeIntervalSince1970 / 86400.0)

        var buf = ArcaBuffer(data: nil, len: 0)
        let status = current.withUnsafeBytes { ptr -> Int32 in
            let base = ptr.baseAddress?.assumingMemoryBound(to: UInt8.self)
            return arca_passport_update(base, ptr.count, today, Int32(trackers), Int32(active), &buf)
        }

        guard status == ArcaStatusCode.ok, let dataPtr = buf.data, buf.len == 36 else {
            arca_buffer_free(buf)
            return
        }

        let updated = Data(bytes: dataPtr, count: buf.len)
        arca_buffer_free(buf)

        cachedRecord = updated
        try? updated.write(to: fileURL, options: [.atomic, .completeFileProtection])
    }

    /// Resets the local patron statistics.
    public func reset() {
        cachedRecord = nil
        try? FileManager.default.removeItem(at: fileURL)
    }
}
