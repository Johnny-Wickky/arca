// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.content.Context;

import org.arca.viewer.core.Fmt;
import org.arca.viewer.core.Model;
import org.arca.viewer.core.StatsStore;
import org.arca.viewer.core.StatsView;

import java.util.TimeZone;

/**
 * The local usage statistics of this device. What is counted is decided by the Rust core
 * ({@code arca_engine::stats}); this class gives it the day (the local calendar, the core has no
 * clock) and keeps the opaque blob in private storage through {@link Prefs}. Nothing about a
 * document is recorded and nothing is sent anywhere.
 */
public final class Stats {
    private Stats() {}

    private static volatile StatsStore store;

    private static StatsStore store(Context c) {
        StatsStore s = store;
        if (s == null) {
            synchronized (Stats.class) {
                s = store;
                if (s == null) {
                    s = new StatsStore(Prefs.statsStorage(c), new NativeCodec());
                    store = s;
                }
            }
        }
        return s;
    }

    /** The blob functions of the native library. */
    private static final class NativeCodec implements StatsStore.Codec {
        @Override
        public byte[] recordScan(byte[] blob, int day, int verdict, int trackers, int active) {
            return Native.statsRecordScan(blob, day, verdict, trackers, active);
        }

        @Override
        public byte[] recordEvent(byte[] blob, int day, int event) {
            return Native.statsRecordEvent(blob, day, event);
        }

        @Override
        public byte[] summary(byte[] blob, int today) {
            return Native.statsSummary(blob, today);
        }

        @Override
        public byte[] foldLegacy(byte[] blob, byte[] passport, long documents, long threats, int today) {
            return Native.statsFoldLegacy(blob, passport, documents, threats, today);
        }

        @Override
        public byte[] passport(int lang, byte[] blob, int today) {
            return Native.statsPassport(lang, blob, today);
        }
    }

    /** Today's day number in the device's local calendar. */
    public static int today() {
        long now = System.currentTimeMillis();
        return Fmt.epochDay(now, TimeZone.getDefault().getOffset(now));
    }

    /** Counts a scanned document by its verdict and the engine's counts of findings. */
    public static void recordScan(Context c, Model.Report r) {
        store(c).recordScan(today(), StatsView.verdictTag(r.verdict), r.trackers, r.active);
    }

    /** Counts a sterile copy that was written. */
    public static void recordSterileCopy(Context c) {
        store(c).recordEvent(today(), StatsStore.EVENT_STERILE_COPY);
    }

    /** Counts a picture the reader opened. */
    public static void recordPicture(Context c) {
        store(c).recordEvent(today(), StatsStore.EVENT_PICTURE_OPENED);
    }

    /** Week, month and all-time totals (empty when statistics are off). */
    public static StatsView.Summary summary(Context c) {
        return store(c).summary(today());
    }

    /** The Patron Passport JSON from the all-time totals, {@code null} when statistics are off. */
    public static byte[] passportJson(Context c) {
        return store(c).passportJson(Prefs.engineLang(c), today());
    }

    public static boolean enabled(Context c) {
        return store(c).enabled();
    }

    /** Switches statistics on or off; off deletes everything that was counted. */
    public static void setEnabled(Context c, boolean on) {
        store(c).setEnabled(on);
    }

    /** Forgets everything that was counted. */
    public static void reset(Context c) {
        store(c).reset();
    }
}
