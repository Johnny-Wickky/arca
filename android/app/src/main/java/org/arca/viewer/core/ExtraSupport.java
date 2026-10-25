// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/**
 * The extra ways to support the creators (crypto wallets, Boosty), as the core reports them
 * ({@code Native.extraSupport}). The list is empty until the owner provides the addresses in
 * {@code crates/arca-attribution/src/support.rs}; no address is ever invented here. The core has
 * already validated every entry; this class reads them again defensively, because an address
 * is something a person will copy into a wallet.
 */
public final class ExtraSupport {
    private ExtraSupport() {}

    public static final String KIND_CRYPTO = "crypto";
    public static final String KIND_BOOSTY = "boosty";

    /** At most this many entries are shown. */
    public static final int MAX_ENTRIES = 16;

    /** One entry. */
    public static final class Entry {
        public String kind = "";
        public String label = "";
        public String network = "";
        public String value = "";
        public String memo = "";

        public boolean isWallet() {
            return KIND_CRYPTO.equals(kind);
        }
    }

    /** Parses {@code {"items":[...]}}; entries that are not well formed are dropped. */
    public static List<Entry> parse(byte[] json) {
        List<Entry> out = new ArrayList<>();
        if (json == null || json.length == 0) {
            return out;
        }
        try {
            Map<String, Object> root = Json.obj(Json.parse(json));
            for (Object o : Json.arr(root.get("items"))) {
                if (out.size() >= MAX_ENTRIES) {
                    break;
                }
                Map<String, Object> m = Json.obj(o);
                Entry e = new Entry();
                e.kind = Json.str(m, "kind");
                e.label = Json.str(m, "label");
                e.network = Json.str(m, "network");
                e.value = Json.str(m, "value");
                e.memo = Json.str(m, "memo");
                if (wellFormed(e)) {
                    out.add(e);
                }
            }
        } catch (RuntimeException ex) {
            out.clear();
        }
        return out;
    }

    private static boolean wellFormed(Entry e) {
        if (e.label.isEmpty() || e.label.length() > 40 || e.value.isEmpty() || e.value.length() > 200) {
            return false;
        }
        if (KIND_BOOSTY.equals(e.kind)) {
            return e.value.startsWith("https://boosty.to/") && printable(e.value);
        }
        if (KIND_CRYPTO.equals(e.kind)) {
            return printable(e.value) && (e.memo.isEmpty() || printable(e.memo)) && !e.network.isEmpty();
        }
        return false;
    }

    /** Printable ASCII without spaces: nothing that could hide a look-alike character in an address. */
    private static boolean printable(String s) {
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c <= 0x20 || c >= 0x7f) {
                return false;
            }
        }
        return true;
    }
}
