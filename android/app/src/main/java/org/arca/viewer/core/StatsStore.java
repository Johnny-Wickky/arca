// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

/**
 * Keeps the opaque statistics blob of the Rust core in private storage, and nothing else: the
 * app never learns, stores or sends anything about a document beyond the counters inside the
 * blob. The class holds the rules around the blob:
 *
 * <ul>
 *   <li>statistics can be switched off; then nothing is recorded and the blob is deleted;
 *   <li>the older counters (the 36-byte Patron Passport record and the two plain counters) are
 *       folded into "all time" once and then deleted;
 *   <li>a failing core never breaks the caller: recording is best effort.
 * </ul>
 *
 * It has no Android dependencies so that a desktop JVM can test it; {@code Storage} is
 * implemented over SharedPreferences and {@code Codec} over the native library.
 */
public final class StatsStore {
    /** The private storage. */
    public interface Storage {
        /** The stored blob, empty if none. */
        byte[] readBlob();

        void writeBlob(byte[] blob);

        void deleteBlob();

        /** Whether statistics are switched on (default yes). */
        boolean enabled();

        void setEnabled(boolean on);

        /** Whether any older counter is still stored. */
        boolean hasLegacy();

        /** The old 36-byte record, empty if none. */
        byte[] legacyPassport();

        long legacyDocuments();

        long legacyThreats();

        void deleteLegacy();
    }

    /** The Rust core's blob functions. */
    public interface Codec {
        byte[] recordScan(byte[] blob, int day, int verdict, int trackers, int active);

        byte[] recordEvent(byte[] blob, int day, int event);

        byte[] summary(byte[] blob, int today);

        byte[] foldLegacy(byte[] blob, byte[] passport, long documents, long threats, int today);

        byte[] passport(int lang, byte[] blob, int today);
    }

    /** Event codes of {@link #recordEvent}, as in the core. */
    public static final int EVENT_STERILE_COPY = 1;
    public static final int EVENT_PICTURE_OPENED = 2;

    /** The blob never legitimately exceeds this (the core's limit is 48 KiB). */
    static final int MAX_BLOB = 48 * 1024;

    private final Storage storage;
    private final Codec codec;

    public StatsStore(Storage storage, Codec codec) {
        this.storage = storage;
        this.codec = codec;
    }

    public boolean enabled() {
        return storage.enabled();
    }

    /** Switches statistics on or off. Off deletes the blob and every older counter at once. */
    public synchronized void setEnabled(boolean on) {
        storage.setEnabled(on);
        if (!on) {
            storage.deleteBlob();
            storage.deleteLegacy();
        }
    }

    /** Forgets everything that was counted (the switch stays as it is). */
    public synchronized void reset() {
        storage.deleteBlob();
        storage.deleteLegacy();
    }

    /** Counts one scanned document. {@code verdict} is the core's verdict tag (0 clean ... 4 unverified). */
    public synchronized void recordScan(int today, int verdict, long trackers, long active) {
        if (!storage.enabled()) {
            return;
        }
        try {
            byte[] blob = migrated(today);
            store(codec.recordScan(blob, today, verdict, clampInt(trackers), clampInt(active)));
        } catch (RuntimeException | LinkageError ignored) {
            // statistics are a courtesy; never let them break a scan
        }
    }

    /** Counts a sterile copy or an opened picture. */
    public synchronized void recordEvent(int today, int event) {
        if (!storage.enabled()) {
            return;
        }
        try {
            byte[] blob = migrated(today);
            store(codec.recordEvent(blob, today, event));
        } catch (RuntimeException | LinkageError ignored) {
            // best effort
        }
    }

    /** The totals; empty when statistics are off or the core is unavailable. */
    public synchronized StatsView.Summary summary(int today) {
        if (!storage.enabled()) {
            return StatsView.empty();
        }
        try {
            byte[] blob = migrated(today);
            return StatsView.parse(codec.summary(blob, today));
        } catch (RuntimeException | LinkageError e) {
            return StatsView.empty();
        }
    }

    /** The Patron Passport JSON built from the all-time totals; {@code null} when statistics are off or unavailable. */
    public synchronized byte[] passportJson(int lang, int today) {
        if (!storage.enabled()) {
            return null;
        }
        try {
            byte[] blob = migrated(today);
            return codec.passport(lang, blob, today);
        } catch (RuntimeException | LinkageError e) {
            return null;
        }
    }

    /**
     * The blob to continue from, after the older counters (if any) were folded in and deleted.
     * When the fold fails they stay for the next attempt.
     */
    private byte[] migrated(int today) {
        byte[] blob = storage.readBlob();
        if (!storage.hasLegacy()) {
            return blob;
        }
        byte[] folded = codec.foldLegacy(blob, storage.legacyPassport(), storage.legacyDocuments(),
            storage.legacyThreats(), today);
        if (folded != null && folded.length > 0 && folded.length <= MAX_BLOB) {
            storage.writeBlob(folded);
            storage.deleteLegacy();
            return folded;
        }
        return blob;
    }

    private void store(byte[] next) {
        if (next != null && next.length > 0 && next.length <= MAX_BLOB) {
            storage.writeBlob(next);
        }
    }

    private static int clampInt(long v) {
        return (int) Math.max(0L, Math.min(v, Integer.MAX_VALUE));
    }
}
