// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.app.ActivityManager;
import android.content.Context;
import android.os.Handler;
import android.os.Looper;

import org.arca.viewer.core.Model;

import java.util.ArrayList;
import java.util.List;

/**
 * Starts the native engine once per process, off the main thread, and tells listeners when it
 * is ready (or why it is not). Nothing else in the app calls {@link Native#init}.
 */
public final class Engine {
    private Engine() {}

    /** Lifecycle of the native engine. */
    public enum State { STARTING, READY, FAILED }

    /** Told (on the main thread) when {@link #state()} changes. */
    public interface Listener {
        void onEngineChanged();
    }

    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    private static final List<Listener> LISTENERS = new ArrayList<>();
    private static State state = State.STARTING;
    private static String failure = "";
    private static Model.EngineInfo info = new Model.EngineInfo();
    private static Model.ReleaseInfo release = new Model.ReleaseInfo();
    private static boolean started;
    private static int maxInputMiB = 256;

    /** Starts the engine (idempotent). Call from {@code Application.onCreate}. */
    public static synchronized void start(final Context app) {
        if (started) {
            return;
        }
        started = true;
        final int totalMiB = totalMemoryMiB(app);
        Thread t = new Thread(new Runnable() {
            @Override
            public void run() {
                boot(totalMiB);
            }
        }, "arca-engine-init");
        t.start();
    }

    private static int totalMemoryMiB(Context app) {
        try {
            ActivityManager am = (ActivityManager) app.getSystemService(Context.ACTIVITY_SERVICE);
            ActivityManager.MemoryInfo mi = new ActivityManager.MemoryInfo();
            if (am != null) {
                am.getMemoryInfo(mi);
                return (int) Math.min(Integer.MAX_VALUE, mi.totalMem >> 20);
            }
        } catch (RuntimeException ignored) {
            // fall through to a conservative guess
        }
        return 3072;
    }

    private static void boot(int totalMiB) {
        String error = "";
        if (!Native.isLoaded()) {
            Throwable t = Native.loadError();
            error = "libarca_jni: " + (t == null ? "not loaded" : t.toString());
        } else {
            // The engine clamps these to its own safe range; they only bring the defaults down
            // on small devices (and give slower ones more time).
            int memory = Math.max(192, Math.min(768, totalMiB / 3));
            int input = Math.max(16, Math.min(256, totalMiB / 8));
            int slow = totalMiB < 3000 ? 2 : 1;
            int status = Native.init(input, memory, 120 * slow, 180 * slow);
            if (status != Native.OK) {
                error = "init failed: status " + status;
            } else {
                try {
                    byte[] infoJson = Native.info();
                    if (infoJson == null) {
                        error = "engine information is unavailable";
                    } else {
                        Model.EngineInfo parsed = Model.parseEngineInfo(infoJson);
                        byte[] rel = Native.releaseInfo();
                        Model.ReleaseInfo r = rel == null ? new Model.ReleaseInfo() : Model.parseReleaseInfo(rel);
                        synchronized (Engine.class) {
                            info = parsed;
                            release = r;
                            maxInputMiB = (int) Math.max(1, parsed.maxInputBytes >> 20);
                        }
                    }
                } catch (RuntimeException e) {
                    error = "engine information: " + e;
                }
            }
        }
        final String result = error;
        synchronized (Engine.class) {
            failure = result;
            state = result.isEmpty() ? State.READY : State.FAILED;
        }
        MAIN.post(new Runnable() {
            @Override
            public void run() {
                notifyListeners();
            }
        });
    }

    private static void notifyListeners() {
        List<Listener> copy;
        synchronized (Engine.class) {
            copy = new ArrayList<>(LISTENERS);
        }
        for (Listener l : copy) {
            l.onEngineChanged();
        }
    }

    public static synchronized State state() {
        return state;
    }

    /**
     * Waits (on a worker thread, never the main thread) until the engine has started or failed.
     *
     * @return the state reached; {@link State#STARTING} if {@code millis} ran out first
     */
    public static State await(long millis) {
        long deadline = System.nanoTime() + millis * 1_000_000L;
        while (true) {
            State s = state();
            if (s != State.STARTING) {
                return s;
            }
            if (System.nanoTime() >= deadline) {
                return s;
            }
            try {
                Thread.sleep(25);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                return state();
            }
        }
    }

    /** Why the engine could not start (technical, English), or empty. */
    public static synchronized String failure() {
        return failure;
    }

    public static synchronized Model.EngineInfo info() {
        return info;
    }

    public static synchronized Model.ReleaseInfo release() {
        return release;
    }

    /** The largest document the engine accepts, in MiB. */
    public static synchronized int maxInputMiB() {
        return maxInputMiB;
    }

    public static synchronized void addListener(Listener l) {
        if (!LISTENERS.contains(l)) {
            LISTENERS.add(l);
        }
    }

    public static synchronized void removeListener(Listener l) {
        LISTENERS.remove(l);
    }
}
