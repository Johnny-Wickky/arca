// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

/**
 * The JNI surface of {@code libarca_jni.so} (crate {@code arca-jni}). Nothing else in the app
 * loads native code.
 *
 * <p>Conventions (mirrored by {@code crates/arca-jni/src/api.rs}):
 * <ul>
 *   <li>handles are positive {@code long}s; a negative return value is {@code -status};
 *   <li>methods returning {@code byte[]} return UTF-8 JSON (or text), or {@code null} when
 *       there is nothing to return;
 *   <li>the engine never throws into Java; every failure is a status code;
 *   <li>{@link #docReadFd} and {@link #sanitizedWriteFd} take ownership of the descriptor and
 *       close it: pass {@code ParcelFileDescriptor.detachFd()}.
 * </ul>
 *
 * <p>This class has no Android dependencies, so it also loads on a desktop JVM, where the JNI
 * tests run with {@code -Xcheck:jni}.
 */
public final class Native {
    private Native() {}

    // ---- status codes (arca_engine::Status) -------------------------------------------------
    public static final int OK = 0;
    public static final int CANCELLED = 1;
    public static final int TIMEOUT = 2;
    public static final int TOO_LARGE = 3;
    public static final int REJECTED = 4;
    public static final int UNSUPPORTED = 5;
    public static final int ENCRYPTED = 6;
    public static final int FAILED = 7;
    public static final int RESOURCE_LIMIT = 8;
    public static final int FAULT = 9;
    public static final int BAD_HANDLE = 10;
    public static final int IO = 11;
    public static final int BAD_ARGUMENT = 12;
    public static final int NOT_READY = 13;
    /** The document is encrypted and the password given does not open it. */
    public static final int WRONG_PASSWORD = 14;

    // ---- operations and flags (arca_abi::request) -------------------------------------------
    public static final int OP_SCAN = 1;
    public static final int OP_SANITIZE = 2;
    public static final int OP_PREVIEW = 3;
    /** The drawing of one page; the flags hold the page number (from 0). */
    public static final int OP_RENDER = 4;
    /** One picture of the document, decoded in the sandbox; the flags hold the picture's id. */
    public static final int OP_IMAGE = 5;
    public static final int SANITIZE_ACCEPT_REVISIONS = 1;
    public static final int SANITIZE_REMOVE_COMMENTS = 2;
    public static final int SANITIZE_STRIP_METADATA = 4;
    public static final int SANITIZE_REMOVE_CUSTOM_XML = 8;
    public static final int SANITIZE_ALL = 15;
    public static final int PREVIEW_REVEAL_HIDDEN = 1;

    // ---- licence texts ----------------------------------------------------------------------
    public static final int LICENSE_AGPL = 0;
    public static final int LICENSE_TERMS_RU = 1;
    public static final int LICENSE_TERMS_EN = 2;

    private static final Throwable LOAD_ERROR;

    static {
        Throwable error = null;
        try {
            System.loadLibrary("arca_jni");
        } catch (Throwable t) {
            error = t;
        }
        LOAD_ERROR = error;
    }

    /** {@code true} if the native library was loaded. */
    public static boolean isLoaded() {
        return LOAD_ERROR == null;
    }

    /** Why the native library could not be loaded, or {@code null}. */
    public static Throwable loadError() {
        return LOAD_ERROR;
    }

    /** Initialises the engine (idempotent). Limits are clamped by the engine. */
    public static native int init(int maxInputMiB, int maxMemoryMiB, int scanSeconds, int previewSeconds);

    /** Facts about the engine and the pinned guest module (JSON). */
    public static native byte[] info();

    /** Runs the built-in self-test through the sandbox (JSON report). */
    public static native byte[] selfTest();

    // ---- documents --------------------------------------------------------------------------
    public static native long docNew(long maxBytes);

    public static native int docAppend(long doc, byte[] buf, int off, int len);

    /**
     * Remembers the password (UTF-8 bytes, at most 1024) for the next runs over the document; an
     * empty one forgets it. The caller wipes its own copy.
     */
    public static native int docSetPassword(long doc, byte[] buf, int off, int len);

    /** Reads the descriptor to its end into the document and closes it. */
    public static native long docReadFd(long doc, int fd);

    public static native long docLen(long doc);

    public static native void docFree(long doc);

    // ---- jobs -------------------------------------------------------------------------------
    public static native long jobNew();

    public static native void jobCancel(long job);

    public static native void jobFree(long job);

    // ---- running ----------------------------------------------------------------------------
    /** Runs an operation (blocking): returns the outcome handle, or {@code -status}. */
    public static native long run(long job, long doc, int op, int flags);

    public static native int outcomeStatus(long outcome);

    public static native byte[] outcomeInfo(long outcome);

    public static native byte[] report(long outcome);

    public static native byte[] previewMeta(long outcome);

    /** The drawing of a page (JSON) from an {@link #OP_RENDER} run. */
    public static native byte[] page(long outcome);

    /** Facts about a picture (JSON) from an {@link #OP_IMAGE} run. */
    public static native byte[] imageInfo(long outcome);

    /** The pixels of a picture from an {@link #OP_IMAGE} run: 4 bytes per pixel (R, G, B, A; premultiplied). */
    public static native byte[] imagePixels(long outcome);

    public static native byte[] previewItems(long outcome, int section, long from, int count);

    public static native byte[] sanitizedInfo(long outcome);

    /** Writes the sterile copy to the descriptor and closes it. */
    public static native long sanitizedWriteFd(long outcome, int fd);

    public static native void outcomeFree(long outcome);

    // ---- static views -----------------------------------------------------------------------
    public static native byte[] about(int lang);

    public static native byte[] passport(int lang, byte[] record, int today);

    public static native byte[] passportUpdate(byte[] record, int today, int trackers, int active);

    public static native byte[] licenseText(int which);

    public static native byte[] releaseInfo();
}
