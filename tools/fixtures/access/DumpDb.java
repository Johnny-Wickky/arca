// SPDX-License-Identifier: AGPL-3.0-only
// Independent oracle for the Access reader: writes every user table of a database as typed,
// canonical tokens using Jackcess (https://jackcess.sourceforge.io/, Apache-2.0).
//
//   javac -encoding UTF-8 -cp jackcess.jar:commons-lang3.jar:commons-logging.jar -d out DumpDb.java
//   java  -cp out:jackcess.jar:... DumpDb file.mdb > file.mdb.dump
//
// One token per value, separated by TAB, one line per row:
//   ~ null, b0/b1 yes-no, i<n> integer, f<hex> single bits, d<hex> double bits,
//   m<n> currency in 1/10000, n<text> decimal, t<yyyy-MM-dd HH:mm:ss.SSS> date,
//   s<text> text (\\, \t, \n, \r escaped), x<n> binary length, o<n> OLE length,
//   g<GUID> upper case, c attachment / multi-value.
import com.healthmarketscience.jackcess.*;
import com.healthmarketscience.jackcess.impl.*;
import com.healthmarketscience.jackcess.complex.ComplexValueForeignKey;
import java.io.File;
import java.math.BigDecimal;
import java.time.LocalDateTime;
import java.time.format.DateTimeFormatter;
import java.util.*;

public class DumpDb {
    static final DateTimeFormatter F = DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm:ss.SSS");

    static String esc(String s) {
        StringBuilder b = new StringBuilder();
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '\\': b.append("\\\\"); break;
                case '\t': b.append("\\t"); break;
                case '\n': b.append("\\n"); break;
                case '\r': b.append("\\r"); break;
                default: b.append(c);
            }
        }
        return b.toString();
    }

    static String token(Column col, Object v) throws Exception {
        if (v == null) return "~";
        if (v instanceof Boolean) return ((Boolean) v) ? "b1" : "b0";
        if (v instanceof Byte) return "i" + (((Byte) v) & 0xFF);
        if (v instanceof Short || v instanceof Integer || v instanceof Long) return "i" + v;
        if (v instanceof Float) return "f" + Integer.toHexString(Float.floatToRawIntBits((Float) v));
        if (v instanceof Double) return "d" + Long.toHexString(Double.doubleToRawLongBits((Double) v));
        if (v instanceof BigDecimal) {
            BigDecimal d = (BigDecimal) v;
            if (col.getType() == DataType.MONEY) return "m" + d.unscaledValue();
            return "n" + d.toPlainString();
        }
        if (v instanceof LocalDateTime) return "t" + F.format((LocalDateTime) v);
        if (v instanceof java.util.Date) {
            LocalDateTime t = LocalDateTime.ofInstant(((java.util.Date) v).toInstant(), java.time.ZoneOffset.UTC);
            return "t" + F.format(t);
        }
        if (v instanceof String) {
            if (col.getType() == DataType.GUID) return "g" + ((String) v).toUpperCase();
            return "s" + esc((String) v);
        }
        if (v instanceof byte[]) {
            return (col.getType() == DataType.OLE ? "o" : "x") + ((byte[]) v).length;
        }
        if (v instanceof ComplexValueForeignKey) return "c";
        return "?" + v.getClass().getSimpleName();
    }

    public static void main(String[] a) throws Exception {
        System.setOut(new java.io.PrintStream(new java.io.FileOutputStream(java.io.FileDescriptor.out), false, "UTF-8"));
        try (Database db = new DatabaseBuilder(new File(a[0])).setReadOnly(true).open()) {
            db.setDateTimeType(DateTimeType.LOCAL_DATE_TIME);
            db.setTimeZone(TimeZone.getTimeZone("UTC"));
            TreeSet<String> names = new TreeSet<>(db.getTableNames());
            for (String n : names) {
                Table t = db.getTable(n);
                System.out.println("T " + n);
                StringBuilder h = new StringBuilder("C");
                for (Column c : t.getColumns()) h.append('\t').append(esc(c.getName())).append(':').append(c.getType().getValue() & 0xFF);
                System.out.println(h);
                for (Row r : t) {
                    StringBuilder b = new StringBuilder("R");
                    for (Column c : t.getColumns()) b.append('\t').append(token(c, r.get(c.getName())));
                    System.out.println(b);
                }
            }
        }
        System.out.flush();
    }
}
