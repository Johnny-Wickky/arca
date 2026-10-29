// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import Foundation
import ArcaCore

// Thin bindings of the platform-neutral statistics and reminder rules of the Rust core
// (arca_engine::stats and arca_engine::schedule). They hold no state and call no network:
// the app keeps the opaque counter blob (flat all-time totals, no dates) in its own protected
// storage, only while the person has switched the optional counter on. These files are not built
// in this repository's CI; they mirror what the Android app does through JNI.

/// The flat totals of the optional counter.
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

/// The optional local counter. Every function takes the stored blob (empty = nothing yet) and
/// returns the new blob; the caller stores it (and deletes it when the person turns the counter
/// off or zeroes it).
public enum ArcaStats {
    /// Today's day number in the local calendar (for the passport's "days with Arca" line only).
    public static func localDay(_ date: Date = Date(), calendar: Calendar = .current) -> Int32 {
        let offset = calendar.timeZone.secondsFromGMT(for: date)
        let days = ((date.timeIntervalSince1970 + Double(offset)) / 86400.0).rounded(.down)
        return Int32(clamping: Int(days))
    }

    public static func recordScan(
        blob: Data, verdict: StatsVerdict, trackers: Int32, active: Int32
    ) throws -> Data {
        try call(blob) { base, len, out in
            arca_stats_record_scan(base, len, verdict.rawValue, trackers, active, out)
        }
    }

    /// `event` 1 = a sterile copy was made, 2 = a picture was opened.
    public static func recordEvent(blob: Data, event: Int32) throws -> Data {
        try call(blob) { base, len, out in arca_stats_record_event(base, len, event, out) }
    }

    public static func summary(blob: Data) throws -> StatsSummary {
        let json = try call(blob) { base, len, out in arca_stats_summary(base, len, out) }
        return try JSONDecoder().decode(StatsSummary.self, from: json)
    }

    public static func passport(
        blob: Data, firstUseDay: Int32, today: Int32, isRussian: Bool
    ) throws -> PassportInfo {
        let json = try call(blob) { base, len, out in
            arca_stats_passport(isRussian ? 0 : 1, base, len, firstUseDay, today, out)
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

    /// The index of the message for the stored position; see `arca_reminder_pick`.
    public static func pick(seq: Int32) -> Int32 {
        arca_reminder_pick(seq)
    }

    public static var itemCount: Int32 { arca_reminder_item_count() }
}

/// The person's switch for the weekly support card ("Weekly support reminders" / "Еженедельные
/// напоминания о поддержке"). On until they turn it off; "About and support" stays reachable.
public enum SupportReminderSetting {
    public static let key = "arca.weeklySupportReminders"

    /// The stored choice, or `true` (the Rust default, `reminder::ENABLED_BY_DEFAULT`) if none.
    public static func isEnabled(_ defaults: UserDefaults = .standard) -> Bool {
        defaults.object(forKey: key) == nil ? true : defaults.bool(forKey: key)
    }

    public static func set(_ on: Bool, _ defaults: UserDefaults = .standard) {
        defaults.set(on, forKey: key)
    }
}
