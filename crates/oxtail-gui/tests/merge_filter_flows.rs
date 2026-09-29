//! Headless UI flows for the filter view of a merged tab (Ctrl+Shift+F).

mod common;

use std::sync::Arc;

use common::*;
use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use oxtail_core::{Document, MemSource};
use oxtail_gui::OxTailApp;
use oxtail_gui::mergeview::MergedView;

/// `n` lines a second apart starting at `first_sec`, `step` seconds apart;
/// every third line says ERR.
fn stamped(prefix: &str, first_sec: u32, step: u32, n: u32) -> String {
    (0..n)
        .map(|i| {
            let s = first_sec + i * step;
            let tag = if i % 3 == 0 { "ERR" } else { "ok" };
            format!(
                "2026-09-29 10:{:02}:{:02} {tag} {prefix}{i}\n",
                s / 60,
                s % 60
            )
        })
        .collect()
}

fn mem_doc(text: &str) -> (Arc<MemSource>, Arc<Document>) {
    let mem = Arc::new(MemSource::new(text.as_bytes().to_vec()));
    let doc = Arc::new(Document::from_source(mem.clone(), "m.log"));
    (mem, doc)
}

fn merged(app: &OxTailApp) -> &MergedView {
    app.tab_list()
        .iter()
        .find_map(|t| t.merged())
        .expect("a merged tab")
}

/// Every row drawn is loaded.
fn rows_loaded(m: &MergedView) -> bool {
    !m.last_rows.is_empty() && m.last_rows.iter().all(|(_, l)| l.is_some())
}

/// Two merged sources of 30 lines: a_i is merged row 2i, b_i is 2i+1.
fn open_merged() -> (
    egui_kittest::Harness<'static, OxTailApp>,
    Arc<MemSource>,
    Arc<MemSource>,
) {
    let mut app = new_app();
    let (ma, a) = mem_doc(&stamped("a", 0, 2, 30));
    let (mb, b) = mem_doc(&stamped("b", 1, 2, 30));
    app.open_document("a.log", a);
    app.open_document("b.log", b);
    let ids: Vec<u64> = app.merge_candidates().iter().map(|c| c.0).collect();
    app.merge_tabs(&ids).expect("merged tab");
    let mut h = harness(app);
    step_until(&mut h, "merged", |a| merged(a).len() == 60);
    (h, ma, mb)
}

fn type_filter(h: &mut egui_kittest::Harness<'static, OxTailApp>, text: &str) {
    h.key_press_modifiers(CTRL | Modifiers::SHIFT, Key::F);
    h.step();
    assert!(merged(h.state()).filter.open);
    let field = h.get_by_role(egui::accesskit::Role::TextInput);
    field.click();
    h.step();
    h.event(egui::Event::Text(text.into()));
}

#[test]
fn the_filter_shows_matching_lines_of_both_sources_with_badges() {
    let (mut h, _ma, _mb) = open_merged();
    type_filter(&mut h, "ERR");
    // 10 ERR lines per source: 20 rows, the last is b27 (merged row 55).
    step_until(&mut h, "filtered rows at the tail", |a| {
        let m = merged(a);
        m.filter_progress() == Some((20, true))
            && rows_loaded(m)
            && m.last_rows.last().is_some_and(|(i, _)| *i == 55)
    });
    let m = merged(h.state());
    assert_eq!(m.row_count(), 20);
    assert!(m.follow);
    let mut seen = [false; 2];
    for (i, l) in &m.last_rows {
        let l = l.as_ref().unwrap();
        assert_eq!(l.source, i % 2, "badge of merged row {i}");
        let prefix = if l.source == 0 { 'a' } else { 'b' };
        assert!(
            l.line.text.contains(&format!("ERR {prefix}{}", i / 2)),
            "{} at {i}",
            l.line.text
        );
        seen[l.source] = true;
    }
    assert!(seen[0] && seen[1], "both sources show up");
    // Ascending merged rows, none of the hidden ones.
    assert!(m.last_rows.windows(2).all(|w| w[1].0 > w[0].0));
    assert!(m.last_rows.iter().all(|(i, _)| m.is_shown(*i)));
}

#[test]
fn matching_lines_appended_while_following_reach_the_tail() {
    let (mut h, ma, mb) = open_merged();
    type_filter(&mut h, "ERR");
    step_until(&mut h, "filtered", |a| {
        merged(a).filter_progress() == Some((20, true)) && rows_loaded(merged(a))
    });
    // Later than everything merged so far. The lines of different sources can
    // be merged in either order (a quiet source stops holding the merge back),
    // and the quiet line does not pass the filter wherever it lands.
    ma.append(b"2026-09-29 10:01:00 ERR a-late\n");
    mb.append(b"2026-09-29 10:01:01 ERR b-late\n");
    ma.append(b"2026-09-29 10:01:02 ok a-quiet\n");
    step_until(&mut h, "late lines filtered in and followed", |a| {
        let m = merged(a);
        let shows = |suffix: &str| {
            m.last_rows
                .iter()
                .any(|(_, l)| l.as_ref().is_some_and(|l| l.line.text.ends_with(suffix)))
        };
        m.len() == 63
            && m.filter_progress() == Some((22, true))
            && rows_loaded(m)
            && m.last_rows
                .last()
                .is_some_and(|(_, l)| l.as_ref().is_some_and(|l| l.line.text.ends_with("-late")))
            && shows("a-late")
            && shows("b-late")
    });
    let m = merged(h.state());
    assert!(m.follow);
    assert!(
        m.last_rows
            .iter()
            .all(|(_, l)| !l.as_ref().unwrap().line.text.contains("a-quiet"))
    );
}

#[test]
fn switching_the_filter_off_returns_to_the_full_view() {
    let (mut h, _ma, _mb) = open_merged();
    type_filter(&mut h, "ERR");
    step_until(&mut h, "filtered", |a| {
        merged(a).filter_progress() == Some((20, true)) && rows_loaded(merged(a))
    });
    h.get_by_label("Show only matching lines").click();
    step_until(&mut h, "full view at the tail", |a| {
        let m = merged(a);
        !m.filter_view_active()
            && m.row_count() == 60
            && rows_loaded(m)
            && m.last_rows.last().is_some_and(|(i, _)| *i == 59)
            && m.last_rows
                .iter()
                .any(|(_, l)| l.as_ref().is_some_and(|l| l.line.text.contains(" ok ")))
    });
}
