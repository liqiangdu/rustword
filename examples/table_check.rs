#![allow(dead_code)]

#[path = "../src/docx.rs"]
mod docx;
#[path = "../src/legacy_doc.rs"]
mod legacy_doc;

use std::path::Path;

fn main() -> anyhow::Result<()> {
    let mut document = docx::Document::empty();
    let table_id = document.body.alloc_id();
    let mut next = document.body.next_id;
    let mut table = docx::Table::new(table_id, 2, 3, &mut next);
    document.body.next_id = next;
    table.rows[0].cells[0].blocks[0] = docx::CellBlock::Paragraph(docx::Paragraph::new(
        100,
        "Header A",
        docx::Format::default(),
        true,
    ));
    table.rows[0].cells[1].blocks[0] = docx::CellBlock::Paragraph(docx::Paragraph::new(
        101,
        "Header B",
        docx::Format::default(),
        true,
    ));
    table.rows[0].cells[2].blocks[0] = docx::CellBlock::Paragraph(docx::Paragraph::new(
        102,
        "Header C",
        docx::Format::default(),
        true,
    ));
    document.body.blocks.push(docx::Block {
        id: table_id,
        kind: docx::BlockKind::Table(table),
    });
    let target = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("table-check.docx");
    docx::save(&document, &target)?;
    println!("{}", target.display());
    Ok(())
}
