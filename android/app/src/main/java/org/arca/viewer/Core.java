// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicLong;

/**
 * The document calls of the app, in the shape of {@link Native}: handles in, handles out. Behind
 * them every document has a {@link Backend} of its own (an isolated process, or this process as a
 * fallback, see {@link Isolation}). The handles handed out here are the app's own numbers; each
 * maps to a real handle inside one backend, so handles of two processes can never be confused.
 *
 * <p>A backend lives while any handle that belongs to it does: when the last one is freed, the
 * backend is closed and the isolated process ends. A job handle belongs to no backend until it is
 * run, because a job is created before it is known which document it will work on; a cancel that
 * arrives earlier is remembered and applied at the start of the run.
 */
final class Core {
    private Core() {}

    /** Counts the live handles of one backend and closes it with the last one. */
    private static final class Lease {
        final Backend backend;
        private int refs;

        Lease(Backend backend) {
            this.backend = backend;
        }

        synchronized void retain() {
            refs++;
        }

        void release() {
            boolean last;
            synchronized (this) {
                refs--;
                last = refs <= 0;
            }
            if (last) {
                backend.close();
            }
        }
    }

    private static final class Doc {
        final Lease lease;
        final long real;

        Doc(Lease lease, long real) {
            this.lease = lease;
            this.real = real;
        }
    }

    private static final class Outcome {
        final Lease lease;
        final long real;

        Outcome(Lease lease, long real) {
            this.lease = lease;
            this.real = real;
        }
    }

    private static final class Job {
        Lease lease;
        long real;
        boolean cancelled;
    }

    private static final AtomicLong NEXT = new AtomicLong(1);
    private static final ConcurrentHashMap<Long, Object> TABLE = new ConcurrentHashMap<>();

    private static long put(Object o) {
        long h = NEXT.getAndIncrement();
        TABLE.put(h, o);
        return h;
    }

    private static Doc doc(long h) {
        Object o = TABLE.get(h);
        return o instanceof Doc ? (Doc) o : null;
    }

    private static Outcome outcome(long h) {
        Object o = TABLE.get(h);
        return o instanceof Outcome ? (Outcome) o : null;
    }

    private static Job job(long h) {
        Object o = TABLE.get(h);
        return o instanceof Job ? (Job) o : null;
    }

    // ---- documents --------------------------------------------------------------------------
    static long docNew(long maxBytes) {
        Backend b = Isolation.acquire();
        long real = b.docNew(maxBytes);
        if (real <= 0) {
            b.close();
            return real;
        }
        Lease lease = new Lease(b);
        lease.retain();
        return put(new Doc(lease, real));
    }

    static int docAppend(long h, byte[] buf, int off, int len) {
        Doc d = doc(h);
        return d == null ? Native.BAD_HANDLE : d.lease.backend.docAppend(d.real, buf, off, len);
    }

    static int docSetPassword(long h, byte[] buf, int off, int len) {
        Doc d = doc(h);
        return d == null ? Native.BAD_HANDLE : d.lease.backend.docSetPassword(d.real, buf, off, len);
    }

    static long docReadFd(long h, int fd) {
        Doc d = doc(h);
        if (d == null) {
            closeFd(fd);
            return -Native.BAD_HANDLE;
        }
        return d.lease.backend.docReadFd(d.real, fd);
    }

    static long docLen(long h) {
        Doc d = doc(h);
        return d == null ? -Native.BAD_HANDLE : d.lease.backend.docLen(d.real);
    }

    static void docFree(long h) {
        Object o = TABLE.remove(h);
        if (o instanceof Doc) {
            Doc d = (Doc) o;
            d.lease.backend.docFree(d.real);
            d.lease.release();
        }
    }

    // ---- jobs -------------------------------------------------------------------------------
    static long jobNew() {
        return put(new Job());
    }

    static void jobCancel(long h) {
        Job j = job(h);
        if (j == null) {
            return;
        }
        synchronized (j) {
            j.cancelled = true;
            if (j.lease != null) {
                j.lease.backend.jobCancel(j.real);
            }
        }
    }

    static void jobFree(long h) {
        Object o = TABLE.remove(h);
        if (o instanceof Job) {
            Job j = (Job) o;
            synchronized (j) {
                if (j.lease != null) {
                    j.lease.backend.jobFree(j.real);
                    j.lease.release();
                    j.lease = null;
                }
            }
        }
    }

    // ---- running ----------------------------------------------------------------------------
    static long run(long jobHandle, long docHandle, int op, int flags) {
        Job j = job(jobHandle);
        Doc d = doc(docHandle);
        if (j == null || d == null) {
            return -Native.BAD_HANDLE;
        }
        long realJob;
        synchronized (j) {
            if (j.lease == null) {
                long created = d.lease.backend.jobNew();
                if (created <= 0) {
                    return created;
                }
                j.lease = d.lease;
                j.real = created;
                d.lease.retain();
                if (j.cancelled) {
                    d.lease.backend.jobCancel(created);
                }
            } else if (j.lease != d.lease) {
                return -Native.BAD_ARGUMENT;
            }
            realJob = j.real;
        }
        long real = d.lease.backend.run(realJob, d.real, op, flags);
        if (real <= 0) {
            return real;
        }
        d.lease.retain();
        return put(new Outcome(d.lease, real));
    }

    // ---- outcomes ---------------------------------------------------------------------------
    static int outcomeStatus(long h) {
        Outcome o = outcome(h);
        return o == null ? Native.BAD_HANDLE : o.lease.backend.outcomeStatus(o.real);
    }

    static byte[] outcomeInfo(long h) {
        Outcome o = outcome(h);
        return o == null ? null : o.lease.backend.outcomeInfo(o.real);
    }

    static byte[] report(long h) {
        Outcome o = outcome(h);
        return o == null ? null : o.lease.backend.report(o.real);
    }

    static byte[] previewMeta(long h) {
        Outcome o = outcome(h);
        return o == null ? null : o.lease.backend.previewMeta(o.real);
    }

    static byte[] page(long h) {
        Outcome o = outcome(h);
        return o == null ? null : o.lease.backend.page(o.real);
    }

    static byte[] imageInfo(long h) {
        Outcome o = outcome(h);
        return o == null ? null : o.lease.backend.imageInfo(o.real);
    }

    static byte[] imagePixels(long h) {
        Outcome o = outcome(h);
        return o == null ? null : o.lease.backend.imagePixels(o.real);
    }

    static byte[] previewItems(long h, int section, long from, int count) {
        Outcome o = outcome(h);
        return o == null ? null : o.lease.backend.previewItems(o.real, section, from, count);
    }

    static byte[] sanitizedInfo(long h) {
        Outcome o = outcome(h);
        return o == null ? null : o.lease.backend.sanitizedInfo(o.real);
    }

    static long sanitizedWriteFd(long h, int fd) {
        Outcome o = outcome(h);
        if (o == null) {
            closeFd(fd);
            return -Native.BAD_HANDLE;
        }
        return o.lease.backend.sanitizedWriteFd(o.real, fd);
    }

    static void outcomeFree(long h) {
        Object o = TABLE.remove(h);
        if (o instanceof Outcome) {
            Outcome out = (Outcome) o;
            out.lease.backend.outcomeFree(out.real);
            out.lease.release();
        }
    }

    /** Closes a descriptor that the caller handed over but that no backend will take. */
    private static void closeFd(int fd) {
        try {
            android.os.ParcelFileDescriptor.adoptFd(fd).close();
        } catch (java.io.IOException | RuntimeException ignored) {
            // nothing to do
        }
    }
}
