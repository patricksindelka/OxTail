//! Multi-line records in the table view: continuation lines (stack traces)
//! attach to their record and span all columns.

mod common;

use common::*;
use egui_kittest::kittest::Queryable;

#[test]
fn continuation_lines_attach_to_their_record_and_do_not_break_the_table() {
    let mut text = String::new();
    for i in 0..60 {
        let level = if i % 5 == 0 { "ERROR" } else { "INFO" };
        text.push_str(&format!(
            "2026-09-29 10:00:{:02},{:03} {level} [main] request {i} finished\n",
            i % 60,
            i
        ));
        if i % 5 == 0 {
            text.push_str("    at com.example.Foo.bar(Foo.java:42)\n");
            text.push_str("    at com.example.Main.run(Main.java:7)\n");
        }
    }
    let mut app = new_app();
    app.open_document("java.log", doc_from(text));
    let mut h = harness(app);
    step_until(&mut h, "a suggestion", |a| {
        a.active_view().is_some_and(|v| v.st.suggestion.is_some())
    });
    h.get_by_label("Accept").click();
    step_until(&mut h, "the table with rows", |a| {
        a.active_view()
            .is_some_and(|v| v.table_active() && v.last_rows.len() > 8 && !v.tcache.is_empty())
    });
    let v = h.state().active_view().unwrap();
    let rows = v.last_rows.clone();
    let mut records = 0;
    let mut continuations = 0;
    for l in &rows {
        let p = v.hl.borrow_mut().prepare(l);
        if p.record.is_some() {
            records += 1;
            assert!(!l.text.starts_with(' '), "{}", l.text);
        } else {
            continuations += 1;
            assert!(l.text.trim_start().starts_with("at "), "{}", l.text);
        }
    }
    assert!(
        records > 0 && continuations > 0,
        "{records} records / {continuations} continuation lines"
    );
}
