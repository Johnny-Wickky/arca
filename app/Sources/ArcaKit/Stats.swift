// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import Foundation
import ArcaCore

// Thin bindings of the platform-neutral statistics and reminder rules of the Rust core
// (arca_engine::stats and arca_engine::schedule). They hold no state and call no network:
// the app keeps the opaque statistics blob in its own protected storage and passes the local
// calendar day (days since 1970-01-01 in the LOCAL time zone) with every call. These files are
// not built in this repository's CI; they mirror what the Android app does through JNI.

/// One window of the statistics (week, month or all time).
public struct StatsTotals: Codable, Sendable, Equatable {
    public let opened: Int64
    public let clean: Int64
    public let trackers: Int64
    public let active: Int64
    public let threats: Int64
    public let rejected: Int64
    public let unverified: Int64
    public let sterile: Int64
    public let pictures: Int64
}

/// The summary of `arca_stats_summary`.
public struct StatsSummary: Codable, Sendable, Equatable {
    public let version: Int
    public let empty: Bool
    public let firstDay: Int64
    public let days: Int64
    public let week: StatsTotals
    public let month: StatsTotals
    public let all: StatsTotals
}

/// What happened to a scanned document, as the wire tag of the X-ray verdict.
public enum StatsVerdict: Int32, Sendable {
    case clean = 0
    case hiddenTrackers = 1
    case activeContentNeutralized = 2
    case rejected = 3
    case unverified = 4
}

/// The local statistics. Every function takes the stored blob (empty = nothing yet) and returns
/// the new blob; the caller stores it (and deletes it when the person turns statistics off).
public enum ArcaStats {
    /// Today's day number in the local calendar.
    public static func localDay(_ date: Date = Date(), calendar: Calendar = .current) -> Int32 {
        let offset = calendar.timeZone.secondsFromGMT(for: date)
        let days = ((date.timeIntervalSince1970 + Double(offset)) / 86400.0).rounded(.down)
        return Int32(clamping: Int(days))
    }

    public static func recordScan(
        blob: Data, day: Int32, verdict: StatsVerdict, trackers: Int32, active: Int32
    ) throws -> Data {
        try call(blob) { base, len, out in
            arca_stats_record_scan(base, len, day, verdict.rawValue, trackers, active, out)
        }
    }

    /// `event` 1 = a sterile copy was made, 2 = a picture was opened.
    public static func recordEvent(blob: Data, day: Int32, event: Int32) throws -> Data {
        try call(blob) { base, len, out in arca_stats_record_event(base, len, day, event, out) }
    }

    public static func summary(blob: Data, today: Int32) throws -> StatsSummary {
        let json = try call(blob) { base, len, out in arca_stats_summary(base, len, today, out) }
        return try JSONDecoder().decode(StatsSummary.self, from: json)
    }

    /// Folds the old 36-byte patron record (or the two old plain counters) into the all-time
    /// total. Call once, then delete the old data.
    public static func foldLegacy(
        blob: Data, passport: Data, documents: Int64, threats: Int64, today: Int32
    ) throws -> Data {
        try call(blob) { base, len, out in
            passport.withUnsafeBytes { p in
                arca_stats_fold_legacy(
                    base, len, p.baseAddress?.assumingMemoryBound(to: UInt8.self), p.count,
                    documents, threats, today, out)
            }
        }
    }

    public static func passport(blob: Data, today: Int32, isRussian: Bool) throws -> PassportInfo {
        let json = try call(blob) { base, len, out in
            arca_stats_passport(isRussian ? 0 : 1, base, len, today, out)
        }
        return try JSONDecoder().decode(PassportInfo.self, from: json)
    }

    private static func call(
        _ blob: Data,
        _ body: (UnsafePointer<UInt8>?, Int, UnsafeMutablePointer<ArcaBuffer>) -> Int32
    ) throws -> Data {
        var buf = ArcaBuffer(data: nil, len: 0)
        let status = blob.withUnsafeBytes { raw in
            body(raw.baseAddress?.assumingMemoryBound(to: UInt8.self), raw.count, &buf)
        }
        defer { arca_buffer_free(buf) }
        guard status == ARCA_STATUS_OK.rawValue else { throw ArcaError(code: status) }
        guard let ptr = buf.data, buf.len > 0 else { return Data() }
        return Data(bytes: ptr, count: buf.len)
    }
}

/// What to do when the reminder fires or the app starts (the rules live in Rust).
public struct ReminderDecision: Codable, Sendable, Equatable {
    /// fire: `show`, `quietHours`, `tooSoon`, `noPermission`, `disabled`;
    /// start: `off`, `keep`, `plan`.
    public let action: String
    /// When to arm the next trigger (epoch milliseconds); 0 for `disabled` and `off`.
    public let at: Int64
}

/// The reminder rules. The app only owns the operating-system calls (a local notification
/// trigger, posting the notification); "when" and "whether" are decided here.
public enum ArcaReminderRules {
    public static func plan(now: Date, calendar: Calendar = .current) -> Date {
        let ms = Int64(now.timeIntervalSince1970 * 1000)
        let offset = Int32(clamping: calendar.timeZone.secondsFromGMT(for: now) * 1000)
        let rnd = Int32.random(in: Int32.min...Int32.max)
        return Date(timeIntervalSince1970: Double(arca_reminder_plan(ms, offset, rnd)) / 1000)
    }

    public static func onFire(
        now: Date, lastShown: Date?, enabled: Bool, canPost: Bool, calendar: Calendar = .current
    ) throws -> ReminderDecision {
        var buf = ArcaBuffer(data: nil, len: 0)
        let ms = Int64(now.timeIntervalSince1970 * 1000)
        let last = lastShown.map { Int64($0.timeIntervalSince1970 * 1000) } ?? 0
        let offset = Int32(clamping: calendar.timeZone.secondsFromGMT(for: now) * 1000)
        let status = arca_reminder_on_fire(
            ms, offset, last, enabled ? 1 : 0, canPost ? 1 : 0,
            Int32.random(in: Int32.min...Int32.max), &buf)
        defer { arca_buffer_free(buf) }
        guard status == ARCA_STATUS_OK.rawValue, let ptr = buf.data else {
            throw ArcaError(code: status)
        }
        return try JSONDecoder().decode(ReminderDecision.self, from: Data(bytes: ptr, count: buf.len))
    }

    public static func onStart(
        stored: Date?, now: Date, enabled: Bool, calendar: Calendar = .current
    ) throws -> ReminderDecision {
        var buf = ArcaBuffer(data: nil, len: 0)
        let ms = Int64(now.timeIntervalSince1970 * 1000)
        let next = stored.map { Int64($0.timeIntervalSince1970 * 1000) } ?? 0
        let offset = Int32(clamping: calendar.timeZone.secondsFromGMT(for: now) * 1000)
        let status = arca_reminder_on_start(
            next, ms, offset, enabled ? 1 : 0, Int32.random(in: Int32.min...Int32.max), &buf)
        defer { arca_buffer_free(buf) }
        guard status == ARCA_STATUS_OK.rawValue, let ptr = buf.data else {
            throw ArcaError(code: status)
        }
        return try JSONDecoder().decode(ReminderDecision.self, from: Data(bytes: ptr, count: buf.len))
    }

    /// `index | (uses << 8)`; see `arca_reminder_pick`.
    public static func pick(seq: Int32, documents: Int64, threats: Int64) -> Int32 {
        arca_reminder_pick(seq, documents, threats)
    }

    public static var itemCount: Int32 { arca_reminder_item_count() }
}
