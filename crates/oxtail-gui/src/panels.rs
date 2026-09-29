//! The bars around the log view: find bar, filter panel, status bar and
//! banners. Thin egui code over the state in [`DocView`].

use std::time::Instant;

use egui::text::{CCursor, CCursorRange};
use egui::{Color32, Id, Key, Modifiers, RichText, TextEdit, Ui};
use oxtail_config::ProfileSet;
use oxtail_core::{DocState, EncodingChoice, LineEnding, TextEncoding};
use oxtail_search::CaseMode;

use crate::colors::Colors;
use crate::docview::{Banner, BannerKind, DocView};
use crate::filter::FilterEntry;
use crate::find::{Dir, history_step, push_history};
use crate::util::{find_count_label, fmt_bytes, fmt_count};

/// Selects all text of the text edit `id` (so typing replaces it).
fn select_all_text(ctx: &egui::Context, id: Id, len_chars: usize) {
    if let Some(mut state) = TextEditState::load(ctx, id) {
        state.cursor.set_char_range(Some(CCursorRange::two(
            CCursor::new(0),
            CCursor::new(len_chars),
        )));
        state.store(ctx, id);
    }
}

use egui::widgets::text_edit::TextEditState;

/// Draws the find bar. Returns `true` when the search history changed.
pub fn find_bar(
    ui: &mut Ui,
    tab_id: u64,
    view: &mut DocView,
    history: &mut Vec<String>,
    colors: &Colors,
    now: Instant,
) -> bool {
    let mut history_changed = false;
    let text_id = Id::new(("find-text", tab_id));
    ui.horizontal(|ui| {
        ui.label("Find");
        let out = TextEdit::singleline(&mut view.find.text)
            .id(text_id)
            .hint_text("text or regex")
            .desired_width(280.0)
            .show(ui);
        let resp = out.response;
        if view.find.focus {
            resp.request_focus();
            select_all_text(ui.ctx(), text_id, view.find.text.chars().count());
            view.find.focus = false;
        }
        if resp.changed() {
            view.find.query_changed(now);
        }
        if resp.has_focus() {
            let (up, down) = ui.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                )
            });
            if up || down {
                let (pos, text) = history_step(history, view.find.history_pos, up);
                view.find.history_pos = pos;
                if let Some(t) = text {
                    view.find.text = t;
                    view.find.restart_now(now);
                }
            }
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            let shift = ui.input(|i| i.modifiers.shift);
            push_history(history, &view.find.text);
            history_changed = true;
            view.find
                .step(if shift { Dir::Prev } else { Dir::Next }, view.pos.top);
            resp.request_focus();
        }
        let regex = ui
            .toggle_value(&mut view.find.regex, ".*")
            .on_hover_text("Regular expression")
            .changed();
        let case_label = match view.find.case {
            CaseMode::Smart => "Aa",
            CaseMode::Sensitive => "Aa",
            CaseMode::Insensitive => "aa",
        };
        let case_text = match view.find.case {
            CaseMode::Smart => {
                "Case: smart (insensitive unless the pattern has capitals). Click to change."
            }
            CaseMode::Sensitive => "Case: sensitive. Click to change.",
            CaseMode::Insensitive => "Case: insensitive. Click to change.",
        };
        let case_resp = ui
            .selectable_label(view.find.case != CaseMode::Smart, case_label)
            .on_hover_text(case_text);
        let mut case_changed = false;
        if case_resp.clicked() {
            view.find.case = match view.find.case {
                CaseMode::Smart => CaseMode::Sensitive,
                CaseMode::Sensitive => CaseMode::Insensitive,
                CaseMode::Insensitive => CaseMode::Smart,
            };
            case_changed = true;
        }
        let word = ui
            .toggle_value(&mut view.find.whole_word, "W")
            .on_hover_text("Whole word")
            .changed();
        if regex || case_changed || word {
            view.find.query_changed(now);
        }
        if ui
            .button("\u{2191}")
            .on_hover_text("Previous match (Shift+F3)")
            .clicked()
        {
            view.find.step(Dir::Prev, view.pos.top);
        }
        if ui
            .button("\u{2193}")
            .on_hover_text("Next match (F3)")
            .clicked()
        {
            view.find.step(Dir::Next, view.pos.top);
        }
        if view.find.searching || !view.find.status.done {
            ui.spinner();
        }
        if view.find.problem.is_none() && !view.find.text.is_empty() {
            let label = find_count_label(
                view.find.status.matches_found,
                view.find.status.done,
                view.find.current_rank(),
            );
            ui.label(label);
        }
        if ui
            .button("Filter")
            .on_hover_text("Filter view (Ctrl+Shift+F)")
            .clicked()
        {
            view.filter.open = !view.filter.open;
        }
        if ui.button("\u{d7}").on_hover_text("Close (Esc)").clicked() {
            view.find.open = false;
        }
    });
    if let Some(p) = &view.find.problem {
        let mut msg = format!("Invalid regular expression: {}", p.message);
        if let Some(span) = &p.span
            && let Some(bad) = view.find.text.get(span.clone())
            && !bad.is_empty()
        {
            msg.push_str(&format!(" (near \u{201c}{bad}\u{201d})"));
        }
        ui.label(
            RichText::new(msg).color(colors.resolve(&oxtail_highlight::ColorRef::solid(
                oxtail_highlight::SemanticColor::Error,
            ))),
        );
    }
    history_changed
}

/// Draws the filter panel.
pub fn filter_panel(ui: &mut Ui, tab_id: u64, view: &mut DocView, colors: &Colors, now: Instant) {
    ui.horizontal(|ui| {
        ui.strong("Filters");
        let mut enabled = view.filter.enabled;
        if ui
            .checkbox(&mut enabled, "Show only matching lines")
            .on_hover_text("Switch between the filtered and the full view; your position is kept")
            .changed()
        {
            view.set_filter_view(enabled);
        }
        let has_hide = view.hl.borrow().hide.is_some();
        if has_hide
            && ui
                .checkbox(&mut view.filter.show_hidden, "Show hidden lines")
                .on_hover_text("Lines folded away by the profile's hide rules")
                .changed()
        {
            view.filter.changed();
        }
        if view.filter.has_job() {
            if !view.filter.status.done {
                ui.spinner();
            }
            let n = view.filter.set.len() as u64;
            ui.label(format!(
                "{} lines{}",
                fmt_count(n),
                if view.filter.status.done {
                    ""
                } else {
                    " so far"
                }
            ));
        }
        if ui.button("Add").clicked() {
            view.filter.entries.push(FilterEntry::default());
        }
        if ui.button("Clear").clicked() {
            view.filter.entries.clear();
            view.filter.changed();
        }
        if ui
            .button("\u{d7}")
            .on_hover_text("Close the panel")
            .clicked()
        {
            view.filter.open = false;
        }
    });
    let mut remove = None;
    let problems = view.filter.problems.clone();
    let mut typed = false;
    let mut toggled = false;
    for (i, e) in view.filter.entries.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            toggled |= ui.checkbox(&mut e.enabled, "").changed();
            let label = if e.include { "Include" } else { "Exclude" };
            egui::ComboBox::from_id_salt(("filter-mode", tab_id, i))
                .selected_text(label)
                .width(70.0)
                .show_ui(ui, |ui| {
                    toggled |= ui
                        .selectable_value(&mut e.include, true, "Include")
                        .changed();
                    toggled |= ui
                        .selectable_value(&mut e.include, false, "Exclude")
                        .changed();
                });
            let resp = TextEdit::singleline(&mut e.text)
                .id(Id::new(("filter-text", tab_id, i)))
                .hint_text("text or regex")
                .desired_width(260.0)
                .show(ui)
                .response;
            typed |= resp.changed();
            if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                toggled = true;
            }
            toggled |= ui
                .toggle_value(&mut e.regex, ".*")
                .on_hover_text("Regular expression")
                .changed();
            let case_label = match e.case {
                CaseMode::Smart | CaseMode::Sensitive => "Aa",
                CaseMode::Insensitive => "aa",
            };
            if ui
                .selectable_label(e.case != CaseMode::Smart, case_label)
                .on_hover_text("Case: smart / sensitive / insensitive")
                .clicked()
            {
                e.case = match e.case {
                    CaseMode::Smart => CaseMode::Sensitive,
                    CaseMode::Sensitive => CaseMode::Insensitive,
                    CaseMode::Insensitive => CaseMode::Smart,
                };
                toggled = true;
            }
            toggled |= ui
                .toggle_value(&mut e.whole_word, "W")
                .on_hover_text("Whole word")
                .changed();
            if ui.button("\u{d7}").on_hover_text("Remove").clicked() {
                remove = Some(i);
            }
            if let Some((_, p)) = problems.iter().find(|(idx, _)| *idx == i) {
                ui.label(RichText::new(&p.message).color(colors.resolve(
                    &oxtail_highlight::ColorRef::solid(oxtail_highlight::SemanticColor::Error),
                )));
            }
        });
    }
    if let Some(i) = remove {
        view.filter.entries.remove(i);
        toggled = true;
    }
    ui.horizontal(|ui| {
        ui.label("Context");
        let mut b = view.filter.before;
        let mut a = view.filter.after;
        let cb = ui
            .add(
                egui::DragValue::new(&mut b)
                    .range(0..=200)
                    .prefix("before "),
            )
            .changed();
        let ca = ui
            .add(egui::DragValue::new(&mut a).range(0..=200).prefix("after "))
            .changed();
        if cb || ca {
            view.filter.before = b;
            view.filter.after = a;
            toggled = true;
        }
        ui.label(RichText::new("context lines are dimmed").weak());
    });
    if toggled {
        view.filter.changed();
    } else if typed {
        view.filter.edited(now);
    }
}

/// What the status bar asks the application to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusAction {
    /// Nothing.
    None,
    /// Use this profile for the tab (`None` means the default rules).
    SetProfile(Option<String>),
    /// Open the rule editor.
    EditRules,
}

/// Encodings offered in the status bar menu: label, WHATWG name.
pub const ENCODINGS: &[(&str, &str)] = &[
    ("UTF-8", "utf-8"),
    ("UTF-16 LE", "utf-16le"),
    ("UTF-16 BE", "utf-16be"),
    ("Windows-1252 (Western)", "windows-1252"),
    ("Windows-1250 (Central European)", "windows-1250"),
    ("Windows-1251 (Cyrillic)", "windows-1251"),
    ("ISO-8859-2", "iso-8859-2"),
    ("ISO-8859-15", "iso-8859-15"),
    ("KOI8-R", "koi8-r"),
    ("Shift_JIS", "shift_jis"),
    ("GBK", "gbk"),
    ("Big5", "big5"),
    ("EUC-KR", "euc-kr"),
];

/// Draws the status bar contents.
pub fn status_bar(ui: &mut Ui, view: &mut DocView, profiles: &ProfileSet) -> StatusAction {
    let mut action = StatusAction::None;
    let snap = view.snapshot.clone();
    ui.horizontal(|ui| {
        // Follow state.
        let (dot, text) = if view.follow {
            (Color32::from_rgb(0x4c, 0xc3, 0x6b), "Following")
        } else {
            (Color32::from_rgb(0xe0, 0xaf, 0x68), "Paused")
        };
        ui.label(RichText::new("\u{25cf}").color(dot));
        if ui
            .selectable_label(false, text)
            .on_hover_text("Click to toggle following (F)")
            .clicked()
        {
            view.toggle_follow();
        }
        ui.separator();

        // Encoding.
        ui.menu_button(snap.encoding.name(), |ui| {
            if ui.button("Auto-detect").clicked() {
                view.set_encoding(EncodingChoice::Auto);
                ui.close();
            }
            ui.separator();
            for (label, name) in ENCODINGS {
                if ui.button(*label).clicked() {
                    if let Ok(enc) = TextEncoding::from_label(name) {
                        view.set_encoding(EncodingChoice::Fixed(enc));
                    }
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text("Text encoding (click to change)");
        ui.label(match snap.line_ending {
            LineEnding::Lf => "LF",
            LineEnding::Cr => "CR",
        });
        ui.separator();
        ui.label(fmt_bytes(snap.utf8_len));
        ui.separator();
        let lines = if snap.lines.exact {
            format!("{} lines", fmt_count(snap.lines.known))
        } else {
            format!("\u{2248}{} lines", fmt_count(snap.lines.estimated_total))
        };
        ui.label(lines);
        match &snap.state {
            DocState::Opening => {
                ui.label("Opening\u{2026}");
            }
            DocState::Indexing { fraction } => {
                ui.label(format!("Indexing {:.0}%", fraction * 100.0));
            }
            DocState::Ready => {}
            DocState::Error(msg) => {
                ui.label(RichText::new(format!("Error: {msg}")).color(Color32::LIGHT_RED));
            }
        }
        if let Some((n, exact)) = view.selection_count() {
            ui.separator();
            ui.label(format!(
                "{}{} selected",
                if exact { "" } else { "\u{2248}" },
                fmt_count(n)
            ));
        }
        if view.is_filtered() {
            ui.separator();
            ui.label(format!(
                "Filtered: {} lines",
                fmt_count(view.filter.set.len() as u64)
            ));
        }
        ui.separator();

        // Profile switcher.
        let current = view
            .hl
            .borrow()
            .profile
            .clone()
            .unwrap_or_else(|| "Default rules".into());
        ui.menu_button(format!("Profile: {current}"), |ui| {
            if ui.button("Default rules").clicked() {
                action = StatusAction::SetProfile(None);
                ui.close();
            }
            ui.separator();
            for p in profiles.profiles() {
                if ui.selectable_label(p.name == current, &p.name).clicked() {
                    action = StatusAction::SetProfile(Some(p.name.clone()));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Edit rules\u{2026}").clicked() {
                action = StatusAction::EditRules;
                ui.close();
            }
        })
        .response
        .on_hover_text("Highlight profile for this tab");

        if let Some((msg, _)) = &view.toast {
            ui.separator();
            ui.label(RichText::new(msg).strong());
        }
    });
    action
}

/// Draws the banner (truncation, rotation, errors) if there is one.
pub fn banner(ui: &mut Ui, view: &mut DocView, colors: &Colors) {
    let Some(b) = view.banner.clone() else { return };
    let text = banner_text(&b);
    let fill = colors.resolve(&oxtail_highlight::ColorRef::subtle(
        oxtail_highlight::SemanticColor::Warn,
    ));
    egui::Frame::new()
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(text);
                if ui.button("Dismiss").clicked() {
                    view.banner = None;
                }
            });
        });
}

/// The banner message.
pub fn banner_text(b: &Banner) -> String {
    match &b.kind {
        BannerKind::Truncated => format!(
            "The file was truncated at {} (UTC). Showing it from the start; still following.",
            b.at
        ),
        BannerKind::Rotated => format!(
            "The file was rotated at {} (UTC). Now following the new file.",
            b.at
        ),
        BannerKind::Removed => format!(
            "The file was removed at {} (UTC). Still following the open file handle.",
            b.at
        ),
        BannerKind::EncodingChanged => format!("Encoding changed at {} (UTC).", b.at),
        BannerKind::Error(m) => format!("{m} ({} UTC)", b.at),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_messages_mention_the_time() {
        for kind in [
            BannerKind::Truncated,
            BannerKind::Rotated,
            BannerKind::Removed,
            BannerKind::EncodingChanged,
            BannerKind::Error("disk on fire".into()),
        ] {
            let t = banner_text(&Banner {
                kind,
                at: "12:03:44".into(),
            });
            assert!(t.contains("12:03:44"), "{t}");
        }
    }

    #[test]
    fn every_listed_encoding_resolves() {
        for (label, name) in ENCODINGS {
            assert!(TextEncoding::from_label(name).is_ok(), "{label}: {name}");
        }
    }
}
