# SPDX-License-Identifier: AGPL-3.0-only
"""Builds tests/fixtures/drawing.vsdx, a small hand-written Visio drawing.

There is no Visio (and no Visio exporter in LibreOffice) to make a real one, so the package is
assembled from the VSDX schema with the standard library only: two pages, shapes with text and
fields, shape data, a hyperlink, a group, a picture, connectors, and a background page. The
output is deterministic. All text is synthetic; nothing here is, or contains, malware.

    python3 tools/fixtures/make_visio.py tests/fixtures/drawing.vsdx
"""
import sys
import zipfile

NS = ('xmlns="http://schemas.microsoft.com/office/visio/2012/main" '
      'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"')
REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
VREL = "http://schemas.microsoft.com/visio/2010/relationships"


def rels(items):
    body = "".join(f'<Relationship Id="{i}" Type="{t}" Target="{g}"/>' for i, t, g in items)
    return ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            + body + "</Relationships>")


CONTENT_TYPES = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
    '<Default Extension="xml" ContentType="application/xml"/>'
    '<Override PartName="/visio/document.xml" ContentType="application/vnd.ms-visio.drawing.main+xml"/>'
    '<Override PartName="/visio/pages/pages.xml" ContentType="application/vnd.ms-visio.pages+xml"/>'
    '<Override PartName="/visio/pages/page1.xml" ContentType="application/vnd.ms-visio.page+xml"/>'
    '<Override PartName="/visio/pages/page2.xml" ContentType="application/vnd.ms-visio.page+xml"/>'
    '</Types>')

PAGES = (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Pages {NS}>'
         '<Page ID="0" Name="Процесс заказа" NameU="Order process"><PageSheet><Cell N="PageWidth" V="8.5"/></PageSheet>'
         '<Rel r:id="rId1"/></Page>'
         '<Page ID="1" Name="Фон" NameU="Background" Background="1"><Rel r:id="rId2"/></Page></Pages>')

PAGE1 = (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><PageContents {NS}><Shapes>'
         '<Shape ID="1" Name="Start" Type="Shape"><Cell N="PinX" V="2"/><Cell N="PinY" V="9"/>'
         '<Text>Принять заказ</Text></Shape>'
         '<Shape ID="2" Name="Check" Type="Shape"><Cell N="PinX" V="2"/><Cell N="PinY" V="6"/>'
         '<Text><cp IX="0"/>Проверить <fld IX="0" Type="0">оплату</fld></Text>'
         '<Section N="Property"><Row N="Owner"><Cell N="Label" V="Ответственный"/><Cell N="Value" V="Анна"/></Row>'
         '<Row N="Cost"><Cell N="Value" V="1200"/></Row></Section>'
         '<Section N="Hyperlink"><Row N="Hyperlink1"><Cell N="Address" V="https://example.org/regulations"/>'
         '<Cell N="SubAddress" V="payment"/></Row></Section>'
         '<Section N="Geometry" IX="0"><Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row></Section>'
         '</Shape>'
         '<Shape ID="3" Name="Group" Type="Group"><Cell N="PinX" V="6"/><Cell N="PinY" V="6"/><Shapes>'
         '<Shape ID="4" Name="Inner" Type="Shape"><Cell N="PinX" V="1"/><Cell N="PinY" V="1"/><Text>Склад</Text></Shape>'
         '</Shapes></Shape>'
         '<Shape ID="5" Name="Logo" Type="Foreign"><Cell N="PinX" V="9"/><Cell N="PinY" V="9"/>'
         '<ForeignData ForeignType="Bitmap" CompressionType="PNG"><Rel r:id="rId7"/></ForeignData></Shape>'
         '<Shape ID="6" Name="Dynamic connector" Type="Shape"><Cell N="PinX" V="2"/><Cell N="PinY" V="7.5"/></Shape>'
         '</Shapes><Connects>'
         '<Connect FromSheet="6" FromCell="BeginX" ToSheet="1" ToCell="PinX"/>'
         '<Connect FromSheet="6" FromCell="EndX" ToSheet="2" ToCell="PinX"/></Connects></PageContents>')

PAGE2 = (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><PageContents {NS}><Shapes>'
         '<Shape ID="1" Name="Note" Type="Shape"><Text>Внутренний документ</Text></Shape></Shapes></PageContents>')

PARTS = [
    ("[Content_Types].xml", CONTENT_TYPES),
    ("_rels/.rels", rels([("rId1", REL + "/officeDocument", "visio/document.xml")])),
    ("visio/document.xml", f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><VisioDocument {NS}/>'),
    ("visio/_rels/document.xml.rels", rels([("rId1", VREL + "/pages", "pages/pages.xml")])),
    ("visio/pages/pages.xml", PAGES),
    ("visio/pages/_rels/pages.xml.rels",
     rels([("rId1", VREL + "/page", "page1.xml"), ("rId2", VREL + "/page", "page2.xml")])),
    ("visio/pages/page1.xml", PAGE1),
    ("visio/pages/page2.xml", PAGE2),
]


def main(out):
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        for name, text in PARTS:
            info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, text.encode("utf-8"))


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "drawing.vsdx")
