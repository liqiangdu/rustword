# I Built a Word Editor in Rust. Real .docx Files Taught Me More Than Any Spec.

*A small native editor that opens real Word documents, edits text, tables and images, paginates them, and writes the changes back without rebuilding the whole package.*

RustWord started with a deliberately modest promise: open a `.docx`, let me change the words, and put it back without damaging everything else. No cloud account. No collaboration server. No attempt to reimplement thirty years of Word layout. Just a small native Windows app, written in Rust, that treats a Word file like a package instead of a canvas.

That promise turned out to be enough to make a useful editor, and difficult enough to be interesting.

The repository is public: [github.com/liqiangdu/rustword](https://github.com/liqiangdu/rustword).

## A .docx is a ZIP, but that is the easy part

The first version could unzip a document, find `word/document.xml`, pull out the text, and show it in a window. That took very little code. It also taught me almost nothing about Word files.

A real `.docx` is a package with many moving parts:

- `document.xml` contains the body.
- `styles.xml` defines paragraph and run formatting.
- `numbering.xml` describes lists.
- `comments.xml` holds review comments.
- `header*.xml` and `footer*.xml` hold page furniture.
- relationships connect parts to images, hyperlinks, footnotes and other resources.
- section properties define paper size, margins and page behavior.
- media parts store images in whatever format the producer chose.

A parser can ignore all of that and still display some text. A writer cannot ignore it without losing data. The moment you rewrite a document, every part you did not understand becomes a liability.

So the central design rule became: **do not rewrite what you did not edit.**

## The block model

RustWord models the body as a sequence of blocks:

- paragraphs
- tables
- inline images
- raw, preserved blocks

Each block keeps its original XML. A paragraph also keeps its original text and formatting, so the editor can tell whether it changed. A table keeps its table properties, grid, rows, cells, cell properties and original XML. An image keeps its relationship ID, media path, dimensions and drawing markup.

On save, unchanged blocks are emitted byte-for-byte from their original XML. Edited paragraphs and tables are rebuilt from the model. The writer opens the original ZIP, copies unrelated package parts through without recompressing them, adds new media and relationships when necessary, and replaces the destination through a temporary file.

That one rule produced a surprising amount of robustness. A document can contain features RustWord does not display and still survive a save. Comments, headers, footers, footnotes, custom styles, section settings and macros remain intact because the editor never tried to regenerate them.

It also makes the failure mode obvious. If I edit a paragraph, I am responsible for that paragraph. If I do not touch a table, the table is not my business.

## Tables broke first

The first table implementation looked reasonable in a synthetic test. It parsed rows and cells, rendered borders, and allowed text editing. Then I opened a real research report.

The report’s cover was a table. Several cells contained multiple paragraphs. And the app rendered those paragraphs side by side, squeezing the cover title into a narrow vertical strip.

The bug was a layout assumption. A table row is horizontal. A cell is vertical. I had reused the wrong container inside every cell, so the first paragraph occupied one "column" and the next paragraph occupied the next one. The fix was almost embarrassingly small: every cell now uses a vertical layout, regardless of the row’s horizontal structure.

That was the first hint that the real document would be the test suite.

The report also exposed ignored geometry. Word tables declare a preferred width, row heights, cell margins and vertical alignment. The cover was supposed to be a tall exact-height row with generous padding and centered content. Without those properties it collapsed into a thin band. RustWord now reads and applies:

- `w:tblW` for table width
- `w:trHeight` for row height
- `w:tcMar` for cell margins
- `w:vAlign` for top, center and bottom alignment

The cover finally looked like a cover: a large dark panel, centered text, and a metrics table below it.

## The charts that were not PNGs

The same report contained four charts. They appeared in the document as PNG images. RustWord showed nothing.

The files had `.png` names, but the bytes were WebP. That is legal enough for Word to display, because Word looks at the actual image data. RustWord was trusting the extension. Enabling WebP decoding in the `image` crate fixed it, and the charts appeared.

This is a small example of a larger lesson: Office documents are full of claims that are not quite true. A relationship can point to a part whose extension says one thing and whose bytes say another. A style can inherit from a style that inherits from another. A table can be a layout device rather than a grid of data.

The editor has to be suspicious in useful ways.

## Pagination is layout, not scrolling

For a long time RustWord was a continuous sheet. That is easy to scroll and easy to implement, but it is not what a Word document is.

A Word document has pages. It has a paper size, margins and section breaks. Text flows from one page to the next. A serious editor should show that structure.

RustWord now reads page size and margins from the document’s `sectPr`, measures each block at the current zoom level, and assigns blocks to pages. It honors section breaks and `pageBreakBefore`, and it draws page numbers in the footer area.

There are two views:

- **Single** page view, with previous and next page controls.
- **Multi** page view, a continuous stack of pages with visible gaps.

Pagination is deliberately page-oriented rather than a full layout engine. A table is moved to the next page when it does not fit; tables are not split across pages. Paragraphs are kept whole. That produces some white space at page bottoms, but it avoids the much worse failure of clipping content.

Real Word layout can break lines inside paragraphs and rows inside tables. RustWord is not there yet. It shows pages, respects the document’s page geometry, and remains honest about its limits.

## Zoom without zooming the UI

Zoom is one of those features that sounds trivial until the toolbar starts growing.

The obvious implementation is to scale the entire egui interface. That makes the menus and toolbar enormous, which is not what a document editor should do. The page should zoom; the application chrome should not.

RustWord applies zoom manually to the document surface: font sizes, paragraph spacing, table widths, row heights, cell margins, image dimensions and page margins. The toolbar stays the same size. The user can select 40% to 200%, press the plus and minus buttons, or use **Fit** to fit the whole page to the window.

Keyboard shortcuts cover the common cases: `Ctrl++`, `Ctrl+-`, `Ctrl+0`, plus `Ctrl+1` and `Ctrl+2` for single-page and multi-page view.

## The real test

I used a 21-table, 4-chart investment research report as the acceptance test. It was not a synthetic fixture. It had a designed cover, callout boxes, eight-column scenario tables, mixed Chinese and Latin text, embedded charts, comments, a header, a footer, footnotes and multiple sections.

RustWord loaded:

- 553 editable paragraphs
- 21 tables
- 4 images

A round-trip save produced the same 553 paragraphs, 21 tables and 4 images. The text comparison showed only the two cells I intentionally edited. Comments, header, footer, footnotes and media parts were preserved.

That does not mean RustWord is a general Word replacement. It means the conservative model works on a document that was not designed for it.

## What RustWord is not

It is important to be clear about the boundaries.

RustWord is not a full Word layout engine. It does not render headers and footers in the editor, although it preserves them in the file. It does not split table rows across pages. It does not implement find and replace, spell check, tracked changes, comments UI or PDF export.

It is a focused editor for people who want to open a document, make ordinary text, table or image changes, and save it without running the file through a lossy converter.

That focus is the point. A small tool with a narrow promise can be useful. A small tool pretending to be Word would not be.

## The stack

RustWord is built with:

- **Rust** for the application and document engine.
- **eframe/egui** for the native UI.
- **roxmltree** for OOXML parsing.
- **zip** for package handling.
- **cfb** for legacy Word 97 `.doc` import.
- **image** for PNG, JPEG and WebP decoding.
- **rfd** for native file dialogs.

The release build is a single portable Windows executable of roughly 6 MB. There is no runtime, installer or account requirement.

The project has 12 tests covering DOCX parsing, table and image editing, page settings and break markers, in-place saving and legacy `.doc` import. GitHub Actions runs formatting, tests and a release build on every push.

## Try it

- Repository: [github.com/liqiangdu/rustword](https://github.com/liqiangdu/rustword)
- Latest release: [RustWord 0.3.0](https://github.com/liqiangdu/rustword/releases/tag/v0.3.0)
- Windows build: [RustWord-0.3.0-win64.zip](https://github.com/liqiangdu/rustword/releases/download/v0.3.0/RustWord-0.3.0-win64.zip)

The sample document is in `samples/rustword-demo.docx`. Open it, move the cursor, edit a table cell, insert an image, switch between single-page and multi-page view, and save.

If you find a document that breaks, open an issue with a redacted sample. Real files are the best bug reports.

## What I learned

The most useful lesson was not about Rust or egui. It was about preservation.

When you edit a complex file format, the safest feature is often the one you do not implement. Every part you leave untouched is a part that cannot be corrupted. Every block you preserve byte-for-byte is a promise kept.

RustWord is small because it refuses to own the whole document. It owns the edits.

That turned out to be a good place to start.

#RustLang #OpenSource #DOCX
