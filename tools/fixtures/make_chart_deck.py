#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Writes tests/fixtures/charts.pptx: a deck whose slides hold every kind of chart Arca draws.

The chart parts are written the way PowerPoint writes them (c:chartSpace with the values cached in
c:numCache / c:strCache; the workbook behind a chart is not needed and not included), so the file shows
what the drawing makes of a real chart part. Standard library only:

    python3 tools/fixtures/make_chart_deck.py tests/fixtures/charts.pptx
"""
import sys
import zipfile

C = "http://schemas.openxmlformats.org/drawingml/2006/chart"
A = "http://schemas.openxmlformats.org/drawingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
P = "http://schemas.openxmlformats.org/presentationml/2006/main"
PR = "http://schemas.openxmlformats.org/package/2006/relationships"
CT = "http://schemas.openxmlformats.org/package/2006/content-types"
HDR = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'

MONTHS = ["Jan", "Feb", "Mar", "Apr", "May"]
REGIONS = ["Север", "Юг", "Запад", "Восток"]


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def str_cache(ref, items):
    pts = "".join(f'<c:pt idx="{i}"><c:v>{esc(v)}</c:v></c:pt>' for i, v in enumerate(items))
    return f'<c:strRef><c:f>{ref}</c:f><c:strCache><c:ptCount val="{len(items)}"/>{pts}</c:strCache></c:strRef>'


def num_cache(ref, items, fmt="General"):
    pts = "".join(
        f'<c:pt idx="{i}"><c:v>{v}</c:v></c:pt>' for i, v in enumerate(items) if v is not None
    )
    return (
        f'<c:numRef><c:f>{ref}</c:f><c:numCache><c:formatCode>{esc(fmt)}</c:formatCode>'
        f'<c:ptCount val="{len(items)}"/>{pts}</c:numCache></c:numRef>'
    )


def solid(rgb=None, scheme=None):
    inner = f'<a:srgbClr val="{rgb}"/>' if rgb else f'<a:schemeClr val="{scheme}"/>'
    return f"<a:solidFill>{inner}</a:solidFill>"


def ser(idx, name, cats, vals, color=None, kind="bar", fmt="General", labels="", extra="", dpts=""):
    col = "BCDEFGHIJ"[idx]
    sp = ""
    if color and kind == "line":
        sp = f'<c:spPr><a:ln w="28575" cap="rnd">{solid(color)}<a:round/></a:ln></c:spPr>'
    elif color:
        sp = f"<c:spPr>{solid(color)}</c:spPr>"
    inv = '<c:invertIfNegative val="0"/>' if kind == "bar" else ""
    return (
        f'<c:ser><c:idx val="{idx}"/><c:order val="{idx}"/>'
        f'<c:tx>{str_cache(f"Sheet1!${col}$1", [name])}</c:tx>{sp}{inv}{extra}{dpts}{labels}'
        f'<c:cat>{str_cache(f"Sheet1!$A$2:$A${len(cats) + 1}", cats)}</c:cat>'
        f'<c:val>{num_cache(f"Sheet1!${col}$2:${col}${len(cats) + 1}", vals, fmt)}</c:val>'
        f"</c:ser>"
    )


def dlbls(val=False, pct=False, cat=False, fmt=None):
    f = f'<c:numFmt formatCode="{esc(fmt)}" sourceLinked="0"/>' if fmt else ""
    return (
        f"<c:dLbls>{f}<c:spPr><a:noFill/></c:spPr>"
        f'<c:showLegendKey val="0"/><c:showVal val="{int(val)}"/><c:showCatName val="{int(cat)}"/>'
        f'<c:showSerName val="0"/><c:showPercent val="{int(pct)}"/><c:showBubbleSize val="0"/></c:dLbls>'
    )


def cat_ax(delete=False, title=None, pos="b", grid=False, reverse=False):
    t = ""
    if title:
        t = f'<c:title><c:tx><c:rich><a:bodyPr/><a:p><a:r><a:t>{esc(title)}</a:t></a:r></a:p></c:rich></c:tx><c:overlay val="0"/></c:title>'
    return (
        f'<c:catAx><c:axId val="111"/><c:scaling><c:orientation val="{"maxMin" if reverse else "minMax"}"/></c:scaling>'
        f'<c:delete val="{int(delete)}"/><c:axPos val="{pos}"/>{t}<c:numFmt formatCode="General" sourceLinked="1"/>'
        f'<c:majorTickMark val="none"/><c:minorTickMark val="none"/><c:tickLblPos val="nextTo"/>'
        f'<c:crossAx val="222"/><c:crosses val="autoZero"/><c:auto val="1"/><c:lblAlgn val="ctr"/><c:lblOffset val="100"/><c:noMultiLvlLbl val="0"/></c:catAx>'
    )


def val_ax(title=None, pos="l", grid=True, fmt="General", linked=True, between="between", mn=None, mx=None, major=None):
    t = ""
    if title:
        t = f'<c:title><c:tx><c:rich><a:bodyPr/><a:p><a:r><a:t>{esc(title)}</a:t></a:r></a:p></c:rich></c:tx><c:overlay val="0"/></c:title>'
    g = '<c:majorGridlines><c:spPr><a:ln w="9525"><a:solidFill><a:srgbClr val="D9D9D9"/></a:solidFill></a:ln></c:spPr></c:majorGridlines>' if grid else ""
    sc = "<c:scaling><c:orientation val=\"minMax\"/>"
    if mx is not None:
        sc += f'<c:max val="{mx}"/>'
    if mn is not None:
        sc += f'<c:min val="{mn}"/>'
    sc += "</c:scaling>"
    mu = f'<c:majorUnit val="{major}"/>' if major else ""
    return (
        f'<c:valAx><c:axId val="222"/>{sc}<c:delete val="0"/><c:axPos val="{pos}"/>{g}{t}'
        f'<c:numFmt formatCode="{esc(fmt)}" sourceLinked="{int(linked)}"/><c:majorTickMark val="none"/><c:minorTickMark val="none"/>'
        f'<c:tickLblPos val="nextTo"/><c:crossAx val="111"/><c:crosses val="autoZero"/><c:crossBetween val="{between}"/>{mu}</c:valAx>'
    )


def chart_space(plot, title=None, legend="b", size=1000):
    t = ""
    if title:
        t = (
            f'<c:title><c:tx><c:rich><a:bodyPr/><a:p><a:pPr><a:defRPr sz="1400" b="0"/></a:pPr>'
            f'<a:r><a:rPr lang="en-US" sz="1400"/><a:t>{esc(title)}</a:t></a:r></a:p></c:rich></c:tx><c:overlay val="0"/></c:title>'
        )
    lg = f'<c:legend><c:legendPos val="{legend}"/><c:overlay val="0"/></c:legend>' if legend else ""
    return (
        HDR
        + f'<c:chartSpace xmlns:c="{C}" xmlns:a="{A}" xmlns:r="{R}"><c:date1904 val="0"/><c:roundedCorners val="0"/>'
        f'<c:chart>{t}<c:autoTitleDeleted val="{int(not title)}"/><c:plotArea><c:layout/>{plot}</c:plotArea>{lg}'
        f'<c:plotVisOnly val="1"/><c:dispBlanksAs val="gap"/></c:chart>'
        f'<c:spPr><a:noFill/><a:ln><a:noFill/></a:ln></c:spPr>'
        f'<c:txPr><a:bodyPr/><a:lstStyle/><a:p><a:pPr><a:defRPr sz="{size}"/></a:pPr><a:endParaRPr lang="en-US"/></a:p></c:txPr>'
        f'</c:chartSpace>'
    )


def bar_plot(series, direction="col", grouping="clustered", overlap=None, gap=150, axes=""):
    ov = f'<c:overlap val="{overlap}"/>' if overlap is not None else ""
    return (
        f'<c:barChart><c:barDir val="{direction}"/><c:grouping val="{grouping}"/><c:varyColors val="0"/>{series}'
        f'<c:gapWidth val="{gap}"/>{ov}<c:axId val="111"/><c:axId val="222"/></c:barChart>{axes}'
    )


def line_plot(series, grouping="standard", axes="", markers=True):
    return (
        f'<c:lineChart><c:grouping val="{grouping}"/><c:varyColors val="0"/>{series}'
        f'<c:marker val="{int(markers)}"/><c:axId val="111"/><c:axId val="222"/></c:lineChart>{axes}'
    )


def area_plot(series, grouping="stacked", axes=""):
    return (
        f'<c:areaChart><c:grouping val="{grouping}"/><c:varyColors val="0"/>{series}'
        f'<c:axId val="111"/><c:axId val="222"/></c:areaChart>{axes}'
    )


def pie_plot(series, doughnut=False, hole=55, first=0):
    if doughnut:
        return f'<c:doughnutChart><c:varyColors val="1"/>{series}<c:firstSliceAng val="{first}"/><c:holeSize val="{hole}"/></c:doughnutChart>'
    return f'<c:pieChart><c:varyColors val="1"/>{series}<c:firstSliceAng val="{first}"/></c:pieChart>'


def charts():
    out = []
    # 1. clustered columns, three series, title, legend at the bottom, grid lines
    s = (
        ser(0, "Plan", MONTHS, [30, 42, 38, 55, 61], "4472C4")
        + ser(1, "Fact", MONTHS, [28, 45, 33, 58, 70], "ED7D31")
        + ser(2, "Last year", MONTHS, [20, 25, 31, 40, 44], "A5A5A5")
    )
    out.append(("Sales by month", chart_space(bar_plot(s, axes=cat_ax() + val_ax()), "Sales by month")))
    # 2. stacked horizontal bars with labels inside and a legend on the right
    s = (
        ser(0, "Север", MONTHS, [12, 18, 9, 22, 15], None, labels=dlbls(val=True))
        + ser(1, "Юг", MONTHS, [8, 11, 14, 10, 19], None, labels=dlbls(val=True))
        + ser(2, "Запад", MONTHS, [5, 6, 7, 9, 12], None, labels=dlbls(val=True))
    )
    out.append(("Stacked bars", chart_space(
        bar_plot(s, "bar", "stacked", 100, 60, axes=cat_ax(pos="l") + val_ax(pos="b")), "Stacked bars", "r")))
    # 3. lines with markers, one of them smooth, one with a gap
    s = (
        ser(0, "Visitors", MONTHS, [120, 150, 135, 180, 210], "4472C4", "line", extra='<c:marker><c:symbol val="circle"/><c:size val="5"/></c:marker>')
        + ser(1, "Buyers", MONTHS, [30, 45, None, 70, 95], "ED7D31", "line", extra='<c:marker><c:symbol val="none"/></c:marker>')
        + ser(2, "Trend", MONTHS, [100, 140, 150, 170, 200], "70AD47", "line", extra='<c:marker><c:symbol val="none"/></c:marker><c:smooth val="1"/>')
    )
    out.append(("Traffic", chart_space(line_plot(s, axes=cat_ax() + val_ax()), "Traffic")))
    # 4. stacked areas, points on the ticks
    s = ser(0, "Север", MONTHS, [12, 18, 9, 22, 15]) + ser(1, "Юг", MONTHS, [8, 11, 14, 10, 19]) + ser(2, "Запад", MONTHS, [5, 6, 7, 9, 12])
    out.append(("Areas", chart_space(area_plot(s, axes=cat_ax() + val_ax(between="midCat")), "Areas", "b")))
    # 5. pie with percentages and a legend
    s = ser(0, "Share", REGIONS, [38, 27, 21, 14], None, "pie", labels=dlbls(pct=True))
    out.append(("Pie", chart_space(pie_plot(s), "Share by region", "r")))
    # 6. doughnut with values and the first slice turned
    s = ser(0, "Share", REGIONS, [38, 27, 21, 14], None, "pie", labels=dlbls(val=True),
            dpts="".join(f'<c:dPt><c:idx val="{i}"/><c:bubble3D val="0"/><c:spPr>{solid(c)}</c:spPr></c:dPt>' for i, c in enumerate(["264478", "9E480E", "636363", "997300"])))
    out.append(("Doughnut", chart_space(pie_plot(s, True, 55, 90), "Doughnut", "b")))
    # 7. percent stacked columns with a percent axis and an axis title
    s = ser(0, "Север", MONTHS, [12, 18, 9, 22, 15]) + ser(1, "Юг", MONTHS, [8, 11, 14, 10, 19]) + ser(2, "Запад", MONTHS, [5, 6, 7, 9, 12])
    out.append(("Percent", chart_space(
        bar_plot(s, "col", "percentStacked", 100, 50, axes=cat_ax(title="Month") + val_ax(title="Share of sales", fmt="0%", linked=False)),
        None, "b")))
    # 8. columns with negative values, data labels and a fixed axis
    s = ser(0, "Profit", MONTHS, [12.5, -4.2, 8.8, -1.5, 15.1], "70AD47", fmt="0.0", labels=dlbls(val=True, fmt="0.0"))
    out.append(("Profit", chart_space(
        bar_plot(s, axes=cat_ax() + val_ax(fmt="0", linked=False, mn=-10, mx=20, major=5)), "Profit, M", None)))
    return out


def slide(frames):
    xs = []
    for i, (x, y, w, h, rid, name) in enumerate(frames):
        xs.append(
            f'<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="{i + 2}" name="{esc(name)}"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr>'
            f'<p:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{w}" cy="{h}"/></p:xfrm><a:graphic>'
            f'<a:graphicData uri="{C}"><c:chart xmlns:c="{C}" r:id="{rid}"/></a:graphicData></a:graphic></p:graphicFrame>'
        )
    return (
        HDR
        + f'<p:sld xmlns:a="{A}" xmlns:r="{R}" xmlns:p="{P}"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>'
        + "".join(xs)
        + "</p:spTree></p:cSld></p:sld>"
    )


def rels(items):
    body = "".join(f'<Relationship Id="{i}" Type="{t}" Target="{g}"/>' for i, t, g in items)
    return f'{HDR}<Relationships xmlns="{PR}">{body}</Relationships>'


def main(out):
    cs = charts()
    W, H = 12192000, 6858000
    cw, ch = W // 3 - 120000, H // 2 - 150000
    cells = [(100000 + (i % 3) * (W // 3), 100000 + (i // 3) * (H // 2)) for i in range(6)]
    slide1 = [(x, y, cw, ch, f"rId{10 + i}", cs[i][0]) for i, (x, y) in enumerate(cells)]
    slide2 = [
        (200000, 200000, W // 2 - 300000, H - 400000, "rId10", cs[6][0]),
        (W // 2 + 100000, 200000, W // 2 - 300000, H - 400000, "rId11", cs[7][0]),
    ]
    parts = {}
    parts["[Content_Types].xml"] = (
        HDR + f'<Types xmlns="{CT}"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/>'
        '<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>'
        '<Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>'
        '<Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>'
        '<Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/>'
        '<Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/>'
        '<Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>'
        + "".join(
            f'<Override PartName="/ppt/charts/chart{i + 1}.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/>'
            for i in range(len(cs))
        )
        + "</Types>"
    )
    parts["_rels/.rels"] = rels([("rId1", "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument", "ppt/presentation.xml")])
    parts["ppt/presentation.xml"] = (
        HDR + f'<p:presentation xmlns:a="{A}" xmlns:r="{R}" xmlns:p="{P}"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst>'
        '<p:sldIdLst><p:sldId id="256" r:id="rId11"/><p:sldId id="257" r:id="rId12"/></p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/></p:presentation>'
    )
    parts["ppt/_rels/presentation.xml.rels"] = rels([
        ("rId1", R + "/slideMaster", "slideMasters/slideMaster1.xml"),
        ("rId11", R + "/slide", "slides/slide1.xml"),
        ("rId12", R + "/slide", "slides/slide2.xml"),
    ])
    grp = '<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>'
    parts["ppt/slideMasters/slideMaster1.xml"] = (
        HDR + f'<p:sldMaster xmlns:a="{A}" xmlns:r="{R}" xmlns:p="{P}"><p:cSld><p:spTree>{grp}</p:spTree></p:cSld>'
        '<p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/></p:sldMaster>'
    )
    parts["ppt/slideMasters/_rels/slideMaster1.xml.rels"] = rels([
        ("rId1", R + "/slideLayout", "../slideLayouts/slideLayout1.xml"),
        ("rId2", R + "/theme", "../theme/theme1.xml"),
    ])
    parts["ppt/slideLayouts/slideLayout1.xml"] = (
        HDR + f'<p:sldLayout xmlns:a="{A}" xmlns:r="{R}" xmlns:p="{P}"><p:cSld><p:spTree>{grp}</p:spTree></p:cSld></p:sldLayout>'
    )
    parts["ppt/slideLayouts/_rels/slideLayout1.xml.rels"] = rels([("rId1", R + "/slideMaster", "../slideMasters/slideMaster1.xml")])
    parts["ppt/theme/theme1.xml"] = (
        HDR + f'<a:theme xmlns:a="{A}" name="Office"><a:themeElements><a:clrScheme name="Office">'
        '<a:dk1><a:srgbClr val="000000"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2>'
        '<a:accent1><a:srgbClr val="4472C4"/></a:accent1><a:accent2><a:srgbClr val="ED7D31"/></a:accent2><a:accent3><a:srgbClr val="A5A5A5"/></a:accent3>'
        '<a:accent4><a:srgbClr val="FFC000"/></a:accent4><a:accent5><a:srgbClr val="5B9BD5"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6>'
        '<a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink></a:clrScheme></a:themeElements></a:theme>'
    )
    parts["ppt/slides/slide1.xml"] = slide(slide1)
    parts["ppt/slides/_rels/slide1.xml.rels"] = rels(
        [("rId1", R + "/slideLayout", "../slideLayouts/slideLayout1.xml")]
        + [(f"rId{10 + i}", R + "/chart", f"../charts/chart{i + 1}.xml") for i in range(6)]
    )
    parts["ppt/slides/slide2.xml"] = slide(slide2)
    parts["ppt/slides/_rels/slide2.xml.rels"] = rels(
        [("rId1", R + "/slideLayout", "../slideLayouts/slideLayout1.xml")]
        + [("rId10", R + "/chart", "../charts/chart7.xml"), ("rId11", R + "/chart", "../charts/chart8.xml")]
    )
    for i, (_, xml) in enumerate(cs):
        parts[f"ppt/charts/chart{i + 1}.xml"] = xml
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in parts.items():
            info = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, data)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "charts.pptx")
