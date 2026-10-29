// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

/** The native library called in this process (the fallback when the isolated process is unavailable). */
final class LocalBackend implements Backend {
    @Override public int init(int maxInputMiB, int maxMemoryMiB, int scanSeconds, int previewSeconds) {
        return Native.init(maxInputMiB, maxMemoryMiB, scanSeconds, previewSeconds);
    }

    @Override public long docNew(long maxBytes) { return Native.docNew(maxBytes); }

    @Override public int docAppend(long doc, byte[] buf, int off, int len) { return Native.docAppend(doc, buf, off, len); }

    @Override public int docSetPassword(long doc, byte[] buf, int off, int len) { return Native.docSetPassword(doc, buf, off, len); }

    @Override public long docReadFd(long doc, int fd) { return Native.docReadFd(doc, fd); }

    @Override public long docLen(long doc) { return Native.docLen(doc); }

    @Override public void docFree(long doc) { Native.docFree(doc); }

    @Override public long jobNew() { return Native.jobNew(); }

    @Override public void jobCancel(long job) { Native.jobCancel(job); }

    @Override public void jobFree(long job) { Native.jobFree(job); }

    @Override public long run(long job, long doc, int op, int flags) { return Native.run(job, doc, op, flags); }

    @Override public int outcomeStatus(long outcome) { return Native.outcomeStatus(outcome); }

    @Override public byte[] outcomeInfo(long outcome) { return Native.outcomeInfo(outcome); }

    @Override public byte[] report(long outcome) { return Native.report(outcome); }

    @Override public byte[] previewMeta(long outcome) { return Native.previewMeta(outcome); }

    @Override public byte[] page(long outcome) { return Native.page(outcome); }

    @Override public byte[] imageInfo(long outcome) { return Native.imageInfo(outcome); }

    @Override public byte[] imagePixels(long outcome) { return Native.imagePixels(outcome); }

    @Override public byte[] previewItems(long outcome, int section, long from, int count) {
        return Native.previewItems(outcome, section, from, count);
    }

    @Override public byte[] sanitizedInfo(long outcome) { return Native.sanitizedInfo(outcome); }

    @Override public long sanitizedWriteFd(long outcome, int fd) { return Native.sanitizedWriteFd(outcome, fd); }

    @Override public void outcomeFree(long outcome) { Native.outcomeFree(outcome); }

    @Override public void close() {
        // the library lives as long as this process
    }
}
