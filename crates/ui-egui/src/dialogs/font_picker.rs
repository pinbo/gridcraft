use super::show::ok_cancel;
use super::{Dialog, Json, json};
use crate::SheetApp;

/// Web: Load System Fonts. The browser listed the installed font families without reading any file
/// (`families`); the person ticks the ones to load and OK reads just those. The ticked families
/// are kept in `selected`, the filter text in `filter`.
pub(super) fn font_picker(app: &mut SheetApp, ui: &mut egui::Ui, d: &mut Dialog, confirm: &mut bool, open: &mut bool) {
    let lang = crate::i18n::current(ui.ctx());
    let families: Vec<String> = d
        .values
        .get("families")
        .and_then(Json::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let mut selected: std::collections::HashSet<String> = d
        .values
        .get("selected")
        .and_then(Json::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let mut filter = d.values.get("filter").and_then(Json::as_str).unwrap_or_default().to_string();
    ui.label(lang.tr("Tick the fonts to load from this computer."));
    let r = ui.add(egui::TextEdit::singleline(&mut filter).hint_text(lang.tr("Type to filter…")).desired_width(f32::INFINITY));
    if !d.focus_initialized {
        r.request_focus();
        d.focus_initialized = true;
    }
    let needle = filter.to_lowercase();
    let shown: Vec<&String> = families.iter().filter(|f| needle.is_empty() || f.to_lowercase().contains(&needle)).collect();
    ui.horizontal(|ui| {
        if ui.button(lang.tr("Select all shown")).clicked() {
            selected.extend(shown.iter().map(|f| (*f).clone()));
        }
        if ui.button(lang.tr("Select none")).clicked() {
            selected.clear();
        }
        ui.label(format!("{} / {}", selected.len(), families.len()));
    });
    let row = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y + 2.0;
    egui::ScrollArea::vertical().max_height(300.0).auto_shrink([false, true]).show_rows(ui, row, shown.len(), |ui, range| {
        for family in shown.iter().skip(range.start).take(range.len()) {
            let mut on = selected.contains(*family);
            if ui.checkbox(&mut on, family.as_str()).changed() {
                if on {
                    selected.insert((*family).clone());
                } else {
                    selected.remove(*family);
                }
            }
        }
    });
    let mut chosen: Vec<String> = selected.into_iter().collect();
    chosen.sort_by_key(|f| f.to_lowercase());
    d.values.insert("selected".into(), json!(chosen));
    d.values.insert("filter".into(), json!(filter));
    ok_cancel(ui, confirm, open);
    if *confirm {
        *confirm = false;
        if !chosen.is_empty()
            && let Some(load) = &app.services.load_chosen_fonts
        {
            load(chosen);
        }
        *open = false;
    }
}
