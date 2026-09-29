//! Headless UI flows for several documents (M5): the merged tab, split
//! panes and the search across tabs.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use egui::{Key, Modifiers};
use oxtail_core::{Document, MemSource};
use oxtail_gui::{OpenRequest, OxTailApp};

fn stamped(prefix: &str, first_sec: u32, step: u32, n: u32) -> String {
    (0..n)
        .map(|i| {
            let s = first_sec + i * step;
            format!("2026-09-29 10:{:02}:{:02} {prefix}{i}\n", s / 60, s % 60)
        })
        .collect()
}

fn mem_doc(text: &str) -> (Arc<MemSource>, Arc<Document>) {
    let mem = Arc::new(MemSource::new(text.as_bytes().to_vec()));
    let doc = Arc::new(Document::from_source(mem.clone(), "m.log"));
    (mem, doc)
}

fn merged(app: &OxTailApp) -> &oxtail_gui::mergeview::MergedView {
    app.tab_list()
        .iter()
        .find_map(|t| t.merged())
        .expect("a merged tab")
}

#[test]
fn a_merged_tab_shows_interleaved_lines_with_source_badges() {
    let mut app = new_app();
    // a: :00 :02 :04 ...; b: :01 :03 :05 ...
    let (_ma, a) = mem_doc(&stamped("a", 0, 2, 30));
    let (_mb, b) = mem_doc(&stamped("b", 1, 2, 30));
    app.open_document("a.log", a);
    app.open_document("b.log", b);
    let ids: Vec<u64> = app.merge_candidates().iter().map(|c| c.0).collect();
    let id = app.merge_tabs(&ids).expect("merged tab");
    let mut h = harness(app);
    // Wait for the view to reach the tail too: the rows can be laid out while
    // the merge is still growing (seen with the suite pinned to one core).
    step_until(&mut h, "merged rows at the tail", |a| {
        let m = merged(a);
        m.len() == 60
            && m.last_rows.len() > 8
            && m.last_rows.iter().all(|(_, l)| l.is_some())
            && m.last_rows.last().is_some_and(|(i, _)| *i == 59)
    });
    let app = h.state();
    let m = merged(app);
    assert_eq!(app.active_tab().unwrap().id, id);
    // Following: the tail of the interleaved order, alternating a and b.
    let rows: Vec<_> = m
        .last_rows
        .iter()
        .map(|(i, l)| (*i, l.clone().unwrap()))
        .collect();
    assert_eq!(rows.last().unwrap().0, 59);
    for (i, l) in &rows {
        assert_eq!(l.source, i % 2, "row {i} comes from source {}", i % 2);
        let prefix = if l.source == 0 { 'a' } else { 'b' };
        assert!(
            l.line.text.contains(&format!(" {prefix}{}", i / 2)),
            "{} at {i}",
            l.line.text
        );
    }
    // Sources are named for the badge hover; 8 bytes per merged line.
    assert_eq!(m.sources[0].name, "a.log");
    assert_eq!(m.store.bytes(), 60 * 8);
    // The tab label says merged.
    assert_eq!(app.active_tab().unwrap().label(), "a.log + b.log (merged)");
}

#[test]
fn a_merged_tab_follows_growth_of_its_sources() {
    let mut app = new_app();
    let (ma, a) = mem_doc(&stamped("a", 0, 2, 5));
    let (mb, b) = mem_doc(&stamped("b", 1, 2, 5));
    app.open_document("a.log", a);
    app.open_document("b.log", b);
    let ids: Vec<u64> = app.merge_candidates().iter().map(|c| c.0).collect();
    app.merge_tabs(&ids);
    let mut h = harness(app);
    step_until(&mut h, "initial merge", |a| merged(a).len() == 10);
    // New lines arrive later than everything already merged.
    ma.append(b"2026-09-29 10:01:00 a-late\n");
    mb.append(b"2026-09-29 10:01:01 b-late\n");
    step_until(&mut h, "new lines merged in", |a| merged(a).len() == 12);
    // Each source notices its growth on its own, and a source that has been
    // quiet longer than the idle delay no longer holds the others back, so the
    // two late lines can be merged in either order (seen on macos-latest).
    // What matters: the view follows to the tail and shows both of them.
    let late = |a: &OxTailApp, suffix: &str| {
        merged(a)
            .last_rows
            .iter()
            .any(|(_, l)| l.as_ref().is_some_and(|l| l.line.text.ends_with(suffix)))
    };
    step_until(&mut h, "tail visible", |a| {
        let m = merged(a);
        m.last_rows
            .last()
            .is_some_and(|(i, l)| *i == 11 && l.is_some())
            && late(a, "a-late")
            && late(a, "b-late")
    });
    let m = merged(h.state());
    assert!(m.follow);
    let last = m.last_rows.last().unwrap().1.as_ref().unwrap();
    assert!(last.line.text.ends_with("-late"), "{}", last.line.text);
}

#[test]
fn find_in_a_merged_tab_jumps_across_sources() {
    let mut app = new_app();
    let (_ma, a) = mem_doc(&stamped("a", 0, 2, 40));
    let (_mb, b) = mem_doc(&stamped("b", 1, 2, 40));
    app.open_document("a.log", a);
    app.open_document("b.log", b);
    let ids: Vec<u64> = app.merge_candidates().iter().map(|c| c.0).collect();
    app.merge_tabs(&ids);
    let mut h = harness(app);
    step_until(&mut h, "merged", |a| merged(a).len() == 80);
    h.key_press_modifiers(CTRL, Key::F);
    h.step();
    h.event(egui::Event::Text("b7".into()));
    step_until(&mut h, "search", |a| {
        merged(a)
            .find
            .search
            .as_ref()
            .is_some_and(|s| s.scanned() >= 80 && s.count() >= 1)
    });
    h.key_press(Key::Enter);
    // b7 is merged row 15 (a0 b0 a1 b1 ... b7 at index 2*7+1).
    step_until(&mut h, "match on screen", |a| {
        let m = merged(a);
        m.find.current == Some(15)
            && m.last_rows
                .iter()
                .any(|(i, l)| *i == 15 && l.as_ref().is_some_and(|l| l.line.text.ends_with("b7")))
    });
    assert!(!merged(h.state()).follow);
}

fn write_log(dir: &std::path::Path, name: &str, lines: usize) -> std::path::PathBuf {
    let p = dir.join(name);
    let text: String = (0..lines)
        .map(|i| {
            if i % 20 == 5 {
                format!("2026-09-29 10:00:{:02} ERROR needle {name} {i}\n", i % 60)
            } else {
                format!("2026-09-29 10:00:{:02} INFO {name} line {i}\n", i % 60)
            }
        })
        .collect();
    std::fs::write(&p, text).unwrap();
    p
}

fn wait_ready(h: &mut egui_kittest::Harness<'static, OxTailApp>, n: usize) {
    step_until(h, "tabs ready", |a| {
        a.tab_list().len() == n && a.tab_list().iter().all(|t| t.view().is_some())
    });
}

#[test]
fn splitting_shows_two_panes_with_their_own_tabs() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_log(dir.path(), "app.log", 300);
    let mut app = new_app();
    app.open_request(OpenRequest::file(&p));
    let mut h = harness(app);
    wait_ready(&mut h, 1);
    h.state_mut()
        .split_active(oxtail_config::SplitDirection::Horizontal);
    wait_ready(&mut h, 2);
    step_until(&mut h, "both panes draw rows", |a| {
        a.pane_count() == 2
            && a.pane_rects().len() == 2
            && a.tab_list()
                .iter()
                .all(|t| t.view().is_some_and(|v| !v.last_rows.is_empty()))
    });
    let a = h.state();
    let rects = a.pane_rects();
    // Side by side, sharing the height.
    assert!(rects[0].1.right() < rects[1].1.left());
    assert_eq!(a.tabs_in_pane(rects[0].0).len(), 1);
    assert_eq!(a.tabs_in_pane(rects[1].0).len(), 1);
    // The copy is a separate view of the same file: filter one of them.
    let second = a.tab_list()[1].id;
    let i = a.tab_list().iter().position(|t| t.id == second).unwrap();
    h.state_mut().select_tab(i);
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .filter
        .set_from_query_strings(&["needle".to_string()]);
    step_until(&mut h, "one pane filtered, the other full", |a| {
        let (v0, v1) = (
            a.tab_list()[0].view().unwrap(),
            a.tab_list()[1].view().unwrap(),
        );
        v1.is_filtered() && v1.filter.status.done && !v0.is_filtered()
    });
    // Closing the second tab removes its pane.
    h.state_mut().close_tab(1);
    h.step();
    assert_eq!(h.state().pane_count(), 1);
}

#[test]
fn synced_cursors_move_the_other_pane() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_log(dir.path(), "app.log", 500);
    let mut app = new_app();
    app.open_request(OpenRequest::file(&p));
    let mut h = harness(app);
    wait_ready(&mut h, 1);
    h.state_mut()
        .split_active(oxtail_config::SplitDirection::Vertical);
    wait_ready(&mut h, 2);
    step_until(&mut h, "rows", |a| {
        a.tab_list().iter().all(|t| {
            t.view()
                .is_some_and(|v| v.snapshot.lines.exact && !v.last_rows.is_empty())
        })
    });
    h.state_mut().set_sync_cursor(true);
    h.step();
    // Select a line in the focused (second) tab's view.
    h.state_mut().active_view_mut().unwrap().jump_top();
    step_until(&mut h, "top rows", |a| {
        a.active_view()
            .is_some_and(|v| v.last_rows.first().is_some_and(|l| l.number == 0))
    });
    let target = {
        let v = h.state_mut().active_view_mut().unwrap();
        let line = v.last_rows[3].clone();
        v.select(&line, false);
        line
    };
    step_until(&mut h, "the other pane follows", |a| {
        let other = a
            .tab_list()
            .iter()
            .find(|t| t.id != a.active_tab().unwrap().id)
            .unwrap();
        other
            .view()
            .is_some_and(|v| v.user_cursor() == Some(target.offset))
    });
    // Sync off: the other pane stays where it is.
    h.state_mut().set_sync_cursor(false);
    assert!(!h.state().sync_cursor());
}

#[test]
fn searching_all_tabs_groups_results_and_jumps() {
    let dir = tempfile::tempdir().unwrap();
    let p1 = write_log(dir.path(), "one.log", 200);
    let p2 = write_log(dir.path(), "two.log", 100);
    let mut app = new_app();
    app.open_request(OpenRequest::file(&p1));
    app.open_request(OpenRequest::file(&p2));
    let mut h = harness(app);
    wait_ready(&mut h, 2);
    step_until(&mut h, "indexed", |a| {
        a.tab_list()
            .iter()
            .all(|t| t.view().is_some_and(|v| v.snapshot.lines.exact))
    });
    h.state_mut().start_cross_search("needle");
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.state().cross_running() {
        h.step();
        assert!(Instant::now() < deadline, "search did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
    h.step();
    let results = h.state().cross_results().to_vec();
    assert_eq!(results.len(), 2);
    let one = results.iter().find(|r| r.title == "one.log").unwrap();
    let two = results.iter().find(|r| r.title == "two.log").unwrap();
    // Every 20th line from line 5: 10 in 200 lines, 5 in 100.
    assert_eq!((one.total, two.total), (10, 5));
    assert_eq!(one.hits[0].line, 5);
    assert!(one.hits[0].text.contains("needle one.log 5"));
    // Clicking a hit selects the tab and scrolls to the line.
    let (tab_id, offset, line) = (two.tab_id, two.hits[3].offset, two.hits[3].line);
    h.state_mut().jump_to_hit(tab_id, offset);
    step_until(&mut h, "the hit is on screen", |a| {
        a.active_tab().is_some_and(|t| t.id == tab_id)
            && a.active_view()
                .is_some_and(|v| v.last_rows.iter().any(|l| l.number == line))
    });
}

#[test]
fn ctrl_alt_f_opens_the_search_window() {
    let mut app = new_app();
    app.open_document("a.log", doc_from("hello\n".into()));
    let mut h = harness(app);
    step_until(&mut h, "rows", |a| !visible_text(a).is_empty());
    h.key_press_modifiers(Modifiers { alt: true, ..CTRL }, Key::F);
    h.step();
    assert!(h.state().cross_window_open());
}

#[test]
fn opening_files_as_a_merge_request_makes_one_merged_tab() {
    let dir = tempfile::tempdir().unwrap();
    let p1 = write_log(dir.path(), "w1.log", 40);
    let p2 = write_log(dir.path(), "w2.log", 40);
    let mut app = new_app();
    app.open_request(OpenRequest {
        files: vec![p1, p2],
        merge: true,
        ..OpenRequest::default()
    });
    let mut h = harness(app);
    step_until(&mut h, "merged tab with rows", |a| {
        a.tab_list()
            .iter()
            .find_map(|t| t.merged())
            .is_some_and(|m| m.len() == 80)
    });
    // One tab, not two; nothing restorable as a plain tab.
    assert_eq!(h.state().tab_count(), 1);
    assert_eq!(
        h.state().active_tab().unwrap().label(),
        "w1.log + w2.log (merged)"
    );
    assert!(h.state().current_session().tabs.is_empty());
    let defs = h.state().merged_defs();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].paths.len(), 2);
}
