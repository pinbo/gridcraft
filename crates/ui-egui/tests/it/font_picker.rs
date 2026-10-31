//! Web: Load System Fonts lists the installed families in a picker, and OK reads only the ticked ones.

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::kittest::Queryable;
use gridcraft_engine::Session;
use gridcraft_ui_egui::SheetApp;
use serde_json::json;

type Harness = egui_kittest::Harness<'static, SheetApp>;

fn harness() -> (Harness, Rc<RefCell<Vec<Vec<String>>>>) {
    let mut session = Session::new();
    session.new_workbook();
    let mut h = crate::support::stepped_harness(session);
    let loads: Rc<RefCell<Vec<Vec<String>>>> = Rc::default();
    let sink = loads.clone();
    h.state_mut().services.load_chosen_fonts = Some(Box::new(move |families| sink.borrow_mut().push(families)));
    h.state_mut().open_dialog("fontPicker", json!({"families": ["Alpha Sans", "Beta Serif", "Gamma Mono"]}));
    h.run_steps(4);
    (h, loads)
}

#[test]
/// Ticking one family and pressing OK loads just that family and closes the picker.
fn ok_loads_only_the_ticked_families() {
    let (mut h, loads) = harness();
    h.get_by_label("Beta Serif").click();
    h.run_steps(3);
    h.get_by_label_contains("OK").click();
    h.run_steps(3);
    assert_eq!(*loads.borrow(), vec![vec!["Beta Serif".to_string()]]);
    assert!(h.state().dialog.is_none(), "the picker closes");
}

#[test]
/// Select all shown follows the filter, and OK with nothing ticked loads nothing.
fn select_all_shown_follows_the_filter_and_an_empty_choice_loads_nothing() {
    let (mut h, loads) = harness();
    h.get_by_label_contains("OK").click();
    h.run_steps(3);
    assert!(loads.borrow().is_empty());

    let (mut h, loads) = harness();
    h.get_by_label("Select all shown").click();
    h.run_steps(3);
    h.get_by_label_contains("OK").click();
    h.run_steps(3);
    assert_eq!(*loads.borrow(), vec![vec!["Alpha Sans".to_string(), "Beta Serif".to_string(), "Gamma Mono".to_string()]]);
}
