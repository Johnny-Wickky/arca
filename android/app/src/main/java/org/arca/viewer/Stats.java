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
 * The optional local counter ("Passport") of this device, OFF by default. What is counted is
 * decided by the Rust core ({@code arca_engine::stats}); this class keeps its opaque blob of flat
 * totals in private storage through {@link Prefs}. No date, time or history is recorded, nothing
 * about a document is recorded and nothing is sent anywhere.
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
        public byte[] recordScan(byte[] blob, int verdict, int trackers, int active) {
            return Native.statsRecordScan(blob, verdict, trackers, active);
        }

        @Override
        public byte[] recordEvent(byte[] blob, int event) {
            return Native.statsRecordEvent(blob, event);
        }

        @Override
        public byte[] summary(byte[] blob) {
            return Native.statsSummary(blob);
        }

        @Override
        public byte[] passport(int lang, byte[] blob, int firstUseDay, int today) {
            return Native.statsPassport(lang, blob, firstUseDay, today);
        }
    }

    /** Today's day number in the device's local calendar (only for the "days with Arca" line of the passport). */
    static int today() {
        long now = System.currentTimeMillis();
        return Fmt.epochDay(now, TimeZone.getDefault().getOffset(now));
    }

    /** At start: deletes a counter blob that an older version left while the counter is off. */
    public static void purgeIfOff(Context c) {
        store(c).purgeIfOff();
    }

    /** Counts a scanned document by its verdict and the engine's counts of findings. */
    public static void recordScan(Context c, Model.Report r) {
        store(c).recordScan(StatsView.verdictTag(r.verdict), r.trackers, r.active);
    }

    /** Counts a sterile copy that was written. */
    public static void recordSterileCopy(Context c) {
        store(c).recordEvent(StatsStore.EVENT_STERILE_COPY);
    }

    /** Counts a picture the reader opened. */
    public static void recordPicture(Context c) {
        store(c).recordEvent(StatsStore.EVENT_PICTURE_OPENED);
    }

    /** All-time totals (empty when the counter is off). */
    public static StatsView.Summary summary(Context c) {
        return store(c).summary();
    }

    /** The Patron Passport JSON from the totals, {@code null} when the counter is off. */
    public static byte[] passportJson(Context c) {
        return store(c).passportJson(Prefs.engineLang(c), Prefs.installDay(c), today());
    }

    public static boolean enabled(Context c) {
        return store(c).enabled();
    }

    /** Switches the counter on or off; off deletes everything that was counted. */
    public static void setEnabled(Context c, boolean on) {
        store(c).setEnabled(on);
    }

    /** Instantly and irreversibly zeroes the counter. */
    public static void reset(Context c) {
        store(c).reset();
    }
}
