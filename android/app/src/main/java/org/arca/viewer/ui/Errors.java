// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import org.arca.viewer.Native;
import org.arca.viewer.R;
import org.arca.viewer.Session;

/** Maps failure codes to the texts that explain them to the person holding the phone. */
public final class Errors {
    private Errors() {}

    /** Title string resource for a failure status. */
    public static int title(int status) {
        switch (status) {
            case Native.TOO_LARGE: return R.string.err_too_large_title;
            case Native.TIMEOUT: return R.string.err_timeout_title;
            case Native.REJECTED: return R.string.err_rejected_title;
            case Native.UNSUPPORTED: return R.string.err_unsupported_title;
            case Native.ENCRYPTED: return R.string.err_encrypted_title;
            case Native.RESOURCE_LIMIT: return R.string.err_resource_title;
            case Native.IO: return R.string.err_io_title;
            case Session.F_OPEN: return R.string.err_open_title;
            case Session.F_EMPTY: return R.string.err_empty_title;
            case Session.F_ENGINE: return R.string.err_engine_title;
            default: return R.string.err_failed_title;
        }
    }

    /** Explanation string resource for a failure status. */
    public static int text(int status) {
        switch (status) {
            case Native.TOO_LARGE: return R.string.err_too_large_text;
            case Native.TIMEOUT: return R.string.err_timeout_text;
            case Native.REJECTED: return R.string.err_rejected_text;
            case Native.UNSUPPORTED: return R.string.err_unsupported_text;
            case Native.ENCRYPTED: return R.string.err_encrypted_text;
            case Native.RESOURCE_LIMIT: return R.string.err_resource_text;
            case Native.IO: return R.string.err_io_text;
            case Session.F_OPEN: return R.string.err_open_text;
            case Session.F_EMPTY: return R.string.err_empty_text;
            case Session.F_ENGINE: return R.string.err_engine_text;
            default: return R.string.err_failed_text;
        }
    }

    /** The resource name suffix of a status, for the technical line. */
    public static String code(int status) {
        switch (status) {
            case Native.CANCELLED: return "cancelled";
            case Native.TIMEOUT: return "timeout";
            case Native.TOO_LARGE: return "too-large";
            case Native.REJECTED: return "rejected";
            case Native.UNSUPPORTED: return "unsupported";
            case Native.ENCRYPTED: return "encrypted";
            case Native.FAILED: return "failed";
            case Native.RESOURCE_LIMIT: return "resource-limit";
            case Native.FAULT: return "fault";
            case Native.BAD_HANDLE: return "bad-handle";
            case Native.IO: return "io";
            case Native.BAD_ARGUMENT: return "bad-argument";
            case Native.NOT_READY: return "not-ready";
            case Session.F_OPEN: return "open";
            case Session.F_EMPTY: return "empty";
            case Session.F_ENGINE: return "engine";
            case Session.F_PARSE: return "parse";
            default: return "status-" + status;
        }
    }
}
