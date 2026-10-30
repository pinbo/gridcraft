//! Fonts from the user's computer: the browser's Local Font Access API (`queryLocalFonts`, which
//! asks permission) and font files picked in a dialog. Both hand the bytes to
//! [`gridcraft_ui_egui::fonts::add_web_fonts`], after which the font list offers them and cells
//! render with them. Each failure is reported to the user through [`Notes`], never panics.

use std::cell::RefCell;
use std::rc::Rc;

use gridcraft_ui_egui::fonts::{MAX_WEB_FONT_FILE, add_web_fonts};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;

/// Messages for the user, shown as toasts by the shell.
pub type Notes = Rc<RefCell<Vec<String>>>;

/// Most font files one request reads (a designer's machine has thousands of faces).
const MAX_FONT_FILES: usize = 500;
/// Most font bytes one request reads into memory (wasm memory is small).
const MAX_FONT_BYTES: usize = 192 << 20;
/// Most local fonts looked at, whether they end up added or not.
const MAX_TRIED: u32 = 4000;

fn say(notes: &Notes, ctx: &egui::Context, text: impl Into<String>) {
    notes.borrow_mut().push(text.into());
    ctx.request_repaint();
}

fn finish(files: Vec<(String, Vec<u8>)>, notes: &Notes, ctx: &egui::Context) {
    let added = add_web_fonts(files);
    if added > 0 {
        say(notes, ctx, format!("Added {added} font faces."));
    } else {
        say(notes, ctx, "No new fonts: they are already available, or the files are not fonts.");
    }
}

/// Asks the browser for the fonts installed on the computer and adds them.
pub fn load_system_fonts(ctx: egui::Context, notes: Notes) {
    wasm_bindgen_futures::spawn_local(async move {
        let Some(window) = web_sys::window() else { return };
        let query = match js_sys::Reflect::get(&window, &JsValue::from_str("queryLocalFonts")) {
            Ok(f) if f.is_function() => js_sys::Function::from(f),
            _ => return say(&notes, &ctx, "This browser can't read installed fonts. Use Add Font Files… instead."),
        };
        let promise = match query.call0(&window) {
            Ok(p) => js_sys::Promise::from(p),
            Err(e) => {
                log::warn!("queryLocalFonts failed: {e:?}");
                return say(&notes, &ctx, "Access to the installed fonts was denied.");
            }
        };
        let list = match wasm_bindgen_futures::JsFuture::from(promise).await {
            Ok(v) => js_sys::Array::from(&v),
            Err(e) => {
                log::warn!("queryLocalFonts rejected: {e:?}");
                return say(&notes, &ctx, "Access to the installed fonts was denied.");
            }
        };
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut total = 0usize;
        for i in 0..list.length().min(MAX_TRIED) {
            if files.len() >= MAX_FONT_FILES || total >= MAX_FONT_BYTES {
                log::warn!("local fonts: stopped after {} files ({total} bytes)", files.len());
                break;
            }
            let face = list.get(i);
            let Some(bytes) = face_bytes(&face, MAX_FONT_BYTES.saturating_sub(total)).await else { continue };
            total = total.saturating_add(bytes.len());
            let name = js_sys::Reflect::get(&face, &JsValue::from_str("postscriptName")).ok().and_then(|v| v.as_string());
            files.push((format!("{}.ttf", name.unwrap_or_else(|| format!("local-font-{i}"))), bytes));
        }
        finish(files, &notes, &ctx);
    });
}

/// The bytes of one `FontData`, unless the face is too big for `room` or can't be read.
async fn face_bytes(face: &JsValue, room: usize) -> Option<Vec<u8>> {
    let blob_fn = js_sys::Reflect::get(face, &JsValue::from_str("blob")).ok().filter(JsValue::is_function)?;
    let blob_promise = js_sys::Function::from(blob_fn).call0(face).ok()?;
    let blob: web_sys::Blob = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::from(blob_promise)).await.ok()?.dyn_into().ok()?;
    // Judged by its size before it is read into memory.
    let size = blob.size().clamp(0.0, usize::MAX as f64) as usize;
    if size > MAX_WEB_FONT_FILE || size > room {
        return None;
    }
    let buffer = wasm_bindgen_futures::JsFuture::from(blob.array_buffer()).await.ok()?;
    Some(js_sys::Uint8Array::new(&buffer).to_vec())
}

/// Opens a file dialog for font files and adds the ones picked.
pub fn add_font_files(ctx: egui::Context, notes: Notes) {
    wasm_bindgen_futures::spawn_local(async move {
        let Some(picked) = rfd::AsyncFileDialog::new().add_filter("Fonts", &["ttf", "otf", "ttc", "otc"]).pick_files().await else { return };
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut total = 0usize;
        for file in picked.into_iter().take(MAX_FONT_FILES) {
            let bytes = file.read().await;
            if total.saturating_add(bytes.len()) > MAX_FONT_BYTES {
                break;
            }
            total += bytes.len();
            files.push((file.file_name(), bytes));
        }
        finish(files, &notes, &ctx);
    });
}
