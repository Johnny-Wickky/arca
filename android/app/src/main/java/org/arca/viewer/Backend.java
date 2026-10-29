// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

/**
 * The document half of {@link Native}: the calls that touch an untrusted document. There are two
 * implementations. {@link LocalBackend} calls the native library in this process;
 * {@link RemoteBackend} sends the same calls to {@link SandboxService}, which runs in an isolated
 * process of its own. {@link Core} chooses one per document and hands the rest of the app one
 * set of handles, so nothing else in the app knows which one is in use.
 *
 * <p>Return values follow {@link Native}: a handle is positive and a negative return value is
 * {@code -status}; a failure of the channel itself is {@code -Native.FAULT}.
 */
interface Backend {
    int init(int maxInputMiB, int maxMemoryMiB, int scanSeconds, int previewSeconds);

    long docNew(long maxBytes);

    int docAppend(long doc, byte[] buf, int off, int len);

    int docSetPassword(long doc, byte[] buf, int off, int len);

    /** Reads the descriptor to its end into the document and closes it. */
    long docReadFd(long doc, int fd);

    long docLen(long doc);

    void docFree(long doc);

    long jobNew();

    void jobCancel(long job);

    void jobFree(long job);

    long run(long job, long doc, int op, int flags);

    int outcomeStatus(long outcome);

    byte[] outcomeInfo(long outcome);

    byte[] report(long outcome);

    byte[] previewMeta(long outcome);

    byte[] page(long outcome);

    byte[] imageInfo(long outcome);

    byte[] imagePixels(long outcome);

    byte[] previewItems(long outcome, int section, long from, int count);

    byte[] sanitizedInfo(long outcome);

    /** Writes the sterile copy to the descriptor and closes it. */
    long sanitizedWriteFd(long outcome, int fd);

    void outcomeFree(long outcome);

    /** Ends the backend; for the remote one this unbinds the service, which ends its process. */
    void close();
}
