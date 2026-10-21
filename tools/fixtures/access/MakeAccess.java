// SPDX-License-Identifier: AGPL-3.0-only
// Writes the Access test databases of tests/fixtures with Jackcess (Apache-2.0), an independent
// implementation of the format:
//
//   javac -encoding UTF-8 -cp jackcess.jar:commons-lang3.jar:commons-logging.jar -d out MakeAccess.java
//   java  -cp out:jackcess.jar:... MakeAccess tests/fixtures
//
// then, for each database, the expected reading is written by DumpDb:
//
//   java -cp out:... DumpDb tests/fixtures/access-2000.mdb > tests/fixtures/access-2000.mdb.dump
//
// Jackcess can write Access 2000 and later, not Access 97; the Jet 3 reader was checked against
// real Access 97 files with the same dump (their licence does not allow committing them here).
import com.healthmarketscience.jackcess.*;
import java.io.File;
import java.math.BigDecimal;
import java.time.LocalDateTime;
import java.util.*;

public class MakeAccess {
    static final String[] NAMES = {
        "Alice Smith", "Bob O'Neil", "Иван Иванов", "Мария Петрова", "山田太郎", "田中 花子",
        "Žluťoučký kůň", "Zoë Müller", "😀 Smile", "Tab\tNewline\nName", "", "x"
    };

    static String story(int n, int len) {
        StringBuilder b = new StringBuilder();
        int i = 0;
        while (b.length() < len) {
            b.append("Record ").append(n).append(" paragraph ").append(i++).append(": the quick brown fox ")
                    .append("jumps over the lazy dog; быстрая лиса прыгает; 速い茶色の狐. ");
        }
        return b.substring(0, len);
    }

    static void make(File out, Database.FileFormat fmt) throws Exception {
        out.delete();
        try (Database db = DatabaseBuilder.create(fmt, out)) {
            db.setDateTimeType(DateTimeType.LOCAL_DATE_TIME);

            Table customers = new TableBuilder("Customers")
                    .addColumn(new ColumnBuilder("ID", DataType.LONG).setAutoNumber(true))
                    .addColumn(new ColumnBuilder("Name", DataType.TEXT).setLengthInUnits(100))
                    .addColumn(new ColumnBuilder("Joined", DataType.SHORT_DATE_TIME))
                    .addColumn(new ColumnBuilder("Balance", DataType.MONEY))
                    .addColumn(new ColumnBuilder("Active", DataType.BOOLEAN))
                    .addColumn(new ColumnBuilder("Notes", DataType.MEMO))
                    .addColumn(new ColumnBuilder("Site", DataType.MEMO).setHyperlink(true))
                    .toTable(db);
            for (int i = 0; i < 24; i++) {
                int kind = i % 4;
                String note = kind == 0 ? null : kind == 1 ? "short note " + i : kind == 2 ? story(i, 2500) : story(i, 8200);
                customers.addRow(Column.AUTO_NUMBER, NAMES[i % NAMES.length],
                        LocalDateTime.of(2020 + i % 5, 1 + i % 12, 1 + i % 28, i % 24, (i * 7) % 60, (i * 13) % 60),
                        new BigDecimal(i * 1234 + 5).movePointLeft(2).setScale(4), i % 3 != 0, note,
                        i % 2 == 0 ? "Arca " + i + "#https://example.org/c/" + i + "#section#" : null);
            }

            Table orders = new TableBuilder("Orders")
                    .addColumn(new ColumnBuilder("OrderID", DataType.LONG).setAutoNumber(true))
                    .addColumn(new ColumnBuilder("Customer", DataType.LONG))
                    .addColumn(new ColumnBuilder("Placed", DataType.SHORT_DATE_TIME))
                    .addColumn(new ColumnBuilder("Total", DataType.MONEY))
                    .addColumn(new ColumnBuilder("Rate", DataType.FLOAT))
                    .addColumn(new ColumnBuilder("Weight", DataType.DOUBLE))
                    .addColumn(new ColumnBuilder("Paid", DataType.BOOLEAN))
                    .addColumn(new ColumnBuilder("Comment", DataType.TEXT).setLengthInUnits(80))
                    .toTable(db);
            for (int i = 0; i < 240; i++) {
                orders.addRow(Column.AUTO_NUMBER, 1 + i % 24,
                        LocalDateTime.of(2019 + i % 6, 1 + i % 12, 1 + i % 27, 8 + i % 10, i % 60, 0),
                        new BigDecimal(i * 37 + 1).movePointLeft(2).setScale(4),
                        i / 7.0f, i * 1.25 - 100.0 / 3.0, i % 2 == 0,
                        i % 5 == 0 ? null : (i % 7 == 0 ? "Заказ №" + i : "order " + i));
            }
            // Delete every seventh order: the rows stay in the file until the database is compacted.
            Cursor cur = CursorBuilder.createCursor(orders);
            int n = 0;
            for (Row r = cur.getNextRow(); r != null; r = cur.getNextRow()) {
                if (n++ % 7 == 3) cur.deleteCurrentRow();
            }

            Table all = new TableBuilder("Everything")
                    .addColumn(new ColumnBuilder("ByteCol", DataType.BYTE))
                    .addColumn(new ColumnBuilder("IntCol", DataType.INT))
                    .addColumn(new ColumnBuilder("LongCol", DataType.LONG))
                    .addColumn(new ColumnBuilder("MoneyCol", DataType.MONEY))
                    .addColumn(new ColumnBuilder("FloatCol", DataType.FLOAT))
                    .addColumn(new ColumnBuilder("DoubleCol", DataType.DOUBLE))
                    .addColumn(new ColumnBuilder("DateCol", DataType.SHORT_DATE_TIME))
                    .addColumn(new ColumnBuilder("TextCol", DataType.TEXT).setLengthInUnits(255))
                    .addColumn(new ColumnBuilder("BinaryCol", DataType.BINARY).setLengthInUnits(40))
                    .addColumn(new ColumnBuilder("OleCol", DataType.OLE))
                    .addColumn(new ColumnBuilder("MemoCol", DataType.MEMO))
                    .addColumn(new ColumnBuilder("GuidCol", DataType.GUID))
                    .addColumn(new ColumnBuilder("NumericCol", DataType.NUMERIC).setPrecision(12).setScale(3))
                    .addColumn(new ColumnBuilder("BoolCol", DataType.BOOLEAN))
                    .toTable(db);
            byte[] ole = new byte[9000];
            for (int i = 0; i < ole.length; i++) ole[i] = (byte) (i * 31);
            all.addRow((byte) 200, (short) -1234, 2_000_000_000, new BigDecimal("-12345.6789"), 3.14159f, -2.718281828459045,
                    LocalDateTime.of(1899, 12, 30, 6, 0, 0), "text with ünïcödé and 日本語", new byte[]{1, 2, 3, 4, 5}, ole,
                    story(7, 300), "{12345678-9ABC-DEF0-1234-56789ABCDEF0}", new BigDecimal("-98765.432"), true);
            all.addRow((byte) 0, (short) 0, 0, new BigDecimal("0.0000"), 0f, 0.0, LocalDateTime.of(1899, 12, 29, 18, 0, 0),
                    "", new byte[0], null, "", "{00000000-0000-0000-0000-000000000000}", new BigDecimal("0.000"), false);
            all.addRow(null, null, null, null, null, null, null, null, null, null, null, null, null, false);
            all.addRow((byte) 255, Short.MAX_VALUE, Integer.MIN_VALUE, new BigDecimal("922337203685477.5807"), Float.MAX_VALUE,
                    Double.MIN_VALUE, LocalDateTime.of(9999, 12, 31, 23, 59, 59), "end", new byte[]{(byte) 0xFF}, new byte[]{0},
                    "end", "{FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF}", new BigDecimal("999999999.999"), true);
        }
    }

    public static void main(String[] a) throws Exception {
        File dir = new File(a[0]);
        make(new File(dir, "access-2000.mdb"), Database.FileFormat.V2000);
        make(new File(dir, "access-2016.accdb"), Database.FileFormat.V2016);
    }
}
