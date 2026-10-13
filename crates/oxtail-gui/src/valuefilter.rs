//! The value filter of a table column. The funnel in a column header opens a
//! list of the column's values; selecting values filters the view to the
//! rows that have one of them. The filter is a column-query entry of the
//! filter stack ([`FilterState::set_column_values`]), so it shows in the
//! filter panel and is saved with the session like any other.
//!
//! A level column lists every level. Other columns list the values seen in
//! the rows drawn while the list was open, and the top values of the
//! last statistics run: listing them all would mean scanning the file.
//!
//! [`FilterState::set_column_values`]: crate::filter::FilterState::set_column_values

use std::cmp::Ordering;
use std::collections::BTreeSet;

use egui::{Id, Rect, RichText, Ui};
use oxtail_columns::{ColumnKind, Level};

use crate::docview::DocView;
use crate::table::FilterMark;
use crate::util::shorten;

/// At most this many values are remembered per column, and listed.
pub const MAX_VALUES: usize = 200;

/// Longer lists scroll.
const SCROLL_AFTER: usize = 14;

/// Longer values are not remembered (a message column is no filter column).
const MAX_VALUE_CHARS: usize = 200;

const LEVELS: [Level; 6] = [
    Level::Trace,
    Level::Debug,
    Level::Info,
    Level::Warn,
    Level::Error,
    Level::Fatal,
];

/// The open value filter: which column, below which header cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValueMenu {
    /// Schema index of the column.
    pub col: usize,
    /// The header cell, in screen coordinates.
    pub anchor: Rect,
}

/// What a click on a column's funnel does to the open value filter: opens
/// it for that column, or closes it when it already is.
pub fn toggle(open: Option<ValueMenu>, col: usize, anchor: Rect) -> Option<ValueMenu> {
    match open {
        Some(m) if m.col == col => None,
        _ => Some(ValueMenu { col, anchor }),
    }
}

/// The header's funnel for the column `name` of `kind`, `active` when the
/// column is being filtered by value. Only for columns the query language
/// can name.
pub fn mark(name: &str, kind: ColumnKind, active: bool) -> FilterMark {
    match kind {
        _ if !queryable_name(name) => FilterMark::None,
        _ if active => FilterMark::Active,
        ColumnKind::Timestamp | ColumnKind::Json => FilterMark::None,
        ColumnKind::Level => FilterMark::Always,
        _ => FilterMark::OnHover,
    }
}

/// Whether `column:...` names the column in the query language: it starts
/// with a letter, `_` or `@`, followed by letters, digits or `_ . @ - $`.
fn queryable_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '@')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '@' | '-' | '$'))
}

/// Whether the query language can say "the cell is exactly `v`": `*` is a
/// wildcard even when quoted, and a trailing `\` would escape the closing
/// quote.
fn listable(v: &str) -> bool {
    !v.is_empty() && !v.contains('*') && !v.ends_with('\\')
}

/// Numbers in numeric order before other text.
fn value_order(a: &str, b: &str) -> Ordering {
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => x.total_cmp(&y).then_with(|| a.cmp(b)),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => a.cmp(b),
    }
}

/// The values to offer for a column of `kind`: every level for a level
/// column, then the values `seen` and the statistics' `top` values that are
/// no level (`30` in a pino log), sorted (at most [`MAX_VALUES`]). The
/// `selected` values are always among them.
pub fn candidates(
    kind: ColumnKind,
    selected: &[String],
    seen: &BTreeSet<String>,
    top: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = if kind == ColumnKind::Level {
        LEVELS.iter().map(|l| l.as_str().to_string()).collect()
    } else {
        Vec::new()
    };
    let mut rest: Vec<&str> = seen
        .iter()
        .chain(top)
        .map(String::as_str)
        .filter(|v| listable(v))
        .filter(|v| kind != ColumnKind::Level || Level::parse(v).is_none())
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .collect();
    rest.sort_by(|a, b| value_order(a, b));
    out.extend(rest.into_iter().take(MAX_VALUES).map(str::to_string));
    for s in selected {
        if !out.contains(s) {
            out.push(s.clone());
        }
    }
    out
}

/// `selected` with `value` switched on or off, in the order of `values`.
pub fn toggled(values: &[String], selected: &[String], value: &str, on: bool) -> Vec<String> {
    values
        .iter()
        .filter(|v| {
            if v.as_str() == value {
                on
            } else {
                selected.contains(v)
            }
        })
        .cloned()
        .collect()
}

/// Remembers the values of column `col` in the rows drawn last frame,
/// under `key`.
fn learn(view: &mut DocView, col: usize, key: &str) {
    let mut found = Vec::new();
    {
        let mut hl = view.hl.borrow_mut();
        for line in &view.last_rows {
            let p = hl.prepare(line);
            if let Some(v) = p.record.as_ref().and_then(|r| r.get(col))
                && listable(v)
                && v.chars().count() <= MAX_VALUE_CHARS
            {
                found.push(v.to_string());
            }
        }
    }
    let seen = view.seen_values.entry(key.to_string()).or_default();
    for v in found {
        if seen.len() >= MAX_VALUES {
            break;
        }
        seen.insert(v);
    }
}

/// Shows the open value filter of `view`, if any. Call after the header,
/// in table mode.
pub fn show(ui: &mut Ui, id: Id, view: &mut DocView) {
    let Some(menu) = view.value_menu else {
        return;
    };
    let Some(info) = view
        .st
        .parser
        .as_ref()
        .and_then(|p| p.schema().columns.get(menu.col).cloned())
    else {
        view.value_menu = None;
        return;
    };
    // Values are remembered per structure and column.
    let key = format!("{}\u{0}{}", view.st.name, info.name);
    learn(view, menu.col, &key);
    let selected = view.filter.column_values(&info.name);
    let top: Vec<String> = view
        .stats
        .table
        .as_ref()
        .filter(|t| {
            view.st.parser.as_ref().map(|p| p.schema().columns.len()) == Some(t.columns.len())
        })
        .and_then(|t| t.columns.get(menu.col))
        .map(|c| c.top(MAX_VALUES).into_iter().map(|t| t.value).collect())
        .unwrap_or_default();
    let values = {
        let empty = BTreeSet::new();
        let seen = view.seen_values.get(&key).unwrap_or(&empty);
        candidates(info.kind, &selected, seen, &top)
    };
    let mut open = true;
    let mut close = false;
    let mut pick: Option<Vec<String>> = None;
    egui::Popup::new(
        id.with(("value-filter", menu.col)),
        ui.ctx().clone(),
        menu.anchor,
        ui.layer_id(),
    )
    .open_bool(&mut open)
    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
    .show(|ui| {
        ui.set_min_width(160.0);
        ui.set_max_width(300.0);
        ui.label(RichText::new(format!("Show rows where {} is", info.name)).strong());
        if values.is_empty() {
            ui.label(RichText::new("No values seen yet").weak());
        }
        let mut rows = |ui: &mut Ui| {
            for v in &values {
                let mut on = selected.contains(v);
                let mut r = ui.toggle_value(&mut on, shorten(v, 40));
                if v.chars().count() > 40 {
                    r = r.on_hover_text(v);
                }
                if r.changed() {
                    pick = Some(toggled(&values, &selected, v, on));
                }
            }
        };
        if values.len() > SCROLL_AFTER {
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| rows(ui));
        } else {
            rows(ui);
        }
        if info.kind != ColumnKind::Level {
            ui.label(
                RichText::new("Values in the rows seen so far")
                    .weak()
                    .small(),
            );
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!selected.is_empty(), egui::Button::new("Show all rows"))
                .clicked()
            {
                pick = Some(Vec::new());
            }
            if ui.button("Close").clicked() {
                close = true;
            }
        });
    });
    if let Some(values) = pick {
        view.filter.set_column_values(&info.name, &values);
        ui.ctx().request_repaint();
    }
    if !open || close {
        view.value_menu = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_level_column_lists_every_level() {
        let seen = BTreeSet::from(["w".to_string()]);
        assert_eq!(
            candidates(ColumnKind::Level, &[], &seen, &[]),
            s(&["TRACE", "DEBUG", "INFO", "WARN", "ERROR", "FATAL"])
        );
        // Values that are no level (pino's numbers) are listed after them.
        let seen = BTreeSet::from(["30".to_string(), "info".to_string(), "40".to_string()]);
        assert_eq!(
            candidates(ColumnKind::Level, &[], &seen, &[])[6..],
            s(&["30", "40"])
        );
    }

    #[test]
    fn values_the_query_cannot_match_exactly_are_left_out() {
        let seen = BTreeSet::from([
            "C:\\temp\\".to_string(),
            "/api/*".to_string(),
            "/api/users".to_string(),
        ]);
        assert_eq!(
            candidates(ColumnKind::Text, &[], &seen, &[]),
            s(&["/api/users"])
        );
    }

    #[test]
    fn numbers_are_listed_in_numeric_order() {
        let seen: BTreeSet<String> = ["10", "9", "200", "abc", "1.5"].map(String::from).into();
        assert_eq!(
            candidates(ColumnKind::Number, &[], &seen, &[]),
            s(&["1.5", "9", "10", "200", "abc"])
        );
    }

    #[test]
    fn other_columns_list_what_was_seen_and_the_top_values() {
        let seen = BTreeSet::from(["web-2".to_string(), "web-1".to_string(), String::new()]);
        let top = s(&["db", "web-1"]);
        assert_eq!(
            candidates(ColumnKind::Text, &[], &seen, &top),
            s(&["db", "web-1", "web-2"])
        );
        // A selected value is listed even when it was not seen.
        assert_eq!(
            candidates(ColumnKind::Text, &s(&["cache"]), &seen, &top),
            s(&["db", "web-1", "web-2", "cache"])
        );
        let many: BTreeSet<String> = (0..MAX_VALUES + 50).map(|i| format!("{i:04}")).collect();
        assert_eq!(
            candidates(ColumnKind::Number, &[], &many, &[]).len(),
            MAX_VALUES
        );
    }

    #[test]
    fn selecting_keeps_the_list_order() {
        let values = s(&["INFO", "WARN", "ERROR"]);
        assert_eq!(
            toggled(&values, &s(&["ERROR"]), "WARN", true),
            s(&["WARN", "ERROR"])
        );
        assert_eq!(
            toggled(&values, &s(&["WARN", "ERROR"]), "WARN", false),
            s(&["ERROR"])
        );
        assert!(toggled(&values, &s(&["WARN"]), "WARN", false).is_empty());
    }

    #[test]
    fn the_funnel_toggles_and_moves_between_columns() {
        let r = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(10.0, 10.0));
        let a = toggle(None, 1, r);
        assert_eq!(a.map(|m| m.col), Some(1));
        assert_eq!(toggle(a, 1, r), None);
        assert_eq!(toggle(a, 2, r).map(|m| m.col), Some(2));
    }

    #[test]
    fn marks_follow_the_column_kind() {
        assert_eq!(mark("level", ColumnKind::Level, false), FilterMark::Always);
        assert_eq!(mark("host", ColumnKind::Text, false), FilterMark::OnHover);
        assert_eq!(mark("ts", ColumnKind::Timestamp, false), FilterMark::None);
        assert_eq!(mark("host", ColumnKind::Text, true), FilterMark::Active);
        // Names the query language cannot write get no funnel.
        for name in ["cs(User-Agent)", "Response Time", "2xx", ""] {
            assert_eq!(
                mark(name, ColumnKind::Text, false),
                FilterMark::None,
                "{name}"
            );
        }
        assert_eq!(
            mark("@timestamp.ms", ColumnKind::Number, false),
            FilterMark::OnHover
        );
    }
}
