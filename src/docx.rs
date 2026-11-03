//! WordprocessingML reading/writing for RustWord.
//!
//! The editable model is a tree of body blocks: paragraphs, tables, inline
//! images, and raw blocks preserved verbatim. A paragraph/image that was not
//! touched is written back from its original XML; edited objects are rebuilt
//! while retaining their paragraph/table/cell properties.
//!
//! The package writer copies every unrelated part (styles, headers, macros,
//! media, ...) without recompressing it and only replaces `document.xml`,
//! relationships/content types when new images require it.

use anyhow::{bail, Context, Result};
use roxmltree::{Document as XmlDocument, Node};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const WP_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const PIC_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
const REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const IMAGE_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
const EMU_PER_PIXEL: i64 = 9525;

// ---------------------------------------------------------------------------
// Formatting model
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParaStyle {
    Normal,
    Title,
    Subtitle,
    Heading1,
    Heading2,
    Heading3,
    Quote,
}

impl ParaStyle {
    pub const ALL: [Self; 6] = [
        Self::Normal,
        Self::Title,
        Self::Heading1,
        Self::Heading2,
        Self::Heading3,
        Self::Quote,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Title => "Title",
            Self::Subtitle => "Subtitle",
            Self::Heading1 => "Heading 1",
            Self::Heading2 => "Heading 2",
            Self::Heading3 => "Heading 3",
            Self::Quote => "Quote",
        }
    }

    pub fn style_id(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Title => "Title",
            Self::Subtitle => "Subtitle",
            Self::Heading1 => "Heading1",
            Self::Heading2 => "Heading2",
            Self::Heading3 => "Heading3",
            Self::Quote => "Quote",
        }
    }

    fn from_style_id(id: &str) -> Self {
        match id.to_ascii_lowercase().as_str() {
            "title" => Self::Title,
            "subtitle" => Self::Subtitle,
            "heading1" | "heading 1" => Self::Heading1,
            "heading2" | "heading 2" => Self::Heading2,
            "heading3" | "heading 3" => Self::Heading3,
            "quote" | "intensequote" | "intense quote" => Self::Quote,
            _ => Self::Normal,
        }
    }

    pub fn default_size(self) -> f32 {
        match self {
            Self::Normal => 11.0,
            Self::Title => 28.0,
            Self::Subtitle => 14.0,
            Self::Heading1 => 20.0,
            Self::Heading2 => 16.0,
            Self::Heading3 => 13.0,
            Self::Quote => 11.0,
        }
    }

    fn default_bold(self) -> bool {
        matches!(
            self,
            Self::Title | Self::Heading1 | Self::Heading2 | Self::Heading3
        )
    }

    fn default_italic(self) -> bool {
        matches!(self, Self::Subtitle | Self::Quote)
    }

    fn default_color(self) -> [u8; 3] {
        match self {
            Self::Title | Self::Heading1 => [31, 56, 100],
            Self::Heading2 | Self::Heading3 => [47, 84, 150],
            Self::Subtitle | Self::Quote => [89, 89, 89],
            Self::Normal => [31, 31, 31],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Center,
    Right,
    Justify,
}

impl Alignment {
    pub const ALL: [Self; 4] = [Self::Left, Self::Center, Self::Right, Self::Justify];

    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Center => "Center",
            Self::Right => "Right",
            Self::Justify => "Justify",
        }
    }

    fn xml_value(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
            Self::Justify => "both",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Format {
    pub style: ParaStyle,
    pub alignment: Alignment,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub size_pt: f32,
    pub color: [u8; 3],
    pub font: String,
}

impl Default for Format {
    fn default() -> Self {
        Self {
            style: ParaStyle::Normal,
            alignment: Alignment::Left,
            bold: false,
            italic: false,
            underline: false,
            size_pt: 11.0,
            color: [31, 31, 31],
            font: "Calibri".to_owned(),
        }
    }
}

impl Format {
    pub fn for_style(style: ParaStyle) -> Self {
        Self {
            style,
            size_pt: style.default_size(),
            bold: style.default_bold(),
            italic: style.default_italic(),
            color: style.default_color(),
            ..Self::default()
        }
    }

    pub fn apply_style(&mut self, style: ParaStyle) {
        self.style = style;
        self.size_pt = style.default_size();
        self.bold = style.default_bold();
        self.italic = style.default_italic();
        self.color = style.default_color();
        if !matches!(style, ParaStyle::Subtitle | ParaStyle::Quote) {
            self.underline = false;
        }
    }
}

// ---------------------------------------------------------------------------
// Editable document model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ParaPath {
    Body(usize),
    Cell {
        block: usize,
        row: usize,
        cell: usize,
        para: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ImagePath {
    Body(usize),
    Cell {
        block: usize,
        row: usize,
        cell: usize,
        image: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageSettings {
    pub width_twips: u32,
    pub height_twips: u32,
    pub margin_top: u32,
    pub margin_right: u32,
    pub margin_bottom: u32,
    pub margin_left: u32,
}

impl Default for PageSettings {
    fn default() -> Self {
        Self {
            width_twips: 11906,
            height_twips: 16838,
            margin_top: 1440,
            margin_right: 1440,
            margin_bottom: 1440,
            margin_left: 1440,
        }
    }
}

impl PageSettings {
    pub fn width_px(self) -> f32 {
        self.width_twips as f32 / 15.0
    }

    pub fn height_px(self) -> f32 {
        self.height_twips as f32 / 15.0
    }

    pub fn content_width_px(self) -> f32 {
        (self
            .width_twips
            .saturating_sub(self.margin_left + self.margin_right)) as f32
            / 15.0
    }

    pub fn content_height_px(self) -> f32 {
        (self
            .height_twips
            .saturating_sub(self.margin_top + self.margin_bottom)) as f32
            / 15.0
    }
}

#[derive(Clone, Debug)]
pub struct Paragraph {
    pub id: u64,
    pub text: String,
    pub format: Format,
    pub ppr: String,
    pub rpr: String,
    pub raw: String,
    pub original_text: String,
    pub original_format: Format,
    pub in_table: bool,
    pub page_break_before: bool,
    pub section_break_after: bool,
}

impl Paragraph {
    pub fn new(id: u64, text: impl Into<String>, format: Format, in_table: bool) -> Self {
        let text = text.into();
        let mut ppr = String::new();
        if format.style != ParaStyle::Normal || format.alignment != Alignment::Left {
            ppr = make_ppr(&format, "");
        }
        Self {
            id,
            original_text: text.clone(),
            original_format: format.clone(),
            text,
            format,
            ppr,
            rpr: String::new(),
            raw: String::new(),
            in_table,
            page_break_before: false,
            section_break_after: false,
        }
    }

    pub fn new_blank(id: u64, in_table: bool) -> Self {
        Self::new(id, "", Format::default(), in_table)
    }

    pub fn altered(&self) -> bool {
        self.text != self.original_text || self.format != self.original_format
    }

    pub fn needs_render(&self) -> bool {
        self.raw.is_empty() || self.altered()
    }
}

#[derive(Clone, Debug)]
pub struct ImageBlock {
    pub id: u64,
    pub doc_pr_id: u64,
    pub name: String,
    pub rel_id: String,
    pub media_target: String,
    pub width_emu: i64,
    pub height_emu: i64,
    pub original_width_emu: i64,
    pub original_height_emu: i64,
    pub raw: String,
}

impl ImageBlock {
    pub fn aspect(&self) -> f32 {
        if self.height_emu <= 0 {
            1.0
        } else {
            (self.width_emu as f32 / self.height_emu as f32).max(0.05)
        }
    }

    pub fn width_px(&self) -> f32 {
        (self.width_emu as f32 / EMU_PER_PIXEL as f32).max(24.0)
    }

    pub fn set_width_px(&mut self, width: f32) {
        let width = width.max(24.0);
        let height = width / self.aspect();
        self.width_emu = (width * EMU_PER_PIXEL as f32).round() as i64;
        self.height_emu = (height * EMU_PER_PIXEL as f32).round() as i64;
    }

    pub fn needs_render(&self) -> bool {
        self.raw.is_empty()
            || self.width_emu != self.original_width_emu
            || self.height_emu != self.original_height_emu
    }
}

#[derive(Clone, Debug)]
pub enum CellBlock {
    Paragraph(Paragraph),
    Image(ImageBlock),
    Raw(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerticalAlign {
    Top,
    Center,
    Bottom,
}

#[derive(Clone, Copy, Debug)]
pub struct CellMargins {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Default for CellMargins {
    fn default() -> Self {
        Self {
            left: 108,
            top: 0,
            right: 108,
            bottom: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TableCell {
    pub blocks: Vec<CellBlock>,
    pub tc_pr: String,
    pub grid_span: usize,
    pub shading: Option<[u8; 3]>,
    pub margins: CellMargins,
    pub vertical_align: VerticalAlign,
}

#[derive(Clone, Debug)]
pub struct TableRow {
    pub cells: Vec<TableCell>,
    pub tr_pr: String,
    pub height_twips: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Table {
    pub id: u64,
    pub raw: String,
    pub tbl_pr: String,
    pub tbl_grid: String,
    pub col_widths: Vec<u32>,
    pub width_twips: Option<u32>,
    pub rows: Vec<TableRow>,
    pub modified: bool,
    pub merged: bool,
}

impl Table {
    pub fn column_count(&self) -> usize {
        self.rows
            .iter()
            .map(|row| row.cells.iter().map(|cell| cell.grid_span.max(1)).sum())
            .max()
            .unwrap_or(1)
            .max(1)
    }

    pub fn cell_paragraphs_changed(&self) -> bool {
        self.rows.iter().any(|row| {
            row.cells.iter().any(|cell| {
                cell.blocks.iter().any(|block| match block {
                    CellBlock::Paragraph(paragraph) => paragraph.needs_render(),
                    CellBlock::Image(image) => image.needs_render(),
                    CellBlock::Raw(_) => false,
                })
            })
        })
    }

    pub fn needs_render(&self) -> bool {
        self.raw.is_empty() || self.modified || self.cell_paragraphs_changed()
    }

    pub fn rebuild_grid(&mut self) {
        let widths = if self.col_widths.is_empty() {
            vec![2400; self.column_count()]
        } else {
            self.col_widths.clone()
        };
        self.tbl_grid = format!(
            "<w:tblGrid>{}</w:tblGrid>",
            widths
                .iter()
                .map(|width| format!("<w:gridCol w:w=\"{width}\"/>"))
                .collect::<String>()
        );
    }

    pub fn new(id: u64, rows: usize, columns: usize, next_id: &mut u64) -> Self {
        let columns = columns.clamp(1, 12);
        let rows = rows.clamp(1, 100);
        let width = (7200 / columns as u32).max(900);
        let mut table_rows = Vec::with_capacity(rows);
        for _ in 0..rows {
            let mut cells = Vec::with_capacity(columns);
            for _ in 0..columns {
                let paragraph = Paragraph::new_blank(*next_id, true);
                *next_id += 1;
                cells.push(TableCell {
                    blocks: vec![CellBlock::Paragraph(paragraph)],
                    tc_pr: format!("<w:tcPr><w:tcW w:w=\"{width}\" w:type=\"dxa\"/></w:tcPr>"),
                    grid_span: 1,
                    shading: None,
                    margins: CellMargins::default(),
                    vertical_align: VerticalAlign::Top,
                });
            }
            table_rows.push(TableRow {
                cells,
                tr_pr: String::new(),
                height_twips: None,
            });
        }
        let grid = (0..columns)
            .map(|_| format!("<w:gridCol w:w=\"{width}\"/>"))
            .collect::<String>();
        Self {
            id,
            raw: String::new(),
            tbl_pr: format!(
                "<w:tblPr><w:tblW w:w=\"{total_width}\" w:type=\"dxa\"/>\
<w:tblBorders><w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"B7B1A5\"/>\
<w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"B7B1A5\"/>\
<w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"B7B1A5\"/>\
<w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"B7B1A5\"/>\
<w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"B7B1A5\"/>\
<w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"B7B1A5\"/>\
</w:tblBorders></w:tblPr>",
                total_width = width * columns as u32
            ),
            tbl_grid: format!("<w:tblGrid>{grid}</w:tblGrid>"),
            col_widths: vec![width; columns],
            width_twips: Some(width * columns as u32),
            rows: table_rows,
            modified: true,
            merged: false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum BlockKind {
    Paragraph(Paragraph),
    Table(Table),
    Image(ImageBlock),
    Raw(String),
}

#[derive(Clone, Debug)]
pub struct Block {
    #[allow(dead_code)]
    pub id: u64,
    pub kind: BlockKind,
}

pub struct Body {
    pub blocks: Vec<Block>,
    pub sect_pr: String,
    pub page: PageSettings,
    pub next_id: u64,
}

impl Body {
    pub fn empty() -> Self {
        let mut next_id = 1;
        let paragraph = Paragraph::new_blank(next_id, false);
        next_id += 1;
        Self {
            blocks: vec![Block {
                id: paragraph.id,
                kind: BlockKind::Paragraph(paragraph),
            }],
            sect_pr: default_sect_pr(),
            page: PageSettings::default(),
            next_id,
        }
    }

    pub fn from_texts(texts: Vec<String>) -> Self {
        let mut body = Self {
            blocks: Vec::new(),
            sect_pr: default_sect_pr(),
            page: PageSettings::default(),
            next_id: 1,
        };
        for text in texts {
            let id = body.alloc_id();
            body.push_paragraph(Paragraph::new(id, text, Format::default(), false));
        }
        if body.blocks.is_empty() {
            let id = body.alloc_id();
            body.push_paragraph(Paragraph::new_blank(id, false));
        }
        body
    }

    fn push_paragraph(&mut self, paragraph: Paragraph) {
        self.next_id = self.next_id.max(paragraph.id + 1);
        self.blocks.push(Block {
            id: paragraph.id,
            kind: BlockKind::Paragraph(paragraph),
        });
    }

    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        id
    }

    pub fn first_paragraph_path(&self) -> Option<ParaPath> {
        for (block_index, block) in self.blocks.iter().enumerate() {
            match &block.kind {
                BlockKind::Paragraph(_) => return Some(ParaPath::Body(block_index)),
                BlockKind::Table(table) => {
                    for (row_index, row) in table.rows.iter().enumerate() {
                        for (cell_index, cell) in row.cells.iter().enumerate() {
                            for (para_index, cell_block) in cell.blocks.iter().enumerate() {
                                if matches!(cell_block, CellBlock::Paragraph(_)) {
                                    return Some(ParaPath::Cell {
                                        block: block_index,
                                        row: row_index,
                                        cell: cell_index,
                                        para: para_index,
                                    });
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    pub fn paragraph(&self, path: &ParaPath) -> Option<&Paragraph> {
        match path {
            ParaPath::Body(index) => match &self.blocks.get(*index)?.kind {
                BlockKind::Paragraph(paragraph) => Some(paragraph),
                _ => None,
            },
            ParaPath::Cell {
                block,
                row,
                cell,
                para,
            } => match &self.blocks.get(*block)?.kind {
                BlockKind::Table(table) => {
                    match table.rows.get(*row)?.cells.get(*cell)?.blocks.get(*para)? {
                        CellBlock::Paragraph(paragraph) => Some(paragraph),
                        _ => None,
                    }
                }
                _ => None,
            },
        }
    }

    pub fn paragraph_mut(&mut self, path: &ParaPath) -> Option<&mut Paragraph> {
        match path {
            ParaPath::Body(index) => match &mut self.blocks.get_mut(*index)?.kind {
                BlockKind::Paragraph(paragraph) => Some(paragraph),
                _ => None,
            },
            ParaPath::Cell {
                block,
                row,
                cell,
                para,
            } => match &mut self.blocks.get_mut(*block)?.kind {
                BlockKind::Table(table) => {
                    match table
                        .rows
                        .get_mut(*row)?
                        .cells
                        .get_mut(*cell)?
                        .blocks
                        .get_mut(*para)?
                    {
                        CellBlock::Paragraph(paragraph) => Some(paragraph),
                        _ => None,
                    }
                }
                _ => None,
            },
        }
    }

    pub fn block_index(&self, path: &ParaPath) -> usize {
        match path {
            ParaPath::Body(index) => *index,
            ParaPath::Cell { block, .. } => *block,
        }
    }

    pub fn split_paragraph_newlines(&mut self, path: &ParaPath) -> Option<ParaPath> {
        let text = self.paragraph(path)?.text.clone();
        if !text.contains('\n') {
            return Some(path.clone());
        }
        let parts: Vec<&str> = text.split('\n').collect();
        let format = self.paragraph(path)?.format.clone();
        let in_table = self.paragraph(path)?.in_table;
        if let Some(paragraph) = self.paragraph_mut(path) {
            paragraph.text = parts[0].to_owned();
        }

        match path {
            ParaPath::Body(index) => {
                let mut insert_at = *index + 1;
                let mut last = ParaPath::Body(*index);
                for part in parts.iter().skip(1) {
                    let id = self.alloc_id();
                    let paragraph = Paragraph::new(id, *part, format.clone(), in_table);
                    self.blocks.insert(
                        insert_at,
                        Block {
                            id,
                            kind: BlockKind::Paragraph(paragraph),
                        },
                    );
                    last = ParaPath::Body(insert_at);
                    insert_at += 1;
                }
                Some(last)
            }
            ParaPath::Cell {
                block,
                row,
                cell,
                para,
            } => {
                let new_paragraphs: Vec<Paragraph> = parts
                    .iter()
                    .skip(1)
                    .map(|part| {
                        let id = self.alloc_id();
                        Paragraph::new(id, *part, format.clone(), true)
                    })
                    .collect();
                let Some(Block {
                    kind: BlockKind::Table(table),
                    ..
                }) = self.blocks.get_mut(*block)
                else {
                    return Some(path.clone());
                };
                let Some(TableCell { blocks, .. }) = table
                    .rows
                    .get_mut(*row)
                    .and_then(|row| row.cells.get_mut(*cell))
                else {
                    return Some(path.clone());
                };
                let mut insert_at = *para + 1;
                let mut last = ParaPath::Cell {
                    block: *block,
                    row: *row,
                    cell: *cell,
                    para: *para,
                };
                for paragraph in new_paragraphs {
                    blocks.insert(insert_at, CellBlock::Paragraph(paragraph));
                    last = ParaPath::Cell {
                        block: *block,
                        row: *row,
                        cell: *cell,
                        para: insert_at,
                    };
                    insert_at += 1;
                }
                table.modified = true;
                Some(last)
            }
        }
    }

    pub fn insert_block_after(&mut self, path: Option<&ParaPath>, block: Block) -> usize {
        let index = match path {
            Some(path) => (self.block_index(path) + 1).min(self.blocks.len()),
            None => self.blocks.len(),
        };
        self.blocks.insert(index, block);
        index
    }

    pub fn remove_block(&mut self, index: usize) -> bool {
        if index >= self.blocks.len() {
            return false;
        }
        self.blocks.remove(index);
        if self.blocks.is_empty() {
            let id = self.alloc_id();
            self.push_paragraph(Paragraph::new_blank(id, false));
        }
        true
    }

    pub fn remove_image(&mut self, path: &ImagePath) -> bool {
        match path {
            ImagePath::Body(index) => {
                if matches!(
                    self.blocks.get(*index).map(|block| &block.kind),
                    Some(BlockKind::Image(_))
                ) {
                    return self.remove_block(*index);
                }
                false
            }
            ImagePath::Cell {
                block,
                row,
                cell,
                image,
            } => {
                let blank = Paragraph::new_blank(self.alloc_id(), true);
                let Some(Block {
                    kind: BlockKind::Table(table),
                    ..
                }) = self.blocks.get_mut(*block)
                else {
                    return false;
                };
                let Some(TableCell { blocks, .. }) = table
                    .rows
                    .get_mut(*row)
                    .and_then(|row| row.cells.get_mut(*cell))
                else {
                    return false;
                };
                if !matches!(blocks.get(*image), Some(CellBlock::Image(_))) {
                    return false;
                }
                blocks.remove(*image);
                table.modified = true;
                if !blocks
                    .iter()
                    .any(|block| matches!(block, CellBlock::Paragraph(_)))
                {
                    blocks.insert(0, CellBlock::Paragraph(blank));
                }
                true
            }
        }
    }

    pub fn table_mut(&mut self, block: usize) -> Option<&mut Table> {
        match &mut self.blocks.get_mut(block)?.kind {
            BlockKind::Table(table) => Some(table),
            _ => None,
        }
    }

    pub fn table_add_row(&mut self, block: usize) -> bool {
        let Some(table) = self.table_mut(block) else {
            return false;
        };
        let column_count = table.column_count();
        let template: Vec<(String, usize, Option<[u8; 3]>)> = table
            .rows
            .last()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|cell| (cell.tc_pr.clone(), cell.grid_span, cell.shading))
                    .collect()
            })
            .unwrap_or_default();
        let cell_count = template.len().max(column_count).max(1);
        let width = table
            .col_widths
            .first()
            .copied()
            .unwrap_or((7200 / cell_count as u32).max(900));
        let mut ids = Vec::with_capacity(cell_count);
        for _ in 0..cell_count {
            ids.push(self.alloc_id());
        }
        let mut cells = Vec::with_capacity(cell_count);
        for (index, id) in ids.into_iter().enumerate() {
            let (tc_pr, grid_span, shading) = template.get(index).cloned().unwrap_or_else(|| {
                (
                    format!("<w:tcPr><w:tcW w:w=\"{width}\" w:type=\"dxa\"/></w:tcPr>"),
                    1,
                    None,
                )
            });
            cells.push(TableCell {
                blocks: vec![CellBlock::Paragraph(Paragraph::new_blank(id, true))],
                tc_pr,
                grid_span,
                shading,
                margins: CellMargins::default(),
                vertical_align: VerticalAlign::Top,
            });
        }
        let Some(table) = self.table_mut(block) else {
            return false;
        };
        table.rows.push(TableRow {
            cells,
            tr_pr: String::new(),
            height_twips: None,
        });
        table.modified = true;
        table.rebuild_grid();
        true
    }

    pub fn table_add_column(&mut self, block: usize) -> bool {
        let Some(table) = self.table_mut(block) else {
            return false;
        };
        if table.merged {
            return false;
        }
        let rows = table.rows.len();
        let width = table
            .col_widths
            .last()
            .copied()
            .unwrap_or((7200 / (table.column_count() + 1) as u32).max(900));
        let mut ids = Vec::with_capacity(rows);
        for _ in 0..rows {
            ids.push(self.alloc_id());
        }
        let Some(table) = self.table_mut(block) else {
            return false;
        };
        for (row, id) in table.rows.iter_mut().zip(ids) {
            row.cells.push(TableCell {
                blocks: vec![CellBlock::Paragraph(Paragraph::new_blank(id, true))],
                tc_pr: format!("<w:tcPr><w:tcW w:w=\"{width}\" w:type=\"dxa\"/></w:tcPr>"),
                grid_span: 1,
                shading: None,
                margins: CellMargins::default(),
                vertical_align: VerticalAlign::Top,
            });
        }
        table.col_widths.push(width);
        table.modified = true;
        table.rebuild_grid();
        true
    }

    pub fn table_delete_row(&mut self, block: usize, row_index: usize) -> bool {
        let spare_id = self.alloc_id();
        let Some(table) = self.table_mut(block) else {
            return false;
        };
        if row_index >= table.rows.len() {
            return false;
        }
        table.rows.remove(row_index);
        if table.rows.is_empty() {
            let id = spare_id;
            table.rows.push(TableRow {
                cells: vec![TableCell {
                    blocks: vec![CellBlock::Paragraph(Paragraph::new_blank(id, true))],
                    tc_pr: String::new(),
                    grid_span: 1,
                    shading: None,
                    margins: CellMargins::default(),
                    vertical_align: VerticalAlign::Top,
                }],
                tr_pr: String::new(),
                height_twips: None,
            });
        }
        table.modified = true;
        table.rebuild_grid();
        true
    }

    pub fn table_delete_column(&mut self, block: usize, column_index: usize) -> bool {
        let spare_id = self.alloc_id();
        let Some(table) = self.table_mut(block) else {
            return false;
        };
        if table.merged {
            return false;
        }
        let columns = table.column_count();
        if column_index >= columns {
            return false;
        }
        let width = table
            .col_widths
            .get(column_index)
            .copied()
            .unwrap_or((7200 / columns.max(1) as u32).max(900));
        table
            .rows
            .retain(|row| row.cells.len() > 1 || column_index > 0);
        for row in &mut table.rows {
            if column_index < row.cells.len() {
                row.cells.remove(column_index);
            }
        }
        table.rows.retain(|row| !row.cells.is_empty());
        if table.rows.is_empty() {
            let id = spare_id;
            table.rows.push(TableRow {
                cells: vec![TableCell {
                    blocks: vec![CellBlock::Paragraph(Paragraph::new_blank(id, true))],
                    tc_pr: format!("<w:tcPr><w:tcW w:w=\"{width}\" w:type=\"dxa\"/></w:tcPr>"),
                    grid_span: 1,
                    shading: None,
                    margins: CellMargins::default(),
                    vertical_align: VerticalAlign::Top,
                }],
                tr_pr: String::new(),
                height_twips: None,
            });
        }
        if column_index < table.col_widths.len() {
            table.col_widths.remove(column_index);
        }
        if table.col_widths.is_empty() {
            table.col_widths.push(width);
        }
        table.modified = true;
        table.rebuild_grid();
        true
    }

    pub fn image(&self, path: &ImagePath) -> Option<&ImageBlock> {
        match path {
            ImagePath::Body(index) => match &self.blocks.get(*index)?.kind {
                BlockKind::Image(image) => Some(image),
                _ => None,
            },
            ImagePath::Cell {
                block,
                row,
                cell,
                image,
            } => match &self.blocks.get(*block)?.kind {
                BlockKind::Table(table) => {
                    match table.rows.get(*row)?.cells.get(*cell)?.blocks.get(*image)? {
                        CellBlock::Image(image) => Some(image),
                        _ => None,
                    }
                }
                _ => None,
            },
        }
    }

    pub fn image_mut(&mut self, path: &ImagePath) -> Option<&mut ImageBlock> {
        match path {
            ImagePath::Body(index) => match &mut self.blocks.get_mut(*index)?.kind {
                BlockKind::Image(image) => Some(image),
                _ => None,
            },
            ImagePath::Cell {
                block,
                row,
                cell,
                image,
            } => match &mut self.blocks.get_mut(*block)?.kind {
                BlockKind::Table(table) => {
                    match table
                        .rows
                        .get_mut(*row)?
                        .cells
                        .get_mut(*cell)?
                        .blocks
                        .get_mut(*image)?
                    {
                        CellBlock::Image(image) => Some(image),
                        _ => None,
                    }
                }
                _ => None,
            },
        }
    }

    pub fn all_paragraphs(&self) -> Vec<&Paragraph> {
        let mut result = Vec::new();
        for block in &self.blocks {
            match &block.kind {
                BlockKind::Paragraph(paragraph) => result.push(paragraph),
                BlockKind::Table(table) => {
                    for row in &table.rows {
                        for cell in &row.cells {
                            for block in &cell.blocks {
                                if let CellBlock::Paragraph(paragraph) = block {
                                    result.push(paragraph);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        result
    }

    pub fn image_targets(&self) -> Vec<String> {
        let mut targets = Vec::new();
        for block in &self.blocks {
            match &block.kind {
                BlockKind::Image(image) => targets.push(image.media_target.clone()),
                BlockKind::Table(table) => {
                    for row in &table.rows {
                        for cell in &row.cells {
                            for block in &cell.blocks {
                                if let CellBlock::Image(image) = block {
                                    targets.push(image.media_target.clone());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        targets
    }

    pub fn text_stats(&self) -> (usize, usize, usize) {
        let paragraphs = self.all_paragraphs();
        let words = paragraphs
            .iter()
            .map(|paragraph| paragraph.text.split_whitespace().count())
            .sum();
        let chars = paragraphs
            .iter()
            .map(|paragraph| {
                paragraph
                    .text
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .count()
            })
            .sum();
        (paragraphs.len(), words, chars)
    }
}

// ---------------------------------------------------------------------------
// Package document
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct NewMedia {
    pub zip_path: String,
    pub bytes: Vec<u8>,
}

pub struct Document {
    pub title: String,
    pub path: Option<PathBuf>,
    pub package_path: Option<PathBuf>,
    pub source_xml: String,
    pub body: Body,
    pub kind: DocKind,
    pub rels_xml: String,
    pub content_types_xml: String,
    pub media: HashMap<String, Vec<u8>>,
    pub new_media: Vec<NewMedia>,
    pub package_dirty: bool,
    pub next_rel: u32,
    pub next_image: u32,
    pub next_doc_pr: u64,
}

impl Document {
    pub fn empty() -> Self {
        Self {
            title: "Untitled".to_owned(),
            path: None,
            package_path: None,
            source_xml: String::new(),
            body: Body::empty(),
            kind: DocKind::Docx,
            rels_xml: new_document_rels(),
            content_types_xml: new_content_types(),
            media: HashMap::new(),
            new_media: Vec::new(),
            package_dirty: false,
            next_rel: 2,
            next_image: 1,
            next_doc_pr: 1,
        }
    }

    pub fn from_legacy(path: &Path, texts: Vec<String>) -> Self {
        Self {
            title: file_title(path),
            path: Some(path.to_path_buf()),
            package_path: None,
            source_xml: String::new(),
            body: Body::from_texts(texts),
            kind: DocKind::LegacyDoc,
            rels_xml: new_document_rels(),
            content_types_xml: new_content_types(),
            media: HashMap::new(),
            new_media: Vec::new(),
            package_dirty: false,
            next_rel: 2,
            next_image: 1,
            next_doc_pr: 1,
        }
    }

    pub fn image_bytes(&self, target: &str) -> Option<&[u8]> {
        self.media.get(target).map(Vec::as_slice)
    }

    pub fn add_image(
        &mut self,
        bytes: Vec<u8>,
        extension: &str,
        pixel_width: u32,
        pixel_height: u32,
        display_name: &str,
    ) -> ImageBlock {
        let extension = normalise_image_extension(extension);
        let target = self.unique_media_target(extension);
        let rel_id = format!("rId{}", self.next_rel);
        self.next_rel += 1;
        append_image_relationship(&mut self.rels_xml, &rel_id, &target);
        add_image_content_type(&mut self.content_types_xml, extension);
        self.media.insert(target.clone(), bytes.clone());
        self.new_media.push(NewMedia {
            zip_path: format!("word/{target}"),
            bytes,
        });
        self.package_dirty = true;

        let width = (pixel_width.max(1) as f32 * EMU_PER_PIXEL as f32)
            .min(5_900_000.0)
            .max(120.0 * EMU_PER_PIXEL as f32);
        let aspect = pixel_width.max(1) as f32 / pixel_height.max(1) as f32;
        let height = width / aspect;
        let id = self.body.alloc_id();
        let doc_pr_id = self.next_doc_pr;
        self.next_doc_pr += 1;
        ImageBlock {
            id,
            doc_pr_id,
            name: if display_name.trim().is_empty() {
                "Picture".to_owned()
            } else {
                display_name.to_owned()
            },
            rel_id,
            media_target: target,
            width_emu: width as i64,
            height_emu: height as i64,
            original_width_emu: 0,
            original_height_emu: 0,
            raw: String::new(),
        }
    }

    pub fn mark_saved(&mut self) {
        self.new_media.clear();
        self.package_dirty = false;
    }

    fn unique_media_target(&mut self, extension: &str) -> String {
        loop {
            let candidate = format!("media/image{}.{}", self.next_image, extension);
            self.next_image += 1;
            if !self.media.contains_key(&candidate) {
                return candidate;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocKind {
    Docx,
    LegacyDoc,
}

impl DocKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Docx => "DOCX",
            Self::LegacyDoc => "DOC (legacy import)",
        }
    }
}

pub struct SaveResult {
    pub xml: String,
    pub body: Body,
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

pub fn load(path: &Path) -> Result<Document> {
    let mut file =
        File::open(path).with_context(|| format!("Could not open {}", path.display()))?;
    let mut head = [0_u8; 8];
    let read = file.read(&mut head).unwrap_or(0);
    drop(file);

    if read >= 4 && &head[..2] == b"PK" {
        load_docx(path)
    } else if read >= 8 && head == [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1] {
        crate::legacy_doc::load(path)
    } else {
        bail!(
            "{} is not a Word document. Expected a .docx/.docm ZIP package or a legacy .doc file.",
            path.display()
        )
    }
}

pub fn load_docx(path: &Path) -> Result<Document> {
    let file = File::open(path).with_context(|| format!("Could not open {}", path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("{} is not a valid DOCX package", path.display()))?;
    let xml = read_zip_text(&mut archive, "word/document.xml")
        .context("This package has no word/document.xml part, so it is not a Word document")?;
    let xml = xml.trim_start_matches('\u{feff}').to_owned();
    let rels_xml = read_zip_text(&mut archive, "word/_rels/document.xml.rels")
        .unwrap_or_else(|| format!("<Relationships xmlns=\"{REL_NS}\"></Relationships>"));
    let content_types_xml =
        read_zip_text(&mut archive, "[Content_Types].xml").unwrap_or_else(new_content_types);
    let relationships = parse_relationships(&rels_xml);
    let mut next_id = 1_u64;
    let body = parse_body(&xml, &relationships, &mut next_id)?;
    let mut media = HashMap::new();
    for target in body.image_targets() {
        if media.contains_key(&target) {
            continue;
        }
        let entry_name = format!("word/{}", target.trim_start_matches('/'));
        if let Ok(mut entry) = archive.by_name(&entry_name) {
            let mut bytes = Vec::new();
            if entry.read_to_end(&mut bytes).is_ok() {
                media.insert(target, bytes);
            }
        }
    }

    let next_rel = next_relationship_id(&rels_xml);
    let next_image = next_media_number(&archive);
    let next_doc_pr = next_drawing_id(&xml);

    Ok(Document {
        title: file_title(path),
        path: Some(path.to_path_buf()),
        package_path: Some(path.to_path_buf()),
        source_xml: xml,
        body,
        kind: DocKind::Docx,
        rels_xml,
        content_types_xml,
        media,
        new_media: Vec::new(),
        package_dirty: false,
        next_rel,
        next_image,
        next_doc_pr,
    })
}

fn read_zip_text(archive: &mut ZipArchive<File>, name: &str) -> Option<String> {
    let mut entry = archive.by_name(name).ok()?;
    let mut text = String::new();
    entry.read_to_string(&mut text).ok()?;
    Some(text)
}

fn parse_body(
    xml: &str,
    relationships: &HashMap<String, Relationship>,
    next_id: &mut u64,
) -> Result<Body> {
    let doc = XmlDocument::parse(xml).context("Could not parse word/document.xml")?;
    let body_node = doc
        .descendants()
        .find(|node| is_w(*node, "body"))
        .context("document.xml has no w:body element")?;
    let mut body = Body {
        blocks: Vec::new(),
        sect_pr: String::new(),
        page: PageSettings::default(),
        next_id: *next_id,
    };

    for child in body_node.children().filter(Node::is_element) {
        let range = child.range();
        if range.end > xml.len() {
            continue;
        }
        let raw = xml[range].to_owned();
        match child.tag_name().name() {
            "p" => {
                if let Some(image) = parse_image_only(child, xml, relationships, &mut body.next_id)
                {
                    body.blocks.push(Block {
                        id: image.id,
                        kind: BlockKind::Image(image),
                    });
                } else {
                    let paragraph = parse_paragraph(child, xml, false, &mut body.next_id);
                    body.blocks.push(Block {
                        id: paragraph.id,
                        kind: BlockKind::Paragraph(paragraph),
                    });
                }
            }
            "tbl" => {
                let table = parse_table(child, xml, relationships, &mut body.next_id);
                body.blocks.push(Block {
                    id: table.id,
                    kind: BlockKind::Table(table),
                });
            }
            "sectPr" => {
                body.page = page_settings_from_node(child);
                body.sect_pr = raw;
            }
            _ => body.blocks.push(Block {
                id: {
                    let id = body.next_id;
                    body.next_id += 1;
                    id
                },
                kind: BlockKind::Raw(raw),
            }),
        }
    }

    if body.blocks.is_empty() {
        let id = body.next_id;
        body.next_id += 1;
        body.blocks.push(Block {
            id,
            kind: BlockKind::Paragraph(Paragraph::new_blank(id, false)),
        });
    }
    if body.sect_pr.is_empty() {
        body.sect_pr = default_sect_pr();
    }
    *next_id = body.next_id;
    Ok(body)
}

fn parse_paragraph(node: Node<'_, '_>, xml: &str, in_table: bool, next_id: &mut u64) -> Paragraph {
    let id = *next_id;
    *next_id += 1;
    let ppr_node = direct_child(node, "pPr");
    let ppr = ppr_node
        .map(|child| xml[child.range()].to_owned())
        .unwrap_or_default();
    let page_break_before = ppr_node
        .and_then(|pr| direct_child(pr, "pageBreakBefore"))
        .and_then(on_off)
        .unwrap_or(false);
    let section_break_after = ppr_node.and_then(|pr| direct_child(pr, "sectPr")).is_some();
    let rpr = first_run(node)
        .and_then(|run| direct_child(run, "rPr"))
        .map(|child| xml[child.range()].to_owned())
        .unwrap_or_default();
    let mut text = String::new();
    collect_text(node, &mut text);
    text = sanitize_editor_text(&text);
    let format = parse_format(node);
    Paragraph {
        id,
        original_text: text.clone(),
        original_format: format.clone(),
        text,
        format,
        ppr,
        rpr,
        raw: xml[node.range()].to_owned(),
        in_table,
        page_break_before,
        section_break_after,
    }
}

fn parse_table(
    node: Node<'_, '_>,
    xml: &str,
    relationships: &HashMap<String, Relationship>,
    next_id: &mut u64,
) -> Table {
    let id = *next_id;
    *next_id += 1;
    let tbl_pr = direct_child(node, "tblPr")
        .map(|child| xml[child.range()].to_owned())
        .unwrap_or_default();
    let width_twips = direct_child(node, "tblPr")
        .and_then(|pr| direct_child(pr, "tblW"))
        .and_then(|width| {
            let kind = attr_any(width, "type").unwrap_or("dxa");
            if kind != "dxa" {
                return None;
            }
            attr_any(width, "w")
                .and_then(|width| width.parse::<u32>().ok())
                .filter(|width| *width > 0)
        });
    let tbl_grid = direct_child(node, "tblGrid")
        .map(|child| xml[child.range()].to_owned())
        .unwrap_or_default();
    let mut col_widths = Vec::new();
    if let Some(grid) = direct_child(node, "tblGrid") {
        for column in grid.children().filter(|child| is_w(*child, "gridCol")) {
            if let Some(width) = attr_any(column, "w") {
                if let Ok(width) = width.parse::<u32>() {
                    col_widths.push(width.max(200));
                }
            }
        }
    }
    let mut rows = Vec::new();
    let mut merged = false;
    for row_node in node.children().filter(|child| is_w(*child, "tr")) {
        let tr_pr = direct_child(row_node, "trPr")
            .map(|child| xml[child.range()].to_owned())
            .unwrap_or_default();
        let height_twips = direct_child(row_node, "trPr")
            .and_then(|pr| direct_child(pr, "trHeight"))
            .and_then(|height| attr_any(height, "val"))
            .and_then(|height| height.parse::<u32>().ok());
        let mut cells = Vec::new();
        for cell_node in row_node.children().filter(|child| is_w(*child, "tc")) {
            let tc_pr_node = direct_child(cell_node, "tcPr");
            let tc_pr = tc_pr_node
                .map(|child| xml[child.range()].to_owned())
                .unwrap_or_default();
            let grid_span = direct_child(cell_node, "tcPr")
                .and_then(|pr| direct_child(pr, "gridSpan"))
                .and_then(|span| attr_any(span, "val"))
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(1)
                .max(1);
            if grid_span > 1 {
                merged = true;
            }
            if direct_child(cell_node, "tcPr")
                .and_then(|pr| direct_child(pr, "vMerge"))
                .is_some()
            {
                merged = true;
            }
            let shading = tc_pr_node
                .and_then(|pr| direct_child(pr, "shd"))
                .and_then(|shd| attr_any(shd, "fill"))
                .and_then(parse_hex_color);
            let margins = tc_pr_node
                .and_then(|pr| direct_child(pr, "tcMar"))
                .map(|margin| CellMargins {
                    left: direct_child(margin, "left")
                        .and_then(|value| attr_any(value, "w"))
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(108),
                    top: direct_child(margin, "top")
                        .and_then(|value| attr_any(value, "w"))
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0),
                    right: direct_child(margin, "right")
                        .and_then(|value| attr_any(value, "w"))
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(108),
                    bottom: direct_child(margin, "bottom")
                        .and_then(|value| attr_any(value, "w"))
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0),
                })
                .unwrap_or_default();
            let vertical_align = tc_pr_node
                .and_then(|pr| direct_child(pr, "vAlign"))
                .and_then(|align| attr_any(align, "val"))
                .map(|align| match align {
                    "center" => VerticalAlign::Center,
                    "bottom" => VerticalAlign::Bottom,
                    _ => VerticalAlign::Top,
                })
                .unwrap_or(VerticalAlign::Top);
            let mut blocks = Vec::new();
            for child in cell_node.children().filter(Node::is_element) {
                let range = child.range();
                if range.end > xml.len() {
                    continue;
                }
                match child.tag_name().name() {
                    "p" => {
                        if let Some(image) = parse_image_only(child, xml, relationships, next_id) {
                            blocks.push(CellBlock::Image(image));
                        } else {
                            blocks.push(CellBlock::Paragraph(parse_paragraph(
                                child, xml, true, next_id,
                            )));
                        }
                    }
                    "tbl" => {
                        merged = true;
                        blocks.push(CellBlock::Raw(xml[range].to_owned()));
                    }
                    "tcPr" => {}
                    _ => blocks.push(CellBlock::Raw(xml[range].to_owned())),
                }
            }
            if !blocks
                .iter()
                .any(|block| matches!(block, CellBlock::Paragraph(_)))
            {
                blocks.push(CellBlock::Paragraph(Paragraph::new_blank(*next_id, true)));
                *next_id += 1;
            }
            cells.push(TableCell {
                blocks,
                tc_pr,
                grid_span,
                shading,
                margins,
                vertical_align,
            });
        }
        if cells.is_empty() {
            let paragraph = Paragraph::new_blank(*next_id, true);
            *next_id += 1;
            cells.push(TableCell {
                blocks: vec![CellBlock::Paragraph(paragraph)],
                tc_pr: String::new(),
                grid_span: 1,
                shading: None,
                margins: CellMargins::default(),
                vertical_align: VerticalAlign::Top,
            });
        }
        rows.push(TableRow {
            cells,
            tr_pr,
            height_twips,
        });
    }
    if rows.is_empty() {
        let paragraph = Paragraph::new_blank(*next_id, true);
        *next_id += 1;
        rows.push(TableRow {
            cells: vec![TableCell {
                blocks: vec![CellBlock::Paragraph(paragraph)],
                tc_pr: String::new(),
                grid_span: 1,
                shading: None,
                margins: CellMargins::default(),
                vertical_align: VerticalAlign::Top,
            }],
            tr_pr: String::new(),
            height_twips: None,
        });
    }
    if col_widths.is_empty() {
        let columns = rows
            .iter()
            .map(|row| row.cells.len())
            .max()
            .unwrap_or(1)
            .max(1);
        col_widths = vec![(7200 / columns as u32).max(900); columns];
    }
    Table {
        id,
        raw: xml[node.range()].to_owned(),
        tbl_pr,
        tbl_grid,
        col_widths,
        width_twips,
        rows,
        modified: false,
        merged,
    }
}

fn parse_image_only(
    node: Node<'_, '_>,
    xml: &str,
    relationships: &HashMap<String, Relationship>,
    next_id: &mut u64,
) -> Option<ImageBlock> {
    let mut text = String::new();
    collect_text(node, &mut text);
    if !text.trim().is_empty() {
        return None;
    }
    let drawing = find_descendant(node, "drawing")?;
    let blip = find_descendant(drawing, "blip")?;
    let rel_id = attr_any(blip, "embed")?.to_owned();
    let relationship = relationships.get(&rel_id)?;
    let target = normalise_target(&relationship.target);
    let extent = find_descendant(drawing, "extent");
    let width_emu = extent
        .and_then(|extent| attr_any(extent, "cx"))
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(4_000_000);
    let height_emu = extent
        .and_then(|extent| attr_any(extent, "cy"))
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(3_000_000);
    let doc_pr = find_descendant(drawing, "docPr");
    let name = doc_pr
        .and_then(|node| attr_any(node, "name").or_else(|| attr_any(node, "descr")))
        .unwrap_or("Picture")
        .to_owned();
    let doc_pr_id = doc_pr
        .and_then(|node| attr_any(node, "id"))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(*next_id);
    let id = *next_id;
    *next_id += 1;
    Some(ImageBlock {
        id,
        doc_pr_id,
        name,
        rel_id,
        media_target: target,
        width_emu: width_emu.max(24),
        height_emu: height_emu.max(24),
        original_width_emu: width_emu.max(24),
        original_height_emu: height_emu.max(24),
        raw: xml[node.range()].to_owned(),
    })
}

fn find_descendant<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.descendants()
        .find(|child| child.is_element() && child.tag_name().name() == name)
}

// ---------------------------------------------------------------------------
// Saving
// ---------------------------------------------------------------------------

pub fn save(doc: &Document, dest: &Path) -> Result<SaveResult> {
    let xml = render_document(doc)?;
    write_package(doc, dest, &xml)?;
    let relationships = parse_relationships(&doc.rels_xml);
    let mut next_id = 1;
    let body = parse_body(&xml, &relationships, &mut next_id)?;
    Ok(SaveResult { xml, body })
}

fn render_document(doc: &Document) -> Result<String> {
    let mut content = String::new();
    for block in &doc.body.blocks {
        content.push_str(&render_block(block)?);
    }
    if content.is_empty() {
        content.push_str(&render_paragraph_xml("", "", ""));
    }
    content.push_str(&doc.body.sect_pr);

    if doc.source_xml.is_empty() {
        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\" xmlns:wp=\"{WP_NS}\" xmlns:a=\"{A_NS}\" xmlns:pic=\"{PIC_NS}\">\
<w:body>{content}</w:body></w:document>"
        ))
    } else {
        let open_start = doc
            .source_xml
            .find("<w:body")
            .context("document.xml has no w:body element")?;
        let open_end = doc.source_xml[open_start..]
            .find('>')
            .map(|offset| open_start + offset + 1)
            .context("document.xml has a malformed w:body element")?;
        let close = doc
            .source_xml
            .rfind("</w:body>")
            .context("document.xml has no closing w:body element")?;
        if close < open_end {
            bail!("document.xml has a malformed w:body element");
        }
        let mut result = String::with_capacity(doc.source_xml.len() + content.len());
        result.push_str(&doc.source_xml[..open_end]);
        result.push_str(&content);
        result.push_str(&doc.source_xml[close..]);
        Ok(result)
    }
}

fn render_block(block: &Block) -> Result<String> {
    match &block.kind {
        BlockKind::Paragraph(paragraph) => Ok(render_paragraph(paragraph)),
        BlockKind::Table(table) => Ok(render_table(table)),
        BlockKind::Image(image) => Ok(render_image(image)),
        BlockKind::Raw(raw) => Ok(raw.clone()),
    }
}

fn render_paragraph(paragraph: &Paragraph) -> String {
    if !paragraph.needs_render() && !paragraph.raw.is_empty() {
        return paragraph.raw.clone();
    }
    let format = &paragraph.format;
    let ppr = if paragraph.ppr.is_empty()
        || paragraph.original_format.style != format.style
        || paragraph.original_format.alignment != format.alignment
    {
        make_ppr(format, &paragraph.ppr)
    } else {
        paragraph.ppr.clone()
    };
    let rpr = if paragraph.original_format == *format && !paragraph.rpr.is_empty() {
        paragraph.rpr.clone()
    } else {
        make_rpr(format)
    };
    render_paragraph_xml(&paragraph.text, &ppr, &rpr)
}

fn render_table(table: &Table) -> String {
    if !table.needs_render() && !table.raw.is_empty() {
        return table.raw.clone();
    }
    let tbl_pr = if table.tbl_pr.is_empty() {
        "<w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/></w:tblPr>".to_owned()
    } else {
        table.tbl_pr.clone()
    };
    let tbl_grid = if table.modified || table.tbl_grid.is_empty() {
        let widths = if table.col_widths.is_empty() {
            vec![2400; table.column_count()]
        } else {
            table.col_widths.clone()
        };
        format!(
            "<w:tblGrid>{}</w:tblGrid>",
            widths
                .iter()
                .map(|width| format!("<w:gridCol w:w=\"{width}\"/>"))
                .collect::<String>()
        )
    } else {
        table.tbl_grid.clone()
    };
    let mut xml = String::from("<w:tbl>");
    xml.push_str(&tbl_pr);
    xml.push_str(&tbl_grid);
    for row in &table.rows {
        xml.push_str("<w:tr>");
        xml.push_str(&row.tr_pr);
        for cell in &row.cells {
            xml.push_str("<w:tc>");
            xml.push_str(&cell.tc_pr);
            let mut has_paragraph = false;
            for block in &cell.blocks {
                match block {
                    CellBlock::Paragraph(paragraph) => {
                        has_paragraph = true;
                        xml.push_str(&render_paragraph(paragraph));
                    }
                    CellBlock::Image(image) => {
                        has_paragraph = true;
                        xml.push_str(&render_image(image));
                    }
                    CellBlock::Raw(raw) => xml.push_str(raw),
                }
            }
            if !has_paragraph {
                xml.push_str(&render_paragraph_xml("", "", ""));
            }
            xml.push_str("</w:tc>");
        }
        xml.push_str("</w:tr>");
    }
    xml.push_str("</w:tbl>");
    xml
}

fn render_image(image: &ImageBlock) -> String {
    let name = xml_escape(&image.name);
    let width = image.width_emu.max(24);
    let height = image.height_emu.max(24);
    format!(
        "<w:p><w:r><w:drawing xmlns:wp=\"{WP_NS}\" xmlns:a=\"{A_NS}\" xmlns:pic=\"{PIC_NS}\" xmlns:r=\"{R_NS}\">\
<wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">\
<wp:extent cx=\"{width}\" cy=\"{height}\"/>\
<wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/>\
<wp:docPr id=\"{doc_pr}\" name=\"{name}\"/>\
<a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
<pic:pic><pic:nvPicPr><pic:cNvPr id=\"{doc_pr}\" name=\"{name}\"/><pic:cNvPicPr/></pic:nvPicPr>\
<pic:blipFill><a:blip r:embed=\"{rel}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
<pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{width}\" cy=\"{height}\"/></a:xfrm>\
<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>\
</pic:pic></a:graphicData></a:graphic>\
</wp:inline></w:drawing></w:r></w:p>",
        doc_pr = image.doc_pr_id,
        rel = xml_escape(&image.rel_id),
    )
}

fn make_ppr(format: &Format, original: &str) -> String {
    let mut additions = String::new();
    if format.style != ParaStyle::Normal {
        additions.push_str(&format!(
            "<w:pStyle w:val=\"{}\"/>",
            format.style.style_id()
        ));
    }
    if format.alignment != Alignment::Left {
        additions.push_str(&format!(
            "<w:jc w:val=\"{}\"/>",
            format.alignment.xml_value()
        ));
    }

    if original.trim().is_empty() {
        if additions.is_empty() {
            String::new()
        } else {
            format!("<w:pPr>{additions}</w:pPr>")
        }
    } else {
        let mut ppr = remove_self_closing(original, "w:pStyle");
        ppr = remove_self_closing(&ppr, "w:jc");
        if let Some(close) = ppr.rfind("</w:pPr>") {
            ppr.insert_str(close, &additions);
            ppr
        } else {
            format!("{ppr}{additions}")
        }
    }
}

fn make_rpr(format: &Format) -> String {
    let size = (format.size_pt * 2.0).round().clamp(12.0, 192.0) as u32;
    let color = format!(
        "{:02X}{:02X}{:02X}",
        format.color[0], format.color[1], format.color[2]
    );
    let font = xml_escape(&format.font);
    let bold = if format.bold {
        "<w:b/>"
    } else {
        "<w:b w:val=\"0\"/>"
    };
    let italic = if format.italic {
        "<w:i/>"
    } else {
        "<w:i w:val=\"0\"/>"
    };
    let underline = if format.underline {
        "<w:u w:val=\"single\"/>"
    } else {
        "<w:u w:val=\"none\"/>"
    };
    format!(
        "<w:rPr><w:rFonts w:ascii=\"{font}\" w:hAnsi=\"{font}\" w:eastAsia=\"Microsoft YaHei\"/>{bold}{italic}{underline}<w:sz w:val=\"{size}\"/><w:szCs w:val=\"{size}\"/><w:color w:val=\"{color}\"/></w:rPr>"
    )
}

fn render_paragraph_xml(text: &str, ppr: &str, rpr: &str) -> String {
    let mut xml = String::with_capacity(text.len() + ppr.len() + rpr.len() + 64);
    xml.push_str("<w:p>");
    xml.push_str(ppr);
    xml.push_str("<w:r>");
    xml.push_str(rpr);
    if text.is_empty() {
        xml.push_str("<w:t xml:space=\"preserve\"></w:t>");
    } else {
        let mut chunk = String::new();
        for ch in text.chars() {
            if ch == '\t' {
                push_text_run(&mut xml, &mut chunk);
                xml.push_str("<w:tab/>");
            } else if ch == '\n' || ch == '\r' {
                push_text_run(&mut xml, &mut chunk);
                xml.push_str("<w:br/>");
            } else {
                chunk.push(ch);
            }
        }
        push_text_run(&mut xml, &mut chunk);
    }
    xml.push_str("</w:r></w:p>");
    xml
}

fn push_text_run(xml: &mut String, chunk: &mut String) {
    if chunk.is_empty() {
        return;
    }
    xml.push_str("<w:t xml:space=\"preserve\">");
    xml.push_str(&xml_escape(chunk));
    xml.push_str("</w:t>");
    chunk.clear();
}

// ---------------------------------------------------------------------------
// Package writing
// ---------------------------------------------------------------------------

fn write_package(doc: &Document, dest: &Path, document_xml: &str) -> Result<()> {
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Could not create {}", parent.display()))?;
        }
    }
    if let (Some(source), false) = (&doc.package_path, doc.source_xml.is_empty()) {
        write_existing_package(doc, source, dest, document_xml)
    } else {
        write_new_package(doc, dest, document_xml)
    }
}

fn write_new_package(doc: &Document, dest: &Path, document_xml: &str) -> Result<()> {
    let temp = temp_path(dest);
    let result = (|| -> Result<()> {
        let file =
            File::create(&temp).with_context(|| format!("Could not create {}", temp.display()))?;
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o644);
        writer.start_file("[Content_Types].xml", options)?;
        writer.write_all(doc.content_types_xml.as_bytes())?;
        writer.start_file("_rels/.rels", options)?;
        writer.write_all(ROOT_RELS.as_bytes())?;
        writer.start_file("word/document.xml", options)?;
        writer.write_all(document_xml.as_bytes())?;
        writer.start_file("word/styles.xml", options)?;
        writer.write_all(STYLES_XML.as_bytes())?;
        writer.start_file("word/_rels/document.xml.rels", options)?;
        writer.write_all(doc.rels_xml.as_bytes())?;
        for (target, bytes) in &doc.media {
            writer.start_file(format!("word/{}", target.trim_start_matches('/')), options)?;
            writer.write_all(bytes)?;
        }
        writer.finish()?;
        Ok(())
    })();

    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    if let Err(error) = fs::copy(&temp, dest) {
        let _ = fs::remove_file(&temp);
        return Err(error).with_context(|| format!("Could not write {}", dest.display()));
    }
    let _ = fs::remove_file(&temp);
    Ok(())
}

fn write_existing_package(
    doc: &Document,
    source: &Path,
    dest: &Path,
    document_xml: &str,
) -> Result<()> {
    let file =
        File::open(source).with_context(|| format!("Could not read {}", source.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("{} is no longer a valid DOCX package", source.display()))?;
    let temp = temp_path(dest);
    let result = (|| -> Result<()> {
        let file =
            File::create(&temp).with_context(|| format!("Could not create {}", temp.display()))?;
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o644);
        let mut found = false;
        for index in 0..archive.len() {
            let entry = archive.by_index_raw(index)?;
            let name = entry.name().to_owned();
            if name == "word/document.xml" {
                found = true;
                continue;
            }
            if doc.package_dirty
                && (name == "word/_rels/document.xml.rels" || name == "[Content_Types].xml")
            {
                continue;
            }
            writer.raw_copy_file(entry)?;
        }
        if !found {
            bail!("The source package has no word/document.xml part");
        }
        writer.start_file("word/document.xml", options)?;
        writer.write_all(document_xml.as_bytes())?;
        if doc.package_dirty {
            writer.start_file("word/_rels/document.xml.rels", options)?;
            writer.write_all(doc.rels_xml.as_bytes())?;
            writer.start_file("[Content_Types].xml", options)?;
            writer.write_all(doc.content_types_xml.as_bytes())?;
        }
        for media in &doc.new_media {
            writer.start_file(&media.zip_path, options)?;
            writer.write_all(&media.bytes)?;
        }
        writer.finish()?;
        Ok(())
    })();

    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    drop(archive);
    if let Err(error) = fs::copy(&temp, dest) {
        let _ = fs::remove_file(&temp);
        return Err(error).with_context(|| format!("Could not write {}", dest.display()));
    }
    let _ = fs::remove_file(&temp);
    Ok(())
}

fn temp_path(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("document.docx"))
        .to_os_string();
    name.push(format!(
        ".rustword-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    dest.with_file_name(name)
}

// ---------------------------------------------------------------------------
// Relationships and content types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Relationship {
    target: String,
}

fn parse_relationships(xml: &str) -> HashMap<String, Relationship> {
    let mut result = HashMap::new();
    let Ok(doc) = XmlDocument::parse(xml) else {
        return result;
    };
    for node in doc
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "Relationship")
    {
        let Some(id) = attr_any(node, "Id") else {
            continue;
        };
        let Some(target) = attr_any(node, "Target") else {
            continue;
        };
        result.insert(
            id.to_owned(),
            Relationship {
                target: target.to_owned(),
            },
        );
    }
    result
}

fn next_relationship_id(rels_xml: &str) -> u32 {
    let mut highest = 1_u32;
    let Ok(doc) = XmlDocument::parse(rels_xml) else {
        return 2;
    };
    for node in doc
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "Relationship")
    {
        if let Some(id) = attr_any(node, "Id") {
            if let Some(number) = id.strip_prefix("rId").and_then(|n| n.parse::<u32>().ok()) {
                highest = highest.max(number);
            }
        }
    }
    highest + 1
}

fn next_media_number(archive: &ZipArchive<File>) -> u32 {
    let mut highest = 0_u32;
    for name in archive.file_names() {
        if let Some(rest) = name.strip_prefix("word/media/image") {
            let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            if let Ok(number) = digits.parse::<u32>() {
                highest = highest.max(number);
            }
        }
    }
    highest + 1
}

fn next_drawing_id(xml: &str) -> u64 {
    let mut highest = 0_u64;
    if let Ok(doc) = XmlDocument::parse(xml) {
        for node in doc
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "docPr")
        {
            if let Some(id) = attr_any(node, "id").and_then(|id| id.parse::<u64>().ok()) {
                highest = highest.max(id);
            }
        }
    }
    highest + 1
}

fn append_image_relationship(rels_xml: &mut String, id: &str, target: &str) {
    let relationship = format!(
        "<Relationship Id=\"{}\" Type=\"{}\" Target=\"{}\"/>",
        xml_escape(id),
        IMAGE_REL_TYPE,
        xml_escape(target)
    );
    if let Some(close) = rels_xml.rfind("</Relationships>") {
        rels_xml.insert_str(close, &relationship);
    } else {
        *rels_xml = format!("<Relationships xmlns=\"{REL_NS}\">{relationship}</Relationships>");
    }
}

fn add_image_content_type(content_types: &mut String, extension: &str) {
    let extension = normalise_image_extension(extension);
    let needle = format!("Extension=\"{extension}\"");
    if content_types
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
    {
        return;
    }
    let content_type = if extension == "jpg" {
        "image/jpeg"
    } else {
        "image/png"
    };
    let default = format!("<Default Extension=\"{extension}\" ContentType=\"{content_type}\"/>");
    if let Some(close) = content_types.rfind("</Types>") {
        content_types.insert_str(close, &default);
    }
}

fn normalise_image_extension(extension: &str) -> &'static str {
    match extension
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "jpeg" | "jpg" => "jpg",
        _ => "png",
    }
}

fn normalise_target(target: &str) -> String {
    target.trim_start_matches('/').to_owned()
}

// ---------------------------------------------------------------------------
// XML helpers
// ---------------------------------------------------------------------------

fn file_title(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("Untitled")
        .to_owned()
}

fn is_w(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name && node.tag_name().namespace() == Some(W_NS)
}

fn direct_child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == name)
}

fn first_run<'a, 'input>(node: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    for child in node.children() {
        if !child.is_element() {
            continue;
        }
        if is_w(child, "p") {
            continue;
        }
        if is_w(child, "r") {
            return Some(child);
        }
        if let Some(found) = first_run(child) {
            return Some(found);
        }
    }
    None
}

fn collect_text(node: Node<'_, '_>, out: &mut String) {
    for child in node.children() {
        if child.is_text() {
            if let Some(text) = child.text() {
                out.push_str(text);
            }
            continue;
        }
        if !child.is_element() {
            continue;
        }
        if is_w(child, "p") {
            continue;
        }
        match child.tag_name().name() {
            "t" => {
                if let Some(text) = child.text() {
                    out.push_str(text);
                }
            }
            "tab" | "ptab" => out.push('\t'),
            "br" | "cr" => out.push('\n'),
            "noBreakHyphen" => out.push('-'),
            "softHyphen" => {}
            "delText" | "instrText" => {}
            _ => collect_text(child, out),
        }
    }
}

fn attr_any<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|attribute| attribute.name() == name)
        .map(|attribute| attribute.value())
}

fn parse_format(node: Node<'_, '_>) -> Format {
    let style = direct_child(node, "pPr")
        .and_then(|ppr_node| direct_child(ppr_node, "pStyle"))
        .and_then(|style_node| attr_any(style_node, "val"))
        .map(ParaStyle::from_style_id)
        .unwrap_or(ParaStyle::Normal);
    let mut format = Format::for_style(style);

    if let Some(ppr_node) = direct_child(node, "pPr") {
        if let Some(jc) = direct_child(ppr_node, "jc").and_then(|n| attr_any(n, "val")) {
            format.alignment = match jc {
                "center" => Alignment::Center,
                "right" | "end" => Alignment::Right,
                "both" | "distribute" => Alignment::Justify,
                _ => Alignment::Left,
            };
        }
    }

    if let Some(rpr_node) = first_run(node)
        .and_then(|run| direct_child(run, "rPr"))
        .or_else(|| direct_child(node, "pPr").and_then(|p| direct_child(p, "rPr")))
    {
        if let Some(value) = direct_child(rpr_node, "b").and_then(on_off) {
            format.bold = value;
        }
        if let Some(value) = direct_child(rpr_node, "i").and_then(on_off) {
            format.italic = value;
        }
        if let Some(underline) = direct_child(rpr_node, "u") {
            let value = attr_any(underline, "val").unwrap_or("single");
            format.underline = !matches!(value, "none" | "0" | "false");
        }
        if let Some(size) = direct_child(rpr_node, "sz").and_then(|n| attr_any(n, "val")) {
            if let Ok(half_points) = size.parse::<f32>() {
                if half_points > 0.0 {
                    format.size_pt = (half_points / 2.0).clamp(6.0, 96.0);
                }
            }
        }
        if let Some(color) = direct_child(rpr_node, "color").and_then(|n| attr_any(n, "val")) {
            if let Some(rgb) = parse_hex_color(color) {
                format.color = rgb;
            }
        }
        if let Some(fonts) = direct_child(rpr_node, "rFonts") {
            if let Some(font) = attr_any(fonts, "ascii").or_else(|| attr_any(fonts, "hAnsi")) {
                if !font.is_empty() {
                    format.font = font.to_owned();
                }
            }
        }
    }
    format
}

fn on_off(node: Node<'_, '_>) -> Option<bool> {
    match attr_any(node, "val") {
        None => Some(true),
        Some("0" | "false" | "off") => Some(false),
        Some(_) => Some(true),
    }
}

fn parse_hex_color(value: &str) -> Option<[u8; 3]> {
    let value = value.trim_start_matches('#');
    if value.len() != 6 || value.eq_ignore_ascii_case("auto") {
        return None;
    }
    let r = u8::from_str_radix(&value[0..2], 16).ok()?;
    let g = u8::from_str_radix(&value[2..4], 16).ok()?;
    let b = u8::from_str_radix(&value[4..6], 16).ok()?;
    Some([r, g, b])
}

fn sanitize_editor_text(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '\r' | '\n' => ' ',
            '\u{0000}'..='\u{0008}' | '\u{000B}'..='\u{001F}' => ' ',
            _ => ch,
        })
        .collect()
}

fn remove_self_closing(xml: &str, tag: &str) -> String {
    let mut result = xml.to_owned();
    let open = format!("<{tag}");
    loop {
        let Some(start) = result.find(&open) else {
            break;
        };
        let after = &result[start + open.len()..];
        if !after.starts_with([' ', '\t', '\r', '\n', '/', '>']) {
            break;
        }
        let Some(end_offset) = result[start..].find("/>") else {
            break;
        };
        result.replace_range(start..start + end_offset + 2, "");
    }
    result
}

fn xml_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn page_settings_from_node(root: Node<'_, '_>) -> PageSettings {
    let mut settings = PageSettings::default();
    if let Some(size) = direct_child(root, "pgSz") {
        if let Some(width) = attr_any(size, "w").and_then(|value| value.parse().ok()) {
            settings.width_twips = width;
        }
        if let Some(height) = attr_any(size, "h").and_then(|value| value.parse().ok()) {
            settings.height_twips = height;
        }
    }
    if let Some(margins) = direct_child(root, "pgMar") {
        if let Some(value) = attr_any(margins, "top").and_then(|value| value.parse().ok()) {
            settings.margin_top = value;
        }
        if let Some(value) = attr_any(margins, "right").and_then(|value| value.parse().ok()) {
            settings.margin_right = value;
        }
        if let Some(value) = attr_any(margins, "bottom").and_then(|value| value.parse().ok()) {
            settings.margin_bottom = value;
        }
        if let Some(value) = attr_any(margins, "left").and_then(|value| value.parse().ok()) {
            settings.margin_left = value;
        }
    }
    settings
}

fn default_sect_pr() -> String {
    "<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/></w:sectPr>".to_owned()
}

// ---------------------------------------------------------------------------
// Static package parts
// ---------------------------------------------------------------------------

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

fn new_document_rels() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"{REL_NS}\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
</Relationships>"
    )
}

fn new_content_types() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Default Extension=\"png\" ContentType=\"image/png\"/>\
<Default Extension=\"jpg\" ContentType=\"image/jpeg\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
</Types>"
    )
}

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:docDefaults>
<w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="Microsoft YaHei"/><w:sz w:val="22"/><w:szCs w:val="22"/></w:rPr></w:rPrDefault>
<w:pPrDefault><w:pPr><w:spacing w:after="160" w:line="259" w:lineRule="auto"/></w:pPr></w:pPrDefault>
</w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:rPr><w:b/><w:sz w:val="56"/><w:szCs w:val="56"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Subtitle"><w:name w:val="Subtitle"/><w:basedOn w:val="Normal"/><w:rPr><w:i/><w:color w:val="595959"/><w:sz w:val="28"/><w:szCs w:val="28"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:rPr><w:b/><w:color w:val="1F3864"/><w:sz w:val="40"/><w:szCs w:val="40"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:rPr><w:b/><w:color w:val="2F5496"/><w:sz w:val="32"/><w:szCs w:val="32"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading3"><w:name w:val="heading 3"/><w:basedOn w:val="Normal"/><w:rPr><w:b/><w:color w:val="2F5496"/><w:sz w:val="26"/><w:szCs w:val="26"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Quote"><w:name w:val="Quote"/><w:basedOn w:val="Normal"/><w:rPr><w:i/><w:color w:val="595959"/></w:rPr></w:style>
</w:styles>"#;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp");
        fs::create_dir_all(&directory).unwrap();
        directory.join(format!(
            "rustword-{name}-{}-{stamp}.docx",
            std::process::id()
        ))
    }

    #[test]
    fn parses_tables_images_and_paragraphs() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("samples")
            .join("rustword-rich.docx");
        let document = load_docx(&source).unwrap();
        assert!(document
            .body
            .blocks
            .iter()
            .any(|block| matches!(block.kind, BlockKind::Table(_))));
        assert!(document
            .body
            .blocks
            .iter()
            .any(|block| matches!(block.kind, BlockKind::Image(_))));
        let image = document
            .body
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockKind::Image(image) => Some(image),
                _ => None,
            })
            .unwrap();
        assert!(document.image_bytes(&image.media_target).is_some());
        assert!(image.width_emu > 0);
    }

    #[test]
    fn edits_a_table_cell_and_round_trips() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("samples")
            .join("rustword-rich.docx");
        let mut document = load_docx(&source).unwrap();
        let table_index = document
            .body
            .blocks
            .iter()
            .position(|block| matches!(block.kind, BlockKind::Table(_)))
            .unwrap();
        let path = ParaPath::Cell {
            block: table_index,
            row: 1,
            cell: 1,
            para: 0,
        };
        document.body.paragraph_mut(&path).unwrap().text = "RustWord table edit".to_owned();
        let target = temp_path("table");
        let saved = save(&document, &target).unwrap();
        assert_eq!(
            saved
                .body
                .paragraph(&ParaPath::Cell {
                    block: table_index,
                    row: 1,
                    cell: 1,
                    para: 0
                })
                .unwrap()
                .text,
            "RustWord table edit"
        );
        let reloaded = load_docx(&target).unwrap();
        assert!(reloaded
            .body
            .all_paragraphs()
            .iter()
            .any(|p| p.text == "RustWord table edit"));
        let _ = fs::remove_file(target);
    }

    #[test]
    fn inserts_an_image_and_preserves_it() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("samples")
            .join("rustword-demo.docx");
        let mut document = load_docx(&source).unwrap();
        let bytes = make_png();
        let image = document.add_image(bytes, "png", 64, 32, "Test image");
        let id = image.id;
        let block = Block {
            id,
            kind: BlockKind::Image(image),
        };
        document.body.insert_block_after(None, block);
        let target = temp_path("image");
        save(&document, &target).unwrap();
        let reloaded = load_docx(&target).unwrap();
        let found = reloaded.body.blocks.iter().any(|block| match &block.kind {
            BlockKind::Image(image) => reloaded.image_bytes(&image.media_target).is_some(),
            _ => false,
        });
        assert!(found);
        let file = File::open(&target).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        assert!(archive.by_name("word/media/image1.png").is_ok());
        let _ = fs::remove_file(target);
    }

    #[test]
    fn creates_new_document_with_an_image() {
        let mut document = Document::empty();
        let image = document.add_image(make_png(), "png", 64, 32, "New image");
        let id = image.id;
        document.body.insert_block_after(
            None,
            Block {
                id,
                kind: BlockKind::Image(image),
            },
        );
        let target = temp_path("new-image");
        save(&document, &target).unwrap();
        let reloaded = load_docx(&target).unwrap();
        let image = reloaded
            .body
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockKind::Image(image) => Some(image),
                _ => None,
            })
            .unwrap();
        assert!(reloaded.image_bytes(&image.media_target).is_some());
        let file = File::open(&target).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        assert!(archive.by_name("word/media/image1.png").is_ok());
        let _ = fs::remove_file(target);
    }

    #[test]
    fn creates_new_document_with_a_table() {
        let mut document = Document::empty();
        let table_id = document.body.alloc_id();
        let mut next = document.body.next_id;
        let table = Table::new(table_id, 2, 3, &mut next);
        document.body.next_id = next;
        document.body.blocks.push(Block {
            id: table.id,
            kind: BlockKind::Table(table),
        });
        let target = temp_path("new-table");
        save(&document, &target).unwrap();
        let reloaded = load_docx(&target).unwrap();
        let table = reloaded
            .body
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockKind::Table(table) => Some(table),
                _ => None,
            })
            .unwrap();
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].cells.len(), 3);
        let _ = fs::remove_file(target);
    }

    #[test]
    fn in_place_save_preserves_image_and_table() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("samples")
            .join("rustword-rich.docx");
        let target = temp_path("in-place-rich");
        fs::copy(&source, &target).unwrap();
        let mut document = load_docx(&target).unwrap();
        let paragraph = document
            .body
            .blocks
            .iter_mut()
            .find_map(|block| match &mut block.kind {
                BlockKind::Paragraph(paragraph) => Some(paragraph),
                _ => None,
            })
            .unwrap();
        paragraph.text = format!("{} edited", paragraph.text);
        save(&document, &target).unwrap();
        let reloaded = load_docx(&target).unwrap();
        assert!(reloaded
            .body
            .blocks
            .iter()
            .any(|block| matches!(block.kind, BlockKind::Table(_))));
        assert!(reloaded
            .body
            .blocks
            .iter()
            .any(|block| matches!(block.kind, BlockKind::Image(_))));
        let _ = fs::remove_file(target);
    }

    #[test]
    fn splits_paragraphs_in_body_and_table_cells() {
        let mut document = Document::empty();
        let path = document.body.first_paragraph_path().unwrap();
        document.body.paragraph_mut(&path).unwrap().text = "first\nsecond\nthird".to_owned();
        let last = document.body.split_paragraph_newlines(&path).unwrap();
        assert_eq!(document.body.all_paragraphs().len(), 3);
        assert_eq!(document.body.paragraph(&last).unwrap().text, "third");

        let mut table = Table::new(10, 1, 1, &mut document.body.next_id);
        let table_block = document.body.blocks.len();
        table.rows[0].cells[0].blocks[0] = CellBlock::Paragraph(Paragraph::new(
            20,
            "cell one\ncell two",
            Format::default(),
            true,
        ));
        table.modified = true;
        document.body.blocks.push(Block {
            id: 10,
            kind: BlockKind::Table(table),
        });
        let cell_path = ParaPath::Cell {
            block: table_block,
            row: 0,
            cell: 0,
            para: 0,
        };
        let last = document.body.split_paragraph_newlines(&cell_path).unwrap();
        assert_eq!(document.body.paragraph(&last).unwrap().text, "cell two");
    }

    #[test]
    fn parses_page_settings_and_page_breaks() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:pageBreakBefore/></w:pPr><w:r><w:t>new page</w:t></w:r></w:p>
<w:p><w:pPr><w:sectPr/></w:pPr><w:r><w:t>section end</w:t></w:r></w:p>
<w:sectPr><w:pgSz w:w="16838" w:h="11906"/><w:pgMar w:top="720" w:right="900" w:bottom="1080" w:left="1200"/></w:sectPr>
</w:body></w:document>"#;
        let relationships = HashMap::new();
        let mut next_id = 1;
        let body = parse_body(xml, &relationships, &mut next_id).unwrap();
        let paragraphs = body.all_paragraphs();
        assert!(paragraphs[0].page_break_before);
        assert!(paragraphs[1].section_break_after);
        assert_eq!(body.page.width_twips, 16838);
        assert_eq!(body.page.height_twips, 11906);
        assert_eq!(body.page.margin_top, 720);
        assert_eq!(body.page.margin_right, 900);
        assert_eq!(body.page.margin_bottom, 1080);
        assert_eq!(body.page.margin_left, 1200);
    }

    #[test]
    fn parses_multi_paragraph_cell_layout_properties() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:tbl>
<w:tblPr><w:tblW w:type="dxa" w:w="5000"/></w:tblPr>
<w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid>
<w:tr><w:trPr><w:trHeight w:val="600" w:hRule="exact"/></w:trPr>
<w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/>
<w:tcMar><w:top w:w="100" w:type="dxa"/><w:left w:w="200" w:type="dxa"/><w:bottom w:w="300" w:type="dxa"/><w:right w:w="400" w:type="dxa"/></w:tcMar>
<w:vAlign w:val="center"/></w:tcPr>
<w:p><w:r><w:t>first</w:t></w:r></w:p><w:p><w:r><w:t>second</w:t></w:r></w:p>
</w:tc></w:tr></w:tbl><w:sectPr/></w:body></w:document>"#;
        let relationships = HashMap::new();
        let mut next_id = 1;
        let body = parse_body(xml, &relationships, &mut next_id).unwrap();
        let table = body
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockKind::Table(table) => Some(table),
                _ => None,
            })
            .unwrap();
        assert_eq!(table.width_twips, Some(5000));
        assert_eq!(table.rows[0].height_twips, Some(600));
        let cell = &table.rows[0].cells[0];
        assert_eq!(cell.blocks.len(), 2);
        assert_eq!(cell.vertical_align, VerticalAlign::Center);
        assert_eq!(cell.margins.left, 200);
        assert_eq!(cell.margins.top, 100);
        assert_eq!(cell.margins.right, 400);
        assert_eq!(cell.margins.bottom, 300);
        let rendered = render_table(table);
        assert!(rendered.contains("<w:t>first</w:t>"));
        assert!(rendered.contains("<w:t>second</w:t>"));
    }

    #[test]
    fn adds_and_removes_table_rows_and_columns() {
        let mut document = Document::empty();
        let table_id = document.body.alloc_id();
        let mut next = document.body.next_id;
        let table = Table::new(table_id, 2, 2, &mut next);
        document.body.next_id = next;
        document.body.blocks.push(Block {
            id: table_id,
            kind: BlockKind::Table(table),
        });
        assert!(document.body.table_add_row(1));
        assert!(document.body.table_add_column(1));
        let table = document.body.table_mut(1).unwrap();
        assert_eq!(table.rows.len(), 3);
        assert_eq!(table.rows[0].cells.len(), 3);
        assert!(document.body.table_delete_row(1, 2));
        assert!(document.body.table_delete_column(1, 2));
        let table = document.body.table_mut(1).unwrap();
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].cells.len(), 2);
    }

    #[test]
    fn resizes_an_existing_image() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("samples")
            .join("rustword-rich.docx");
        let mut document = load_docx(&source).unwrap();
        let image = document
            .body
            .blocks
            .iter_mut()
            .find_map(|block| match &mut block.kind {
                BlockKind::Image(image) => Some(image),
                _ => None,
            })
            .unwrap();
        let old_width = image.width_emu;
        image.set_width_px(240.0);
        assert_ne!(image.width_emu, old_width);
        let target = temp_path("resized");
        save(&document, &target).unwrap();
        let reloaded = load_docx(&target).unwrap();
        let resized = reloaded
            .body
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockKind::Image(image) => Some(image),
                _ => None,
            })
            .unwrap();
        assert!((resized.width_emu - 240 * 9525).abs() <= 2);
        let _ = fs::remove_file(target);
    }

    #[test]
    fn imports_legacy_doc_sample() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("samples")
            .join("legacy-sample.doc");
        let document = load(&path).unwrap();
        assert_eq!(document.kind, DocKind::LegacyDoc);
        let text = document
            .body
            .all_paragraphs()
            .iter()
            .map(|paragraph| paragraph.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.len() > 40);
    }

    fn make_png() -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&[
                    255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
                ])
                .unwrap();
        }
        bytes
    }
}
