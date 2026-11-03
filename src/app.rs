use crate::docx::{
    self, Alignment, Block, BlockKind, CellBlock, DocKind, Document, Format, ImageBlock, ImagePath,
    ParaPath, ParaStyle, Paragraph, Table, VerticalAlign,
};
use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, CornerRadius, FontData, FontDefinitions, FontFamily,
    FontId, Frame, Key, Layout, Margin, Modifiers, Pos2, Rect, RichText, ScrollArea, Sense, Stroke,
    TextEdit, TextFormat, TextStyle, TextureHandle, TextureOptions, Vec2, ViewportCommand,
};
use image::GenericImageView;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ACCENT: Color32 = Color32::from_rgb(43, 87, 154);
const ACCENT_DARK: Color32 = Color32::from_rgb(31, 64, 116);
const PAGE_BG: Color32 = Color32::from_rgb(244, 242, 237);
const PAGE_WHITE: Color32 = Color32::from_rgb(255, 255, 255);
const BORDER: Color32 = Color32::from_rgb(222, 218, 209);
const MUTED: Color32 = Color32::from_rgb(110, 106, 98);
const SUCCESS: Color32 = Color32::from_rgb(45, 120, 75);
const DANGER: Color32 = Color32::from_rgb(176, 50, 45);
const BOLD_FAMILY: &str = "RustWordBold";
const PAGE_INNER_WIDTH: f32 = 668.0;

pub fn run() -> eframe::Result {
    let initial_path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .filter(|path| path.exists());

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("RustWord")
            .with_inner_size([1120.0, 820.0])
            .with_min_inner_size([760.0, 540.0])
            .with_icon(Arc::new(app_icon())),
        ..Default::default()
    };

    eframe::run_native(
        "RustWord",
        options,
        Box::new(move |creation| Ok(Box::new(RustWordApp::new(creation, initial_path)))),
    )
}

struct RustWordApp {
    doc: Document,
    dirty: bool,
    status: Status,
    error: Option<String>,
    about_open: bool,
    pending: Option<PendingAction>,
    allow_close: bool,
    active: Option<ParaPath>,
    selected_image: Option<ImagePath>,
    focus_id: Option<(u64, usize)>,
    cursor_col: usize,
    textures: HashMap<String, TextureHandle>,
    table_dialog: Option<TableDialog>,
    zoom: f32,
    view_mode: PageView,
    current_page: usize,
    pages: Vec<Vec<usize>>,
    pages_dirty: bool,
    follow_active_page: bool,
    last_window_title: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PageView {
    Single,
    Multi,
}

#[derive(Clone, Copy)]
enum StatusKind {
    Info,
    Success,
    Error,
}

struct Status {
    text: String,
    kind: StatusKind,
}

#[derive(Clone)]
enum PendingAction {
    New,
    OpenDialog,
    OpenPath(PathBuf),
    Close,
}

struct TableDialog {
    rows: usize,
    columns: usize,
}

#[derive(Default)]
struct EditorEvents {
    changed: bool,
    splits: Vec<ParaPath>,
    active: Option<ParaPath>,
    selected_image: Option<ImagePath>,
    clear_image_selection: bool,
    delete_image: Option<ImagePath>,
    cursor_col: usize,
}

impl RustWordApp {
    fn new(creation: &eframe::CreationContext<'_>, initial_path: Option<PathBuf>) -> Self {
        install_fonts(&creation.egui_ctx);
        configure_style(&creation.egui_ctx);

        let mut app = Self {
            doc: Document::empty(),
            dirty: false,
            status: Status {
                text: "New document — start typing, insert a table or image, or open a file."
                    .to_owned(),
                kind: StatusKind::Info,
            },
            error: None,
            about_open: false,
            pending: None,
            allow_close: false,
            active: None,
            selected_image: None,
            focus_id: None,
            cursor_col: 0,
            textures: HashMap::new(),
            table_dialog: None,
            zoom: 0.75,
            view_mode: PageView::Multi,
            current_page: 0,
            pages: Vec::new(),
            pages_dirty: true,
            follow_active_page: false,
            last_window_title: String::new(),
        };
        app.focus_first_paragraph();

        if let Some(path) = initial_path {
            app.open_path(&path);
        }
        app
    }

    fn focus_first_paragraph(&mut self) {
        self.active = self.doc.body.first_paragraph_path();
        if let Some(path) = self.active.clone() {
            if let Some(paragraph) = self.doc.body.paragraph(&path) {
                self.focus_id = Some((paragraph.id, 0));
            }
        }
    }

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Word documents", &["docx", "docm", "doc"])
            .add_filter("All files", &["*"])
            .set_title("Open a Word document")
            .pick_file()
        {
            self.open_path(&path);
        }
    }

    fn open_path(&mut self, path: &Path) {
        match docx::load(path) {
            Ok(document) => {
                let legacy = document.kind == DocKind::LegacyDoc;
                self.doc = document;
                self.textures.clear();
                self.selected_image = None;
                self.pages.clear();
                self.pages_dirty = true;
                self.current_page = 0;
                self.dirty = false;
                self.pending = None;
                self.table_dialog = None;
                self.focus_first_paragraph();
                self.focus_id = None;
                self.status = if legacy {
                    Status {
                        text: "Imported legacy .doc as editable text. Saving creates a .docx copy; the original is left untouched.".to_owned(),
                        kind: StatusKind::Info,
                    }
                } else {
                    Status {
                        text: format!(
                            "Opened {}",
                            path.file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("document")
                        ),
                        kind: StatusKind::Success,
                    }
                };
            }
            Err(error) => {
                self.status = Status {
                    text: "Could not open the document.".to_owned(),
                    kind: StatusKind::Error,
                };
                self.error = Some(format!("Could not open “{}”\n\n{error:#}", path.display()));
            }
        }
    }

    fn new_document(&mut self) {
        self.doc = Document::empty();
        self.textures.clear();
        self.selected_image = None;
        self.pages.clear();
        self.pages_dirty = true;
        self.current_page = 0;
        self.dirty = false;
        self.pending = None;
        self.table_dialog = None;
        self.cursor_col = 0;
        self.focus_first_paragraph();
        self.status = Status {
            text: "New document — start typing, insert a table or image, or open a file."
                .to_owned(),
            kind: StatusKind::Info,
        };
    }

    fn request(&mut self, action: PendingAction) {
        if self.dirty {
            self.pending = Some(action);
        } else {
            self.perform(action);
        }
    }

    fn perform(&mut self, action: PendingAction) {
        self.pending = None;
        match action {
            PendingAction::New => self.new_document(),
            PendingAction::OpenDialog => self.open_dialog(),
            PendingAction::OpenPath(path) => self.open_path(&path),
            PendingAction::Close => self.allow_close = true,
        }
    }

    fn save_as_dialog(&self) -> Option<PathBuf> {
        let current = self.doc.path.as_ref();
        let extension = current
            .and_then(|path| path.extension())
            .and_then(|extension| extension.to_str())
            .filter(|extension| extension.eq_ignore_ascii_case("docm"))
            .unwrap_or("docx");
        let stem = current
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .unwrap_or(&self.doc.title);
        let default_name = format!("{stem}.{extension}");

        let mut dialog = rfd::FileDialog::new()
            .add_filter("Word document (.docx)", &["docx"])
            .add_filter("Macro-enabled Word document (.docm)", &["docm"])
            .set_file_name(&default_name)
            .set_title("Save Word document");
        if let Some(parent) = current.and_then(|path| path.parent()) {
            if parent.is_dir() {
                dialog = dialog.set_directory(parent);
            }
        }
        let path = dialog.save_file()?;
        Some(ensure_extension(path, extension))
    }

    fn save(&mut self, save_as: bool) -> bool {
        let must_dialog = save_as || self.doc.kind == DocKind::LegacyDoc || self.doc.path.is_none();
        let path = if must_dialog {
            let Some(path) = self.save_as_dialog() else {
                return false;
            };
            path
        } else {
            self.doc.path.clone().unwrap_or_default()
        };

        self.save_to(&path)
    }

    fn save_to(&mut self, path: &Path) -> bool {
        let previous_active = self.active.clone();
        let previous_cursor = self.cursor_col;
        match docx::save(&self.doc, path) {
            Ok(result) => {
                self.doc.source_xml = result.xml;
                self.doc.body = result.body;
                self.pages_dirty = true;
                self.doc.package_path = Some(path.to_path_buf());
                self.doc.path = Some(path.to_path_buf());
                self.doc.kind = DocKind::Docx;
                self.doc.title = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("Untitled")
                    .to_owned();
                self.doc.mark_saved();
                self.dirty = false;
                if let Some(path) = previous_active {
                    if let Some(paragraph) = self.doc.body.paragraph(&path) {
                        self.active = Some(path);
                        self.focus_id = Some((paragraph.id, previous_cursor));
                    } else {
                        self.focus_first_paragraph();
                    }
                } else {
                    self.focus_first_paragraph();
                }
                self.status = Status {
                    text: format!(
                        "Saved {}",
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("document")
                    ),
                    kind: StatusKind::Success,
                };
                true
            }
            Err(error) => {
                self.status = Status {
                    text: "Could not save the document.".to_owned(),
                    kind: StatusKind::Error,
                };
                self.error = Some(format!(
                    "Could not save to “{}”\n\n{error:#}",
                    path.display()
                ));
                false
            }
        }
    }

    fn active_format(&self) -> Format {
        self.active
            .as_ref()
            .and_then(|path| self.doc.body.paragraph(path))
            .map(|paragraph| paragraph.format.clone())
            .unwrap_or_default()
    }

    fn apply_format(&mut self, change: impl FnOnce(&mut Format)) {
        if self.active.is_none() {
            self.active = self.doc.body.first_paragraph_path();
        }
        let Some(path) = self.active.clone() else {
            return;
        };
        let Some(paragraph) = self.doc.body.paragraph_mut(&path) else {
            return;
        };
        change(&mut paragraph.format);
        let id = paragraph.id;
        self.focus_id = Some((id, self.cursor_col));
        self.dirty = true;
        self.pages_dirty = true;
        self.pages_dirty = true;
    }

    fn toggle_bold(&mut self) {
        let active = self.active_format().bold;
        self.apply_format(|format| format.bold = !active);
    }

    fn toggle_italic(&mut self) {
        let active = self.active_format().italic;
        self.apply_format(|format| format.italic = !active);
    }

    fn toggle_underline(&mut self) {
        let active = self.active_format().underline;
        self.apply_format(|format| format.underline = !active);
    }

    fn set_alignment(&mut self, alignment: Alignment) {
        self.apply_format(|format| format.alignment = alignment);
    }

    fn set_style(&mut self, style: ParaStyle) {
        self.apply_format(|format| format.apply_style(style));
    }

    fn active_cell(&self) -> Option<(usize, usize, usize, bool)> {
        let Some(ParaPath::Cell {
            block, row, cell, ..
        }) = self.active.as_ref()
        else {
            return None;
        };
        let merged = self
            .doc
            .body
            .blocks
            .get(*block)
            .and_then(|block| match &block.kind {
                BlockKind::Table(table) => Some(table.merged),
                _ => None,
            })
            .unwrap_or(false);
        Some((*block, *row, *cell, merged))
    }

    fn focus_cell(&mut self, block: usize, row: usize, cell: usize) {
        let path = ParaPath::Cell {
            block,
            row,
            cell,
            para: 0,
        };
        if let Some(paragraph) = self.doc.body.paragraph(&path) {
            let id = paragraph.id;
            self.active = Some(path);
            self.focus_id = Some((id, 0));
            self.selected_image = None;
        }
    }

    fn add_table_row(&mut self) {
        let Some((block, row, cell, merged)) = self.active_cell() else {
            return;
        };
        if merged {
            return;
        }
        let target_row = self
            .doc
            .body
            .blocks
            .get(block)
            .and_then(|block| match &block.kind {
                BlockKind::Table(table) => Some(table.rows.len()),
                _ => None,
            })
            .unwrap_or(row + 1);
        if self.doc.body.table_add_row(block) {
            self.focus_cell(block, target_row, cell);
            self.dirty = true;
            self.pages_dirty = true;
        }
    }

    fn add_table_column(&mut self) {
        let Some((block, row, cell, merged)) = self.active_cell() else {
            return;
        };
        if merged {
            return;
        }
        let target_cell = self
            .doc
            .body
            .blocks
            .get(block)
            .and_then(|block| match &block.kind {
                BlockKind::Table(table) => Some(table.column_count()),
                _ => None,
            })
            .unwrap_or(cell + 1);
        if self.doc.body.table_add_column(block) {
            self.focus_cell(block, row, target_cell);
            self.dirty = true;
            self.pages_dirty = true;
        }
    }

    fn delete_table_row(&mut self) {
        let Some((block, row, _, merged)) = self.active_cell() else {
            return;
        };
        if merged {
            return;
        }
        if self.doc.body.table_delete_row(block, row) {
            let remaining = self
                .doc
                .body
                .blocks
                .get(block)
                .and_then(|block| match &block.kind {
                    BlockKind::Table(table) => Some(table.rows.len()),
                    _ => None,
                })
                .unwrap_or(1);
            self.focus_cell(block, row.min(remaining.saturating_sub(1)), 0);
            self.dirty = true;
            self.pages_dirty = true;
        }
    }

    fn delete_table_column(&mut self) {
        let Some((block, row, cell, merged)) = self.active_cell() else {
            return;
        };
        if merged {
            return;
        }
        if self.doc.body.table_delete_column(block, cell) {
            let remaining = self
                .doc
                .body
                .blocks
                .get(block)
                .and_then(|block| match &block.kind {
                    BlockKind::Table(table) => Some(table.column_count()),
                    _ => None,
                })
                .unwrap_or(1);
            self.focus_cell(block, row, cell.min(remaining.saturating_sub(1)));
            self.dirty = true;
            self.pages_dirty = true;
        }
    }

    fn delete_table(&mut self) {
        let Some((block, ..)) = self.active_cell() else {
            return;
        };
        if self.doc.body.remove_block(block) {
            self.active = None;
            self.selected_image = None;
            self.focus_first_paragraph();
            self.dirty = true;
            self.pages_dirty = true;
        }
    }

    fn insert_table(&mut self, rows: usize, columns: usize) {
        let table_id = self.doc.body.alloc_id();
        let mut next = self.doc.body.next_id;
        let table = Table::new(table_id, rows, columns, &mut next);
        self.doc.body.next_id = next;
        let index = self.doc.body.insert_block_after(
            self.active.as_ref(),
            Block {
                id: table_id,
                kind: BlockKind::Table(table),
            },
        );
        self.table_dialog = None;
        self.focus_cell(index, 0, 0);
        self.dirty = true;
        self.pages_dirty = true;
    }

    fn insert_image_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg"])
            .set_title("Insert image")
            .pick_file()
        else {
            return;
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.error = Some(format!("Could not read “{}”\n\n{error}", path.display()));
                return;
            }
        };
        let decoded = match image::load_from_memory(&bytes) {
            Ok(image) => image,
            Err(error) => {
                self.error = Some(format!("Could not decode “{}”\n\n{error}", path.display()));
                return;
            }
        };
        let (width, height) = decoded.dimensions();
        let extension = match image::guess_format(&bytes) {
            Ok(image::ImageFormat::Jpeg) => "jpg",
            _ => "png",
        };
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Picture");
        let image = self.doc.add_image(bytes, extension, width, height, name);
        let id = image.id;
        let index = self.doc.body.insert_block_after(
            self.active.as_ref(),
            Block {
                id,
                kind: BlockKind::Image(image),
            },
        );
        self.selected_image = Some(ImagePath::Body(index));
        self.dirty = true;
        self.pages_dirty = true;
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if self.pending.is_some()
            || self.error.is_some()
            || self.about_open
            || self.table_dialog.is_some()
        {
            return;
        }
        let zoom_in = ctx.input_mut(|input| {
            input.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Plus)
                || input.consume_key(Modifiers::COMMAND, Key::Equals)
        });
        if zoom_in {
            self.zoom = (self.zoom + 0.1).min(2.0);
            self.pages_dirty = true;
            return;
        }
        let zoom_out = ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::Minus));
        if zoom_out {
            self.zoom = (self.zoom - 0.1).max(0.4);
            self.pages_dirty = true;
            return;
        }
        let actual_size = ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::Num0));
        if actual_size {
            self.zoom = 1.0;
            self.pages_dirty = true;
            return;
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::Num1)) {
            self.view_mode = PageView::Single;
            self.follow_active_page = true;
            return;
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::Num2)) {
            self.view_mode = PageView::Multi;
            return;
        }
        let save_as =
            ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::S));
        if save_as {
            self.save(true);
            return;
        }
        let save = ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::S));
        if save {
            self.save(false);
            return;
        }
        let open = ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::O));
        if open {
            self.request(PendingAction::OpenDialog);
            return;
        }
        let new = ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::N));
        if new {
            self.request(PendingAction::New);
            return;
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::B)) {
            self.toggle_bold();
            return;
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::I)) {
            self.toggle_italic();
            return;
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::U)) {
            self.toggle_underline();
        }
    }

    fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu_bar")
            .frame(
                Frame::new()
                    .fill(PAGE_WHITE)
                    .inner_margin(Margin::symmetric(10, 3)),
            )
            .show(ctx, |ui| {
                egui::menu::bar(ui, |ui| {
                    ui.menu_button("File", |ui| {
                        if shortcut_button(ui, "New", "Ctrl+N").clicked() {
                            self.request(PendingAction::New);
                            ui.close_menu();
                        }
                        if shortcut_button(ui, "Open…", "Ctrl+O").clicked() {
                            self.request(PendingAction::OpenDialog);
                            ui.close_menu();
                        }
                        ui.separator();
                        if shortcut_button(ui, "Save", "Ctrl+S").clicked() {
                            self.save(false);
                            ui.close_menu();
                        }
                        if shortcut_button(ui, "Save As…", "Ctrl+Shift+S").clicked() {
                            self.save(true);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Exit").clicked() {
                            self.request(PendingAction::Close);
                            ui.close_menu();
                        }
                    });

                    ui.menu_button("Insert", |ui| {
                        if ui.button("Table…").clicked() {
                            self.table_dialog = Some(TableDialog {
                                rows: 3,
                                columns: 3,
                            });
                            ui.close_menu();
                        }
                        if ui.button("Image…").clicked() {
                            self.insert_image_dialog();
                            ui.close_menu();
                        }
                    });

                    ui.menu_button("Format", |ui| {
                        if shortcut_button(ui, "Bold", "Ctrl+B").clicked() {
                            self.toggle_bold();
                            ui.close_menu();
                        }
                        if shortcut_button(ui, "Italic", "Ctrl+I").clicked() {
                            self.toggle_italic();
                            ui.close_menu();
                        }
                        if shortcut_button(ui, "Underline", "Ctrl+U").clicked() {
                            self.toggle_underline();
                            ui.close_menu();
                        }
                        ui.separator();
                        for style in ParaStyle::ALL {
                            if ui.button(style.label()).clicked() {
                                self.set_style(style);
                                ui.close_menu();
                            }
                        }
                        ui.separator();
                        for alignment in Alignment::ALL {
                            if ui.button(alignment.label()).clicked() {
                                self.set_alignment(alignment);
                                ui.close_menu();
                            }
                        }
                    });

                    ui.menu_button("Help", |ui| {
                        if ui.button("About RustWord").clicked() {
                            self.about_open = true;
                            ui.close_menu();
                        }
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(if self.dirty {
                                "● Unsaved changes"
                            } else {
                                "✓ Saved"
                            })
                            .size(11.5)
                            .color(if self.dirty {
                                MUTED
                            } else {
                                SUCCESS
                            }),
                        );
                    });
                });
            });
    }

    fn toolbar(&mut self, ctx: &egui::Context) {
        let active = self.active_format();
        let mut requested_style = None;
        let mut requested_size = None;
        let mut insert_table = false;
        let mut insert_image = false;
        let mut row_plus = false;
        let mut col_plus = false;
        let mut row_minus = false;
        let mut col_minus = false;
        let mut delete_table = false;
        let mut image_width = None;
        let mut delete_image = false;
        let table_info = self.active_cell();
        let selected_image = self.selected_image.clone().and_then(|path| {
            self.doc
                .body
                .image(&path)
                .map(|image| (path, image.width_px()))
        });

        egui::TopBottomPanel::top("toolbar")
            .frame(
                Frame::new()
                    .fill(Color32::from_rgb(250, 249, 246))
                    .inner_margin(Margin::symmetric(12, 6))
                    .stroke(Stroke::new(1.0, BORDER)),
            )
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if compact_button(ui, "Open").clicked() {
                        self.request(PendingAction::OpenDialog);
                    }
                    if compact_button(ui, "Save").clicked() {
                        self.save(false);
                    }
                    ui.separator();

                    egui::ComboBox::from_id_salt("paragraph_style")
                        .selected_text(RichText::new(active.style.label()).size(13.0))
                        .width(128.0)
                        .show_ui(ui, |ui| {
                            for style in ParaStyle::ALL {
                                if ui
                                    .selectable_label(active.style == style, style.label())
                                    .clicked()
                                {
                                    requested_style = Some(style);
                                }
                            }
                        });

                    let mut size = active.size_pt;
                    if ui
                        .add(
                            egui::DragValue::new(&mut size)
                                .range(8.0..=72.0)
                                .speed(0.25)
                                .suffix(" pt")
                                .max_decimals(1),
                        )
                        .changed()
                    {
                        requested_size = Some(size);
                    }
                    ui.separator();

                    if format_button(ui, "B", active.bold, true, "Bold (Ctrl+B)") {
                        self.toggle_bold();
                    }
                    if format_button(ui, "I", active.italic, false, "Italic (Ctrl+I)") {
                        self.toggle_italic();
                    }
                    if format_button(ui, "U", active.underline, false, "Underline (Ctrl+U)") {
                        self.toggle_underline();
                    }
                    ui.separator();
                    for alignment in Alignment::ALL {
                        let label = match alignment {
                            Alignment::Left => "L",
                            Alignment::Center => "C",
                            Alignment::Right => "R",
                            Alignment::Justify => "J",
                        };
                        if format_button(
                            ui,
                            label,
                            active.alignment == alignment,
                            false,
                            alignment.label(),
                        ) {
                            self.set_alignment(alignment);
                        }
                    }
                    ui.separator();
                    if compact_button(ui, "Table").clicked() {
                        insert_table = true;
                    }
                    if compact_button(ui, "Image").clicked() {
                        insert_image = true;
                    }

                    if let Some((_, _, _, merged)) = table_info {
                        ui.separator();
                        if merged {
                            ui.label(RichText::new("merged table").size(11.5).color(MUTED));
                        } else {
                            if compact_button(ui, "Row +").clicked() {
                                row_plus = true;
                            }
                            if compact_button(ui, "Col +").clicked() {
                                col_plus = true;
                            }
                            if compact_button(ui, "Row −").clicked() {
                                row_minus = true;
                            }
                            if compact_button(ui, "Col −").clicked() {
                                col_minus = true;
                            }
                            if compact_button(ui, "Table ×").clicked() {
                                delete_table = true;
                            }
                        }
                    }

                    if let Some((_, width)) = selected_image {
                        ui.separator();
                        ui.label(RichText::new("Image").size(11.5).color(MUTED));
                        let mut width = width;
                        if ui
                            .add(
                                egui::DragValue::new(&mut width)
                                    .range(40.0..=PAGE_INNER_WIDTH)
                                    .suffix(" px"),
                            )
                            .changed()
                        {
                            image_width = Some(width);
                        }
                        if compact_button(ui, "Remove").clicked() {
                            delete_image = true;
                        }
                    }

                    ui.separator();
                    if compact_button(ui, "−").clicked() {
                        self.zoom = (self.zoom - 0.1).max(0.4);
                        self.pages_dirty = true;
                    }
                    ui.label(
                        RichText::new(format!("{:.0}%", self.zoom * 100.0))
                            .size(11.5)
                            .color(MUTED),
                    );
                    if compact_button(ui, "+").clicked() {
                        self.zoom = (self.zoom + 0.1).min(2.0);
                        self.pages_dirty = true;
                    }
                    if compact_button(ui, "Fit").clicked() {
                        let screen = ctx.screen_rect();
                        let page = self.doc.body.page;
                        let fit_width = (screen.width() - 140.0) / page.width_px().max(1.0);
                        let fit_height = (screen.height() - 220.0) / page.height_px().max(1.0);
                        self.zoom = fit_width.min(fit_height).clamp(0.4, 2.0);
                        self.pages_dirty = true;
                    }
                    ui.separator();
                    if ui
                        .selectable_label(self.view_mode == PageView::Single, "Single")
                        .clicked()
                    {
                        self.view_mode = PageView::Single;
                        self.follow_active_page = true;
                    }
                    if ui
                        .selectable_label(self.view_mode == PageView::Multi, "Multi")
                        .clicked()
                    {
                        self.view_mode = PageView::Multi;
                    }
                });
            });

        if let Some(style) = requested_style {
            self.set_style(style);
        }
        if let Some(size) = requested_size {
            self.apply_format(|format| format.size_pt = size.clamp(8.0, 72.0));
        }
        if insert_table {
            self.table_dialog = Some(TableDialog {
                rows: 3,
                columns: 3,
            });
        }
        if insert_image {
            self.insert_image_dialog();
        }
        if row_plus {
            self.add_table_row();
        }
        if col_plus {
            self.add_table_column();
        }
        if row_minus {
            self.delete_table_row();
        }
        if col_minus {
            self.delete_table_column();
        }
        if delete_table {
            self.delete_table();
        }
        if let Some(width) = image_width {
            if let Some(path) = self.selected_image.clone() {
                if let Some(image) = self.doc.body.image_mut(&path) {
                    image.set_width_px(width);
                    self.dirty = true;
                    self.pages_dirty = true;
                }
            }
        }
        if delete_image {
            if let Some(path) = self.selected_image.clone() {
                if self.doc.body.remove_image(&path) {
                    self.selected_image = None;
                    self.dirty = true;
                    self.pages_dirty = true;
                }
            }
        }
    }

    fn editor(&mut self, ctx: &egui::Context) {
        let mut blocks = std::mem::take(&mut self.doc.body.blocks);
        let page = self.doc.body.page;
        let zoom = self.zoom;
        let is_single_empty = blocks.len() == 1
            && matches!(
                &blocks[0].kind,
                BlockKind::Paragraph(paragraph) if paragraph.text.is_empty()
            );
        let mut events = EditorEvents::default();

        egui::CentralPanel::default()
            .frame(Frame::new().fill(PAGE_BG))
            .show(ctx, |ui| {
                if self.pages_dirty || self.pages.is_empty() {
                    self.pages = paginate_blocks(ui, &blocks, page, zoom);
                    self.pages_dirty = false;
                }
                let page_count = self.pages.len().max(1);
                if self.current_page >= page_count {
                    self.current_page = page_count - 1;
                }
                if self.follow_active_page {
                    if let Some(active) = self.active.as_ref() {
                        let block = self.doc.body.block_index(active);
                        if let Some(index) = self
                            .pages
                            .iter()
                            .position(|page_blocks| page_blocks.contains(&block))
                        {
                            self.current_page = index;
                        }
                    }
                    self.follow_active_page = false;
                }
                let pages = self.pages.clone();
                let page_indices: Vec<usize> = match self.view_mode {
                    PageView::Single => vec![self.current_page.min(pages.len().saturating_sub(1))],
                    PageView::Multi => (0..pages.len()).collect(),
                };

                ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(22.0);
                        for page_index in page_indices {
                            let page_blocks =
                                pages.get(page_index).map(Vec::as_slice).unwrap_or(&[]);
                            self.render_page(
                                ui,
                                page_index,
                                page_blocks,
                                &mut blocks,
                                page,
                                zoom,
                                is_single_empty,
                                &mut events,
                            );
                            ui.add_space(26.0);
                        }
                    });
            });

        self.doc.body.blocks = blocks;

        if events.changed {
            self.dirty = true;
            self.pages_dirty = true;
            self.follow_active_page = true;
        }
        if let Some(path) = events.active {
            self.active = Some(path);
            self.selected_image = None;
        }
        if events.clear_image_selection {
            self.selected_image = None;
        }
        if let Some(path) = events.selected_image {
            self.selected_image = Some(path);
        }
        self.cursor_col = events.cursor_col;
        if let Some(path) = events.delete_image {
            if self.doc.body.remove_image(&path) {
                self.selected_image = None;
                self.dirty = true;
                self.pages_dirty = true;
            }
        }
        if !events.splits.is_empty() {
            for path in events.splits {
                if let Some(last) = self.doc.body.split_paragraph_newlines(&path) {
                    if let Some(paragraph) = self.doc.body.paragraph(&last) {
                        let id = paragraph.id;
                        self.focus_id = Some((id, 0));
                        self.active = Some(last);
                        self.cursor_col = 0;
                    }
                    self.dirty = true;
                    self.pages_dirty = true;
                    self.follow_active_page = true;
                }
            }
        }
    }

    fn render_page(
        &mut self,
        ui: &mut egui::Ui,
        page_index: usize,
        page_blocks: &[usize],
        blocks: &mut [Block],
        page: docx::PageSettings,
        zoom: f32,
        is_single_empty: bool,
        events: &mut EditorEvents,
    ) {
        let page_width = page.width_px() * zoom;
        let page_height = page.height_px() * zoom;
        let available = ui.available_width();
        ui.horizontal(|ui| {
            if page_width < available {
                ui.add_space((available - page_width) * 0.5);
            }
            let (page_rect, _) =
                ui.allocate_exact_size(Vec2::new(page_width, page_height), Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(
                page_rect.translate(Vec2::new(0.0, 5.0)).expand(2.0),
                CornerRadius::same(4),
                Color32::from_black_alpha(34),
            );
            painter.rect_filled(page_rect, CornerRadius::same(2), PAGE_WHITE);
            painter.rect_stroke(
                page_rect,
                CornerRadius::same(2),
                Stroke::new(1.0, BORDER),
                egui::StrokeKind::Inside,
            );

            let margin_left = page.margin_left as f32 / 15.0 * zoom;
            let margin_right = page.margin_right as f32 / 15.0 * zoom;
            let margin_top = page.margin_top as f32 / 15.0 * zoom;
            let margin_bottom = page.margin_bottom as f32 / 15.0 * zoom;
            let content_rect = Rect::from_min_max(
                page_rect.min + Vec2::new(margin_left, margin_top),
                page_rect.max - Vec2::new(margin_right, margin_bottom),
            );
            {
                let mut content = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(content_rect)
                        .layout(Layout::top_down(Align::Min)),
                );
                content.set_clip_rect(page_rect.intersect(ui.clip_rect()));
                content.set_width(content_rect.width());
                content.set_min_height(content_rect.height());
                for block_index in page_blocks {
                    let Some(block) = blocks.get_mut(*block_index) else {
                        continue;
                    };
                    match &mut block.kind {
                        BlockKind::Paragraph(paragraph) => {
                            self.paragraph_ui(
                                &mut content,
                                paragraph,
                                ParaPath::Body(*block_index),
                                events,
                                is_single_empty && *block_index == 0,
                                content_rect.width(),
                            );
                            content.add_space(3.0 * zoom);
                        }
                        BlockKind::Table(table) => {
                            self.table_ui(&mut content, table, *block_index, events);
                            content.add_space(10.0 * zoom);
                        }
                        BlockKind::Image(image) => {
                            self.image_ui(
                                &mut content,
                                image,
                                ImagePath::Body(*block_index),
                                events,
                                content_rect.width(),
                            );
                            content.add_space(8.0 * zoom);
                        }
                        BlockKind::Raw(_) => {}
                    }
                }
            }
            let painter = ui.painter();
            painter.text(
                egui::Pos2::new(page_rect.center().x, page_rect.max.y - margin_bottom * 0.45),
                Align2::CENTER_CENTER,
                format!("{}", page_index + 1),
                FontId::new((9.0 * zoom).max(7.0), FontFamily::Proportional),
                MUTED,
            );
        });
    }
    fn paragraph_ui(
        &mut self,
        ui: &mut egui::Ui,
        paragraph: &mut Paragraph,
        path: ParaPath,
        events: &mut EditorEvents,
        show_hint: bool,
        wrap_width: f32,
    ) {
        let mut format = paragraph.format.clone();
        format.size_pt *= self.zoom;
        let bold_family = FontFamily::Name(BOLD_FAMILY.into());
        let mut layouter = move |ui: &egui::Ui, text: &str, width: f32| {
            layout_paragraph(
                ui,
                text,
                width.min(wrap_width),
                &format,
                bold_family.clone(),
            )
        };
        let id = egui::Id::new(("paragraph", paragraph.id));
        let hint = if show_hint {
            "Start writing, or use Table / Image in the toolbar…"
        } else {
            ""
        };
        let output = TextEdit::multiline(&mut paragraph.text)
            .id(id)
            .desired_width(f32::INFINITY)
            .desired_rows(1)
            .frame(false)
            .margin(Margin::ZERO)
            .hint_text(RichText::new(hint).size(15.0).color(MUTED))
            .layouter(&mut layouter)
            .show(ui);

        if output.response.changed() {
            events.changed = true;
            if paragraph.text.contains('\n') {
                events.splits.push(path.clone());
            }
        }
        if output.response.clicked() || output.response.has_focus() {
            events.active = Some(path);
            events.clear_image_selection = true;
            if let Some(range) = output.cursor_range {
                events.cursor_col = range.primary.ccursor.index;
            }
        }
        if let Some((focus_id, position)) = self.focus_id {
            if focus_id == paragraph.id {
                let mut state = output.state;
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::one(
                        egui::text::CCursor::new(position),
                    )));
                state.store(ui.ctx(), output.response.id);
                output.response.request_focus();
                self.focus_id = None;
            }
        }
    }

    fn table_ui(
        &mut self,
        ui: &mut egui::Ui,
        table: &mut Table,
        block_index: usize,
        events: &mut EditorEvents,
    ) {
        let columns = table.column_count();
        let available = ui.available_width();
        let table_width = table
            .width_twips
            .map(|width| width as f32 / 15.0 * self.zoom)
            .unwrap_or(available)
            .min(available)
            .max(120.0);
        let widths = table_pixel_widths(table, table_width, columns, self.zoom);
        for (row_index, row) in table.rows.iter_mut().enumerate() {
            let row_height = row
                .height_twips
                .map(|height| height as f32 / 15.0 * self.zoom)
                .unwrap_or(0.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let mut column = 0_usize;
                for (cell_index, cell) in row.cells.iter_mut().enumerate() {
                    let span = cell
                        .grid_span
                        .max(1)
                        .min(columns.saturating_sub(column).max(1));
                    let width_start = column.min(widths.len().saturating_sub(1));
                    let width_end = (column + span).min(widths.len());
                    let cell_width: f32 = if width_start < width_end {
                        widths[width_start..width_end].iter().sum()
                    } else {
                        widths.last().copied().unwrap_or(160.0)
                    };
                    let margins = cell.margins;
                    let left = (margins.left as f32 / 15.0 * self.zoom)
                        .round()
                        .clamp(0.0, 90.0) as i8;
                    let right = (margins.right as f32 / 15.0 * self.zoom)
                        .round()
                        .clamp(0.0, 90.0) as i8;
                    let top = (margins.top as f32 / 15.0 * self.zoom)
                        .round()
                        .clamp(0.0, 90.0) as i8;
                    let bottom = (margins.bottom as f32 / 15.0 * self.zoom)
                        .round()
                        .clamp(0.0, 90.0) as i8;
                    let content_width = (cell_width - left as f32 - right as f32 - 2.0).max(24.0);
                    let content_height =
                        estimate_cell_content_height(ui, cell, content_width, self.zoom);
                    let usable_height = (row_height - top as f32 - bottom as f32).max(0.0);
                    let top_pad = match cell.vertical_align {
                        VerticalAlign::Top => 0.0,
                        VerticalAlign::Center => ((usable_height - content_height) * 0.5).max(0.0),
                        VerticalAlign::Bottom => (usable_height - content_height).max(0.0),
                    };
                    let fill = cell
                        .shading
                        .map(|color| Color32::from_rgb(color[0], color[1], color[2]))
                        .unwrap_or(PAGE_WHITE);
                    Frame::new()
                        .fill(fill)
                        .stroke(Stroke::new(1.0, BORDER))
                        .inner_margin(Margin {
                            left,
                            right,
                            top,
                            bottom,
                        })
                        .show(ui, |ui| {
                            // A cell must stack its paragraphs vertically even though the
                            // table row itself is laid out horizontally.
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.set_width(content_width);
                                if top_pad > 0.5 {
                                    ui.add_space(top_pad);
                                }
                                let minimum_height = (usable_height - top_pad)
                                    .max(22.0 * self.zoom - content_height);
                                ui.set_min_height(minimum_height);
                                for (block_index_in_cell, block) in
                                    cell.blocks.iter_mut().enumerate()
                                {
                                    match block {
                                        CellBlock::Paragraph(paragraph) => {
                                            let path = ParaPath::Cell {
                                                block: block_index,
                                                row: row_index,
                                                cell: cell_index,
                                                para: block_index_in_cell,
                                            };
                                            self.paragraph_ui(
                                                ui,
                                                paragraph,
                                                path,
                                                events,
                                                false,
                                                content_width,
                                            );
                                        }
                                        CellBlock::Image(image) => {
                                            let path = ImagePath::Cell {
                                                block: block_index,
                                                row: row_index,
                                                cell: cell_index,
                                                image: block_index_in_cell,
                                            };
                                            self.image_ui(ui, image, path, events, content_width);
                                        }
                                        CellBlock::Raw(_) => {}
                                    }
                                }
                            });
                        });
                    column += span;
                }
            });
        }
    }

    fn image_ui(
        &mut self,
        ui: &mut egui::Ui,
        image: &ImageBlock,
        path: ImagePath,
        events: &mut EditorEvents,
        max_width: f32,
    ) {
        let texture = self.texture_for(ui.ctx(), &image.media_target);
        let width = (image.width_px() * self.zoom).min(max_width).max(24.0);
        let height = (width / image.aspect()).max(16.0);
        let available = ui.available_width();
        ui.horizontal(|ui| {
            ui.add_space(((available - width) * 0.5).max(0.0));
            let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
            if let Some(texture) = &texture {
                ui.painter().image(
                    texture.id(),
                    rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                ui.painter().rect_filled(rect, 2, PAGE_BG);
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    "Image unavailable",
                    FontId::new(12.0, FontFamily::Proportional),
                    MUTED,
                );
            }
            if self.selected_image.as_ref() == Some(&path) {
                ui.painter().rect_stroke(
                    rect,
                    0,
                    Stroke::new(2.0, ACCENT),
                    egui::StrokeKind::Inside,
                );
            }
            if response.clicked() {
                events.selected_image = Some(path);
            }
        });
    }

    fn texture_for(&mut self, ctx: &egui::Context, target: &str) -> Option<TextureHandle> {
        if let Some(texture) = self.textures.get(target) {
            return Some(texture.clone());
        }
        let bytes = self.doc.image_bytes(target)?;
        let decoded = image::load_from_memory(bytes).ok()?;
        let rgba = decoded.to_rgba8();
        let image = ColorImage::from_rgba_unmultiplied(
            [rgba.width() as usize, rgba.height() as usize],
            rgba.as_raw(),
        );
        let texture = ctx.load_texture(format!("rustword:{target}"), image, TextureOptions::LINEAR);
        self.textures.insert(target.to_owned(), texture.clone());
        Some(texture)
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let (paragraphs, words, chars) = self.doc.body.text_stats();
        let active_id = self
            .active
            .as_ref()
            .and_then(|path| self.doc.body.paragraph(path))
            .map(|paragraph| paragraph.id);
        let paragraph_number = active_id
            .and_then(|id| {
                self.doc
                    .body
                    .all_paragraphs()
                    .iter()
                    .position(|paragraph| paragraph.id == id)
            })
            .map(|index| index + 1)
            .unwrap_or(0);

        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(28.0)
            .frame(
                Frame::new()
                    .fill(Color32::from_rgb(248, 247, 243))
                    .inner_margin(Margin::symmetric(12, 0))
                    .stroke(Stroke::new(1.0, BORDER)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let color = match self.status.kind {
                        StatusKind::Info => MUTED,
                        StatusKind::Success => SUCCESS,
                        StatusKind::Error => DANGER,
                    };
                    ui.label(RichText::new(&self.status.text).size(11.5).color(color));

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(self.doc.kind.label()).size(11.0).color(MUTED));
                        ui.separator();
                        ui.label(
                            RichText::new(format!("{words} words · {chars} chars"))
                                .size(11.0)
                                .color(MUTED),
                        );
                        ui.separator();
                        ui.label(
                            RichText::new(format!(
                                "P {} / {} · Col {}",
                                paragraph_number,
                                paragraphs,
                                self.cursor_col + 1
                            ))
                            .size(11.0)
                            .color(MUTED),
                        );
                        ui.separator();
                        let page_count = self.pages.len().max(1);
                        if self.view_mode == PageView::Single {
                            if ui.small_button("›").clicked() {
                                self.current_page =
                                    (self.current_page + 1).min(page_count.saturating_sub(1));
                            }
                            ui.label(
                                RichText::new(format!(
                                    "Page {} / {}",
                                    self.current_page + 1,
                                    page_count
                                ))
                                .size(11.0)
                                .color(MUTED),
                            );
                            if ui.small_button("‹").clicked() {
                                self.current_page = self.current_page.saturating_sub(1);
                            }
                        } else {
                            ui.label(
                                RichText::new(format!("{page_count} pages"))
                                    .size(11.0)
                                    .color(MUTED),
                            );
                        }
                    });
                });
            });
    }

    fn table_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.table_dialog else {
            return;
        };
        let mut insert = false;
        let mut cancel = false;
        egui::Window::new("Insert table")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_width(300.0);
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Rows");
                    ui.add(egui::DragValue::new(&mut dialog.rows).range(1..=100));
                    ui.add_space(12.0);
                    ui.label("Columns");
                    ui.add(egui::DragValue::new(&mut dialog.columns).range(1..=12));
                });
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Insert").strong()).clicked() {
                        insert = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                });
            });
        if insert {
            let rows = dialog.rows;
            let columns = dialog.columns;
            self.insert_table(rows, columns);
        } else if cancel || ctx.input(|input| input.key_pressed(Key::Escape)) {
            self.table_dialog = None;
        }
    }

    fn unsaved_dialog(&mut self, ctx: &egui::Context) {
        let Some(action) = self.pending.clone() else {
            return;
        };
        let action_label = match action {
            PendingAction::New => "creating a new document",
            PendingAction::OpenDialog | PendingAction::OpenPath(_) => "opening another document",
            PendingAction::Close => "closing RustWord",
        };
        let mut choice: Option<Choice> = None;

        egui::Window::new("Unsaved changes")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_width(410.0);
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "Save changes to “{}” before {}?",
                        self.doc.title, action_label
                    ))
                    .size(15.0),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Your edits will be lost if you don’t save them.")
                        .size(12.5)
                        .color(MUTED),
                );
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Save").strong()).clicked() {
                        choice = Some(Choice::Save);
                    }
                    if ui.button("Don’t save").clicked() {
                        choice = Some(Choice::Discard);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            choice = Some(Choice::Cancel);
                        }
                    });
                });
            });

        if ctx.input(|input| input.key_pressed(Key::Escape)) {
            choice = Some(Choice::Cancel);
        }

        match choice {
            Some(Choice::Save) => {
                if self.save(false) {
                    self.perform(action);
                }
            }
            Some(Choice::Discard) => self.perform(action),
            Some(Choice::Cancel) => self.pending = None,
            None => {}
        }
    }

    fn error_dialog(&mut self, ctx: &egui::Context) {
        let Some(message) = self.error.clone() else {
            return;
        };
        let mut close = false;
        egui::Window::new("Something went wrong")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_width(470.0);
                ui.add_space(4.0);
                ui.label(RichText::new(message).size(13.0).color(DANGER));
                ui.add_space(16.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("OK").clicked() {
                        close = true;
                    }
                });
            });
        if close || ctx.input(|input| input.key_pressed(Key::Escape)) {
            self.error = None;
        }
    }

    fn about_dialog(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut close = false;
        egui::Window::new("About RustWord")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.add_space(4.0);
                ui.label(
                    RichText::new("RustWord 0.3.0")
                        .size(22.0)
                        .strong()
                        .color(ACCENT),
                );
                ui.label(
                    RichText::new("A minimal Word editor, written in Rust.")
                        .size(13.0)
                        .color(MUTED),
                );
                ui.add_space(12.0);
                ui.label(
                    "Open and edit .docx and .docm files with paragraphs, editable tables, \
                     and inline images.",
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Legacy .doc files are imported as text and saved as a new .docx. \
                         The original .doc is never modified.",
                    )
                    .size(12.0)
                    .color(MUTED),
                );
                ui.add_space(16.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        if close || ctx.input(|input| input.key_pressed(Key::Escape)) {
            self.about_open = false;
        }
    }

    fn update_window_title(&mut self, ctx: &egui::Context) {
        let title = format!(
            "{}{} — RustWord",
            self.doc.title,
            if self.dirty { "*" } else { "" }
        );
        if title != self.last_window_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.last_window_title = title;
        }
    }

    fn drop_overlay(&self, ctx: &egui::Context) {
        let hovering = ctx.input(|input| !input.raw.hovered_files.is_empty());
        if !hovering {
            return;
        }
        let rect = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("drop_overlay"),
        ));
        painter.rect_filled(rect, 0, Color32::from_black_alpha(90));
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Drop a Word document to open it",
            FontId::new(24.0, FontFamily::Proportional),
            Color32::WHITE,
        );
    }
}

impl eframe::App for RustWordApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|input| input.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            if self.dirty {
                self.pending = Some(PendingAction::Close);
            } else {
                self.allow_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }

        let dropped_path = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .find_map(|file| file.path.clone())
        });
        if let Some(path) = dropped_path {
            if self.pending.is_none() && self.error.is_none() {
                self.request(PendingAction::OpenPath(path));
            }
        }

        self.handle_shortcuts(ctx);
        self.update_window_title(ctx);
        self.menu_bar(ctx);
        self.toolbar(ctx);
        self.editor(ctx);
        self.status_bar(ctx);
        self.drop_overlay(ctx);

        self.unsaved_dialog(ctx);
        self.error_dialog(ctx);
        self.about_dialog(ctx);
        self.table_dialog(ctx);

        if self.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }
}

#[derive(Clone, Copy)]
enum Choice {
    Save,
    Discard,
    Cancel,
}

fn ensure_extension(mut path: PathBuf, extension: &str) -> PathBuf {
    let matches = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(extension))
        .unwrap_or(false);
    if !matches {
        path.set_extension(extension);
    }
    path
}

fn shortcut_button(ui: &mut egui::Ui, label: &str, shortcut: &str) -> egui::Response {
    ui.add(egui::Button::new(label).shortcut_text(shortcut))
}

fn compact_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(label).size(12.5)).min_size(Vec2::new(50.0, 26.0)))
}

fn format_button(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    bold: bool,
    tooltip: &str,
) -> bool {
    let mut text = RichText::new(label).size(13.0);
    if bold {
        text = text.strong();
    }
    let response = ui
        .add(
            egui::Button::new(text)
                .selected(selected)
                .min_size(Vec2::new(28.0, 26.0)),
        )
        .on_hover_text(tooltip);
    response.clicked()
}

fn estimate_cell_content_height(
    ui: &egui::Ui,
    cell: &docx::TableCell,
    wrap_width: f32,
    scale: f32,
) -> f32 {
    let mut height = 0.0;
    for block in &cell.blocks {
        match block {
            CellBlock::Paragraph(paragraph) => {
                let family = if paragraph.format.bold {
                    FontFamily::Name(BOLD_FAMILY.into())
                } else {
                    FontFamily::Proportional
                };
                let font = FontId::new(paragraph.format.size_pt * scale, family);
                let color = Color32::from_rgb(
                    paragraph.format.color[0],
                    paragraph.format.color[1],
                    paragraph.format.color[2],
                );
                let text_height = ui.fonts(|fonts| {
                    fonts
                        .layout(paragraph.text.clone(), font, color, wrap_width.max(24.0))
                        .size()
                        .y
                });
                height += text_height * 1.15 + 3.0 * scale;
            }
            CellBlock::Image(image) => {
                height += (image.width_px() * scale).min(wrap_width).max(24.0) / image.aspect()
                    + 3.0 * scale;
            }
            CellBlock::Raw(_) => {}
        }
    }
    height
}

fn paginate_blocks(
    ui: &egui::Ui,
    blocks: &[Block],
    page: docx::PageSettings,
    zoom: f32,
) -> Vec<Vec<usize>> {
    let content_width = (page.content_width_px() * zoom).max(120.0);
    let content_height = (page.content_height_px() * zoom).max(160.0);
    let mut pages: Vec<Vec<usize>> = vec![Vec::new()];
    let mut used = 0.0_f32;

    for (index, block) in blocks.iter().enumerate() {
        let (break_before, break_after) = match &block.kind {
            BlockKind::Paragraph(paragraph) => {
                (paragraph.page_break_before, paragraph.section_break_after)
            }
            _ => (false, false),
        };
        if break_before && !pages.last().is_some_and(Vec::is_empty) {
            pages.push(Vec::new());
            used = 0.0;
        }
        let height = measure_block_height(ui, block, content_width, zoom);
        if used > 0.0 && used + height > content_height {
            pages.push(Vec::new());
            used = 0.0;
        }
        if pages.last().is_none() {
            pages.push(Vec::new());
        }
        pages.last_mut().unwrap().push(index);
        used += height;
        if break_after {
            pages.push(Vec::new());
            used = 0.0;
        }
    }
    while pages.len() > 1 && pages.last().is_some_and(Vec::is_empty) {
        pages.pop();
    }
    if pages.is_empty() {
        pages.push(Vec::new());
    }
    pages
}

fn measure_block_height(ui: &egui::Ui, block: &Block, content_width: f32, zoom: f32) -> f32 {
    match &block.kind {
        BlockKind::Paragraph(paragraph) => {
            let mut format = paragraph.format.clone();
            format.size_pt *= zoom;
            let galley = layout_paragraph(
                ui,
                &paragraph.text,
                content_width,
                &format,
                FontFamily::Name(BOLD_FAMILY.into()),
            );
            galley.size().y.max(format.size_pt * 1.4) + 3.0 * zoom
        }
        BlockKind::Table(table) => {
            measure_table_height(ui, table, content_width, zoom) + 10.0 * zoom
        }
        BlockKind::Image(image) => {
            (image.width_px() * zoom).min(content_width).max(24.0) / image.aspect() + 8.0 * zoom
        }
        BlockKind::Raw(_) => 0.0,
    }
}

fn measure_table_height(ui: &egui::Ui, table: &Table, content_width: f32, zoom: f32) -> f32 {
    let columns = table.column_count();
    let table_width = table
        .width_twips
        .map(|width| width as f32 / 15.0 * zoom)
        .unwrap_or(content_width)
        .min(content_width)
        .max(120.0);
    let widths = table_pixel_widths(table, table_width, columns, zoom);
    let mut total = 0.0;
    for row in &table.rows {
        let mut row_height = row
            .height_twips
            .map(|height| height as f32 / 15.0 * zoom)
            .unwrap_or(0.0);
        let mut column = 0_usize;
        for cell in &row.cells {
            let span = cell
                .grid_span
                .max(1)
                .min(columns.saturating_sub(column).max(1));
            let width_start = column.min(widths.len().saturating_sub(1));
            let width_end = (column + span).min(widths.len());
            let cell_width: f32 = if width_start < width_end {
                widths[width_start..width_end].iter().sum()
            } else {
                widths.last().copied().unwrap_or(160.0)
            };
            let left = cell.margins.left as f32 / 15.0 * zoom;
            let right = cell.margins.right as f32 / 15.0 * zoom;
            let top = cell.margins.top as f32 / 15.0 * zoom;
            let bottom = cell.margins.bottom as f32 / 15.0 * zoom;
            let content = estimate_cell_content_height(
                ui,
                cell,
                (cell_width - left - right - 2.0).max(24.0),
                zoom,
            );
            row_height = row_height.max(content + top + bottom + 2.0);
            column += span;
        }
        total += row_height.max(22.0 * zoom);
    }
    // TextEdit rows carry a little more vertical padding than font metrics alone
    // predict, especially in wide tables with wrapped cells. Be conservative so
    // a table is moved to the next page instead of being clipped at a boundary.
    total * 1.35 + 4.0 * zoom
}

fn table_pixel_widths(table: &Table, available: f32, columns: usize, scale: f32) -> Vec<f32> {
    let available = available.max(160.0);
    let weights = if table.col_widths.len() >= columns {
        table.col_widths[..columns].to_vec()
    } else {
        let mut weights = table.col_widths.clone();
        weights.resize(columns, 2400);
        weights
    };
    let total: u32 = weights.iter().sum::<u32>().max(1);
    let mut widths: Vec<f32> = weights
        .iter()
        .map(|weight| available * (*weight as f32 / total as f32))
        .collect();
    let minimum = 48.0 * scale;
    let mut deficit = 0.0;
    for width in &mut widths {
        if *width < minimum {
            deficit += minimum - *width;
            *width = minimum;
        }
    }
    if deficit > 0.0 {
        let flexible: Vec<usize> = widths
            .iter()
            .enumerate()
            .filter_map(|(index, width)| if *width > minimum { Some(index) } else { None })
            .collect();
        if !flexible.is_empty() {
            let total_flexible: f32 = flexible.iter().map(|index| widths[*index]).sum();
            for index in flexible {
                widths[index] =
                    (widths[index] - deficit * (widths[index] / total_flexible)).max(minimum);
            }
        }
    }
    widths
}

fn layout_paragraph(
    ui: &egui::Ui,
    text: &str,
    wrap_width: f32,
    format: &Format,
    bold_family: FontFamily,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let family = if format.bold || format.style == ParaStyle::Title {
        bold_family
    } else {
        FontFamily::Proportional
    };
    let color = Color32::from_rgb(format.color[0], format.color[1], format.color[2]);
    let mut text_format = TextFormat {
        font_id: FontId::new(format.size_pt, family),
        color,
        italics: format.italic,
        line_height: Some((format.size_pt * 1.4).max(15.0)),
        ..Default::default()
    };
    if format.underline {
        text_format.underline = Stroke::new(1.0, color);
    }
    let count = text.split('\n').count();
    for (index, line) in text.split('\n').enumerate() {
        let line_width = ui.fonts(|fonts| {
            fonts
                .layout_no_wrap(
                    line.to_owned(),
                    text_format.font_id.clone(),
                    text_format.color,
                )
                .size()
                .x
        });
        let leading_space = if line_width < wrap_width {
            match format.alignment {
                Alignment::Center => (wrap_width - line_width) * 0.5,
                Alignment::Right => wrap_width - line_width,
                Alignment::Left | Alignment::Justify => 0.0,
            }
        } else {
            0.0
        };
        job.append(line, leading_space, text_format.clone());
        if index + 1 < count {
            job.append("\n", 0.0, text_format.clone());
        }
    }
    ui.fonts(|fonts| fonts.layout_job(job))
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let regular = load_font(&[
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\arial.ttf",
    ]);
    let bold = load_font(&[
        r"C:\Windows\Fonts\segoeuib.ttf",
        r"C:\Windows\Fonts\arialbd.ttf",
    ]);
    let cjk_regular = load_font(&[r"C:\Windows\Fonts\msyh.ttc", r"C:\Windows\Fonts\simsun.ttc"]);
    let cjk_bold = load_font(&[
        r"C:\Windows\Fonts\msyhbd.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
    ]);

    if let Some(data) = regular {
        fonts
            .font_data
            .insert("rustword-regular".to_owned(), Arc::new(data));
    }
    if let Some(data) = bold {
        fonts
            .font_data
            .insert("rustword-bold".to_owned(), Arc::new(data));
    }
    if let Some(data) = cjk_regular {
        fonts
            .font_data
            .insert("rustword-cjk".to_owned(), Arc::new(data));
    }
    if let Some(data) = cjk_bold {
        fonts
            .font_data
            .insert("rustword-cjk-bold".to_owned(), Arc::new(data));
    }

    let mut regular_stack = Vec::new();
    if fonts.font_data.contains_key("rustword-regular") {
        regular_stack.push("rustword-regular".to_owned());
    }
    if fonts.font_data.contains_key("rustword-cjk") {
        regular_stack.push("rustword-cjk".to_owned());
    }
    regular_stack.extend([
        "Ubuntu-Light".to_owned(),
        "NotoEmoji-Regular".to_owned(),
        "emoji-icon-font".to_owned(),
    ]);

    let mut bold_stack = Vec::new();
    if fonts.font_data.contains_key("rustword-bold") {
        bold_stack.push("rustword-bold".to_owned());
    }
    if fonts.font_data.contains_key("rustword-cjk-bold") {
        bold_stack.push("rustword-cjk-bold".to_owned());
    }
    bold_stack.extend(regular_stack.iter().cloned());

    fonts
        .families
        .insert(FontFamily::Proportional, regular_stack.clone());
    fonts
        .families
        .insert(FontFamily::Name(BOLD_FAMILY.into()), bold_stack);
    fonts.families.insert(FontFamily::Monospace, regular_stack);
    ctx.set_fonts(fonts);
}

fn load_font(paths: &[&str]) -> Option<FontData> {
    for path in paths {
        if let Ok(bytes) = std::fs::read(path) {
            let mut data = FontData::from_owned(bytes);
            data.index = 0;
            return Some(data);
        }
    }
    None
}

fn configure_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::light();
    visuals.panel_fill = PAGE_BG;
    visuals.window_fill = PAGE_WHITE;
    visuals.extreme_bg_color = PAGE_WHITE;
    visuals.faint_bg_color = Color32::from_rgb(248, 247, 243);
    visuals.selection.bg_fill = Color32::from_rgb(204, 222, 244);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT_DARK);
    visuals.hyperlink_color = ACCENT;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(245, 243, 238);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(237, 234, 227);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(228, 224, 215);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(54, 51, 46));
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(10.0, 5.0);
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(13.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(11.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(20.0, FontFamily::Proportional),
    );
    ctx.set_style(style);
}

fn app_icon() -> egui::IconData {
    let width = 64_u32;
    let height = 64_u32;
    let mut rgba = vec![0_u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let index = ((y * width + x) * 4) as usize;
            let edge = x < 6 || y < 6 || x >= width - 6 || y >= height - 6;
            let inside_page = (14..50).contains(&x) && (10..54).contains(&y);
            let line =
                inside_page && (y == 20 || y == 28 || y == 36 || y == 44) && (20..44).contains(&x);
            let (r, g, b, a) = if edge {
                (31, 64, 116, 255)
            } else if inside_page {
                if line {
                    (43, 87, 154, 255)
                } else {
                    (255, 255, 255, 255)
                }
            } else {
                (43, 87, 154, 255)
            };
            rgba[index] = r;
            rgba[index + 1] = g;
            rgba[index + 2] = b;
            rgba[index + 3] = a;
        }
    }
    egui::IconData {
        rgba,
        width,
        height,
    }
}
