//! The bars around the log view: find bar, filter panel, status bar and
//! banners. Thin egui code over the state in [`DocView`].

use std::time::Instant;

use egui::text::{CCursor, CCursorRange};
use egui::widgets::text_edit::TextEditState;
use egui::{
    Color32, CornerRadius, Id, Key, Modifiers, Response, RichText, Sense, Stroke, StrokeKind,
    TextEdit, Ui, WidgetInfo, WidgetType, pos2, vec2,
};
use oxtail_config::ProfileSet;
use oxtail_core::{DocState, EncodingChoice, LineEnding, TextEncoding};
use oxtail_search::CaseMode;

use crate::colors::Colors;
use crate::docview::{Banner, BannerKind, DocView};
use crate::filter::{FilterEntry, FilterState};
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

// ------------------------------------------------------------------ widgets

/// Which glyph an [`icon_button`] paints. They are drawn as shapes, not text,
/// so they do not depend on the fonts having the arrow or cross glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Icon {
    /// A chevron pointing up (previous match).
    Up,
    /// A chevron pointing down (next match).
    Down,
    /// A cross (close, remove).
    Close,
}

/// Sets the accessible name of the widget `resp` (for widgets egui labels
/// itself with a visible text, or not at all).
pub(crate) fn a11y_label(resp: &Response, label: &str) {
    resp.ctx.accesskit_node_builder(resp.id, |b| {
        b.set_label(label.to_owned());
    });
}

/// Gives the widget `resp` the AccessKit role `role`, on top of what
/// `widget_info` filled in.
pub(crate) fn a11y_role(resp: &Response, role: egui::accesskit::Role) {
    resp.ctx
        .accesskit_node_builder(resp.id, |b| b.set_role(role));
}

/// The bounds of `rect` as AccessKit wants them.
fn ak_rect(rect: egui::Rect) -> egui::accesskit::Rect {
    egui::accesskit::Rect {
        x0: f64::from(rect.min.x),
        y0: f64::from(rect.min.y),
        x1: f64::from(rect.max.x),
        y1: f64::from(rect.max.y),
    }
}

/// Starts an AccessKit list over `rect` for custom-painted rows: returns a
/// child `Ui` to pass to [`a11y_item`], or `None` when no screen reader (or
/// test harness) is listening, so nothing is built for nothing.
pub(crate) fn a11y_list(
    ui: &mut Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    rect: egui::Rect,
    role: egui::accesskit::Role,
    label: &str,
) -> Option<Ui> {
    let list = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(salt)
            .max_rect(rect)
            .sense(Sense::hover()),
    );
    ui.ctx().accesskit_node_builder(list.unique_id(), |b| {
        b.set_role(role);
        b.set_label(label.to_owned());
        b.set_bounds(ak_rect(rect));
    })?;
    Some(list)
}

/// One row of an [`a11y_list`]: its text as the name, whether it is
/// selected, and an optional description (a line number).
pub(crate) fn a11y_item(
    list: &Ui,
    id: Id,
    rect: egui::Rect,
    role: egui::accesskit::Role,
    label: &str,
    selected: Option<bool>,
    description: Option<&str>,
) {
    // Registers the list as the parent of the row's node.
    list.interact(rect, id, Sense::hover());
    list.ctx().accesskit_node_builder(id, |b| {
        b.set_role(role);
        b.set_label(label.to_owned());
        b.set_bounds(ak_rect(rect));
        if let Some(sel) = selected {
            b.set_selected(sel);
        }
        if let Some(d) = description {
            b.set_description(d.to_owned());
        }
    });
}

/// Describes a custom-painted scrollbar to screen readers: its name, its
/// position as `0.0..=1.0` and its direction.
pub(crate) fn a11y_scrollbar(resp: &Response, label: &str, position: f64, vertical: bool) {
    resp.widget_info(|| WidgetInfo {
        value: Some(position),
        ..WidgetInfo::labeled(WidgetType::ScrollBar, true, label)
    });
    resp.ctx.accesskit_node_builder(resp.id, |b| {
        b.set_orientation(if vertical {
            egui::accesskit::Orientation::Vertical
        } else {
            egui::accesskit::Orientation::Horizontal
        });
        b.set_min_numeric_value(0.0);
        b.set_max_numeric_value(1.0);
    });
}

/// A sense that clicks and drags but does not take a Tab stop (scrollbars
/// and the minimap: the keyboard has its own keys for them).
pub(crate) fn pointer_sense() -> Sense {
    Sense::CLICK | Sense::DRAG
}

/// Colours of a chip-like control from the current visuals: the accent shows
/// what is on.
fn chip_style(ui: &Ui, on: bool, hovered: bool, enabled: bool) -> (Color32, Stroke, Color32) {
    let v = ui.visuals();
    let accent = v.hyperlink_color;
    let panel = v.panel_fill;
    let text = v.text_color();
    let (fill, stroke, fg) = if on {
        (
            crate::colors::mix32(panel, accent, if hovered { 0.42 } else { 0.32 }),
            Stroke::new(1.0, accent),
            text,
        )
    } else if hovered {
        (
            crate::colors::mix32(panel, text, 0.10),
            Stroke::new(1.0, v.widgets.hovered.bg_stroke.color),
            text,
        )
    } else {
        (
            Color32::TRANSPARENT,
            Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color),
            crate::colors::mix32(text, panel, 0.25),
        )
    };
    if enabled {
        (fill, stroke, fg)
    } else {
        (
            Color32::TRANSPARENT,
            Stroke::new(1.0, crate::colors::mix32(stroke.color, panel, 0.6)),
            crate::colors::mix32(fg, panel, 0.6),
        )
    }
}

/// A toggle with a clear on/off look (accent fill and outline when on, an
/// outline only when off), a tooltip and an accessible name. `text` is what
/// is drawn, `name` what a screen reader says.
pub(crate) fn toggle_chip(
    ui: &mut Ui,
    on: &mut bool,
    text: &str,
    name: &str,
    tip: &str,
) -> Response {
    let state = *on;
    chip(ui, state, text, name, tip, |on_now| *on = on_now)
}

/// The shared drawing of chips: `set` receives the new state after a click.
fn chip(
    ui: &mut Ui,
    state: bool,
    text: &str,
    name: &str,
    tip: &str,
    set: impl FnOnce(bool),
) -> Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER);
    let size = (galley.size() + vec2(14.0, 6.0)).max(vec2(26.0, ui.spacing().interact_size.y));
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    let mut now = state;
    if resp.clicked() {
        now = !state;
        set(now);
        resp.mark_changed();
    }
    let (fill, stroke, fg) = chip_style(ui, now, resp.hovered() || resp.has_focus(), enabled);
    ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
    ui.painter()
        .rect_stroke(rect, CornerRadius::same(4), stroke, StrokeKind::Inside);
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(1.0),
            CornerRadius::same(5),
            Stroke::new(2.0, ui.visuals().hyperlink_color),
            StrokeKind::Outside,
        );
    }
    let pos = rect.center() - galley.size() * 0.5;
    ui.painter().galley(pos, galley, fg);
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, now, name));
    resp.on_hover_text(tip)
}

/// A small square button with a drawn icon, a tooltip and an accessible name.
pub(crate) fn icon_button(ui: &mut Ui, icon: Icon, name: &str, tip: &str) -> Response {
    let side = ui.spacing().interact_size.y.max(22.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    let enabled = ui.is_enabled();
    let v = ui.visuals();
    let text = v.text_color();
    let panel = v.panel_fill;
    let hot = resp.hovered() || resp.has_focus();
    if hot && enabled {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(4),
            crate::colors::mix32(panel, text, 0.14),
        );
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(2.0, ui.visuals().hyperlink_color),
            StrokeKind::Inside,
        );
    }
    let mut color = if hot {
        text
    } else {
        crate::colors::mix32(text, panel, 0.2)
    };
    if !enabled {
        color = crate::colors::mix32(color, panel, 0.6);
    }
    let stroke = Stroke::new(1.6, color);
    let c = rect.center();
    let painter = ui.painter();
    match icon {
        Icon::Up => painter.add(egui::Shape::line(
            vec![c + vec2(-4.5, 2.5), c + vec2(0.0, -2.5), c + vec2(4.5, 2.5)],
            stroke,
        )),
        Icon::Down => painter.add(egui::Shape::line(
            vec![
                c + vec2(-4.5, -2.5),
                c + vec2(0.0, 2.5),
                c + vec2(4.5, -2.5),
            ],
            stroke,
        )),
        Icon::Close => {
            painter.line_segment([c + vec2(-4.0, -4.0), c + vec2(4.0, 4.0)], stroke);
            painter.line_segment([c + vec2(-4.0, 4.0), c + vec2(4.0, -4.0)], stroke)
        }
    };
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, name));
    resp.on_hover_text(tip)
}

/// The three-way case toggle: automatic (off look), always and never (on
/// look). Clicking cycles. `who` prefixes the accessible name ("Filter 2").
/// Returns whether the mode changed.
pub(crate) fn case_chip(ui: &mut Ui, case: &mut CaseMode, who: &str) -> bool {
    let (label, on, what, tip) = match *case {
        CaseMode::Smart => (
            "Aa",
            false,
            "match case: automatic",
            "Match case: automatic (case-sensitive only when the text has capitals). Click to change.",
        ),
        CaseMode::Sensitive => (
            "Aa",
            true,
            "match case: always",
            "Match case: always. Click to change.",
        ),
        CaseMode::Insensitive => (
            "aa",
            true,
            "match case: never",
            "Match case: never (ignore case). Click to change.",
        ),
    };
    let name = if who.is_empty() {
        let mut c = what.chars();
        c.next()
            .map(|f| f.to_uppercase().chain(c).collect::<String>())
            .unwrap_or_default()
    } else {
        format!("{who}: {what}")
    };
    let mut clicked = false;
    chip(ui, on, label, &name, tip, |_| clicked = true);
    if clicked {
        *case = match *case {
            CaseMode::Smart => CaseMode::Sensitive,
            CaseMode::Sensitive => CaseMode::Insensitive,
            CaseMode::Insensitive => CaseMode::Smart,
        };
    }
    clicked
}

/// What the find bar's count says. `pending` is a restart waiting for the
/// typing pause; until the search has run there is nothing to count, and
/// "No matches" would be wrong.
pub(crate) fn find_status_text(
    found: u64,
    done: bool,
    rank: Option<usize>,
    pending: bool,
) -> String {
    if pending {
        return "Searching\u{2026}".to_string();
    }
    find_count_label(found, done, rank)
}

/// The follow indicator: a filled green dot while following, two pause bars
/// otherwise. Clicking toggles. Returns the response.
pub(crate) fn follow_chip(ui: &mut Ui, following: bool, colors: &Colors) -> Response {
    let text = if following { "Following" } else { "Paused" };
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER);
    let size = vec2(
        galley.size().x + 32.0,
        ui.spacing().interact_size.y.max(galley.size().y + 4.0),
    );
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let v = ui.visuals();
    let (text_c, panel) = (v.text_color(), v.panel_fill);
    if resp.hovered() || resp.has_focus() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(4),
            crate::colors::mix32(panel, text_c, 0.10),
        );
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(2.0, ui.visuals().hyperlink_color),
            StrokeKind::Inside,
        );
    }
    let c = pos2(rect.left() + 13.0, rect.center().y);
    if following {
        let dot = colors.resolve(&oxtail_highlight::ColorRef::solid(
            oxtail_highlight::SemanticColor::Success,
        ));
        ui.painter().circle_filled(c, 4.5, dot);
    } else {
        let bar = Stroke::new(2.2, crate::colors::mix32(text_c, panel, 0.35));
        for dx in [-2.5, 2.5] {
            ui.painter()
                .line_segment([c + vec2(dx, -4.0), c + vec2(dx, 4.0)], bar);
        }
    }
    let fg = if following {
        text_c
    } else {
        crate::colors::mix32(text_c, panel, 0.25)
    };
    ui.painter().galley(
        pos2(rect.left() + 24.0, rect.center().y - galley.size().y * 0.5),
        galley,
        fg,
    );
    resp.widget_info(|| {
        WidgetInfo::selected(
            WidgetType::Checkbox,
            true,
            following,
            "Follow the end of the file",
        )
    });
    a11y_role(&resp, egui::accesskit::Role::Switch);
    resp.on_hover_text(if following {
        "Following the end of the file. Click to pause (F)"
    } else {
        "Paused. Click to follow the end of the file (F)"
    })
}

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
        a11y_label(&resp, "Find text");
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
        let regex = toggle_chip(
            ui,
            &mut view.find.regex,
            ".*",
            "Regular expression",
            "Regular expression: on treats the text as a pattern",
        )
        .changed();
        let case_changed = case_chip(ui, &mut view.find.case, "");
        let word = toggle_chip(
            ui,
            &mut view.find.whole_word,
            "W",
            "Whole word",
            "Whole word: only match complete words",
        )
        .changed();
        if regex || case_changed || word {
            view.find.query_changed(now);
        }
        if icon_button(ui, Icon::Up, "Previous match", "Previous match (Shift+F3)").clicked() {
            view.find.step(Dir::Prev, view.pos.top);
        }
        if icon_button(ui, Icon::Down, "Next match", "Next match (F3)").clicked() {
            view.find.step(Dir::Next, view.pos.top);
        }
        let pending = view.find.debounce_remaining(now).is_some() && !view.find.text.is_empty();
        if view.find.searching || !view.find.status.done || pending {
            ui.spinner();
        }
        if view.find.problem.is_none() && !view.find.text.is_empty() {
            let label = find_status_text(
                view.find.status.matches_found,
                view.find.status.done,
                view.find.current_rank(),
                pending,
            );
            ui.label(label);
        }
        toggle_chip(
            ui,
            &mut view.filter.open,
            "Filter",
            "Filter panel",
            "Filter panel: show only matching lines (Ctrl+Shift+F)",
        );
        if icon_button(ui, Icon::Close, "Close find bar", "Close (Esc)").clicked() {
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

/// What a filter panel offers; the single-file and the merged tab differ.
#[derive(Debug, Clone, Copy)]
pub struct FilterPanelOptions {
    /// The profile has hide rules: offer "Show hidden lines".
    pub has_hide: bool,
    /// Column queries (the `Q` toggle) are available.
    pub queries: bool,
    /// Context lines are available.
    pub context: bool,
    /// The running job: passing lines found so far and whether it is done.
    pub summary: Option<(u64, bool)>,
}

/// Draws the filter panel of a single-file tab.
pub fn filter_panel(ui: &mut Ui, tab_id: u64, view: &mut DocView, colors: &Colors, now: Instant) {
    let opts = FilterPanelOptions {
        has_hide: view.hl.borrow().hide.is_some(),
        queries: true,
        context: true,
        summary: view
            .filter
            .has_job()
            .then(|| (view.filter.set.len() as u64, view.filter.status.done)),
    };
    if let Some(enabled) = filter_panel_ui(ui, tab_id, &mut view.filter, &opts, colors, now) {
        view.set_filter_view(enabled);
    }
}

/// Draws the filter panel over `filter`. Returns `Some(enabled)` when the
/// user flipped "Show only matching lines" (the caller switches the view and
/// stores the flag).
pub fn filter_panel_ui(
    ui: &mut Ui,
    tab_id: u64,
    filter: &mut FilterState,
    opts: &FilterPanelOptions,
    colors: &Colors,
    now: Instant,
) -> Option<bool> {
    let mut switch = None;
    ui.horizontal(|ui| {
        ui.strong("Filters");
        let mut enabled = filter.enabled;
        if ui
            .checkbox(&mut enabled, "Show only matching lines")
            .on_hover_text("Switch between the filtered and the full view; your position is kept")
            .changed()
        {
            switch = Some(enabled);
        }
        if opts.has_hide
            && ui
                .checkbox(&mut filter.show_hidden, "Show hidden lines")
                .on_hover_text("Lines folded away by the profile's hide rules")
                .changed()
        {
            filter.changed();
        }
        if let Some((n, done)) = opts.summary {
            if !done {
                ui.spinner();
            }
            ui.label(format!(
                "{} lines{}",
                fmt_count(n),
                if done { "" } else { " so far" }
            ));
        }
        if ui.button("Add").clicked() {
            filter.entries.push(FilterEntry::default());
            filter.focus_last = true;
        }
        if ui.button("Clear").clicked() {
            filter.entries.clear();
            filter.changed();
        }
        if icon_button(ui, Icon::Close, "Close filter panel", "Close the panel").clicked() {
            filter.open = false;
        }
    });
    let mut remove = None;
    let problems = filter.problems.clone();
    let mut typed = false;
    let mut toggled = false;
    let focus_last = std::mem::take(&mut filter.focus_last);
    let last = filter.entries.len().saturating_sub(1);
    for (i, e) in filter.entries.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let n = i + 1;
            let enable = ui.checkbox(&mut e.enabled, "");
            enable.widget_info(|| {
                WidgetInfo::selected(
                    WidgetType::Checkbox,
                    true,
                    e.enabled,
                    format!("Filter {n} enabled"),
                )
            });
            toggled |= enable.on_hover_text("Turn this filter on or off").changed();
            let label = if e.include { "Include" } else { "Exclude" };
            let mode = egui::ComboBox::from_id_salt(("filter-mode", tab_id, i))
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
            a11y_label(&mode.response, &format!("Filter {n} mode"));
            mode.response
                .on_hover_text("Include: keep matching lines. Exclude: drop them.");
            let bad_span = problems
                .iter()
                .find(|(idx, _)| *idx == i)
                .and_then(|(_, p)| p.span.clone())
                .filter(|_| e.query);
            let err_bg = colors.resolve(&oxtail_highlight::ColorRef::subtle(
                oxtail_highlight::SemanticColor::Error,
            ));
            let mut layouter = |ui: &Ui, text: &dyn egui::TextBuffer, _wrap: f32| {
                query_layout(ui, text.as_str(), bad_span.as_ref(), err_bg)
            };
            let hint = if e.query {
                "column query, e.g. level:ERROR status>=500"
            } else {
                "text or regex"
            };
            let mut edit = TextEdit::singleline(&mut e.text)
                .id(Id::new(("filter-text", tab_id, i)))
                .hint_text(hint)
                .desired_width(260.0);
            if e.query {
                edit = edit.layouter(&mut layouter);
            }
            let resp = edit.show(ui).response;
            a11y_label(&resp, &format!("Filter {n} text"));
            if focus_last && i == last {
                resp.request_focus();
            }
            typed |= resp.changed();
            if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                toggled = true;
            }
            if opts.queries {
                toggled |= toggle_chip(
                    ui,
                    &mut e.query,
                    "Query",
                    &format!("Filter {n}: column query"),
                    "Column query: level:ERROR  status>=500  duration>250ms  msg~\"timeout\"  -path:/health",
                )
                .changed();
            }
            ui.add_enabled_ui(!e.query, |ui| {
                toggled |= toggle_chip(
                    ui,
                    &mut e.regex,
                    ".*",
                    &format!("Filter {n}: regular expression"),
                    "Regular expression: on treats the text as a pattern",
                )
                .changed();
                toggled |= case_chip(ui, &mut e.case, &format!("Filter {n}"));
                toggled |= toggle_chip(
                    ui,
                    &mut e.whole_word,
                    "W",
                    &format!("Filter {n}: whole word"),
                    "Whole word: only match complete words",
                )
                .changed();
            });
            if icon_button(ui, Icon::Close, &format!("Remove filter {n}"), "Remove this filter")
                .clicked()
            {
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
        filter.entries.remove(i);
        toggled = true;
    }
    if !opts.context {
        ui.label(RichText::new("Context lines are not available in merged views").weak());
    } else {
        ui.horizontal(|ui| {
            ui.label("Context");
            let mut b = filter.before;
            let mut a = filter.after;
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
                filter.before = b;
                filter.after = a;
                toggled = true;
            }
            ui.label(RichText::new("context lines are dimmed").weak());
        });
    }
    if toggled {
        filter.changed();
    } else if typed {
        filter.edited(now);
    }
    switch
}

/// Lays out a query's text with the problem span marked (red background).
fn query_layout(
    ui: &Ui,
    text: &str,
    bad: Option<&std::ops::Range<usize>>,
    err_bg: Color32,
) -> std::sync::Arc<egui::Galley> {
    use egui::text::{LayoutJob, TextFormat};
    let font = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().text_color();
    let plain = TextFormat::simple(font.clone(), color);
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    match bad.and_then(|r| {
        let ok = r.start < r.end
            && r.end <= text.len()
            && text.is_char_boundary(r.start)
            && text.is_char_boundary(r.end);
        ok.then(|| r.clone())
    }) {
        Some(r) => {
            job.append(&text[..r.start], 0.0, plain.clone());
            let mut bad_fmt = plain.clone();
            bad_fmt.background = err_bg;
            bad_fmt.underline = egui::Stroke::new(1.0, Color32::LIGHT_RED);
            job.append(&text[r.clone()], 0.0, bad_fmt);
            job.append(&text[r.end..], 0.0, plain);
        }
        None => job.append(text, 0.0, plain),
    }
    ui.fonts_mut(|f| f.layout_job(job))
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
    /// Wrap (or stop wrapping) long lines: goes through the same path as
    /// Alt+Z, so the saved default follows.
    SetWrap(bool),
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
/// `wrap_key` is the current shortcut of the wrap action, for the tooltip.
pub fn status_bar(
    ui: &mut Ui,
    view: &mut DocView,
    profiles: &ProfileSet,
    wrap_key: Option<&str>,
    colors: &Colors,
) -> StatusAction {
    let mut action = StatusAction::None;
    let snap = view.snapshot.clone();
    ui.horizontal(|ui| {
        // Follow state: a filled dot while following, pause bars otherwise.
        if follow_chip(ui, view.follow, colors).clicked() {
            view.toggle_follow();
        }
        // Word wrap (the same action as the shortcut and the palette).
        let wrap_tip = match wrap_key {
            Some(k) => format!("Wrap long lines ({k})"),
            None => "Wrap long lines".to_string(),
        };
        let mut wrap = view.wrap;
        if toggle_chip(ui, &mut wrap, "Wrap", "Wrap long lines", &wrap_tip).changed() {
            action = StatusAction::SetWrap(wrap);
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
                let f = if fraction.is_finite() {
                    fraction.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let bar = ui.add(
                    egui::ProgressBar::new(f as f32)
                        .desired_width(90.0)
                        .text(format!("Indexing {:.0}%", f * 100.0)),
                );
                bar.on_hover_text(
                    "Building the line index: line numbers are approximate until it is done",
                );
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

        // Columns: the format in use, and the table switch.
        if view.st.parser.is_some() {
            let mut table = view.table_active();
            if ui
                .toggle_value(&mut table, "Columns")
                .on_hover_text(format!(
                    "{}\nShow this file as a table of columns",
                    view.st.name
                ))
                .changed()
            {
                view.set_table(table);
            }
        } else if view.st.is_deciding() {
            ui.label(RichText::new("Looking for columns\u{2026}").weak());
        }

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
    fn the_find_count_says_searching_until_the_search_has_run() {
        // A restart waiting for the typing pause: nothing is counted yet, and
        // "No matches" (what a finished, empty search says) would be wrong.
        assert_eq!(find_status_text(0, true, None, true), "Searching\u{2026}");
        assert_eq!(
            find_status_text(12, true, Some(3), true),
            "Searching\u{2026}"
        );
        // A running search that has found nothing yet.
        assert_eq!(find_status_text(0, false, None, false), "Searching\u{2026}");
        // Settled.
        assert_eq!(find_status_text(0, true, None, false), "No matches");
        assert_eq!(find_status_text(80, true, Some(73), false), "73 of 80");
        assert_eq!(
            find_status_text(80, false, None, false),
            "\u{2265} 80 matches"
        );
    }

    #[test]
    fn every_listed_encoding_resolves() {
        for (label, name) in ENCODINGS {
            assert!(TextEncoding::from_label(name).is_ok(), "{label}: {name}");
        }
    }
}
