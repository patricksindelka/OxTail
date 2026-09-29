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
use crate::detail::{Tone, detail_fields, record_csv, record_json, tone_for};
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

/// Most column names listed in the suggestion bar.
const SUGGESTION_COLUMNS: usize = 7;

/// The question the suggestion bar asks.
pub const SUGGESTION_QUESTION: &str = "Show as columns?";

/// The column names to list for `s`: the preferred order first (names the
/// parser does not have are skipped), then the rest of `schema`.
pub fn suggestion_columns(s: &crate::structure::Suggestion, schema: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in &s.order {
        if let Some(found) = schema.iter().find(|c| c.eq_ignore_ascii_case(name))
            && !out.contains(found)
        {
            out.push(found.clone());
        }
    }
    for c in schema {
        if !out.contains(c) {
            out.push(c.clone());
        }
    }
    out
}

/// The detail line of the suggestion bar: what the format is and the columns
/// it would show, e.g. `Java / log4j: time · level · thread · logger · message`.
pub fn suggestion_detail(s: &crate::structure::Suggestion, columns: &[String]) -> String {
    // A profile's own name is the best name; otherwise the detected format.
    let name = s.profile.as_deref().unwrap_or(&s.name);
    if columns.is_empty() {
        return name.to_string();
    }
    let shown: Vec<&str> = columns
        .iter()
        .take(SUGGESTION_COLUMNS)
        .map(String::as_str)
        .collect();
    let mut list = shown.join(" \u{b7} ");
    if columns.len() > SUGGESTION_COLUMNS {
        list.push_str(&format!(
            " \u{b7} +{} more",
            columns.len() - SUGGESTION_COLUMNS
        ));
    }
    format!("{name}: {list}")
}

/// The whole text of the suggestion bar (question and detail).
pub fn suggestion_text(s: &crate::structure::Suggestion, columns: &[String]) -> String {
    format!("{SUGGESTION_QUESTION} {}", suggestion_detail(s, columns))
}

/// Draws the "show as columns?" bar.
pub fn suggestion_bar(ui: &mut Ui, view: &DocView, colors: &Colors) -> SuggestionAction {
    let Some(s) = &view.st.suggestion else {
        return SuggestionAction::None;
    };
    let schema: Vec<String> = view
        .st
        .parser
        .as_ref()
        .map(|p| p.schema().columns.iter().map(|c| c.name.clone()).collect())
        .unwrap_or_default();
    let columns = suggestion_columns(s, &schema);
    let mut action = SuggestionAction::None;
    let fill = colors.resolve(&ColorRef::subtle(SemanticColor::Info));
    egui::Frame::new()
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(SUGGESTION_QUESTION).strong());
                ui.label(suggestion_detail(s, &columns));
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
    // Computed once per frame and shared by the header and the body.
    let field_list = selected
        .as_ref()
        .and_then(|(_, p)| p.record.as_ref())
        .map(|r| detail_fields(parser.schema(), r));
    let field_count = field_list.as_ref().map(Vec::len);
    ui.horizontal(|ui| {
        ui.strong("Record detail");
        if let Some((line, _)) = &selected {
            let n = if line.number_exact {
                (line.number + 1).to_string()
            } else {
                format!("\u{2248}{}", line.number + 1)
            };
            let what = match field_count {
                Some(f) => format!("line {n} \u{b7} {f} fields"),
                None => format!("line {n}"),
            };
            ui.label(RichText::new(what).color(colors.gutter_text));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::panels::icon_button(
                ui,
                crate::panels::Icon::Close,
                "Close the detail pane",
                "Close the detail pane",
            )
            .clicked()
            {
                view.detail_open = false;
            }
            let has_record = selected.as_ref().is_some_and(|(_, p)| p.record.is_some());
            // Right to left: CSV first, so JSON reads first.
            if ui
                .add_enabled(has_record, egui::Button::new("Copy as CSV"))
                .clicked()
                && let Some((_, p)) = &selected
                && let Some(rec) = &p.record
            {
                copy = Some(record_csv(parser.schema(), rec));
            }
            if ui
                .add_enabled(has_record, egui::Button::new("Copy as JSON"))
                .clicked()
                && let Some((_, p)) = &selected
                && let Some(rec) = &p.record
            {
                copy = Some(record_json(parser.schema(), rec));
            }
        });
    });
    ui.separator();
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
                let fields = field_list.as_deref().unwrap_or_default();
                // Keys share one column, right-aligned against their values.
                let key_font = egui::TextStyle::Body.resolve(ui.style());
                let key_w = fields
                    .iter()
                    .map(|f| {
                        ui.fonts_mut(|fo| {
                            fo.layout_no_wrap(f.name.clone(), key_font.clone(), Color32::WHITE)
                        })
                        .size()
                        .x
                    })
                    .fold(0.0_f32, f32::max)
                    .clamp(40.0, 220.0);
                egui::Grid::new("detail-grid")
                    .num_columns(2)
                    .spacing([14.0, 4.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for f in fields {
                            let key = RichText::new(&f.name).color(if f.extra {
                                colors.gutter_text
                            } else {
                                colors.status_text
                            });
                            let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
                            ui.add_sized(
                                egui::vec2(key_w, row_h),
                                egui::Label::new(key).halign(egui::Align::RIGHT).truncate(),
                            );
                            let shown = if f.kind == ColumnKind::Timestamp {
                                view.st
                                    .cell_text(parser.schema().find(&f.name).unwrap_or(0), rec)
                            } else {
                                f.value.clone()
                            };
                            let mut text = RichText::new(shown).monospace();
                            match tone_for(f.kind, &f.value) {
                                Tone::Plain => {}
                                Tone::Muted => {
                                    text = text.color(
                                        colors.resolve(&ColorRef::solid(SemanticColor::Muted)),
                                    );
                                }
                                Tone::Level { color, strong } => {
                                    text = text.color(colors.resolve(&ColorRef::solid(color)));
                                    if strong {
                                        text = text.strong();
                                    }
                                }
                            }
                            ui.label(text);
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

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn suggestion(profile: Option<&str>, name: &str, order: &[&str]) -> Suggestion {
        Suggestion {
            name: name.into(),
            spec: ParserSpec::AccessCombined,
            order: names(order),
            profile: profile.map(str::to_string),
            score: 0.9,
        }
    }

    #[test]
    fn suggestion_text_reads_naturally() {
        // A profile: its name, then the columns in the profile's order.
        let s = suggestion(
            Some("Java / log4j"),
            "Java / log4j (Regex)",
            &["time", "level", "thread", "logger", "message"],
        );
        let schema = names(&["message", "thread", "level", "time", "logger"]);
        let cols = suggestion_columns(&s, &schema);
        assert_eq!(
            suggestion_text(&s, &cols),
            "Show as columns? Java / log4j: time \u{b7} level \u{b7} thread \u{b7} logger \u{b7} message"
        );
        // The parser's name and the profile's are never both spelled out.
        let t = suggestion_text(&s, &cols);
        assert!(!t.contains("Regex") && !t.contains("Profile"), "{t}");
        // Detected format without a profile.
        let d = suggestion(None, "Nginx/Apache combined", &[]);
        assert_eq!(
            suggestion_text(&d, &names(&["remote", "ts", "status"])),
            "Show as columns? Nginx/Apache combined: remote \u{b7} ts \u{b7} status"
        );
        // No known columns (JSON lines with a wildcard): just the name.
        assert_eq!(
            suggestion_text(&suggestion(Some("logfmt"), "logfmt (logfmt)", &[]), &[]),
            "Show as columns? logfmt"
        );
    }

    #[test]
    fn long_column_lists_are_cut_off() {
        let s = suggestion(None, "CSV", &[]);
        let many: Vec<String> = (0..10).map(|i| format!("c{i}")).collect();
        let t = suggestion_detail(&s, &many);
        assert!(t.ends_with("c6 \u{b7} +3 more"), "{t}");
    }

    #[test]
    fn preferred_order_skips_unknown_names() {
        let s = suggestion(Some("p"), "p", &["ts", "nope", "LEVEL"]);
        let cols = suggestion_columns(&s, &names(&["level", "msg", "ts"]));
        assert_eq!(cols, names(&["ts", "level", "msg"]));
    }
}
