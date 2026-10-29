// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.os.IBinder;
import android.os.Parcel;
import android.os.ParcelFileDescriptor;
import android.os.RemoteException;

import java.io.IOException;
import java.io.InputStream;

/**
 * {@link Backend} over Binder: every call is a transaction to {@link SandboxService}. A dead or
 * unreachable service never throws into the app; the call answers {@code -Native.FAULT} (or a
 * {@code null} blob), which the session shows as a failed document.
 */
final class RemoteBackend implements Backend {
    private final IBinder binder;
    private final Runnable unbind;
    private volatile boolean closed;

    RemoteBackend(IBinder binder, Runnable unbind) {
        this.binder = binder;
        this.unbind = unbind;
    }

    private static Parcel begin() {
        Parcel p = Parcel.obtain();
        p.writeInterfaceToken(SandboxProtocol.DESCRIPTOR);
        return p;
    }

    /** Sends {@code data} and recycles it; the reply, or {@code null} when the channel failed. */
    private Parcel call(int code, Parcel data) {
        Parcel reply = Parcel.obtain();
        try {
            if (closed || !binder.transact(code, data, reply, 0)) {
                reply.recycle();
                return null;
            }
            return reply;
        } catch (RemoteException | RuntimeException e) {
            reply.recycle();
            return null;
        } finally {
            data.recycle();
        }
    }

    private long callLong(int code, Parcel data) {
        Parcel r = call(code, data);
        if (r == null) {
            return -Native.FAULT;
        }
        try {
            return r.readLong();
        } catch (RuntimeException e) {
            return -Native.FAULT;
        } finally {
            r.recycle();
        }
    }

    private int callInt(int code, Parcel data) {
        Parcel r = call(code, data);
        if (r == null) {
            return Native.FAULT;
        }
        try {
            return r.readInt();
        } catch (RuntimeException e) {
            return Native.FAULT;
        } finally {
            r.recycle();
        }
    }

    private void callVoid(int code, Parcel data) {
        Parcel r = call(code, data);
        if (r != null) {
            r.recycle();
        }
    }

    /** The uid the service runs under, or -1. */
    int ping() {
        Parcel r = call(SandboxProtocol.PING, begin());
        if (r == null) {
            return -1;
        }
        try {
            return r.readInt();
        } catch (RuntimeException e) {
            return -1;
        } finally {
            r.recycle();
        }
    }

    @Override public int init(int maxInputMiB, int maxMemoryMiB, int scanSeconds, int previewSeconds) {
        Parcel d = begin();
        d.writeInt(maxInputMiB);
        d.writeInt(maxMemoryMiB);
        d.writeInt(scanSeconds);
        d.writeInt(previewSeconds);
        return callInt(SandboxProtocol.INIT, d);
    }

    @Override public long docNew(long maxBytes) {
        Parcel d = begin();
        d.writeLong(maxBytes);
        return callLong(SandboxProtocol.DOC_NEW, d);
    }

    @Override public int docAppend(long doc, byte[] buf, int off, int len) {
        if (buf == null || off < 0 || len < 0 || off > buf.length - len) {
            return Native.BAD_ARGUMENT;
        }
        int pos = off;
        int end = off + len;
        do {
            int n = Math.min(SandboxProtocol.CHUNK_MAX, end - pos);
            Parcel d = begin();
            d.writeLong(doc);
            d.writeByteArray(buf, pos, n);
            int status = callInt(SandboxProtocol.DOC_APPEND, d);
            if (status != Native.OK) {
                return status;
            }
            pos += n;
        } while (pos < end);
        return Native.OK;
    }

    @Override public int docSetPassword(long doc, byte[] buf, int off, int len) {
        if (buf == null || off < 0 || len < 0 || off > buf.length - len) {
            return Native.BAD_ARGUMENT;
        }
        Parcel d = begin();
        d.writeLong(doc);
        d.writeByteArray(buf, off, len);
        return callInt(SandboxProtocol.DOC_SET_PASSWORD, d);
    }

    /** Hands the descriptor over (the service reads it to its end) and closes the app's copy. */
    @Override public long docReadFd(long doc, int fd) {
        ParcelFileDescriptor pfd;
        try {
            pfd = ParcelFileDescriptor.adoptFd(fd);
        } catch (RuntimeException e) {
            return -Native.IO;
        }
        try {
            Parcel d = begin();
            d.writeLong(doc);
            d.writeFileDescriptor(pfd.getFileDescriptor());
            return callLong(SandboxProtocol.DOC_READ_FD, d);
        } finally {
            closeQuietly(pfd);
        }
    }

    @Override public long docLen(long doc) {
        Parcel d = begin();
        d.writeLong(doc);
        return callLong(SandboxProtocol.DOC_LEN, d);
    }

    @Override public void docFree(long doc) {
        Parcel d = begin();
        d.writeLong(doc);
        callVoid(SandboxProtocol.DOC_FREE, d);
    }

    @Override public long jobNew() {
        return callLong(SandboxProtocol.JOB_NEW, begin());
    }

    @Override public void jobCancel(long job) {
        Parcel d = begin();
        d.writeLong(job);
        callVoid(SandboxProtocol.JOB_CANCEL, d);
    }

    @Override public void jobFree(long job) {
        Parcel d = begin();
        d.writeLong(job);
        callVoid(SandboxProtocol.JOB_FREE, d);
    }

    @Override public long run(long job, long doc, int op, int flags) {
        Parcel d = begin();
        d.writeLong(job);
        d.writeLong(doc);
        d.writeInt(op);
        d.writeInt(flags);
        return callLong(SandboxProtocol.RUN, d);
    }

    @Override public int outcomeStatus(long outcome) {
        Parcel d = begin();
        d.writeLong(outcome);
        return callInt(SandboxProtocol.OUTCOME_STATUS, d);
    }

    private byte[] blob(int which, long outcome, int section, long from, int count) {
        Parcel d = begin();
        d.writeInt(which);
        d.writeLong(outcome);
        d.writeInt(section);
        d.writeLong(from);
        d.writeInt(count);
        return readBlob(call(SandboxProtocol.BYTES, d));
    }

    /** An answer: nothing, inline bytes, or the read end of a pipe holding {@code length} bytes. */
    private static byte[] readBlob(Parcel r) {
        if (r == null) {
            return null;
        }
        try {
            int kind = r.readInt();
            if (kind == 1) {
                return r.createByteArray();
            }
            if (kind != 2) {
                return null;
            }
            int length = r.readInt();
            ParcelFileDescriptor pfd = r.readFileDescriptor();
            if (pfd == null || length < 0 || length > SandboxProtocol.ANSWER_MAX) {
                closeQuietly(pfd);
                return null;
            }
            byte[] out = new byte[length];
            try (InputStream in = new ParcelFileDescriptor.AutoCloseInputStream(pfd)) {
                int got = 0;
                while (got < length) {
                    int n = in.read(out, got, length - got);
                    if (n < 0) {
                        return null; // the service died before it sent everything
                    }
                    got += n;
                }
            }
            return out;
        } catch (IOException | RuntimeException | OutOfMemoryError e) {
            return null;
        } finally {
            r.recycle();
        }
    }

    @Override public byte[] outcomeInfo(long outcome) { return blob(SandboxProtocol.B_OUTCOME_INFO, outcome, 0, 0, 0); }

    @Override public byte[] report(long outcome) { return blob(SandboxProtocol.B_REPORT, outcome, 0, 0, 0); }

    @Override public byte[] previewMeta(long outcome) { return blob(SandboxProtocol.B_PREVIEW_META, outcome, 0, 0, 0); }

    @Override public byte[] page(long outcome) { return blob(SandboxProtocol.B_PAGE, outcome, 0, 0, 0); }

    @Override public byte[] imageInfo(long outcome) { return blob(SandboxProtocol.B_IMAGE_INFO, outcome, 0, 0, 0); }

    @Override public byte[] imagePixels(long outcome) { return blob(SandboxProtocol.B_IMAGE_PIXELS, outcome, 0, 0, 0); }

    @Override public byte[] previewItems(long outcome, int section, long from, int count) {
        return blob(SandboxProtocol.B_PREVIEW_ITEMS, outcome, section, from, count);
    }

    @Override public byte[] sanitizedInfo(long outcome) { return blob(SandboxProtocol.B_SANITIZED_INFO, outcome, 0, 0, 0); }

    /** Hands the target descriptor over (the service writes the copy to it) and closes the app's copy. */
    @Override public long sanitizedWriteFd(long outcome, int fd) {
        ParcelFileDescriptor pfd;
        try {
            pfd = ParcelFileDescriptor.adoptFd(fd);
        } catch (RuntimeException e) {
            return -Native.IO;
        }
        try {
            Parcel d = begin();
            d.writeLong(outcome);
            d.writeFileDescriptor(pfd.getFileDescriptor());
            return callLong(SandboxProtocol.SANITIZED_WRITE_FD, d);
        } finally {
            closeQuietly(pfd);
        }
    }

    @Override public void outcomeFree(long outcome) {
        Parcel d = begin();
        d.writeLong(outcome);
        callVoid(SandboxProtocol.OUTCOME_FREE, d);
    }

    /** Unbinds the service: Android ends its process, and with it everything the document left in memory. */
    @Override public void close() {
        if (!closed) {
            closed = true;
            unbind.run();
        }
    }

    private static void closeQuietly(ParcelFileDescriptor pfd) {
        if (pfd != null) {
            try {
                pfd.close();
            } catch (IOException ignored) {
                // nothing to do
            }
        }
    }
}
