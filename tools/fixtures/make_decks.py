# SPDX-License-Identifier: AGPL-3.0-only
"""Generates the PPTX test decks (tests/fixtures/deck-rich.pptx and a 400-slide stress deck).

Needs python-pptx and Pillow. The binary PPT versions are made from these with LibreOffice:

    python3 tools/fixtures/make_decks.py          # writes out/rich.pptx and out/big.pptx
    soffice --headless --convert-to ppt out/rich.pptx

All text is synthetic. Nothing here is, or contains, malware.
"""
import os

os.makedirs("out", exist_ok=True)
import sys
from pptx import Presentation
from pptx.util import Inches, Pt
from pptx.dml.color import RGBColor

def build(path, slides=6, big=False):
    prs = Presentation()
    # title slide
    s = prs.slides.add_slide(prs.slide_layouts[0])
    s.shapes.title.text = "Квартальный обзор 2026"
    s.placeholders[1].text = "Подзаголовок: отдел безопасности"
    s.notes_slide.notes_text_frame.text = "Заметка докладчика: не показывать клиенту"
    # bullets with levels
    s = prs.slides.add_slide(prs.slide_layouts[1])
    s.shapes.title.text = "Итоги и планы"
    tf = s.placeholders[1].text_frame
    tf.text = "Первый пункт"
    for lvl, t in [(1, "Вложенный пункт A"), (2, "Ещё глубже"), (0, "Второй пункт"), (1, "Вложенный B")]:
        p = tf.add_paragraph(); p.text = t; p.level = lvl
    s.notes_slide.notes_text_frame.text = "Вторая заметка\nвторая строка заметки"
    # text box + hyperlink + bold/italic
    s = prs.slides.add_slide(prs.slide_layouts[5])
    s.shapes.title.text = "Ссылки и текстовые поля"
    tb = s.shapes.add_textbox(Inches(1), Inches(2), Inches(6), Inches(1))
    p = tb.text_frame.paragraphs[0]
    r = p.add_run(); r.text = "Перейти на сайт"; r.hyperlink.address = "https://example.org/path?q=1"
    p2 = tb.text_frame.add_paragraph(); r2 = p2.add_run(); r2.text = "Жирный курсив"; r2.font.bold = True; r2.font.italic = True
    tb2 = s.shapes.add_textbox(Inches(1), Inches(4), Inches(6), Inches(1))
    tb2.text_frame.text = "Текст под фигурой — латиница é ü ñ, kanji 日本語"
    # table
    s = prs.slides.add_slide(prs.slide_layouts[5])
    s.shapes.title.text = "Таблица"
    tbl = s.shapes.add_table(3, 3, Inches(1), Inches(2), Inches(6), Inches(2)).table
    for r in range(3):
        for c in range(3):
            tbl.cell(r, c).text = f"R{r+1}C{c+1}"
    # hidden slide
    s = prs.slides.add_slide(prs.slide_layouts[1])
    s.shapes.title.text = "СКРЫТЫЙ СЛАЙД"
    s.placeholders[1].text = "секретный текст скрытого слайда"
    s._element.set("show", "0")
    # picture slide
    s = prs.slides.add_slide(prs.slide_layouts[5])
    s.shapes.title.text = "Картинка"
    from PIL import Image
    im = Image.new("RGB", (64, 64), (200, 30, 30)); im.save("red.png")
    s.shapes.add_picture("red.png", Inches(1), Inches(2))
    if big:
        for i in range(slides):
            s = prs.slides.add_slide(prs.slide_layouts[1])
            s.shapes.title.text = f"Слайд {i}"
            tf = s.placeholders[1].text_frame
            tf.text = "строка " + "lorem ipsum " * 20
            for j in range(10):
                p = tf.add_paragraph(); p.text = f"пункт {i}.{j} " + "dolor " * 10; p.level = j % 3
            s.notes_slide.notes_text_frame.text = f"заметка {i}"
    prs.save(path)

build("out/rich.pptx")
build("out/big.pptx", slides=400, big=True)
