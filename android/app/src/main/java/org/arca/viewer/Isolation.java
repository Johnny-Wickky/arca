// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.ServiceConnection;
import android.os.IBinder;

import org.arca.viewer.ui.Compat;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

/**
 * Chooses where a document is processed and says so honestly.
 *
 * <p>Wall 0 is an isolated process of the operating system ({@link SandboxService}: a throw-away
 * uid in the {@code isolated_app} SELinux domain under a seccomp filter, no app storage, no
 * network). Wall 1 is the Wasm sandbox inside the engine, which every document passes through in
 * either case. Some firmware refuses to bind an isolated service (or kills it at once); then the
 * document is processed in this process behind the Wasm wall alone, and the Diagnostics screen
 * names the level that is actually in force instead of the one the manifest asks for.
 *
 * <p>A fresh process is bound for every document and unbound when the document is closed, so the
 * kernel destroys the address space between documents.
 */
public final class Isolation {
    private Isolation() {}

    /** The level of protection that was in force for the most recent document. */
    public enum Level {
        /** Not decided yet (nothing has been tried since the app started). */
        UNKNOWN,
        /** An isolated process ({@code isolated_app}) and the Wasm sandbox. */
        PROCESS_ISOLATED,
        /** A separate process that the firmware did not isolate, and the Wasm sandbox. */
        PROCESS_SEPARATE,
        /** The Wasm sandbox inside the app process. */
        IN_PROCESS
    }

    /** How long a bind may take before the document is processed in this process instead. */
    static final long BIND_TIMEOUT_MS = 1500;
    /** After a failed attempt the isolated process is not tried again for this long. */
    private static final long RETRY_AFTER_MS = 60_000;

    private static final AtomicInteger INSTANCE = new AtomicInteger();
    private static volatile Context app;
    private static volatile int[] limits;
    private static volatile Level level = Level.UNKNOWN;
    private static volatile String detail = "";
    private static volatile long retryAt;

    /** Whether the current process is an isolated one (the sandbox service itself). */
    public static boolean isIsolatedProcess() {
        return SandboxProtocol.isIsolatedUid(android.os.Process.myUid());
    }

    /** Called once by {@link App}: the context the service is bound from. */
    static void attach(Context c) {
        app = c.getApplicationContext();
    }

    /** Called by {@link Engine} when the engine is up: the limits the service is initialised with. */
    static void configure(int maxInputMiB, int maxMemoryMiB, int scanSeconds, int previewSeconds) {
        limits = new int[] {maxInputMiB, maxMemoryMiB, scanSeconds, previewSeconds};
    }

    public static Level level() {
        return level;
    }

    /** Why the isolated process is not in use ({@code ""} when it is, or when nothing was tried). */
    public static String detail() {
        return detail;
    }

    /** Tries the isolated process once, now, and ends it: decides {@link #level()} at start-up. */
    public static void probe() {
        Backend b = acquire();
        b.close();
    }

    /**
     * A backend for one new document: the isolated process when it is available, else this
     * process. Never called on the main thread (it waits for the bind).
     */
    static Backend acquire() {
        Context c = app;
        int[] lim = limits;
        if (c == null || lim == null) {
            return inProcess("not configured");
        }
        if (System.currentTimeMillis() < retryAt) {
            return inProcess(detail);
        }
        RemoteBackend remote = null;
        String why;
        try {
            remote = connect(c, BIND_TIMEOUT_MS);
            why = remote == null ? "the service did not start within " + BIND_TIMEOUT_MS + " ms" : "";
        } catch (SecurityException e) {
            why = "bind refused: " + e.getMessage();
        } catch (RuntimeException e) {
            why = "bind failed: " + e;
        }
        if (remote != null) {
            int uid = remote.ping();
            if (uid >= 0 && remote.init(lim[0], lim[1], lim[2], lim[3]) == Native.OK) {
                level = SandboxProtocol.isIsolatedUid(uid) ? Level.PROCESS_ISOLATED : Level.PROCESS_SEPARATE;
                detail = "";
                return remote;
            }
            remote.close();
            why = "the service did not answer";
        }
        retryAt = System.currentTimeMillis() + RETRY_AFTER_MS;
        return inProcess(why);
    }

    private static Backend inProcess(String why) {
        level = Level.IN_PROCESS;
        detail = why;
        return new LocalBackend();
    }

    private static RemoteBackend connect(final Context c, long timeoutMs) {
        final CountDownLatch bound = new CountDownLatch(1);
        final AtomicReference<IBinder> service = new AtomicReference<>();
        final ServiceConnection connection = new ServiceConnection() {
            @Override
            public void onServiceConnected(ComponentName name, IBinder binder) {
                service.set(binder);
                bound.countDown();
            }

            @Override
            public void onServiceDisconnected(ComponentName name) {
                // the process ended; calls in flight answer FAULT
            }
        };
        Runnable unbind = new Runnable() {
            @Override
            public void run() {
                try {
                    c.unbindService(connection);
                } catch (IllegalArgumentException ignored) {
                    // not bound any more
                }
            }
        };
        Intent intent = new Intent(c, SandboxService.class);
        // A new instance name gives a new process on Android 10+; before that, unbinding the
        // previous document has already ended the old one.
        String instance = "doc" + INSTANCE.incrementAndGet();
        if (!Compat.bindIsolated(c, intent, instance, connection)) {
            unbind.run();
            return null;
        }
        try {
            if (!bound.await(timeoutMs, TimeUnit.MILLISECONDS)) {
                unbind.run();
                return null;
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            unbind.run();
            return null;
        }
        IBinder binder = service.get();
        if (binder == null) {
            unbind.run();
            return null;
        }
        return new RemoteBackend(binder, unbind);
    }
}
