//! Fonts from the user's computer: the browser's Local Font Access API (`queryLocalFonts`, which
//! asks permission) and font files picked in a dialog. Everything read is handed to
//! [`gridcraft_ui_egui::fonts::add_web_fonts`], after which the font list offers it and cells
//! render with it. Failures are reported to the user through [`Notes`], never panic.
//!
//! Load System Fonts is two steps: the browser's list of installed faces is made without reading
//! any font file, its family names go to the picker (`Picker`), and only the families the person
//! ticks are read.

use std::cell::RefCell;
use std::rc::Rc;

use gridcraft_ui_egui::fonts::{MAX_WEB_FONT_FILE, add_web_fonts};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;

/// Messages for the user, shown as toasts by the shell.
pub type Notes = Rc<RefCell<Vec<String>>>;
/// The family names the Load System Fonts picker should offer, set when the browser has listed the fonts.
pub type Picker = Rc<RefCell<Option<Vec<String>>>>;
/// The fonts an opened file uses that the browser has not been asked for yet, waiting for the person's click.
pub type Offer = Rc<RefCell<Option<Vec<String>>>>;
/// The browser's list of installed faces (`FontData` objects), kept between the two steps.
pub type Listed = Rc<RefCell<Option<js_sys::Array>>>;

/// Most font files one request reads (a designer's machine has thousands of faces).
const MAX_FONT_FILES: usize = 500;
/// Most font bytes one request reads into memory (wasm memory is small).
const MAX_FONT_BYTES: usize = 192 << 20;
/// Most installed faces looked at, whether they end up added or not.
const MAX_TRIED: u32 = 20_000;

fn say(notes: &Notes, ctx: &egui::Context, text: impl Into<String>) {
    notes.borrow_mut().push(text.into());
    ctx.request_repaint();
}

fn finish(files: Vec<(String, Vec<u8>)>, missing: &[String], notes: &Notes, ctx: &egui::Context) {
    let added = add_web_fonts(files);
    let mut text =
        if added > 0 { format!("Added {added} font faces.") } else { "No new fonts: they are already available, or the files are not fonts.".into() };
    if !missing.is_empty() {
        text.push_str(&format!(" Not installed: {}.", missing.join(", ")));
    }
    say(notes, ctx, text);
}

fn string_prop(face: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(face, &JsValue::from_str(key)).ok().and_then(|v| v.as_string())
}

/// The strings a `FontData` is known by: family, full name and PostScript name.
fn face_names(face: &JsValue) -> Vec<String> {
    ["family", "fullName", "postscriptName"].iter().filter_map(|key| string_prop(face, key)).collect()
}

/// Asks the browser for the installed faces (it asks the person's permission first).
async fn query_fonts() -> Result<js_sys::Array, &'static str> {
    let window = web_sys::window().ok_or("no window")?;
    let query = match js_sys::Reflect::get(&window, &JsValue::from_str("queryLocalFonts")) {
        Ok(f) if f.is_function() => js_sys::Function::from(f),
        _ => return Err("This browser can't read installed fonts. Use Add Font Files… instead."),
    };
    let promise = query.call0(&window).map(js_sys::Promise::from).map_err(|e| {
        log::warn!("queryLocalFonts failed: {e:?}");
        "Access to the installed fonts was denied."
    })?;
    match wasm_bindgen_futures::JsFuture::from(promise).await {
        Ok(v) => Ok(js_sys::Array::from(&v)),
        Err(e) => {
            log::warn!("queryLocalFonts rejected: {e:?}");
            Err("Access to the installed fonts was denied.")
        }
    }
}

/// Step one of Load System Fonts: lists the installed faces, keeps the list and offers the family
/// names to the picker.
pub fn choose_system_fonts(ctx: egui::Context, notes: Notes, picker: Picker, listed: Listed) {
    wasm_bindgen_futures::spawn_local(async move {
        let list = match query_fonts().await {
            Ok(list) => list,
            Err(why) => return say(&notes, &ctx, why),
        };
        let mut families: Vec<String> = (0..list.length().min(MAX_TRIED)).filter_map(|i| string_prop(&list.get(i), "family")).collect();
        families.sort_by_key(|f| f.to_lowercase());
        families.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        if families.is_empty() {
            return say(&notes, &ctx, "No installed fonts were listed.");
        }
        *listed.borrow_mut() = Some(list);
        *picker.borrow_mut() = Some(families);
        ctx.request_repaint();
    });
}

/// Step two: reads the installed fonts of the `families` the person ticked.
pub fn load_chosen_fonts(ctx: egui::Context, notes: Notes, listed: Listed, families: Vec<String>) {
    let Some(list) = listed.borrow().clone() else {
        return say(&notes, &ctx, "The font list is no longer available: choose Load System Fonts again.");
    };
    wasm_bindgen_futures::spawn_local(async move { read_and_add(list, families, &notes, &ctx).await });
}

/// The family names (`names`) a file just opened uses: read them straight away when the browser
/// already lets the page read the installed fonts, otherwise leave them in `offer` for the shell to
/// offer with a button (the browser asks for permission only after a click).
pub fn autoload_used_fonts(ctx: egui::Context, notes: Notes, names: Vec<String>, offer: Offer) {
    wasm_bindgen_futures::spawn_local(async move {
        match permission().await.as_deref() {
            Some("granted") => load_used_fonts(ctx, notes, names),
            Some("prompt") => {
                *offer.borrow_mut() = Some(names);
                ctx.request_repaint();
            }
            _ => {}
        }
    });
}

/// The state of the `local-fonts` permission (`granted`, `prompt` or `denied`); `None` when the
/// browser has no Local Font Access API or cannot say.
async fn permission() -> Option<String> {
    let window = web_sys::window()?;
    js_sys::Reflect::get(&window, &JsValue::from_str("queryLocalFonts")).ok().filter(JsValue::is_function)?;
    let permissions = js_sys::Reflect::get(&window.navigator(), &JsValue::from_str("permissions")).ok().filter(|p| p.is_object())?;
    let query = js_sys::Reflect::get(&permissions, &JsValue::from_str("query")).ok().filter(JsValue::is_function)?;
    let descriptor = js_sys::Object::new();
    js_sys::Reflect::set(&descriptor, &JsValue::from_str("name"), &JsValue::from_str("local-fonts")).ok()?;
    let promise = js_sys::Function::from(query).call1(&permissions, &descriptor).ok()?;
    let status = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::from(promise)).await.ok()?;
    string_prop(&status, "state")
}

/// Reads only the installed fonts the open files use (`names`).
pub fn load_used_fonts(ctx: egui::Context, notes: Notes, names: Vec<String>) {
    wasm_bindgen_futures::spawn_local(async move {
        if names.is_empty() {
            return say(&notes, &ctx, "All the fonts used in this file are already available.");
        }
        match query_fonts().await {
            Ok(list) => read_and_add(list, names, &notes, &ctx).await,
            Err(why) => say(&notes, &ctx, why),
        }
    });
}

/// Reads the faces of `list` that go by one of `names` (family, full or PostScript name, ignoring
/// case), within the limits, and adds them; says which of the names matched nothing.
async fn read_and_add(list: js_sys::Array, names: Vec<String>, notes: &Notes, ctx: &egui::Context) {
    // (lower-case, as written): faces are matched ignoring case, and the report keeps the spelling given.
    let wanted: Vec<(String, String)> = names.into_iter().map(|n| (n.to_lowercase(), n)).collect();
    let mut found: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total = 0usize;
    for i in 0..list.length().min(MAX_TRIED) {
        if files.len() >= MAX_FONT_FILES || total >= MAX_FONT_BYTES {
            log::warn!("local fonts: stopped after {} files ({total} bytes)", files.len());
            break;
        }
        let face = list.get(i);
        let known: Vec<String> = face_names(&face).iter().map(|n| n.to_lowercase()).collect();
        let Some((hit, _)) = wanted.iter().find(|(w, _)| known.contains(w)) else { continue };
        found.insert(hit.clone());
        let Some(bytes) = face_bytes(&face, MAX_FONT_BYTES.saturating_sub(total)).await else { continue };
        total = total.saturating_add(bytes.len());
        files.push((format!("{}.ttf", string_prop(&face, "postscriptName").unwrap_or_else(|| format!("local-font-{i}"))), bytes));
    }
    let missing: Vec<String> = wanted.into_iter().filter(|(low, _)| !found.contains(low)).map(|(_, orig)| orig).collect();
    finish(files, &missing, notes, ctx);
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
        finish(files, &[], &notes, &ctx);
    });
}
