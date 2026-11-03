from docx import Document
from docx.shared import Pt, Inches, RGBColor
from docx.enum.text import WD_ALIGN_PARAGRAPH
from pathlib import Path

out = Path(__file__).resolve().parent / "rustword-demo.docx"
out.parent.mkdir(parents=True, exist_ok=True)

from PIL import Image, ImageDraw

banner = Image.new("RGB", (1200, 300), "#2B579A")
draw = ImageDraw.Draw(banner)
draw.rectangle((70, 65, 105, 235), fill="#FFFFFF")
draw.rectangle((135, 85, 760, 115), fill="#FFFFFF")
draw.rectangle((135, 145, 1000, 165), fill="#A9C4E8")
draw.rectangle((135, 190, 880, 210), fill="#A9C4E8")
draw.text((1000, 230), "RustWord", fill="#FFFFFF")
banner_path = out.with_name("rustword-banner.png")
banner.save(banner_path)

doc = Document()
section = doc.sections[0]
section.top_margin = Inches(0.9)
section.bottom_margin = Inches(0.9)
section.left_margin = Inches(1.0)
section.right_margin = Inches(1.0)

normal = doc.styles["Normal"]
normal.font.name = "Calibri"
normal.font.size = Pt(11)
normal.paragraph_format.space_after = Pt(8)

title = doc.add_paragraph(style="Title")
title.add_run("RustWord")
subtitle = doc.add_paragraph(style="Subtitle")
subtitle.add_run("A small, native Word editor written in Rust")

doc.add_paragraph("This is a live document. Put the cursor anywhere, edit the text, and press Ctrl+S. RustWord writes the changes back into the .docx while leaving styles, headers and the rest of the package untouched.")

doc.add_heading("Tables and images", level=2)
table = doc.add_table(rows=2, cols=3)
table.style = "Light Grid Accent 1"
rows = [
    ("Layer", "Technology", "Role"),
    ("Editing surface", "winit + egui", "Editable text, tables and images"),
]
for row, values in zip(table.rows, rows):
    for cell, value in zip(row.cells, values):
        cell.text = value
doc.add_picture(str(banner_path), width=Inches(5.6))

doc.add_heading("A different kind of word processor", level=1)
p = doc.add_paragraph()
p.add_run("RustWord treats a Word document as a sequence of paragraphs. It ")
r = p.add_run("preserves what it does not edit")
r.bold = True
p.add_run(", and rebuilds only the paragraphs you actually change. That keeps the code ")
r = p.add_run("small")
r.italic = True
p.add_run(" and the round-trip predictable. No account, no cloud, no telemetry.")

doc.add_heading("What works today", level=2)
for text in [
    "Open .docx, .docm and legacy .doc files",
    "Edit text with paragraph styles, bold, italic, underline, alignment and font size",
    "Save in place while preserving images, headers, styles and macros",
    "Import legacy .doc text and save it as a modern .docx copy",
]:
    doc.add_paragraph(text, style="List Bullet")

doc.add_heading("中文支持", level=2)
doc.add_paragraph("RustWord 会加载系统中文字体，因此这段中文可以正常显示、编辑并保存回 Word 文档。")

quote = doc.add_paragraph(style="Quote")
quote.add_run("“The most dangerous phrase in the language is: we’ve always done it this way.”")
quote.add_run(" — Grace Hopper")
quote.runs[1].italic = False

doc.add_heading("Try it", level=1)
p = doc.add_paragraph("Select this paragraph and press ")
r = p.add_run("Ctrl+B")
r.bold = True
p.add_run(" to make the whole paragraph bold. Change the paragraph style in the toolbar, then save and reopen the file in Word or LibreOffice.")

doc.core_properties.title = "RustWord demo"
doc.core_properties.author = "RustWord"
doc.save(out)
print(out)
