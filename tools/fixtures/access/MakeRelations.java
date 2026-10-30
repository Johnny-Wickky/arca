// SPDX-License-Identifier: AGPL-3.0-only
// Writes access-relations.accdb / .mdb for tests/fixtures with Jackcess (Apache-2.0), an independent
// implementation of the format: a Northwind-style database whose relationships stress the
// relationship diagram of Arca (every kind of relationship, in one database).
//
//   javac -encoding UTF-8 -cp jackcess.jar:commons-lang3.jar:commons-logging.jar -d out MakeRelations.java
//   java  -cp out:jackcess.jar:commons-lang3.jar:commons-logging.jar MakeRelations tests/fixtures
//
// then the expected reading is written by DumpDb, as for the other databases (see MakeAccess.java).
//
// The tables and relationships:
//   Customers 1-N Orders, Employees 1-N Orders, Shippers 1-N Orders        (three foreign keys of one table)
//   Orders 1-N OrderDetails (cascade delete and update), Products 1-N OrderDetails
//   OrderDetails 1-N OrderDetailNotes                                       (a two-column key)
//   Suppliers 1-N Products, Categories 1-N Products
//   Employees 1-N Employees (ReportsTo)                                     (a table related to itself)
//   Employees 1-N EmployeeTerritories N-1 Territories, Regions 1-N Territories   (a many-to-many by a junction)
//   Addresses 1-N Orders twice (ShipAddressID and BillAddressID)            (two lines between one pair)
//   Employees 1-1 EmployeePhotos                                            (one to one, through a unique key)
//   LegacyReport: a table with 40 columns and no relationship               (a table that is only listed)
import com.healthmarketscience.jackcess.*;
import com.healthmarketscience.jackcess.util.*;
import java.io.File;

public class MakeRelations {
    static Table table(Database db, String name, String pk, String... columns) throws Exception {
        TableBuilder tb = new TableBuilder(name);
        tb.addColumn(new ColumnBuilder(pk, DataType.LONG).setAutoNumber(true));
        for (String c : columns) {
            String[] nt = c.split(":");
            DataType t = nt.length > 1 && nt[1].equals("L") ? DataType.LONG : DataType.TEXT;
            tb.addColumn(new ColumnBuilder(nt[0], t));
        }
        tb.addIndex(new IndexBuilder(IndexBuilder.PRIMARY_KEY_NAME).addColumns(pk).setPrimaryKey());
        return tb.toTable(db);
    }

    static void rel(Database db, String parent, String child, String parentCol, String childCol, boolean cascade) throws Exception {
        RelationshipBuilder rb = new RelationshipBuilder(parent, child).addColumns(parentCol, childCol).setReferentialIntegrity();
        if (cascade) {
            rb.setCascadeDeletes().setCascadeUpdates();
        }
        rb.toRelationship(db);
    }

    static void make(File out, Database.FileFormat fmt) throws Exception {
        out.delete();
        try (Database db = DatabaseBuilder.create(fmt, out)) {
            table(db, "Customers", "CustomerID", "CompanyName", "ContactName", "City", "Country");
            table(db, "Employees", "EmployeeID", "LastName", "FirstName", "Title", "ReportsTo:L");
            table(db, "Shippers", "ShipperID", "CompanyName", "Phone");
            table(db, "Addresses", "AddressID", "Street", "City", "PostalCode");
            table(db, "Suppliers", "SupplierID", "CompanyName", "Country");
            table(db, "Categories", "CategoryID", "CategoryName", "Description");
            table(db, "Products", "ProductID", "ProductName", "SupplierID:L", "CategoryID:L", "UnitPrice");
            table(db, "Orders", "OrderID", "CustomerID:L", "EmployeeID:L", "ShipperID:L", "ShipAddressID:L", "BillAddressID:L", "OrderDate");

            TableBuilder details = new TableBuilder("OrderDetails")
                    .addColumn(new ColumnBuilder("OrderID", DataType.LONG))
                    .addColumn(new ColumnBuilder("ProductID", DataType.LONG))
                    .addColumn(new ColumnBuilder("Quantity", DataType.LONG))
                    .addIndex(new IndexBuilder(IndexBuilder.PRIMARY_KEY_NAME).addColumns("OrderID", "ProductID").setPrimaryKey());
            details.toTable(db);
            new TableBuilder("OrderDetailNotes")
                    .addColumn(new ColumnBuilder("NoteID", DataType.LONG).setAutoNumber(true))
                    .addColumn(new ColumnBuilder("OrderID", DataType.LONG))
                    .addColumn(new ColumnBuilder("ProductID", DataType.LONG))
                    .addColumn(new ColumnBuilder("Note", DataType.TEXT))
                    .addIndex(new IndexBuilder(IndexBuilder.PRIMARY_KEY_NAME).addColumns("NoteID").setPrimaryKey())
                    .toTable(db);

            table(db, "Regions", "RegionID", "RegionName");
            table(db, "Territories", "TerritoryID", "TerritoryName", "RegionID:L");
            new TableBuilder("EmployeeTerritories")
                    .addColumn(new ColumnBuilder("EmployeeID", DataType.LONG))
                    .addColumn(new ColumnBuilder("TerritoryID", DataType.LONG))
                    .addIndex(new IndexBuilder(IndexBuilder.PRIMARY_KEY_NAME).addColumns("EmployeeID", "TerritoryID").setPrimaryKey())
                    .toTable(db);
            new TableBuilder("EmployeePhotos")
                    .addColumn(new ColumnBuilder("EmployeeID", DataType.LONG))
                    .addColumn(new ColumnBuilder("Photo", DataType.TEXT))
                    .addIndex(new IndexBuilder(IndexBuilder.PRIMARY_KEY_NAME).addColumns("EmployeeID").setPrimaryKey())
                    .toTable(db);

            TableBuilder wide = new TableBuilder("LegacyReport").addColumn(new ColumnBuilder("Id", DataType.LONG).setAutoNumber(true));
            for (int i = 1; i <= 40; i++) {
                wide.addColumn(new ColumnBuilder("Field" + i, DataType.TEXT));
            }
            wide.addIndex(new IndexBuilder(IndexBuilder.PRIMARY_KEY_NAME).addColumns("Id").setPrimaryKey()).toTable(db);

            rel(db, "Customers", "Orders", "CustomerID", "CustomerID", false);
            rel(db, "Employees", "Orders", "EmployeeID", "EmployeeID", false);
            rel(db, "Shippers", "Orders", "ShipperID", "ShipperID", false);
            rel(db, "Addresses", "Orders", "AddressID", "ShipAddressID", false);
            RelationshipBuilder bill = new RelationshipBuilder("Addresses", "Orders").addColumns("AddressID", "BillAddressID")
                    .setReferentialIntegrity().setName("AddressesOrdersBill");
            bill.toRelationship(db);
            rel(db, "Orders", "OrderDetails", "OrderID", "OrderID", true);
            rel(db, "Products", "OrderDetails", "ProductID", "ProductID", false);
            new RelationshipBuilder("OrderDetails", "OrderDetailNotes").addColumns("OrderID", "OrderID").addColumns("ProductID", "ProductID")
                    .setReferentialIntegrity().toRelationship(db);
            rel(db, "Suppliers", "Products", "SupplierID", "SupplierID", false);
            rel(db, "Categories", "Products", "CategoryID", "CategoryID", false);
            rel(db, "Employees", "Employees", "EmployeeID", "ReportsTo", false);
            rel(db, "Employees", "EmployeeTerritories", "EmployeeID", "EmployeeID", true);
            rel(db, "Territories", "EmployeeTerritories", "TerritoryID", "TerritoryID", true);
            rel(db, "Regions", "Territories", "RegionID", "RegionID", false);
            rel(db, "Employees", "EmployeePhotos", "EmployeeID", "EmployeeID", true);
        }
    }

    public static void main(String[] args) throws Exception {
        File dir = new File(args.length > 0 ? args[0] : ".");
        make(new File(dir, "access-relations.accdb"), Database.FileFormat.V2010);
        make(new File(dir, "access-relations.mdb"), Database.FileFormat.V2000);
    }
}
