//! Headless UI flows for the column features (M4): the suggestion bar, the
//! table view, column-aware highlighting, query filters, the detail pane,
//! statistics and export.

mod common;

use std::time::{Duration, Instant};

use common::*;
use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use oxtail_gui::filter::FilterEntry;

/// Opens a logfmt document and accepts the suggested structure.
fn open_with_table(lines: usize) -> egui_kittest::Harness<'static, oxtail_gui::OxTailApp> {
    let mut app = new_app();
    app.open_document("app.log", doc_from(logfmt_sample(lines)));
    let mut h = harness(app);
    step_until(&mut h, "a suggestion", |a| {
        a.active_view().is_some_and(|v| v.st.suggestion.is_some())
    });
    h.get_by_label("Accept").click();
    step_until(&mut h, "the table", |a| {
        a.active_view()
            .is_some_and(|v| v.table_active() && !v.tcache.is_empty())
    });
    h
}

#[test]
fn accepting_the_suggestion_renders_the_table_with_a_header_and_cells() {
    let mut h = open_with_table(300);
    let v = h.state().active_view().unwrap();
    assert!(v.st.name.contains("logfmt"), "{}", v.st.name);
    assert!(v.st.suggestion.is_none());
    // Header: one entry per column of the schema, all visible.
    let schema = v.st.parser.as_ref().unwrap().schema().clone();
    assert_eq!(v.st.layout.visible_count(), schema.len());
    assert!(
        schema.find("level").is_some() && schema.find("took").is_some(),
        "{schema:?}"
    );
    // Cells: every visible record row has its parsed record and cached cells.
    let rows = v.last_rows.len();
    assert!(rows > 5);
    assert!(
        v.tcache.len() >= rows * 3,
        "{} cells for {rows} rows",
        v.tcache.len()
    );
    // The choice is remembered for the file (the tab has no path here, so
    // check the saved form).
    let saved = v.st.saved();
    assert!(saved.table && saved.spec.is_some() && saved.layout.is_some());
    // Toggling the table off shows plain text again and the cache is reset.
    h.state_mut().active_view_mut().unwrap().set_table(false);
    h.step();
    assert!(!h.state().active_view().unwrap().table_active());
    h.state_mut().active_view_mut().unwrap().set_table(true);
    step_until(&mut h, "the table again", |a| {
        a.active_view()
            .is_some_and(|v| v.table_active() && !v.tcache.is_empty())
    });
}

#[test]
fn dismissing_the_suggestion_keeps_plain_text_and_remembers_it() {
    let mut app = new_app();
    app.open_document("app.log", doc_from(logfmt_sample(100)));
    let mut h = harness(app);
    step_until(&mut h, "a suggestion", |a| {
        a.active_view().is_some_and(|v| v.st.suggestion.is_some())
    });
    // The notice bar has a Dismiss button too; the suggestion bar is below it.
    h.get_all_by_label("Dismiss").last().unwrap().click();
    h.step();
    let v = h.state().active_view().unwrap();
    assert!(v.st.suggestion.is_none() && !v.table_active());
    assert!(v.st.saved().dismissed);
    // A profile that defines columns keeps its parser (column rules, queries).
    assert!(v.st.parser.is_some());
}

#[test]
fn column_rules_from_a_profile_highlight_after_accepting() {
    let mut h = open_with_table(200);
    // The built-in logfmt profile colours `level == error` lines.
    let name = h
        .state()
        .profile_names()
        .into_iter()
        .find(|n| n.to_lowercase().contains("logfmt"))
        .expect("a logfmt profile");
    h.state_mut().set_profile(Some(name));
    // Wait for the profile's structure decision to be applied, then check the
    // settled state: before that, the table from the accepted suggestion is
    // still showing, which let this test pass by winning a race (and fail on
    // a loaded runner, CI run 17, when switching profiles turned it off).
    step_until(&mut h, "profile applied, table rows again", |a| {
        a.active_view().is_some_and(|v| {
            !v.st.is_deciding()
                && v.table_active()
                && v.last_rows.iter().any(|l| l.text.contains("level=error"))
        })
    });
    let v = h.state().active_view().unwrap();
    let error_row = v
        .last_rows
        .iter()
        .find(|l| l.text.contains("level=error"))
        .unwrap()
        .clone();
    let info_row = v
        .last_rows
        .iter()
        .find(|l| l.text.contains("level=info"))
        .unwrap()
        .clone();
    let e = v.hl.borrow_mut().prepare(&error_row);
    let i = v.hl.borrow_mut().prepare(&info_row);
    assert!(
        e.hl.line_style.is_some() || !e.hl.spans.is_empty(),
        "the error row is highlighted through its column"
    );
    assert!(i.hl.line_style.is_none());
    assert!(e.record.is_some() && i.record.is_some());
}

#[test]
fn a_column_query_filter_shows_only_matching_rows() {
    let mut h = open_with_table(300);
    h.key_press_modifiers(CTRL | Modifiers::SHIFT, Key::F);
    h.step();
    {
        let v = h.state_mut().active_view_mut().unwrap();
        v.filter.entries = vec![FilterEntry::column_query("level:error took>150ms", true)];
        v.filter.changed();
    }
    step_until(&mut h, "filtered rows", |a| {
        a.active_view()
            .is_some_and(|v| v.is_filtered() && v.filter.status.done && !v.last_rows.is_empty())
    });
    let v = h.state().active_view().unwrap();
    // Errors are every fifth line (n % 5 == 0) and took = 10 + n > 150.
    let expected = (0..300).filter(|n| n % 5 == 0 && 10 + n > 150).count();
    assert_eq!(v.filter.set.len(), expected);
    assert!(v.last_rows.iter().all(|l| l.text.contains("level=error")));
    assert!(v.filter.problems.is_empty());
}

#[test]
fn an_unknown_column_in_a_query_is_reported_inline() {
    let mut h = open_with_table(50);
    {
        let v = h.state_mut().active_view_mut().unwrap();
        v.filter.open = true;
        v.filter.entries = vec![FilterEntry::column_query("nosuch:1", true)];
        v.filter.changed();
    }
    step_until(&mut h, "a problem", |a| {
        a.active_view()
            .is_some_and(|v| !v.filter.problems.is_empty())
    });
    let v = h.state().active_view().unwrap();
    let (idx, p) = &v.filter.problems[0];
    assert_eq!(*idx, 0);
    assert!(p.message.contains("nosuch"), "{}", p.message);
    assert_eq!(p.span.clone().map(|s| &"nosuch:1"[s]), Some("nosuch"));
    // The invalid entry filters nothing.
    assert!(!v.is_filtered());
}

#[test]
fn the_detail_pane_shows_the_selected_record() {
    let mut h = open_with_table(100);
    {
        let v = h.state_mut().active_view_mut().unwrap();
        v.detail_open = true;
        let line = v.last_rows[3].clone();
        v.select(&line, false);
    }
    h.step();
    let v = h.state().active_view().unwrap();
    let (line, prepared) = v.selected_record().expect("a selected record");
    let rec = prepared.record.as_ref().expect("a record");
    let schema = v.st.parser.as_ref().unwrap().schema();
    let fields = oxtail_gui::detail::detail_fields(schema, rec);
    assert!(fields.iter().any(|f| f.name == "level"));
    let json = oxtail_gui::detail::record_json(schema, rec);
    assert!(json.contains(&format!("request {}", line.number)), "{json}");
}

#[test]
fn statistics_count_values_and_a_click_adds_a_filter() {
    let mut h = open_with_table(200);
    {
        let v = h.state_mut().active_view_mut().unwrap();
        v.stats.open = true;
        v.start_stats();
    }
    step_until(&mut h, "stats done", |a| {
        a.active_view().is_some_and(|v| v.stats.done)
    });
    let v = h.state().active_view().unwrap();
    let table = v.stats.table.as_ref().unwrap();
    assert_eq!(table.records, 200);
    let level =
        v.st.parser
            .as_ref()
            .unwrap()
            .schema()
            .find("level")
            .unwrap();
    let top = table.columns[level].top(3);
    assert_eq!(top[0].value, "INFO");
    assert_eq!(top[0].count, 160);
    // Clicking a value adds `level:"ERROR"` as an include filter; do what
    // the click does and check the filter works.
    let q = oxtail_gui::qfilter::equals_query("level", "ERROR");
    {
        let v = h.state_mut().active_view_mut().unwrap();
        v.filter
            .entries
            .push(FilterEntry::column_query(q.clone(), true));
        v.filter.changed();
    }
    step_until(&mut h, "filter by value", |a| {
        a.active_view()
            .is_some_and(|v| v.is_filtered() && v.filter.status.done && v.filter.set.len() == 40)
    });
}

#[test]
fn exporting_writes_the_filtered_rows_and_selected_columns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.csv");
    let mut h = open_with_table(100);
    {
        let v = h.state_mut().active_view_mut().unwrap();
        v.filter.entries = vec![FilterEntry::column_query("level:error", true)];
        v.filter.changed();
    }
    step_until(&mut h, "filter", |a| {
        a.active_view()
            .is_some_and(|v| v.is_filtered() && v.filter.status.done)
    });
    {
        let v = h.state_mut().active_view_mut().unwrap();
        let schema = v.st.parser.as_ref().unwrap().schema().clone();
        v.export.sync_columns(schema.len());
        v.export.selected.iter_mut().for_each(|s| *s = false);
        v.export.selected[schema.find("level").unwrap()] = true;
        v.export.selected[schema.find("took").unwrap()] = true;
        v.export.filtered_only = true;
        v.start_export(path.clone());
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        h.step();
        if h.state()
            .active_view()
            .is_some_and(|v| v.export.job.is_none() && v.export.result.is_some())
        {
            break;
        }
        assert!(Instant::now() < deadline, "export did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
    let out = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    // Header in display order (ts, level, took, msg): level, took.
    assert_eq!(lines[0], "level,took");
    assert_eq!(lines.len(), 1 + 20);
    assert!(
        lines[1..].iter().all(|l| l.starts_with("error,")),
        "{lines:?}"
    );
}
