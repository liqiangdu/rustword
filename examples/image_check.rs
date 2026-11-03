#![allow(dead_code)]

#[path = "../src/docx.rs"]
mod docx;
#[path = "../src/legacy_doc.rs"]
mod legacy_doc;

use std::path::Path;

fn main() -> anyhow::Result<()> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("samples")
        .join("rustword-demo.docx");
    let mut document = docx::load_docx(&source)?;

    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 120, 60);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        let mut pixels = Vec::with_capacity(120 * 60 * 4);
        for y in 0..60 {
            for x in 0..120 {
                pixels.extend_from_slice(&[(x * 2) as u8, (y * 4) as u8, 180, 255]);
            }
        }
        writer.write_image_data(&pixels)?;
    }

    let fresh_bytes = bytes.clone();
    let image = document.add_image(bytes, "png", 120, 60, "Inserted swatch");
    let id = image.id;
    document.body.insert_block_after(
        None,
        docx::Block {
            id,
            kind: docx::BlockKind::Image(image),
        },
    );

    let target = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("image-check.docx");
    docx::save(&document, &target)?;
    println!("{}", target.display());

    let mut fresh = docx::Document::empty();
    let image = fresh.add_image(fresh_bytes, "png", 120, 60, "Fresh swatch");
    let id = image.id;
    fresh.body.insert_block_after(
        None,
        docx::Block {
            id,
            kind: docx::BlockKind::Image(image),
        },
    );
    let fresh_target = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("new-image-check.docx");
    docx::save(&fresh, &fresh_target)?;
    println!("{}", fresh_target.display());
    Ok(())
}
