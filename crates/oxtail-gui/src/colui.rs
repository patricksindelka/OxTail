//! Panels and windows for the column features: the suggestion bar, the
//! parser chooser, the detail pane, the statistics window and the export
//! window. Thin egui code over the state in [`DocView`]; the logic they call
//! is in plain modules ([`crate::structure`], [`crate::chooser`],
//! [`crate::detail`], [`crate::stats`], [`crate::export`]).

use egui::{Align2, Color32, Context, RichText, Ui};
use oxtail_columns::{ColumnKind, ParserSpec};
use oxtail_highlight::{ColorRef, SemanticColor};

use crate::chooser::{CustomKind, DELIMITERS, build_spec};
use crate::colors::Colors;
use crate::detail::{detail_fields, record_csv, record_json};
use crate::docview::DocView;
use crate::export::{ExportFormat, ExportMsg};
use crate::filter::FilterEntry;
use crate::qfilter::equals_query;
use crate::stats::{StatsScope, format_stat};
use crate::util::{fmt_count, shorten};

/// What the suggestion bar asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionAction {
    /// Nothing.
    None,
    /// Use the suggested parser and show the table.
    Accept,
    /// Open the chooser.
    Choose,
    /// Do not ask again.
    Dismiss,
}

fn error_color(colors: &Colors) -> Color32 {
    colors.resolve(&ColorRef::solid(SemanticColor::Error))
}

/// The text of the suggestion bar.
pub fn suggestion_text(s: &crate::structure::Suggestion) -> String {
    match &s.profile {
        Some(p) => format!(
            "Profile \u{201c}{p}\u{201d} defines columns ({}). Show as columns?",
            s.name
        ),
        None => format!("Looks like {}. Show as columns?", s.name),
    }
}

/// Draws the "show as columns?" bar.
pub fn suggestion_bar(ui: &mut Ui, view: &DocView, colors: &Colors) -> SuggestionAction {
    let Some(s) = &view.st.suggestion else {
        return SuggestionAction::None;
    };
    let mut action = SuggestionAction::None;
    let fill = colors.resolve(&ColorRef::subtle(SemanticColor::Info));
    egui::Frame::new()
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(suggestion_text(s));
                if ui.button("Accept").clicked() {
                    action = SuggestionAction::Accept;
                }
                if ui.button("Choose parser\u{2026}").clicked() {
                    action = SuggestionAction::Choose;
                }
                if ui.button("Dismiss").clicked() {
                    action = SuggestionAction::Dismiss;
                }
            });
        });
    action
}

/// The parser chooser window.
pub fn chooser_window(ctx: &Context, view: &mut DocView) {
    if !view.chooser.open {
        return;
    }
    let mut open = true;
    let mut pick: Option<Result<ParserSpec, String>> = None;
    let mut plain = false;
    let detections: Vec<(String, f32, ParserSpec)> = view
        .st
        .detections
        .iter()
        .map(|d| (d.name.clone(), d.score, d.spec.clone()))
        .collect();
    let sample = view.sample.clone();
    egui::Window::new("Choose parser")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_TOP, [0.0, 90.0])
        .show(ctx, |ui| {
            ui.strong("Detected formats");
            if detections.is_empty() {
                ui.label(RichText::new("Nothing recognised in the first lines.").weak());
            }
            for (name, score, spec) in &detections {
                ui.horizontal(|ui| {
                    if ui.button("Use").clicked() {
                        pick = Some(Ok(spec.clone()));
                    }
                    ui.label(format!("{name} ({:.0}% match)", score * 100.0));
                });
            }
            ui.separator();
            ui.strong("Build one");
            ui.horizontal(|ui| {
                ui.label("Regex");
                ui.add(
                    egui::TextEdit::singleline(&mut view.chooser.regex)
                        .hint_text("(?P<time>\\S+) (?P<level>\\w+) (?P<msg>.*)")
                        .desired_width(300.0),
                );
                if ui.button("Use").clicked() {
                    pick = Some(build_spec(CustomKind::Regex, &view.chooser.regex, &sample));
                }
            });
            ui.horizontal(|ui| {
                ui.label("log4j");
                ui.add(
                    egui::TextEdit::singleline(&mut view.chooser.log4j)
                        .hint_text("%d %-5p [%t] %c - %m%n")
                        .desired_width(300.0),
                );
                if ui.button("Use").clicked() {
                    pick = Some(build_spec(CustomKind::Log4j, &view.chooser.log4j, &sample));
                }
            });
            ui.horizontal(|ui| {
                if ui.button("JSON Lines").clicked() {
                    pick = Some(build_spec(CustomKind::JsonLines, "", &sample));
                }
                if ui.button("logfmt").clicked() {
                    pick = Some(build_spec(CustomKind::Logfmt, "", &sample));
                }
            });
            ui.horizontal(|ui| {
                let (_, label) = DELIMITERS[view.chooser.delimiter.min(DELIMITERS.len() - 1)];
                egui::ComboBox::from_id_salt("chooser-delim")
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        for (i, (_, l)) in DELIMITERS.iter().enumerate() {
                            ui.selectable_value(&mut view.chooser.delimiter, i, *l);
                        }
                    });
                if ui.button("Delimited (header in the first line)").clicked() {
                    let (d, _) = DELIMITERS[view.chooser.delimiter.min(DELIMITERS.len() - 1)];
                    pick = Some(build_spec(CustomKind::Delimited(d), "", &sample));
                }
            });
            if let Some(e) = &view.chooser.error {
                ui.label(RichText::new(e).color(Color32::LIGHT_RED));
            }
            ui.separator();
            if ui.button("Plain text (no columns)").clicked() {
                plain = true;
            }
        });
    if let Some(res) = pick {
        match res.and_then(|spec| view.choose_parser(spec)) {
            Ok(()) => {
                view.chooser.error = None;
                open = false;
            }
            Err(e) => view.chooser.error = Some(e),
        }
    }
    if plain {
        view.clear_parser();
        open = false;
    }
    view.chooser.open = open;
}

/// The detail pane's content. Returns text to copy, if a copy button was
/// pressed.
pub fn detail_pane(ui: &mut Ui, view: &mut DocView, colors: &Colors) -> Option<String> {
    let mut copy: Option<String> = None;
    let Some(parser) = view.st.parser.clone() else {
        ui.label("This file has no columns.");
        return None;
    };
    let selected = view.selected_record();
    ui.horizontal(|ui| {
        ui.strong("Detail");
        if let Some((line, _)) = &selected {
            let n = if line.number_exact {
                (line.number + 1).to_string()
            } else {
                format!("\u{2248}{}", line.number + 1)
            };
            ui.label(RichText::new(format!("line {n}")).weak());
        }
        let has_record = selected.as_ref().is_some_and(|(_, p)| p.record.is_some());
        if ui
            .add_enabled(has_record, egui::Button::new("Copy as JSON"))
            .clicked()
            && let Some((_, p)) = &selected
            && let Some(rec) = &p.record
        {
            copy = Some(record_json(parser.schema(), rec));
        }
        if ui
            .add_enabled(has_record, egui::Button::new("Copy as CSV"))
            .clicked()
            && let Some((_, p)) = &selected
            && let Some(rec) = &p.record
        {
            copy = Some(record_csv(parser.schema(), rec));
        }
        if ui
            .button("\u{d7}")
            .on_hover_text("Close the detail pane")
            .clicked()
        {
            view.detail_open = false;
        }
    });
    let Some((line, prepared)) = selected else {
        ui.label(RichText::new("Select a line to see its fields.").weak());
        return copy;
    };
    egui::ScrollArea::vertical()
        .id_salt("detail-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| match &prepared.record {
            None => {
                ui.label(
                    RichText::new("Not a record of this format (a continuation line):").weak(),
                );
                ui.label(RichText::new(shorten(&line.text, 4000)).monospace());
            }
            Some(rec) => {
                egui::Grid::new("detail-grid")
                    .num_columns(2)
                    .spacing([12.0, 3.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for f in detail_fields(parser.schema(), rec) {
                            let name = RichText::new(&f.name).strong();
                            ui.label(if f.extra {
                                name.color(colors.gutter_text)
                            } else {
                                name
                            });
                            let shown = if f.kind == ColumnKind::Timestamp {
                                view.st
                                    .cell_text(parser.schema().find(&f.name).unwrap_or(0), rec)
                            } else {
                                f.value.clone()
                            };
                            ui.label(RichText::new(shown).monospace());
                            ui.end_row();
                        }
                    });
            }
        });
    copy
}

/// The statistics window.
pub fn stats_window(ctx: &Context, view: &mut DocView, colors: &Colors) {
    if !view.stats.open {
        return;
    }
    let mut open = true;
    let mut add_filter: Option<String> = None;
    let has_filter = view.is_filtered();
    let Some(parser) = view.st.parser.clone() else {
        view.stats.open = false;
        return;
    };
    let mut start = false;
    egui::Window::new("Column statistics")
        .open(&mut open)
        .default_width(420.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.radio_value(&mut view.stats.scope, StatsScope::File, "Whole file");
                ui.add_enabled_ui(has_filter, |ui| {
                    ui.radio_value(&mut view.stats.scope, StatsScope::Filter, "Current filter");
                });
                if view.stats.running() {
                    if ui.button("Cancel").clicked() {
                        view.stats.cancel();
                    }
                } else if ui
                    .button(if view.stats.table.is_some() {
                        "Refresh"
                    } else {
                        "Compute"
                    })
                    .clicked()
                {
                    start = true;
                }
            });
            let (done, total) = view.stats.progress;
            if view.stats.running() {
                let f = if total > 0 {
                    done as f32 / total as f32
                } else {
                    0.0
                };
                ui.add(egui::ProgressBar::new(f).text(format!(
                    "{} / {} lines",
                    fmt_count(done),
                    fmt_count(total)
                )));
            } else if let Some(r) = view.stats.stopped {
                ui.label(RichText::new(format!("Stopped: {r}")).color(error_color(colors)));
            } else if view.stats.done {
                ui.label(
                    RichText::new(format!(
                        "{} records{}",
                        fmt_count(view.stats.table.as_ref().map_or(0, |t| t.records)),
                        if view.stats.skipped > 0 {
                            format!(" ({} other lines skipped)", fmt_count(view.stats.skipped))
                        } else {
                            String::new()
                        }
                    ))
                    .weak(),
                );
            }
            ui.separator();
            let n = parser.schema().len();
            view.stats.column = view.stats.column.min(n.saturating_sub(1));
            let names: Vec<String> = parser
                .schema()
                .columns
                .iter()
                .map(|c| c.name.clone())
                .collect();
            ui.horizontal(|ui| {
                ui.label("Column");
                egui::ComboBox::from_id_salt("stats-column")
                    .selected_text(names.get(view.stats.column).cloned().unwrap_or_default())
                    .show_ui(ui, |ui| {
                        for (i, name) in names.iter().enumerate() {
                            ui.selectable_value(&mut view.stats.column, i, name);
                        }
                    });
            });
            let Some(table) = &view.stats.table else {
                ui.label(RichText::new("Press Compute to scan the file.").weak());
                return;
            };
            let Some(col) = table.columns.get(view.stats.column) else {
                return;
            };
            let kind = parser
                .schema()
                .columns
                .get(view.stats.column)
                .map_or(ColumnKind::Text, |c| c.kind);
            ui.label(format!(
                "{} values, {} missing, about {} distinct",
                fmt_count(col.count()),
                fmt_count(col.missing()),
                fmt_count(col.distinct_estimate())
            ));
            if col.numeric_count() > 0 {
                let f = |v: Option<f64>| v.map_or("-".to_string(), |v| format_stat(kind, v));
                let p = col.percentiles();
                ui.label(
                    RichText::new(format!(
                        "min {}   max {}   mean {}   p50 {}   p90 {}   p99 {}",
                        f(col.min()),
                        f(col.max()),
                        f(col.mean()),
                        f(p.map(|p| p.p50)),
                        f(p.map(|p| p.p90)),
                        f(p.map(|p| p.p99)),
                    ))
                    .monospace(),
                );
            }
            ui.add_space(4.0);
            ui.label(RichText::new("Top values (click to filter)").weak());
            let top = col.top(20);
            let max = top.first().map_or(1, |t| t.count.max(1));
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .id_salt("stats-top")
                .show(ui, |ui| {
                    egui::Grid::new("stats-grid").num_columns(3).show(ui, |ui| {
                        for t in &top {
                            let label = if t.value.is_empty() {
                                "(empty)".to_string()
                            } else {
                                shorten(&t.value, 60)
                            };
                            if ui.link(label).on_hover_text(&t.value).clicked() {
                                add_filter =
                                    Some(equals_query(&names[view.stats.column], &t.value));
                            }
                            ui.label(RichText::new(fmt_count(t.count)).monospace());
                            ui.add(
                                egui::ProgressBar::new(t.count as f32 / max as f32)
                                    .desired_width(120.0),
                            );
                            ui.end_row();
                        }
                    });
                });
        });
    if start {
        view.start_stats();
    }
    if let Some(q) = add_filter {
        view.filter.entries.retain(|e| !e.text.is_empty());
        view.filter.entries.push(FilterEntry::column_query(q, true));
        view.filter.open = true;
        view.filter.enabled = true;
        view.filter.changed();
    }
    if !open {
        view.stats.cancel();
    }
    view.stats.open = open;
}

/// The export window.
pub fn export_window(ctx: &Context, view: &mut DocView, colors: &Colors) {
    if !view.export.open {
        return;
    }
    let Some(parser) = view.st.parser.clone() else {
        view.export.open = false;
        return;
    };
    view.export.sync_columns(parser.schema().len());
    let mut open = true;
    let mut go = false;
    let has_filter = view.is_filtered();
    let order: Vec<usize> = view.st.layout.cols.iter().map(|c| c.col).collect();
    egui::Window::new("Export")
        .open(&mut open)
        .resizable(false)
        .anchor(Align2::CENTER_TOP, [0.0, 90.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Format");
                ui.radio_value(&mut view.export.format, ExportFormat::Csv, "CSV");
                ui.radio_value(
                    &mut view.export.format,
                    ExportFormat::JsonLines,
                    "JSON Lines",
                );
            });
            ui.add_enabled_ui(has_filter, |ui| {
                ui.checkbox(
                    &mut view.export.filtered_only,
                    "Only the lines of the current filter",
                );
            });
            ui.label("Columns");
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .id_salt("export-cols")
                .show(ui, |ui| {
                    for &c in &order {
                        let name = &parser.schema().columns[c].name;
                        if let Some(sel) = view.export.selected.get_mut(c) {
                            ui.checkbox(sel, name);
                        }
                    }
                });
            ui.horizontal(|ui| {
                if ui.button("All").clicked() {
                    view.export.selected.iter_mut().for_each(|s| *s = true);
                }
                if ui.button("None").clicked() {
                    view.export.selected.iter_mut().for_each(|s| *s = false);
                }
            });
            ui.separator();
            if view.export.job.is_some() {
                let (done, total) = view.export.progress;
                let f = if total > 0 {
                    done as f32 / total as f32
                } else {
                    0.0
                };
                ui.add(egui::ProgressBar::new(f).text(format!(
                    "{} / {} lines",
                    fmt_count(done),
                    fmt_count(total)
                )));
                if ui.button("Cancel").clicked()
                    && let Some(j) = &view.export.job
                {
                    j.cancel();
                }
            } else {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!view.export.choosing, egui::Button::new("Export\u{2026}"))
                        .clicked()
                    {
                        go = true;
                    }
                    if view.export.choosing {
                        ui.spinner();
                    }
                });
            }
            match &view.export.result {
                Some(ExportMsg::Done { written, path }) => {
                    ui.label(format!(
                        "Wrote {} records to {}",
                        fmt_count(*written),
                        path.display()
                    ));
                }
                Some(ExportMsg::Cancelled) => {
                    ui.label("Export cancelled.");
                }
                Some(ExportMsg::Failed(e)) => {
                    ui.label(RichText::new(e).color(error_color(colors)));
                }
                _ => {}
            }
        });
    if go {
        let name = view.doc.display_name().to_string();
        let stem = name
            .rsplit_once('.')
            .map_or(name.as_str(), |(s, _)| s)
            .to_string();
        view.choose_export_path(&format!("{stem}-export"));
    }
    view.export.open = open;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::Suggestion;
    use oxtail_columns::ParserSpec;

    #[test]
    fn suggestion_text_names_the_format_or_the_profile() {
        let mut s = Suggestion {
            name: "Nginx/Apache combined".into(),
            spec: ParserSpec::AccessCombined,
            order: Vec::new(),
            profile: None,
            score: 0.9,
        };
        assert_eq!(
            suggestion_text(&s),
            "Looks like Nginx/Apache combined. Show as columns?"
        );
        s.profile = Some("Nginx".into());
        assert!(suggestion_text(&s).contains("Nginx"));
    }
}
