// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

/**
 * Keeps the opaque counter blob of the Rust core in private storage, and nothing else: the app
 * never learns, stores or sends anything about a document beyond the flat totals inside the blob.
 * The counter ("Passport") is OFF unless the person switches it on, so a fresh install leaves no
 * trace of use. The class holds the rules around the blob:
 *
 * <ul>
 *   <li>while the counter is off nothing is recorded, and any blob found in storage is deleted;
 *   <li>switching it off or resetting it deletes the blob at once;
 *   <li>a failing core never breaks the caller: recording is best effort.
 * </ul>
 *
 * There is no day, time or history anywhere in this class. It has no Android dependencies so that
 * a desktop JVM can test it; {@code Storage} is implemented over SharedPreferences and
 * {@code Codec} over the native library.
 */
public final class StatsStore {
    /** The private storage. */
    public interface Storage {
        /** The stored blob, empty if none. */
        byte[] readBlob();

        void writeBlob(byte[] blob);

        void deleteBlob();

        /** Whether the counter is switched on (default no). */
        boolean enabled();

        void setEnabled(boolean on);
    }

    /** The Rust core's blob functions. */
    public interface Codec {
        byte[] recordScan(byte[] blob, int verdict, int trackers, int active);

        byte[] recordEvent(byte[] blob, int event);

        byte[] summary(byte[] blob);

        byte[] passport(int lang, byte[] blob, int firstUseDay, int today);
    }

    /** Event codes of {@link #recordEvent}, as in the core. */
    public static final int EVENT_STERILE_COPY = 1;
    public static final int EVENT_PICTURE_OPENED = 2;

    /** The blob never legitimately exceeds this (the core's limit is 1 KiB). */
    static final int MAX_BLOB = 1024;

    private final Storage storage;
    private final Codec codec;

    public StatsStore(Storage storage, Codec codec) {
        this.storage = storage;
        this.codec = codec;
    }

    public boolean enabled() {
        return storage.enabled();
    }

    /** Deletes a blob that is stored while the counter is off (left by an older version). Called at start. */
    public synchronized void purgeIfOff() {
        if (!storage.enabled()) {
            storage.deleteBlob();
        }
    }

    /** Switches the counter on or off. Off deletes the blob at once. */
    public synchronized void setEnabled(boolean on) {
        storage.setEnabled(on);
        if (!on) {
            storage.deleteBlob();
        }
    }

    /** Forgets everything that was counted (the switch stays as it is). */
    public synchronized void reset() {
        storage.deleteBlob();
    }

    /** Counts one scanned document. {@code verdict} is the core's verdict tag (0 clean ... 4 unverified). */
    public synchronized void recordScan(int verdict, long trackers, long active) {
        if (!storage.enabled()) {
            return;
        }
        try {
            store(codec.recordScan(storage.readBlob(), verdict, clampInt(trackers), clampInt(active)));
        } catch (RuntimeException | LinkageError ignored) {
            // the counter is a courtesy; never let it break a scan
        }
    }

    /** Counts a sterile copy or an opened picture. */
    public synchronized void recordEvent(int event) {
        if (!storage.enabled()) {
            return;
        }
        try {
            store(codec.recordEvent(storage.readBlob(), event));
        } catch (RuntimeException | LinkageError ignored) {
            // best effort
        }
    }

    /** The totals; empty when the counter is off or the core is unavailable. */
    public synchronized StatsView.Summary summary() {
        if (!storage.enabled()) {
            return StatsView.empty();
        }
        try {
            return StatsView.parse(codec.summary(storage.readBlob()));
        } catch (RuntimeException | LinkageError e) {
            return StatsView.empty();
        }
    }

    /** The Patron Passport JSON built from the totals; {@code null} when the counter is off or unavailable. */
    public synchronized byte[] passportJson(int lang, int firstUseDay, int today) {
        if (!storage.enabled()) {
            return null;
        }
        try {
            return codec.passport(lang, storage.readBlob(), firstUseDay, today);
        } catch (RuntimeException | LinkageError e) {
            return null;
        }
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
