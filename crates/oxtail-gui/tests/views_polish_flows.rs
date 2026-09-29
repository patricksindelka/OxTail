//! Flows for the view polish: the "new lines" pill after a Find jump, the
//! find bar's "Searching…" state, the suggestion bar's text, the status bar,
//! the merged view's scrollbar and the minimap ticks.
mod common;
use common::*;
use egui::Key;
use egui::accesskit::{Role, Toggled};
use egui_kittest::kittest::{NodeT, Queryable};

type Harness = egui_kittest::Harness<'static, oxtail_gui::OxTailApp>;

fn app_with(text: String) -> Harness {
    let mut app = new_app();
    app.open_document("app.log", doc_from(text));
    let mut h = harness(app);
    step_until(&mut h, "rows", |a| !visible_text(a).is_empty());
    h
}

fn type_in_find(h: &mut Harness, text: &str) {
    h.key_press_modifiers(CTRL, Key::F);
    h.step();
    h.event(egui::Event::Text(text.into()));
}

#[test]
fn a_find_jump_in_a_static_file_shows_no_new_lines_pill() {
    // Regression: pausing follow (which a Find jump does) counted every
    // existing line as new: "400 new lines" in a file that did not grow.
    let mut h = app_with(logfmt_sample(400));
    type_in_find(&mut h, "error");
    step_until(&mut h, "matches", |a| {
        a.active_view()
            .is_some_and(|v| v.find.status.done && v.find.matches.len() > 1)
    });
    // Walk back through the matches until the jump leaves the tail (which
    // pauses following).
    for _ in 0..12 {
        h.key_press_modifiers(egui::Modifiers::SHIFT, Key::F3);
        h.step();
        h.step();
        if !h.state().active_view().unwrap().follow {
            break;
        }
    }
    assert!(
        !h.state().active_view().unwrap().follow,
        "the jump should have paused following"
    );
    for _ in 0..5 {
        h.step();
    }
    let v = h.state().active_view().unwrap();
    assert!(!v.follow);
    assert_eq!(v.new_lines_while_paused(), 0);
    assert!(h.query_by_label_contains("Resume following").is_none());
}

#[test]
fn lines_appended_after_pausing_still_show_in_the_pill() {
    let mem = std::sync::Arc::new(oxtail_core::MemSource::new(logfmt_sample(100).into_bytes()));
    let doc = std::sync::Arc::new(oxtail_core::Document::from_source(mem.clone(), "grow.log"));
    let mut app = new_app();
    app.open_document("grow.log", doc);
    let mut h = harness(app);
    step_until(&mut h, "rows", |a| !visible_text(a).is_empty());
    h.get_by_label("Follow the end of the file").click();
    step_until(&mut h, "paused", |a| {
        a.active_view().is_some_and(|v| !v.follow)
    });
    assert!(h.query_by_label_contains("Resume following").is_none());
    mem.append(b"ts=2026-09-29T11:00:00Z level=info msg=\"a\"\nts=2026-09-29T11:00:01Z level=info msg=\"b\"\n");
    step_until(&mut h, "the pill", |a| {
        a.active_view()
            .is_some_and(|v| v.new_lines_while_paused() >= 2)
    });
    h.step();
    h.get_by_label_contains("Resume following, 2 new lines");
}

#[test]
fn the_find_bar_says_searching_until_the_search_has_run() {
    let mut h = app_with(logfmt_sample(400));
    type_in_find(&mut h, "error");
    // Right after typing, nothing has run yet: never "No matches". (Whether
    // "Searching..." shows yet depends on the debounce, so it is not asserted.)
    h.step();
    assert!(h.query_by_label("No matches").is_none());
    // Once settled, the count replaces it.
    step_until(&mut h, "settled", |a| {
        a.active_view()
            .is_some_and(|v| v.find.status.done && !v.find.matches.is_empty())
    });
    h.step();
    assert!(h.query_by_label("Searching\u{2026}").is_none());
    assert!(h.query_by_label("No matches").is_none());
    let counts: Vec<String> = h
        .get_all_by_label_contains(" of ")
        .filter_map(|n| {
            n.accesskit_node()
                .value()
                .map(|v| v.to_string())
                .or_else(|| n.accesskit_node().label())
        })
        .collect();
    assert!(
        counts
            .iter()
            .any(|c| c.chars().next().is_some_and(|f| f.is_ascii_digit())),
        "{counts:?}"
    );
}

#[test]
fn no_matches_is_only_said_once_the_search_is_done() {
    let mut h = app_with(logfmt_sample(400));
    type_in_find(&mut h, "this text is nowhere");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        h.step();
        if h.query_by_label("No matches").is_some() {
            let v = h.state().active_view().unwrap();
            assert!(
                v.find.status.done
                    && v.find
                        .debounce_remaining(std::time::Instant::now())
                        .is_none()
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "never said No matches"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[test]
fn the_suggestion_bar_reads_naturally() {
    let mut h = app_with(logfmt_sample(100));
    step_until(&mut h, "suggestion", |a| {
        a.active_view().is_some_and(|v| v.st.suggestion.is_some())
    });
    h.step();
    h.get_by_label("Show as columns?");
    // The format and its columns, not "Profile ... defines columns (x (x))".
    h.get_by_label_contains("logfmt: ");
    assert!(h.query_by_label_contains("defines columns").is_none());
    assert!(h.query_by_label_contains("(logfmt)").is_none());
    h.get_by_label("Accept");
    h.get_by_label_contains("Choose parser");
    assert!(h.get_all_by_label("Dismiss").count() >= 1);
}

#[test]
fn error_rows_are_marked_in_the_minimap_by_the_profile() {
    let mut h = app_with(logfmt_sample(400));
    step_until(&mut h, "ticks", |a| {
        a.active_view()
            .is_some_and(|v| !v.hl.borrow().ticks.is_empty())
    });
    h.get_by_role_and_label(Role::Slider, "Minimap: search matches and rule hits");
}

#[test]
fn the_follow_state_is_a_named_switch_in_the_status_bar() {
    let mut h = app_with(logfmt_sample(100));
    // Following: the switch is on and says so in its tooltip name.
    let n = h.get_by_label("Follow the end of the file");
    assert_eq!(n.accesskit_node().toggled(), Some(Toggled::True));
    h.key_press(Key::F);
    h.step();
    h.step();
    assert!(!h.state().active_view().unwrap().follow);
    assert_eq!(
        h.get_by_label("Follow the end of the file")
            .accesskit_node()
            .toggled(),
        Some(Toggled::False)
    );
}

fn merged_app() -> oxtail_gui::OxTailApp {
    let mut app = new_app();
    let long = "x".repeat(600);
    for name in ["a.log", "b.log"] {
        app.open_document(
            name,
            doc_from(
                (0..60)
                    .map(|i| {
                        format!(
                            "2026-09-29 10:00:{:02} INFO {name} request {i} {long}\n",
                            i % 60
                        )
                    })
                    .collect(),
            ),
        );
    }
    let ids: Vec<u64> = app.merge_candidates().iter().map(|c| c.0).collect();
    app.merge_tabs(&ids);
    app
}

fn merged(h: &Harness) -> &oxtail_gui::mergeview::MergedView {
    h.state()
        .active_tab()
        .unwrap()
        .merged()
        .expect("merged tab")
}

#[test]
fn the_merged_view_has_the_same_horizontal_scrollbar_as_the_log_view() {
    let mut h = harness(merged_app());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while h
        .query_by_role_and_label(Role::ScrollBar, "Horizontal scrollbar")
        .is_none()
    {
        h.step();
        assert!(
            std::time::Instant::now() < deadline,
            "no horizontal scrollbar"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let bar = h.get_by_role_and_label(Role::ScrollBar, "Horizontal scrollbar");
    let bounds = bar.accesskit_node().bounding_box().expect("bounds");
    assert!(
        (bounds.height() - f64::from(oxtail_gui::logview::HBAR_H)).abs() < 0.6,
        "{bounds:?}"
    );
    assert_eq!(bar.accesskit_node().numeric_value(), Some(0.0));
    // Clicking the track beside the thumb pages sideways.
    let before = merged(&h).h_scroll;
    h.get_by_role_and_label(Role::ScrollBar, "Horizontal scrollbar")
        .click();
    h.step();
    h.step();
    assert!(merged(&h).h_scroll > before, "{}", merged(&h).h_scroll);
    // The hint about cut-off lines, as in the log view, until dismissed.
    assert!(!merged(&h).cut_hint_dismissed);
    h.get_by_label("Dismiss the hint about cut-off lines")
        .click();
    h.step();
    assert!(merged(&h).cut_hint_dismissed);
    // Rows are exposed too.
    h.get_by_role_and_label(Role::ListBox, "Merged log lines");
    assert!(h.get_all_by_role(Role::ListBoxOption).count() > 5);
}
