//! Headless UI flows for sorting a filtered view by a column (PLAN.md 8.3).
//!
//! Every wait checks the condition that is asserted afterwards: the rows on
//! screen are the final order, not merely "some rows exist".

mod common;

use std::sync::Arc;

use common::*;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use oxtail_core::{Document, MemSource};
use oxtail_gui::OxTailApp;
use oxtail_gui::filter::FilterEntry;
use oxtail_gui::sort::{SortOffer, SortSpec};

/// Every third line is an error; `took` is a permutation of 0..45.
fn sample() -> String {
    (0..45)
        .map(|i| {
            let level = if i % 3 == 0 { "error" } else { "info" };
            format!(
                "ts=2026-09-29T10:00:{i:02}Z level={level} took={}ms msg=\"request {i}\"\n",
                (i * 7) % 45
            )
        })
        .collect()
}

fn took(text: &str) -> Option<u32> {
    let rest = text.split("took=").nth(1)?;
    rest.split("ms").next()?.parse().ok()
}

fn tooks(app: &OxTailApp) -> Vec<u32> {
    visible_text(app).iter().filter_map(|t| took(t)).collect()
}

/// `took` of the error lines in file order.
fn error_tooks() -> Vec<u32> {
    (0..45)
        .filter(|i| i % 3 == 0)
        .map(|i| (i * 7) % 45)
        .collect()
}

/// Opens `text` as logfmt with the table showing.
fn open_table(text: String) -> (Harness<'static, OxTailApp>, Arc<MemSource>) {
    let mem = Arc::new(MemSource::new(text.into_bytes()));
    let doc = Arc::new(Document::from_source(mem.clone(), "app.log"));
    let mut app = new_app();
    app.open_document("app.log", doc);
    let mut h = harness(app);
    step_until(&mut h, "a suggestion", |a| {
        a.active_view().is_some_and(|v| v.st.suggestion.is_some())
    });
    h.get_by_label("Accept").click();
    step_until(&mut h, "the table", |a| {
        a.active_view()
            .is_some_and(|v| v.table_active() && !v.tcache.is_empty())
    });
    (h, mem)
}

/// Filters to the error lines and waits for all of them to be on screen.
fn filter_errors(h: &mut Harness<'static, OxTailApp>) {
    {
        let v = h.state_mut().active_view_mut().unwrap();
        v.filter.entries = vec![FilterEntry::column_query("level:error", true)];
        v.filter.changed();
    }
    let want = error_tooks();
    step_until(h, "the filtered rows in file order", |a| tooks(a) == want);
}

fn took_col(app: &OxTailApp) -> usize {
    app.active_view()
        .and_then(|v| v.st.parser.as_ref())
        .and_then(|p| p.schema().find("took"))
        .expect("a took column")
}

/// Clicks the header cell of `col`.
fn click_header(h: &mut Harness<'static, OxTailApp>, col: usize) {
    let rect = h
        .state()
        .active_view()
        .unwrap()
        .header_cells
        .iter()
        .find(|(c, _)| *c == col)
        .map(|(_, r)| *r)
        .expect("the header cell is drawn");
    // Left of the resize grab zone, right of the cell's left edge.
    let pos = egui::pos2(rect.left() + 12.0, rect.center().y);
    h.hover_at(pos);
    h.step();
    h.drag_at(pos);
    h.step();
    h.drop_at(pos);
    h.step();
}

fn sorted_expected(desc: bool) -> Vec<u32> {
    let mut v = error_tooks();
    v.sort_unstable();
    if desc {
        v.reverse();
    }
    v
}

#[test]
fn sorting_a_filtered_view_by_a_number_column_and_back() {
    let (mut h, _mem) = open_table(sample());
    // Sorting is not offered before there is a filtered view.
    {
        let v = h.state().active_view().unwrap();
        assert!(matches!(*v.sort_offer(), SortOffer::Unavailable(_)));
    }
    filter_errors(&mut h);
    assert_eq!(
        *h.state().active_view().unwrap().sort_offer(),
        SortOffer::Available
    );
    let col = took_col(h.state());

    // Ascending.
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .set_sort(Some(SortSpec {
            col,
            descending: false,
        }));
    let asc = sorted_expected(false);
    step_until(&mut h, "the ascending order on screen", |a| tooks(a) == asc);
    {
        let v = h.state().active_view().unwrap();
        assert!(v.sorted_order().is_some());
        assert_eq!(v.sort.spec.map(|s| s.descending), Some(false));
    }

    // Descending.
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .set_sort(Some(SortSpec {
            col,
            descending: true,
        }));
    let desc = sorted_expected(true);
    step_until(&mut h, "the descending order on screen", |a| {
        tooks(a) == desc
    });

    // Clear: back to file order.
    h.state_mut().active_view_mut().unwrap().set_sort(None);
    let file = error_tooks();
    step_until(&mut h, "file order again", |a| {
        tooks(a) == file && a.active_view().is_some_and(|v| v.sorted_order().is_none())
    });
}

#[test]
fn clicking_the_header_cycles_ascending_descending_off() {
    let (mut h, _mem) = open_table(sample());
    filter_errors(&mut h);
    let col = took_col(h.state());

    click_header(&mut h, col);
    let asc = sorted_expected(false);
    step_until(&mut h, "ascending after the first click", |a| {
        tooks(a) == asc
    });
    assert_eq!(
        h.state().active_view().unwrap().sort.spec,
        Some(SortSpec {
            col,
            descending: false
        })
    );

    click_header(&mut h, col);
    let desc = sorted_expected(true);
    step_until(&mut h, "descending after the second click", |a| {
        tooks(a) == desc
    });

    click_header(&mut h, col);
    let file = error_tooks();
    step_until(&mut h, "off after the third click", |a| {
        tooks(a) == file && a.active_view().is_some_and(|v| v.sort.spec.is_none())
    });
}

#[test]
fn a_view_above_the_limit_says_why_sorting_is_unavailable() {
    let (mut h, _mem) = open_table(sample());
    filter_errors(&mut h);
    let v = h.state_mut().active_view_mut().unwrap();
    // The limit comes from the settings; a smaller one than the 15 rows.
    v.sort.max_rows = 5;
    match &*v.sort_offer() {
        SortOffer::Unavailable(why) => {
            assert!(why.contains("up to 5 rows"), "{why}");
            assert!(why.contains("Go to time"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    v.sort.max_rows = 1_000_000;
    assert_eq!(*v.sort_offer(), SortOffer::Available);
}

#[test]
fn turning_the_filter_view_off_clears_the_sort() {
    let (mut h, _mem) = open_table(sample());
    filter_errors(&mut h);
    let col = took_col(h.state());
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .set_sort(Some(SortSpec {
            col,
            descending: false,
        }));
    let asc = sorted_expected(false);
    step_until(&mut h, "the ascending order", |a| tooks(a) == asc);
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .set_filter_view(false);
    step_until(&mut h, "the sort is cleared", |a| {
        a.active_view().is_some_and(|v| {
            v.sort.spec.is_none() && v.sorted_order().is_none() && !v.is_filtered()
        })
    });
}

#[test]
fn continuation_lines_travel_with_their_record() {
    // Three error records with stack lines; `took` decides the order.
    let mut text = String::new();
    for (i, t) in [30, 10, 20].iter().enumerate() {
        text.push_str(&format!(
            "ts=2026-09-29T10:00:0{i}Z level=error took={t}ms msg=\"boom {i}\"\n"
        ));
        text.push_str(&format!("    at com.example.Frame{i}.a(F.java:1)\n"));
        text.push_str(&format!("    at com.example.Frame{i}.b(F.java:2)\n"));
    }
    // More info records so the table is detected as logfmt.
    for i in 3..30 {
        text.push_str(&format!(
            "ts=2026-09-29T10:00:{i:02}Z level=info took=1ms msg=\"ok {i}\"\n"
        ));
    }
    let (mut h, _mem) = open_table(text);
    {
        let v = h.state_mut().active_view_mut().unwrap();
        // Plain-text filter with two lines of context: the stack lines come
        // along as context of their error line.
        v.filter.entries = vec![FilterEntry::include("level=error")];
        v.filter.after = 2;
        v.filter.changed();
    }
    step_until(&mut h, "the filtered rows", |a| {
        visible_text(a).len() == 9 && a.active_view().is_some_and(|v| v.filter.status.done)
    });
    let col = took_col(h.state());
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .set_sort(Some(SortSpec {
            col,
            descending: false,
        }));
    let want: Vec<String> = [1usize, 2, 0]
        .iter()
        .flat_map(|i| {
            [
                format!("boom {i}"),
                format!("Frame{i}.a"),
                format!("Frame{i}.b"),
            ]
        })
        .collect();
    step_until(&mut h, "records sorted with their stack lines", |a| {
        let rows = visible_text(a);
        rows.len() == 9
            && rows
                .iter()
                .zip(&want)
                .all(|(row, marker)| row.contains(marker.as_str()))
    });
}

#[test]
fn the_view_is_sorted_again_when_the_filter_gains_matches() {
    let (mut h, mem) = open_table(sample());
    filter_errors(&mut h);
    let col = took_col(h.state());
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .set_sort(Some(SortSpec {
            col,
            descending: false,
        }));
    let asc = sorted_expected(false);
    step_until(&mut h, "the first order", |a| tooks(a) == asc);
    // A new error whose `took` ties the minimum (0): the sort is stable, so
    // it lands right after the existing 0 and before everything else.
    mem.append(b"ts=2026-09-29T11:00:00Z level=error took=0ms msg=\"late\"\n");
    let mut want = asc.clone();
    want.insert(1, 0); // after the existing 0 (stable), before the rest
    step_until(&mut h, "the re-sorted order with the new line", |a| {
        tooks(a) == want
    });
    let v = h.state().active_view().unwrap();
    assert_eq!(v.sorted_order().unwrap().len(), 16);
}
