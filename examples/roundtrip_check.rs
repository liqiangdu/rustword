#![allow(dead_code)]

#[path = "../src/docx.rs"]
mod docx;
#[path = "../src/legacy_doc.rs"]
mod legacy_doc;

use std::path::Path;

fn main() -> anyhow::Result<()> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("samples")
        .join("rustword-rich.docx");
    let mut document = docx::load_docx(&source)?;

    let paragraph = document
        .body
        .blocks
        .iter_mut()
        .find_map(|block| match &mut block.kind {
            docx::BlockKind::Paragraph(paragraph)
                if paragraph.text.contains("live test fixture") =>
            {
                Some(paragraph)
            }
            _ => None,
        })
        .expect("demo paragraph");
    paragraph.text =
        "This paragraph was rewritten by RustWord and saved into the package.".to_owned();
    paragraph.format.bold = true;

    if let Some(table) = document
        .body
        .blocks
        .iter_mut()
        .find_map(|block| match &mut block.kind {
            docx::BlockKind::Table(table) => Some(table),
            _ => None,
        })
    {
        if let Some(docx::CellBlock::Paragraph(paragraph)) = table
            .rows
            .get_mut(1)
            .and_then(|row| row.cells.get_mut(1))
            .and_then(|cell| cell.blocks.get_mut(0))
        {
            paragraph.text = "winit + egui (edited cell)".to_owned();
        }
    }

    let target = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("roundtrip-check.docx");
    let saved = docx::save(&document, &target)?;
    println!("{}", target.display());
    println!("paragraphs: {}", saved.body.all_paragraphs().len());
    Ok(())
}
