// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.app.Service;
import android.content.Intent;
import android.os.Binder;
import android.os.IBinder;
import android.os.Parcel;
import android.os.ParcelFileDescriptor;
import android.os.RemoteException;

import java.io.IOException;
import java.io.OutputStream;

/**
 * The document engine in a process of its own.
 *
 * <p>The manifest declares this service with {@code android:isolatedProcess="true"}: Android runs
 * it under a throw-away uid in the {@code isolated_app} SELinux domain, with a system seccomp
 * filter, without the app's data directory, without the network and without any permission.
 * Whatever a hostile document does to the parsers, it does inside that process; the Wasm sandbox
 * of the engine is a second wall inside it. One service instance serves one document: the app
 * unbinds when the document is closed or another is opened, and the process ends with it, so no
 * byte of a document outlives its use in memory that another document could see.
 *
 * <p>The class only carries calls to {@link LocalBackend}; it holds no policy.
 */
public final class SandboxService extends Service {
    private final LocalBackend engine = new LocalBackend();

    private final Binder binder = new Binder() {
        @Override
        protected boolean onTransact(int code, Parcel data, Parcel reply, int flags) throws RemoteException {
            if (code < FIRST_CALL_TRANSACTION || code > LAST_CALL_TRANSACTION || reply == null) {
                return super.onTransact(code, data, reply, flags);
            }
            try {
                data.enforceInterface(SandboxProtocol.DESCRIPTOR);
                return dispatch(code, data, reply);
            } catch (RuntimeException | IOException | LinkageError e) {
                return false;
            }
        }
    };

    @Override
    public IBinder onBind(Intent intent) {
        return binder;
    }

    @Override
    public void onDestroy() {
        super.onDestroy();
        // Do not wait for the system: the process holds a document, so it ends now.
        android.os.Process.killProcess(android.os.Process.myPid());
    }

    private boolean dispatch(int code, Parcel in, Parcel out) throws IOException {
        switch (code) {
            case SandboxProtocol.PING:
                out.writeInt(android.os.Process.myUid());
                return true;
            case SandboxProtocol.INIT:
                out.writeInt(engine.init(in.readInt(), in.readInt(), in.readInt(), in.readInt()));
                return true;
            case SandboxProtocol.DOC_NEW:
                out.writeLong(engine.docNew(in.readLong()));
                return true;
            case SandboxProtocol.DOC_APPEND: {
                long doc = in.readLong();
                byte[] buf = in.createByteArray();
                try {
                    out.writeInt(buf == null ? Native.BAD_ARGUMENT : engine.docAppend(doc, buf, 0, buf.length));
                } finally {
                    if (buf != null) {
                        // The engine has its own copy; this one is not kept around.
                        java.util.Arrays.fill(buf, (byte) 0);
                    }
                }
                return true;
            }
            case SandboxProtocol.DOC_SET_PASSWORD: {
                long doc = in.readLong();
                byte[] buf = in.createByteArray();
                int status;
                try {
                    status = buf == null ? Native.BAD_ARGUMENT : engine.docSetPassword(doc, buf, 0, buf.length);
                } finally {
                    if (buf != null) {
                        java.util.Arrays.fill(buf, (byte) 0);
                    }
                }
                out.writeInt(status);
                return true;
            }
            case SandboxProtocol.DOC_READ_FD: {
                long doc = in.readLong();
                ParcelFileDescriptor pfd = in.readFileDescriptor();
                if (pfd == null) {
                    out.writeLong(-Native.BAD_ARGUMENT);
                    return true;
                }
                out.writeLong(handOver(pfd, doc, true));
                return true;
            }
            case SandboxProtocol.DOC_LEN:
                out.writeLong(engine.docLen(in.readLong()));
                return true;
            case SandboxProtocol.DOC_FREE:
                engine.docFree(in.readLong());
                out.writeInt(0);
                return true;
            case SandboxProtocol.JOB_NEW:
                out.writeLong(engine.jobNew());
                return true;
            case SandboxProtocol.JOB_CANCEL:
                engine.jobCancel(in.readLong());
                out.writeInt(0);
                return true;
            case SandboxProtocol.JOB_FREE:
                engine.jobFree(in.readLong());
                out.writeInt(0);
                return true;
            case SandboxProtocol.RUN:
                out.writeLong(engine.run(in.readLong(), in.readLong(), in.readInt(), in.readInt()));
                return true;
            case SandboxProtocol.OUTCOME_STATUS:
                out.writeInt(engine.outcomeStatus(in.readLong()));
                return true;
            case SandboxProtocol.BYTES:
                writeBlob(out, bytes(in));
                return true;
            case SandboxProtocol.SANITIZED_WRITE_FD: {
                long outcome = in.readLong();
                ParcelFileDescriptor pfd = in.readFileDescriptor();
                if (pfd == null) {
                    out.writeLong(-Native.BAD_ARGUMENT);
                    return true;
                }
                out.writeLong(handOver(pfd, outcome, false));
                return true;
            }
            case SandboxProtocol.OUTCOME_FREE:
                engine.outcomeFree(in.readLong());
                out.writeInt(0);
                return true;
            default:
                return false;
        }
    }

    /**
     * Gives a descriptor to the native side, which takes ownership of it and closes it. If the call
     * does not reach the native side (the library is missing, the call is refused with an error
     * that is thrown), the descriptor is still ours and is closed here: a descriptor is never left
     * open in a process that holds a document, whatever happens.
     */
    private long handOver(ParcelFileDescriptor pfd, long handle, boolean read) {
        int fd = pfd.detachFd();
        boolean handed = false;
        try {
            long result = read ? engine.docReadFd(handle, fd) : engine.sanitizedWriteFd(handle, fd);
            handed = true;
            return result;
        } finally {
            if (!handed) {
                closeFd(fd);
            }
        }
    }

    private static void closeQuietly(ParcelFileDescriptor pfd) {
        try {
            pfd.close();
        } catch (IOException ignored) {
            // nothing more can be done
        }
    }

    /** Closes a detached descriptor that no one else will close. */
    static void closeFd(int fd) {
        try {
            ParcelFileDescriptor.adoptFd(fd).close();
        } catch (IOException | RuntimeException ignored) {
            // nothing more can be done
        }
    }

    private byte[] bytes(Parcel in) {
        int which = in.readInt();
        long outcome = in.readLong();
        int section = in.readInt();
        long from = in.readLong();
        int count = in.readInt();
        switch (which) {
            case SandboxProtocol.B_OUTCOME_INFO: return engine.outcomeInfo(outcome);
            case SandboxProtocol.B_REPORT: return engine.report(outcome);
            case SandboxProtocol.B_PREVIEW_META: return engine.previewMeta(outcome);
            case SandboxProtocol.B_PAGE: return engine.page(outcome);
            case SandboxProtocol.B_IMAGE_INFO: return engine.imageInfo(outcome);
            case SandboxProtocol.B_IMAGE_PIXELS: return engine.imagePixels(outcome);
            case SandboxProtocol.B_PREVIEW_ITEMS: return engine.previewItems(outcome, section, from, count);
            case SandboxProtocol.B_SANITIZED_INFO: return engine.sanitizedInfo(outcome);
            default: return null;
        }
    }

    /**
     * Writes an answer: nothing, the bytes themselves when they are short, or, when they are long,
     * the read end of a pipe that a thread fills (a Binder parcel is far too small for a picture).
     */
    private static void writeBlob(Parcel out, byte[] b) throws IOException {
        if (b == null) {
            out.writeInt(0);
            return;
        }
        if (b.length <= SandboxProtocol.INLINE_MAX) {
            out.writeInt(1);
            out.writeByteArray(b);
            return;
        }
        ParcelFileDescriptor[] pipe = ParcelFileDescriptor.createPipe();
        final ParcelFileDescriptor write = pipe[1];
        final byte[] data = b;
        boolean started = false;
        try {
            out.writeInt(2);
            out.writeInt(b.length);
            out.writeFileDescriptor(pipe[0].getFileDescriptor());
            Thread t = new Thread(new Runnable() {
                @Override
                public void run() {
                    try (OutputStream os = new ParcelFileDescriptor.AutoCloseOutputStream(write)) {
                        os.write(data);
                    } catch (IOException ignored) {
                        // the reader went away; nothing more to deliver
                    } finally {
                        // The bytes were pixels or text of the document: not left lying in memory.
                        java.util.Arrays.fill(data, (byte) 0);
                    }
                }
            }, "arca-sandbox-pipe");
            t.setDaemon(true);
            t.start();
            started = true;
        } finally {
            // The parcel holds its own copy of the read end, so ours always goes; the write end goes
            // too unless the thread that owns it is running (a failure before that must not leak it).
            closeQuietly(pipe[0]);
            if (!started) {
                closeQuietly(write);
            }
        }
    }
}
