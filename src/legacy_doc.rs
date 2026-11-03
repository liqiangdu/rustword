//! Best-effort importer for the legacy Word 97-2003 binary format (.doc).
//!
//! Only the main document story is imported, as plain paragraphs. The format
//! has no modern writer here, so RustWord asks you to save imported text as a
//! new .docx rather than pretending to overwrite the original binary file.

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::Path;

use crate::docx::Document;

struct Piece {
    cp_start: usize,
    text: String,
}

pub fn load(path: &Path) -> Result<Document> {
    let mut compound = cfb::open(path)
        .with_context(|| format!("{} is not a readable legacy Word file", path.display()))?;
    let word = read_stream(&mut compound, "WordDocument")?;
    if word.len() < 0x200 {
        bail!("The legacy Word file is too small or truncated");
    }
    if read_u16(&word, 0)? != 0xA5EC {
        bail!("The legacy Word file has an unknown header (not Word 97-2003)");
    }
    let flags = read_u16(&word, 0x0A)?;
    if flags & 0x0100 != 0 {
        bail!("This .doc file is encrypted and cannot be opened without its password");
    }
    let table_name = if flags & 0x0200 != 0 {
        "1Table"
    } else {
        "0Table"
    };
    let table = read_stream(&mut compound, table_name)
        .with_context(|| format!("The legacy Word file has no {table_name} stream"))?;
    let ccp_text = read_u32(&word, 0x4C).unwrap_or(0) as usize;

    let (fc_clx, lcb_clx) = fib_pair(&word, 33)?;
    if lcb_clx == 0 {
        bail!("The legacy Word file has no piece table");
    }
    let start = fc_clx as usize;
    let end = start
        .checked_add(lcb_clx as usize)
        .context("Invalid legacy Word piece table offset")?;
    let clx = table
        .get(start..end)
        .context("Legacy Word piece table is outside the table stream")?;
    let pieces = parse_clx(clx, &word)?;

    let mut text = String::new();
    for piece in pieces {
        if ccp_text > 0 && piece.cp_start >= ccp_text {
            break;
        }
        let remaining = if ccp_text > 0 {
            ccp_text.saturating_sub(piece.cp_start)
        } else {
            usize::MAX
        };
        let mut piece_text = piece.text;
        if remaining < piece_text.encode_utf16().count() {
            let mut kept_units = 0;
            let mut cut = 0;
            for (index, ch) in piece_text.char_indices() {
                let ch_units = ch.len_utf16();
                if kept_units + ch_units > remaining {
                    cut = index;
                    break;
                }
                kept_units += ch_units;
            }
            piece_text.truncate(cut);
        }
        text.push_str(&piece_text);
    }

    let cleaned = clean_story(&text);
    let mut paragraphs: Vec<String> = cleaned
        .split('\n')
        .map(|line| line.replace('\t', "    ").trim_end().to_owned())
        .collect();
    while paragraphs.len() > 1 && paragraphs.last().is_some_and(|line| line.is_empty()) {
        paragraphs.pop();
    }
    if paragraphs.is_empty() {
        paragraphs.push(String::new());
    }

    Ok(Document::from_legacy(path, paragraphs))
}

fn read_stream<F: Read + std::io::Seek>(
    compound: &mut cfb::CompoundFile<F>,
    name: &str,
) -> Result<Vec<u8>> {
    let mut stream = compound
        .open_stream(name)
        .with_context(|| format!("Missing {name} stream"))?;
    let mut data = Vec::new();
    stream
        .read_to_end(&mut data)
        .with_context(|| format!("Could not read {name} stream"))?;
    Ok(data)
}

fn fib_pair(word: &[u8], index: usize) -> Result<(u32, u32)> {
    let mut offset = 32_usize;
    let csw = read_u16(word, offset)? as usize;
    offset = offset
        .checked_add(2 + csw * 2)
        .context("Invalid FIB width table")?;
    let cslw = read_u16(word, offset)? as usize;
    offset = offset
        .checked_add(2 + cslw * 4)
        .context("Invalid FIB length table")?;
    let count = read_u16(word, offset)? as usize;
    offset = offset.checked_add(2).context("Invalid FIB pair table")?;
    if index >= count {
        bail!("Legacy Word FIB does not contain the piece table location");
    }
    offset = offset
        .checked_add(index * 8)
        .context("Invalid FIB pair offset")?;
    let fc = read_u32(word, offset)?;
    let lcb = read_u32(word, offset + 4)?;
    Ok((fc, lcb))
}

fn parse_clx(clx: &[u8], word: &[u8]) -> Result<Vec<Piece>> {
    let mut offset = 0_usize;
    while offset < clx.len() {
        match clx[offset] {
            0x01 => {
                let cb = read_u16(clx, offset + 1)? as usize;
                offset = offset
                    .checked_add(3 + cb)
                    .context("Invalid legacy Word property block")?;
            }
            0x02 => {
                let lcb = read_u32(clx, offset + 1)? as usize;
                let start = offset + 5;
                let end = start
                    .checked_add(lcb)
                    .context("Invalid legacy Word piece table length")?;
                let plc = clx
                    .get(start..end)
                    .context("Legacy Word piece table is truncated")?;
                return parse_plcpcd(plc, word);
            }
            other => bail!("Unsupported legacy Word piece table marker 0x{other:02X}"),
        }
    }
    bail!("Legacy Word piece table has no Pcdt")
}

fn parse_plcpcd(plc: &[u8], word: &[u8]) -> Result<Vec<Piece>> {
    if plc.len() < 4 {
        bail!("Legacy Word piece table is empty");
    }
    let lcb = plc.len();
    if (lcb - 4) % 12 != 0 {
        bail!("Legacy Word piece table has an invalid size");
    }
    let count = (lcb - 4) / 12;
    let cp_base = 0;
    let pcd_base = 4 * (count + 1);
    let mut pieces = Vec::with_capacity(count);

    for index in 0..count {
        let cp_start = read_u32(plc, cp_base + index * 4)? as usize;
        let cp_end = read_u32(plc, cp_base + (index + 1) * 4)? as usize;
        if cp_end < cp_start {
            bail!("Legacy Word piece table has descending character positions");
        }
        let pcd = pcd_base + index * 8;
        let raw_fc = read_u32(plc, pcd + 2)?;
        let compressed = raw_fc & 0x4000_0000 != 0;
        let mut fc = (raw_fc & 0x3FFF_FFFF) as usize;
        let characters = cp_end - cp_start;

        let text = if compressed {
            fc /= 2;
            let bytes = word
                .get(fc..fc + characters)
                .context("Compressed legacy Word text is outside WordDocument")?;
            encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned()
        } else {
            let byte_count = characters
                .checked_mul(2)
                .context("Legacy Word text length overflow")?;
            let bytes = word
                .get(fc..fc + byte_count)
                .context("Unicode legacy Word text is outside WordDocument")?;
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        };

        pieces.push(Piece { cp_start, text });
    }

    Ok(pieces)
}

fn clean_story(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_field_instruction = false;
    for ch in text.chars() {
        match ch {
            '\u{0013}' => in_field_instruction = true,
            '\u{0014}' => in_field_instruction = false,
            '\u{0015}' => in_field_instruction = false,
            _ if in_field_instruction => {}
            '\r' => out.push('\n'),
            '\u{000B}' | '\u{000C}' => out.push('\n'),
            '\u{0007}' => out.push('\t'),
            '\u{001E}' => out.push('-'),
            '\u{001F}' | '\u{0001}' | '\u{0008}' => {}
            '\t' | '\n' => out.push(ch),
            ch if ch.is_control() => {}
            _ => out.push(ch),
        }
    }
    out
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data
        .get(offset..offset + 2)
        .context("Unexpected end of legacy Word data")?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .context("Unexpected end of legacy Word data")?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}
