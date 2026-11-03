#![allow(dead_code)]
//! Generates a tiny, valid Word 97-2003 binary document used as a test
//! fixture. It contains one text piece in the main document story.

use std::io::Write;
use std::path::Path;

fn write_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn main() -> anyhow::Result<()> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("samples")
        .join("legacy-sample.doc");
    let text = "RustWord legacy .doc fixture\r\
                This text was imported from a Word 97-2003 binary document.\r\
                Saving it creates a modern .docx copy.\r";
    let bytes = text.as_bytes();
    assert!(bytes.is_ascii());

    let text_offset = 0x800usize;
    let table = build_clx(bytes.len(), text_offset);
    let mut word = build_fib(text.len() as u32, 0, table.len() as u32);
    word.resize(text_offset, 0);
    word.extend_from_slice(bytes);

    let mut compound = cfb::create(&path)?;
    {
        let mut stream = compound.create_stream("WordDocument")?;
        stream.write_all(&word)?;
    }
    {
        let mut stream = compound.create_stream("1Table")?;
        stream.write_all(&table)?;
    }
    compound.flush()?;
    println!("{}", path.display());
    Ok(())
}

fn build_fib(ccp_text: u32, fc_clx: u32, lcb_clx: u32) -> Vec<u8> {
    let mut fib = vec![0_u8; 0x800];
    write_u16(&mut fib, 0x00, 0xA5EC); // wIdent
    write_u16(&mut fib, 0x02, 0x00C1); // nFib: Word 97
    write_u16(&mut fib, 0x06, 0x0409); // en-US
    write_u16(&mut fib, 0x0A, 0x0200); // fWhichTblStm = 1Table
    write_u16(&mut fib, 0x0C, 0x00C1); // nFibBack
    write_u32(&mut fib, 0x18, 0x0800); // fcMin
    write_u32(&mut fib, 0x1C, 0x0800 + ccp_text); // fcMac

    let mut offset = 32usize;
    write_u16(&mut fib, offset, 14); // csw
    offset += 2 + 14 * 2;
    write_u16(&mut fib, offset, 22); // cslw
    offset += 2;
    write_u32(&mut fib, offset + 12, ccp_text);
    offset += 22 * 4;
    write_u16(&mut fib, offset, 93); // cbRgFcLcb
    offset += 2;
    write_u32(&mut fib, offset + 33 * 8, fc_clx);
    write_u32(&mut fib, offset + 33 * 8 + 4, lcb_clx);
    fib
}

fn build_clx(characters: usize, text_offset: usize) -> Vec<u8> {
    let pieces = 1usize;
    let plc_len = 4 * (pieces + 1) + 8 * pieces;
    let mut plc = Vec::with_capacity(plc_len);
    plc.extend_from_slice(&0_u32.to_le_bytes());
    plc.extend_from_slice(&(characters as u32).to_le_bytes());
    plc.extend_from_slice(&0_u16.to_le_bytes()); // PCD flags
    let raw_fc = ((text_offset as u32) * 2) | 0x4000_0000;
    plc.extend_from_slice(&raw_fc.to_le_bytes());
    plc.extend_from_slice(&0_u16.to_le_bytes()); // prm

    let mut clx = Vec::with_capacity(plc.len() + 5);
    clx.push(0x02);
    clx.extend_from_slice(&(plc.len() as u32).to_le_bytes());
    clx.extend_from_slice(&plc);
    clx
}
