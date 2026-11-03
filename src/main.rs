#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod docx;
mod legacy_doc;

fn main() -> eframe::Result {
    app::run()
}
