// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

/**
 * The wire protocol between {@link RemoteBackend} (in the app process) and {@link SandboxService}
 * (in the isolated process): a hand-written Binder interface, so the build needs no AIDL tool.
 *
 * <p>Two rules keep it inside what Binder can carry. A transaction buffer is about 1 MiB for the
 * whole process, so nothing large travels in a parcel: the document goes in as a file descriptor
 * that the service reads, the sterile copy goes out through a file descriptor that the service
 * writes, and any answer longer than {@link #INLINE_MAX} bytes (a page drawing, the pixels of a
 * picture, a long report) comes back through an anonymous pipe. And the service never touches the
 * app's files: it has no storage, no network and no permission.
 */
final class SandboxProtocol {
    private SandboxProtocol() {}

    static final String DESCRIPTOR = "org.arca.viewer.ISandbox";

    /** Answers up to this size travel inside the reply parcel, longer ones through a pipe. */
    static final int INLINE_MAX = 64 * 1024;
    /** The longest answer the client accepts (a 16-megapixel picture is 64 MiB). */
    static final int ANSWER_MAX = 512 * 1024 * 1024;
    /** The largest piece {@code docAppend} sends in one parcel. */
    static final int CHUNK_MAX = 256 * 1024;

    static final int PING = 1;
    static final int INIT = 2;
    static final int DOC_NEW = 3;
    static final int DOC_APPEND = 4;
    static final int DOC_SET_PASSWORD = 5;
    static final int DOC_READ_FD = 6;
    static final int DOC_LEN = 7;
    static final int DOC_FREE = 8;
    static final int JOB_NEW = 9;
    static final int JOB_CANCEL = 10;
    static final int JOB_FREE = 11;
    static final int RUN = 12;
    static final int OUTCOME_STATUS = 13;
    static final int BYTES = 14;
    static final int SANITIZED_WRITE_FD = 15;
    static final int OUTCOME_FREE = 16;

    /** Selectors of the {@link #BYTES} transaction. */
    static final int B_OUTCOME_INFO = 0;
    static final int B_REPORT = 1;
    static final int B_PREVIEW_META = 2;
    static final int B_PAGE = 3;
    static final int B_IMAGE_INFO = 4;
    static final int B_IMAGE_PIXELS = 5;
    static final int B_PREVIEW_ITEMS = 6;
    static final int B_SANITIZED_INFO = 7;

    /** The first uid of the ranges Android gives to isolated processes (app zygote and plain). */
    static final int FIRST_ISOLATED_APP_ID = 90000;
    static final int LAST_ISOLATED_APP_ID = 99999;

    /** Whether {@code uid} belongs to an isolated process. */
    static boolean isIsolatedUid(int uid) {
        int appId = uid % 100000;
        return appId >= FIRST_ISOLATED_APP_ID && appId <= LAST_ISOLATED_APP_ID;
    }
}
