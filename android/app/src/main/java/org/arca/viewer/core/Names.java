// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

/** Name conversions between the engine's identifiers and Android resource names. */
public final class Names {
    private Names() {}

    /**
     * {@code ExternalImage} to {@code external_image}; {@code PdfJavaScript} to
     * {@code pdf_java_script}; {@code DdeField} to {@code dde_field}. Only ASCII letters and
     * digits are kept, so the result is always a valid resource name (or empty).
     */
    public static String snake(String camel) {
        if (camel == null) {
            return "";
        }
        StringBuilder sb = new StringBuilder(camel.length() + 6);
        for (int i = 0; i < camel.length(); i++) {
            char c = camel.charAt(i);
            if (c >= 'A' && c <= 'Z') {
                if (sb.length() > 0) {
                    sb.append('_');
                }
                sb.append((char) (c + ('a' - 'A')));
            } else if ((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9')) {
                sb.append(c);
            }
        }
        return sb.toString();
    }

    /** Resource name of the title of a threat kind. */
    public static String threatTitle(String kind) {
        return "threat_" + snake(kind) + "_title";
    }

    /** Resource name of the explanation of a threat kind. */
    public static String threatText(String kind) {
        return "threat_" + snake(kind) + "_text";
    }
}
