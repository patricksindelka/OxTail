//! Accessibility flows: what the custom-painted widgets tell AccessKit (tab
//! strip, log rows, scrollbars, minimap, find and filter fields, status bar),
//! and that the keyboard reaches and operates them.
mod common;
use common::*;
use egui::accesskit::{Role, Toggled};
use egui::{Key, Modifiers};
use egui_kittest::kittest::{NodeT, Queryable};

type Harness = egui_kittest::Harness<'static, oxtail_gui::OxTailApp>;

fn sample(lines: usize) -> String {
    (0..lines)
        .map(|i| format!("line {i:04} INFO something happened\n"))
        .collect()
}

fn one_tab(text: String) -> Harness {
    let mut app = new_app();
    app.open_document("app.log", doc_from(text));
    let mut h = harness(app);
    step_until(&mut h, "rows", |a| !visible_text(a).is_empty());
    h
}

fn toggled(h: &Harness, label: &str) -> Option<Toggled> {
    h.get_by_label(label).accesskit_node().toggled()
}

#[test]
fn tabs_are_named_buttons_in_a_tab_list_with_close_buttons() {
    let mut app = new_app();
    app.open_document("a.log", doc_from(sample(50)));
    app.open_document("b.log", doc_from(sample(60)));
    let mut h = harness(app);
    h.step();
    h.step();
    h.get_by_role_and_label(Role::TabList, "Open files");
    let tabs: Vec<_> = h.get_all_by_role(Role::Tab).collect();
    assert_eq!(tabs.len(), 2);
    let names: Vec<String> = tabs
        .iter()
        .map(|t| t.accesskit_node().label().unwrap_or_default())
        .collect();
    assert!(
        names[0].contains("a.log") && names[1].contains("b.log"),
        "{names:?}"
    );
    // The active tab (the last opened) is the selected one.
    assert_eq!(tabs[1].accesskit_node().is_selected(), Some(true));
    assert_eq!(tabs[0].accesskit_node().is_selected(), Some(false));
    // Each tab has a close button that says which tab it closes.
    h.get_by_label_contains("Close a.log");
    h.get_by_label_contains("Close b.log");
    // Tabs and their close buttons work through the accessibility actions.
    h.get_by_role_and_label(Role::Tab, &names[0]).click();
    h.step();
    assert_eq!(h.state().active_tab().unwrap().title, "a.log");
    h.get_by_label_contains("Close a.log").click();
    h.step();
    h.step();
    assert_eq!(h.state().tab_count(), 1);
    assert_eq!(h.state().active_tab().unwrap().title, "b.log");
    h.get_by_label("Open a file");
}

#[test]
fn visible_rows_are_a_list_with_their_text_and_line_numbers() {
    let mut h = one_tab(sample(300));
    h.get_by_role_and_label(Role::ListBox, "Log lines");
    let options: Vec<_> = h.get_all_by_role(Role::ListBoxOption).collect();
    let visible = visible_text(h.state());
    assert_eq!(options.len(), visible.len());
    for (opt, text) in options.iter().zip(&visible) {
        assert_eq!(opt.accesskit_node().label().as_deref(), Some(text.as_str()));
    }
    let first = h.state().active_view().unwrap().last_rows[0].number;
    assert_eq!(
        options[0].accesskit_node().description().as_deref(),
        Some(format!("Line {}", first + 1).as_str())
    );
    // Selection shows up as the selected state of the rows.
    h.state_mut().active_view_mut().unwrap().select_all();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        h.step();
        let all = h.get_all_by_role(Role::ListBoxOption).count() > 0
            && h.get_all_by_role(Role::ListBoxOption)
                .all(|o| o.accesskit_node().is_selected() == Some(true));
        if all {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "rows never showed as selected"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[test]
fn the_log_view_takes_the_keyboard_focus_when_clicked() {
    let mut h = one_tab(sample(100));
    let view = h.get_by_role_and_label(Role::Group, "Log view");
    view.click();
    h.step();
    h.step();
    assert!(h.get_by_label("Log view").accesskit_node().is_focused());
}

#[test]
fn scrollbars_and_the_minimap_have_roles_and_positions() {
    let mut h = one_tab(sample(600));
    // The view follows the end of the file, so the thumb ends up at the
    // bottom (the index may still be settling, so poll).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        h.step();
        let pos = h
            .get_by_role_and_label(Role::ScrollBar, "Vertical scrollbar")
            .accesskit_node()
            .numeric_value()
            .expect("a position");
        assert!((0.0..=1.0).contains(&pos), "{pos}");
        if pos > 0.9 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the thumb never reached the bottom: {pos}"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    h.get_by_role_and_label(Role::Slider, "Minimap: search matches and rule hits");
    // Scrollbars are pointer widgets: they do not add Tab stops.
    assert!(
        !h.get_by_label("Vertical scrollbar")
            .accesskit_node()
            .data()
            .supports_action(egui::accesskit::Action::Focus)
    );
}

#[test]
fn the_find_bar_is_labelled_and_operable_from_the_keyboard() {
    let mut h = one_tab(sample(100));
    h.key_press_modifiers(CTRL, Key::F);
    h.step();
    h.step();
    let field = h.get_by_role_and_label(Role::TextInput, "Find text");
    assert!(field.accesskit_node().is_focused());
    for name in [
        "Regular expression",
        "Match case: automatic",
        "Whole word",
        "Previous match",
        "Next match",
        "Filter panel",
        "Close find bar",
    ] {
        h.get_by_label(name);
    }
    // Toggles report their state.
    assert_eq!(toggled(&h, "Whole word"), Some(Toggled::False));
    assert_eq!(toggled(&h, "Regular expression"), Some(Toggled::False));
    // Tab moves the focus from the field to the first toggle, and Space
    // operates it.
    h.key_press(Key::Tab);
    h.step();
    h.step();
    assert!(
        h.get_by_label("Regular expression")
            .accesskit_node()
            .is_focused()
    );
    h.key_press(Key::Space);
    h.step();
    h.step();
    assert!(h.state().active_view().unwrap().find.regex);
    assert_eq!(toggled(&h, "Regular expression"), Some(Toggled::True));
    // The case toggle cycles and renames itself.
    h.get_by_label("Match case: automatic").click();
    h.step();
    h.step();
    assert_eq!(toggled(&h, "Match case: always"), Some(Toggled::True));
    h.get_by_label("Close find bar").click();
    h.step();
    assert!(!h.state().active_view().unwrap().find.open);
}

#[test]
fn the_filter_panel_fields_are_labelled() {
    let mut h = one_tab(sample(100));
    h.key_press_modifiers(CTRL | Modifiers::SHIFT, Key::F);
    h.step();
    h.step();
    h.get_by_role_and_label(Role::TextInput, "Filter 1 text");
    h.get_by_role_and_label(Role::CheckBox, "Filter 1 enabled");
    for name in [
        "Filter 1: column query",
        "Filter 1: regular expression",
        "Filter 1: match case: automatic",
        "Filter 1: whole word",
        "Remove filter 1",
        "Close filter panel",
    ] {
        h.get_by_label(name);
    }
    // Turning the query mode on greys out the text-only toggles.
    h.get_by_label("Filter 1: column query").click();
    h.step();
    h.step();
    assert_eq!(toggled(&h, "Filter 1: column query"), Some(Toggled::True));
    assert!(
        h.get_by_label("Filter 1: whole word")
            .accesskit_node()
            .is_disabled()
    );
    h.get_by_label("Close filter panel").click();
    h.step();
    assert!(!h.state().active_view().unwrap().filter.open);
}

#[test]
fn the_status_bar_toggles_are_named_and_stateful() {
    let mut h = one_tab(sample(100));
    assert_eq!(
        toggled(&h, "Follow the end of the file"),
        Some(Toggled::True)
    );
    h.get_by_label("Follow the end of the file").click();
    h.step();
    h.step();
    assert!(!h.state().active_view().unwrap().follow);
    assert_eq!(
        toggled(&h, "Follow the end of the file"),
        Some(Toggled::False)
    );
    assert_eq!(toggled(&h, "Wrap long lines"), Some(Toggled::False));
    h.get_by_label("Wrap long lines").click();
    h.step();
    h.step();
    assert!(h.state().active_view().unwrap().wrap);
    assert_eq!(toggled(&h, "Wrap long lines"), Some(Toggled::True));
}

#[test]
fn table_headers_are_column_headers() {
    let mut app = new_app();
    app.open_document("app.log", doc_from(logfmt_sample(100)));
    let mut h = harness(app);
    step_until(&mut h, "suggestion", |a| {
        a.active_view().is_some_and(|v| v.st.suggestion.is_some())
    });
    h.get_by_label("Accept").click();
    step_until(&mut h, "table", |a| {
        a.active_view()
            .is_some_and(|v| v.table_active() && !v.tcache.is_empty())
    });
    h.step();
    let headers: Vec<String> = h
        .get_all_by_role(Role::ColumnHeader)
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .collect();
    assert!(headers.iter().any(|l| l == "level column"), "{headers:?}");
    assert!(headers.iter().any(|l| l == "msg column"), "{headers:?}");
    h.get_by_role_and_label(Role::ListBox, "Log rows (table)");
}

#[test]
fn zoom_and_dpi_change_the_row_height_and_keep_rows_on_pixels() {
    let mut h = one_tab(sample(200));
    let row_h = |h: &Harness| h.state().active_view().unwrap().metrics.row_h;
    let before = row_h(&h);
    h.state_mut().set_font_size(24.0);
    h.step();
    h.step();
    assert!(row_h(&h) > before * 1.3, "{} vs {before}", row_h(&h));
    // A hi-dpi display: rows stay laid out, and their height is a whole
    // number of device pixels.
    h.ctx.set_pixels_per_point(2.0);
    h.step();
    h.step();
    let rh = row_h(&h);
    assert!(!visible_text(h.state()).is_empty());
    assert!(((rh * 2.0) - (rh * 2.0).round()).abs() < 1e-3, "{rh}");
}
