// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * A small, strict JSON parser with hard limits.
 *
 * <p>The engine only ever produces well-formed JSON, but the app treats it like any other
 * input that crossed a trust boundary: nesting is bounded, numbers must fit, strings must be
 * valid UTF-16 after unescaping, and trailing garbage is an error. Objects become
 * {@link Map} (insertion order kept), arrays {@link List}, integers {@link Long}, other
 * numbers {@link Double}.
 *
 * <p>This class has no Android dependencies; it is tested on a desktop JVM.
 */
public final class Json {
    /** Deepest nesting accepted. */
    public static final int MAX_DEPTH = 64;

    /**
     * Largest document accepted as bytes (16 MiB). Native results are bounded by the engine, but a
     * hostile or buggy one must not be able to exhaust the heap through the parser: the bytes
     * become a String (twice the size) and then a tree of objects.
     */
    public static final int MAX_INPUT_BYTES = 16 << 20;

    /** Thrown for malformed or over-limit input. */
    public static final class JsonException extends RuntimeException {
        private static final long serialVersionUID = 1L;

        JsonException(String message, int position) {
            super(message + " at " + position);
        }
    }

    private final String s;
    private int pos;

    private Json(String s) {
        this.s = s;
    }

    /** Parses UTF-8 JSON bytes. */
    public static Object parse(byte[] utf8) {
        if (utf8 == null) {
            throw new JsonException("null input", 0);
        }
        if (utf8.length > MAX_INPUT_BYTES) {
            throw new JsonException("input too large (" + utf8.length + " bytes)", 0);
        }
        return parse(new String(utf8, StandardCharsets.UTF_8));
    }

    /** Parses JSON text. */
    public static Object parse(String text) {
        if (text == null) {
            throw new JsonException("null input", 0);
        }
        Json p = new Json(text);
        p.ws();
        Object v = p.value(0);
        p.ws();
        if (p.pos != p.s.length()) {
            throw new JsonException("trailing characters", p.pos);
        }
        return v;
    }

    private void ws() {
        while (pos < s.length()) {
            char c = s.charAt(pos);
            if (c == ' ' || c == '\t' || c == '\n' || c == '\r') {
                pos++;
            } else {
                break;
            }
        }
    }

    private char peek() {
        if (pos >= s.length()) {
            throw new JsonException("unexpected end", pos);
        }
        return s.charAt(pos);
    }

    private Object value(int depth) {
        if (depth > MAX_DEPTH) {
            throw new JsonException("nesting too deep", pos);
        }
        char c = peek();
        switch (c) {
            case '{':
                return object(depth);
            case '[':
                return array(depth);
            case '"':
                return string();
            case 't':
                literal("true");
                return Boolean.TRUE;
            case 'f':
                literal("false");
                return Boolean.FALSE;
            case 'n':
                literal("null");
                return null;
            default:
                if (c == '-' || (c >= '0' && c <= '9')) {
                    return number();
                }
                throw new JsonException("unexpected character '" + c + "'", pos);
        }
    }

    private void literal(String word) {
        if (!s.startsWith(word, pos)) {
            throw new JsonException("bad literal", pos);
        }
        pos += word.length();
    }

    private Map<String, Object> object(int depth) {
        pos++; // {
        Map<String, Object> map = new LinkedHashMap<>();
        ws();
        if (peek() == '}') {
            pos++;
            return map;
        }
        while (true) {
            ws();
            if (peek() != '"') {
                throw new JsonException("object key expected", pos);
            }
            String key = string();
            ws();
            if (peek() != ':') {
                throw new JsonException("':' expected", pos);
            }
            pos++;
            ws();
            map.put(key, value(depth + 1));
            ws();
            char c = peek();
            pos++;
            if (c == '}') {
                return map;
            }
            if (c != ',') {
                throw new JsonException("',' or '}' expected", pos - 1);
            }
        }
    }

    private List<Object> array(int depth) {
        pos++; // [
        List<Object> list = new ArrayList<>();
        ws();
        if (peek() == ']') {
            pos++;
            return list;
        }
        while (true) {
            ws();
            list.add(value(depth + 1));
            ws();
            char c = peek();
            pos++;
            if (c == ']') {
                return list;
            }
            if (c != ',') {
                throw new JsonException("',' or ']' expected", pos - 1);
            }
        }
    }

    private String string() {
        pos++; // opening quote
        StringBuilder sb = null;
        int start = pos;
        while (true) {
            if (pos >= s.length()) {
                throw new JsonException("unterminated string", pos);
            }
            char c = s.charAt(pos);
            if (c == '"') {
                String out = sb == null ? s.substring(start, pos) : sb.append(s, start, pos).toString();
                pos++;
                return out;
            }
            if (c < 0x20) {
                throw new JsonException("control character in string", pos);
            }
            if (c == '\\') {
                if (sb == null) {
                    sb = new StringBuilder();
                }
                sb.append(s, start, pos);
                pos++;
                char e = peek();
                pos++;
                switch (e) {
                    case '"': sb.append('"'); break;
                    case '\\': sb.append('\\'); break;
                    case '/': sb.append('/'); break;
                    case 'b': sb.append('\b'); break;
                    case 'f': sb.append('\f'); break;
                    case 'n': sb.append('\n'); break;
                    case 'r': sb.append('\r'); break;
                    case 't': sb.append('\t'); break;
                    case 'u': sb.append(unicode()); break;
                    default: throw new JsonException("bad escape", pos - 1);
                }
                start = pos;
            } else {
                pos++;
            }
        }
    }

    private char unicode() {
        if (pos + 4 > s.length()) {
            throw new JsonException("short \\u escape", pos);
        }
        int v = 0;
        for (int i = 0; i < 4; i++) {
            int d = Character.digit(s.charAt(pos + i), 16);
            if (d < 0) {
                throw new JsonException("bad \\u escape", pos + i);
            }
            v = (v << 4) | d;
        }
        pos += 4;
        return (char) v;
    }

    private Object number() {
        int start = pos;
        if (peek() == '-') {
            pos++;
        }
        int digits = 0;
        while (pos < s.length() && s.charAt(pos) >= '0' && s.charAt(pos) <= '9') {
            pos++;
            digits++;
        }
        if (digits == 0) {
            throw new JsonException("digit expected", pos);
        }
        boolean integral = true;
        if (pos < s.length() && s.charAt(pos) == '.') {
            integral = false;
            pos++;
            int frac = 0;
            while (pos < s.length() && s.charAt(pos) >= '0' && s.charAt(pos) <= '9') {
                pos++;
                frac++;
            }
            if (frac == 0) {
                throw new JsonException("fraction digit expected", pos);
            }
        }
        if (pos < s.length() && (s.charAt(pos) == 'e' || s.charAt(pos) == 'E')) {
            integral = false;
            pos++;
            if (pos < s.length() && (s.charAt(pos) == '+' || s.charAt(pos) == '-')) {
                pos++;
            }
            int exp = 0;
            while (pos < s.length() && s.charAt(pos) >= '0' && s.charAt(pos) <= '9') {
                pos++;
                exp++;
            }
            if (exp == 0) {
                throw new JsonException("exponent digit expected", pos);
            }
        }
        String text = s.substring(start, pos);
        if (integral) {
            try {
                return Long.valueOf(text);
            } catch (NumberFormatException e) {
                // Larger than 64 bits: keep the magnitude as a double.
                return Double.valueOf(text);
            }
        }
        return Double.valueOf(text);
    }

    // ---- typed accessors (tolerant: wrong or missing values give the default) ----------------

    /** {@code o} as a map, or an empty map. */
    @SuppressWarnings("unchecked")
    public static Map<String, Object> obj(Object o) {
        return o instanceof Map ? (Map<String, Object>) o : new LinkedHashMap<String, Object>();
    }

    /** {@code o} as a list, or an empty list. */
    @SuppressWarnings("unchecked")
    public static List<Object> arr(Object o) {
        return o instanceof List ? (List<Object>) o : new ArrayList<Object>();
    }

    /** {@code o} as a string, or {@code def}. */
    public static String str(Object o, String def) {
        return o instanceof String ? (String) o : def;
    }

    /** {@code o} as a long, or {@code def}. */
    public static long num(Object o, long def) {
        if (o instanceof Long) {
            return (Long) o;
        }
        if (o instanceof Double) {
            double d = (Double) o;
            return d >= Long.MIN_VALUE && d <= Long.MAX_VALUE ? (long) d : def;
        }
        return def;
    }

    /** {@code o} as a boolean, or {@code def}. */
    public static boolean bool(Object o, boolean def) {
        return o instanceof Boolean ? (Boolean) o : def;
    }

    /** {@code m[key]} as a string. */
    public static String str(Map<String, Object> m, String key) {
        return str(m.get(key), "");
    }

    /** {@code m[key]} as a long. */
    public static long num(Map<String, Object> m, String key) {
        return num(m.get(key), 0L);
    }

    /** {@code m[key]} as an int, saturated. */
    public static int integer(Map<String, Object> m, String key) {
        long v = num(m.get(key), 0L);
        return v > Integer.MAX_VALUE ? Integer.MAX_VALUE : v < Integer.MIN_VALUE ? Integer.MIN_VALUE : (int) v;
    }

    /** {@code m[key]} as a boolean. */
    public static boolean bool(Map<String, Object> m, String key) {
        return bool(m.get(key), false);
    }

    /** {@code m[key]} as a list of strings (non-strings are skipped). */
    public static List<String> strings(Map<String, Object> m, String key) {
        List<String> out = new ArrayList<>();
        for (Object o : arr(m.get(key))) {
            if (o instanceof String) {
                out.add((String) o);
            }
        }
        return out;
    }
}
