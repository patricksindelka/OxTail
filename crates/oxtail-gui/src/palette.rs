//! The command palette (Ctrl+Shift+P): every command in one searchable list.
//!
//! * [`fuzzy`] and [`rank`] are the matcher: plain functions, unit tested.
//! * [`PaletteState`] is what the palette remembers (query, selection, the
//!   commands used last).
//! * [`show`] draws the overlay and returns the item the user chose.
//! * `OxTailApp::palette_items` builds the list from the action registry
//!   plus a few dynamic entries (recent files, profiles, encodings), and
//!   `OxTailApp::run_palette_item` runs one through the same code path as the
//!   menus ([`OxTailApp::perform`]).
//!
//! It is keyboard-only usable: type to filter, Up/Down (or Tab/Shift+Tab,
//! PageUp/PageDown) to move, Enter to run, Esc to close.

use std::path::PathBuf;

use egui::{
    Align2, Color32, Context, FontId, Id, Key, Modifiers, Order, Sense, TextEdit, TextFormat,
    epaint::text::LayoutJob, vec2,
};
use oxtail_core::{EncodingChoice, TextEncoding};

use crate::app::OxTailApp;
use crate::keymap::Action;
use crate::panels::ENCODINGS;
use crate::request::OpenRequest;

/// How many recently used commands float to the top.
pub const RECENT_KEPT: usize = 5;
/// Recent files offered as palette entries.
const RECENT_FILES_SHOWN: usize = 10;
/// Width of the shortcut and category columns of a palette row.
const SHORTCUT_COL: f32 = 112.0;
const CATEGORY_COL: f32 = 74.0;

// ------------------------------------------------------------------ matcher

const CONSECUTIVE_BONUS: i32 = 10;
const WORD_START_BONUS: i32 = 8;
const TEXT_START_BONUS: i32 = 4;
const GAP_PENALTY: i32 = 1;
const NO_SCORE: i32 = i32::MIN / 2;
/// A match may span at most this many characters per query character, plus
/// some slack, unless it is an acronym.
const MAX_SPAN_PER_CHAR: usize = 3;
const MAX_SPAN_SLACK: usize = 6;
/// Longest text and token considered (longer input is cut).
const MAX_TEXT_CHARS: usize = 200;
const MAX_TOKEN_CHARS: usize = 48;
const MAX_TOKENS: usize = 6;

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn is_word_start(t: &[char], j: usize) -> bool {
    j == 0 || !t[j - 1].is_alphanumeric() || (t[j - 1].is_lowercase() && t[j].is_uppercase())
}

/// Best subsequence alignment of one token in `text`: the score and the
/// matched character positions. `None` if the token is not a subsequence.
fn match_token(token: &[char], text: &[char]) -> Option<(i32, Vec<usize>)> {
    let (m, n) = (token.len(), text.len());
    if m == 0 {
        return Some((0, Vec::new()));
    }
    if m > n {
        return None;
    }
    let tl: Vec<char> = text.iter().map(|c| lower(*c)).collect();
    let ql: Vec<char> = token.iter().map(|c| lower(*c)).collect();
    let base = |j: usize, i: usize| -> i32 {
        let mut s = 1;
        if text[j] == token[i] {
            s += 1;
        }
        if is_word_start(text, j) {
            s += WORD_START_BONUS;
        }
        if j == 0 {
            s += TEXT_START_BONUS;
        }
        s
    };
    let mut prev = vec![NO_SCORE; n];
    let mut parents = vec![vec![usize::MAX; n]; m];
    for (j, p) in prev.iter_mut().enumerate() {
        if tl[j] == ql[0] {
            *p = base(j, 0);
        }
    }
    for i in 1..m {
        let mut cur = vec![NO_SCORE; n];
        // Best `prev[k] + GAP * k` over k <= j - 2, and the k that gave it.
        let mut run = (NO_SCORE, usize::MAX);
        for j in 1..n {
            if j >= 2 && prev[j - 2] > NO_SCORE {
                let v = prev[j - 2] + GAP_PENALTY * (j as i32 - 2);
                if v > run.0 {
                    run = (v, j - 2);
                }
            }
            if tl[j] != ql[i] {
                continue;
            }
            let mut best = (NO_SCORE, usize::MAX);
            if prev[j - 1] > NO_SCORE {
                best = (prev[j - 1] + CONSECUTIVE_BONUS, j - 1);
            }
            if run.0 > NO_SCORE {
                let v = run.0 - GAP_PENALTY * (j as i32 - 1);
                if v > best.0 {
                    best = (v, run.1);
                }
            }
            if best.0 > NO_SCORE {
                cur[j] = best.0 + base(j, i);
                parents[i][j] = best.1;
            }
        }
        prev = cur;
    }
    let (end, &score) = prev.iter().enumerate().max_by_key(|(j, s)| (**s, n - *j))?;
    if score <= NO_SCORE {
        return None;
    }
    let mut positions = vec![0; m];
    let mut j = end;
    for i in (0..m).rev() {
        positions[i] = j;
        if i > 0 {
            j = parents[i][j];
        }
    }
    // Reject matches scattered over the whole text ("wrap" in "Windows
    // (Central European)"), unless they spell an acronym (every character
    // starts a word).
    let span = positions[m - 1] - positions[0] + 1;
    let acronym = positions.iter().all(|&p| is_word_start(text, p));
    if span > MAX_SPAN_PER_CHAR * m + MAX_SPAN_SLACK && !acronym {
        return None;
    }
    Some((score, positions))
}

/// Fuzzy-matches `query` against `text`: case-insensitive, every character of
/// a query word must appear in order (words are separated by spaces and may
/// match anywhere). Scores reward matches at the start of words and of the
/// text and consecutive characters, and prefer shorter texts. Returns the
/// score (higher is better) and the matched character indices for
/// highlighting; `None` when it does not match.
pub fn fuzzy(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let text: Vec<char> = text.chars().take(MAX_TEXT_CHARS).collect();
    let mut total = 0;
    let mut positions: Vec<usize> = Vec::new();
    let mut any = false;
    for word in query.split_whitespace().take(MAX_TOKENS) {
        any = true;
        let token: Vec<char> = word.chars().take(MAX_TOKEN_CHARS).collect();
        let (s, p) = match_token(&token, &text)?;
        total += s;
        positions.extend(p);
    }
    if !any {
        return Some((0, Vec::new()));
    }
    positions.sort_unstable();
    positions.dedup();
    // Shorter texts first among equals.
    Some((total - (text.len() as i32) / 4, positions))
}

/// What choosing an entry does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Run an action.
    Action(Action),
    /// Open a recent file.
    Recent(PathBuf),
    /// Use a profile in the active tab (`None`: the default rules).
    Profile(Option<String>),
    /// Set the active tab's encoding (`None`: auto-detect).
    Encoding(Option<&'static str>),
}

/// One line of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Stable id (`view.wrap`, `recent:/path`); recently used ones are
    /// remembered by it.
    pub id: String,
    /// The text.
    pub label: String,
    /// The category chip.
    pub category: &'static str,
    /// The shortcut, if it has one.
    pub shortcut: Option<String>,
    /// Whether it applies to the current state.
    pub enabled: bool,
    /// For toggles: whether it is on.
    pub checked: Option<bool>,
    /// What it does.
    pub target: Target,
}

/// A matching item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Index into the item list.
    pub index: usize,
    /// Match quality (higher is better).
    pub score: i32,
    /// Matched character indices in the label (empty when the match was in
    /// the category or id).
    pub positions: Vec<usize>,
}

/// Filters and orders `items` for `query`: applicable commands before
/// unavailable ones, then best match first, recently used commands lifted.
/// An empty query lists everything, recent commands first.
pub fn rank(items: &[Item], query: &str, recent: &[String]) -> Vec<Hit> {
    let mut hits: Vec<Hit> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let matched = if query.trim().is_empty() {
            Some((0, Vec::new()))
        } else if let Some(m) = fuzzy(query, &item.label) {
            Some(m)
        } else if let Some((s, _)) = fuzzy(query, &format!("{} {}", item.category, item.label)) {
            Some((s - 20, Vec::new()))
        } else {
            fuzzy(query, &item.id).map(|(s, _)| (s - 30, Vec::new()))
        };
        let Some((mut score, positions)) = matched else {
            continue;
        };
        if let Some(r) = recent.iter().position(|id| *id == item.id) {
            let r = r as i32;
            score += if query.trim().is_empty() {
                1000 - r * 10
            } else {
                6 - r
            };
        }
        hits.push(Hit {
            index,
            score,
            positions,
        });
    }
    hits.sort_by(|a, b| {
        items[b.index]
            .enabled
            .cmp(&items[a.index].enabled)
            .then(b.score.cmp(&a.score))
            .then(a.index.cmp(&b.index))
    });
    hits
}

/// Moves the selection by `delta` within `count` selectable rows, wrapping
/// around at both ends.
pub fn step_selection(selected: usize, count: usize, delta: isize) -> usize {
    if count == 0 {
        return 0;
    }
    (selected as isize + delta).rem_euclid(count as isize) as usize
}

// -------------------------------------------------------------------- state

/// What the palette remembers.
#[derive(Debug, Default)]
pub struct PaletteState {
    /// Whether the overlay is showing.
    pub open: bool,
    /// The text typed.
    pub query: String,
    /// The highlighted row (among the selectable ones).
    pub selected: usize,
    /// Ids of the commands used last, most recent first.
    pub recent: Vec<String>,
    scroll_to_selected: bool,
}

impl PaletteState {
    /// Opens the palette with an empty query.
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.scroll_to_selected = true;
    }

    /// Closes the palette.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Notes that the item with `id` was run.
    pub fn record_use(&mut self, id: &str) {
        self.recent.retain(|r| r != id);
        self.recent.insert(0, id.to_string());
        self.recent.truncate(RECENT_KEPT);
    }
}

// ----------------------------------------------------------------------- ui

/// The keys the palette handles itself.
#[derive(Debug, Default, Clone, Copy)]
struct Keys {
    up: bool,
    down: bool,
    page_up: bool,
    page_down: bool,
    enter: bool,
    escape: bool,
}

fn take_keys(ctx: &Context) -> Keys {
    ctx.input_mut(|i| {
        let n = Modifiers::NONE;
        Keys {
            up: i.consume_key(n, Key::ArrowUp)
                | i.consume_key(Modifiers::SHIFT, Key::Tab)
                | i.consume_key(Modifiers::CTRL, Key::P)
                | i.consume_key(Modifiers::CTRL, Key::K),
            down: i.consume_key(n, Key::ArrowDown)
                | i.consume_key(n, Key::Tab)
                | i.consume_key(Modifiers::CTRL, Key::N)
                | i.consume_key(Modifiers::CTRL, Key::J),
            page_up: i.consume_key(n, Key::PageUp),
            page_down: i.consume_key(n, Key::PageDown),
            enter: i.consume_key(n, Key::Enter),
            escape: i.consume_key(n, Key::Escape),
        }
    })
}

fn label_job(
    label: &str,
    positions: &[usize],
    font: FontId,
    color: Color32,
    accent: Color32,
    max_width: f32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = max_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('\u{2026}');
    let mut buf = String::new();
    let mut buf_hit = false;
    let flush = |job: &mut LayoutJob, buf: &mut String, hit: bool| {
        if buf.is_empty() {
            return;
        }
        let fmt = TextFormat {
            font_id: font.clone(),
            color: if hit { accent } else { color },
            underline: if hit {
                egui::Stroke::new(1.0, accent)
            } else {
                egui::Stroke::NONE
            },
            ..TextFormat::default()
        };
        job.append(buf, 0.0, fmt);
        buf.clear();
    };
    for (i, c) in label.chars().enumerate() {
        let hit = positions.binary_search(&i).is_ok();
        if hit != buf_hit {
            flush(&mut job, &mut buf, buf_hit);
            buf_hit = hit;
        }
        buf.push(c);
    }
    flush(&mut job, &mut buf, buf_hit);
    job
}

/// Draws the palette if it is open. Returns the index (into `items`) of the
/// entry the user chose with Enter or a click; the palette closes itself
/// then, and on Esc or a click outside.
pub fn show(ctx: &Context, state: &mut PaletteState, items: &[Item]) -> Option<usize> {
    if !state.open {
        return None;
    }
    let keys = take_keys(ctx);
    if keys.escape {
        state.close();
        return None;
    }
    let text_id = Id::new("palette-query");
    let width = (ctx.content_rect().width() - 32.0).clamp(260.0, 640.0);
    let max_list = (ctx.content_rect().height() * 0.6).clamp(120.0, 420.0);
    let mut chosen: Option<usize> = None;
    let mut click_index: Option<usize> = None;
    let mut hits: Vec<Hit> = Vec::new();
    let mut selectable = 0;
    let area = egui::Area::new(Id::new("command-palette"))
        .order(Order::Foreground)
        .anchor(Align2::CENTER_TOP, [0.0, 56.0])
        .constrain(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(width);
                let before = state.query.clone();
                let edit = ui.add(
                    TextEdit::singleline(&mut state.query)
                        .id(text_id)
                        .hint_text("Type a command\u{2026}")
                        .font(FontId::proportional(16.0))
                        .desired_width(f32::INFINITY)
                        .margin(vec2(6.0, 6.0)),
                );
                edit.widget_info(|| {
                    egui::WidgetInfo::text_edit(
                        true,
                        state.query.clone(),
                        state.query.clone(),
                        "Command palette",
                    )
                });
                // Typing always goes to the query.
                ui.memory_mut(|m| m.request_focus(text_id));
                let changed = state.query != before;
                if changed {
                    state.selected = 0;
                    state.scroll_to_selected = true;
                }
                // Rank after the edit, so Enter acts on what is on screen.
                hits = rank(items, &state.query, &state.recent);
                selectable = hits.iter().take_while(|h| items[h.index].enabled).count();
                let page = 8;
                let mut moved = false;
                if keys.up {
                    state.selected = step_selection(state.selected, selectable, -1);
                    moved = true;
                }
                if keys.down {
                    state.selected = step_selection(state.selected, selectable, 1);
                    moved = true;
                }
                if keys.page_up {
                    state.selected = state.selected.saturating_sub(page);
                    moved = true;
                }
                if keys.page_down {
                    state.selected = (state.selected + page).min(selectable.saturating_sub(1));
                    moved = true;
                }
                state.selected = state.selected.min(selectable.saturating_sub(1));
                if moved {
                    state.scroll_to_selected = true;
                }
                if keys.enter
                    && let Some(h) = hits.get(state.selected).filter(|_| selectable > 0)
                {
                    chosen = Some(h.index);
                }
                ui.add_space(2.0);
                ui.separator();
                let row_h = ui.text_style_height(&egui::TextStyle::Body) + 10.0;
                let visuals = ui.visuals().clone();
                let text_color = visuals.text_color();
                let weak = visuals.weak_text_color();
                let accent = visuals.hyperlink_color;
                egui::ScrollArea::vertical()
                    .max_height(max_list)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if hits.is_empty() {
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new("No matching command").weak());
                            ui.add_space(8.0);
                        }
                        let wide = width > 420.0;
                        for (row, hit) in hits.iter().enumerate() {
                            let item = &items[hit.index];
                            let (rect, resp) = ui.allocate_exact_size(
                                vec2(ui.available_width(), row_h),
                                Sense::click(),
                            );
                            let selected = row == state.selected && item.enabled;
                            let disabled = !item.enabled;
                            resp.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    item.enabled,
                                    item.label.clone(),
                                )
                            });
                            if selected {
                                ui.painter().rect_filled(
                                    rect,
                                    visuals.widgets.active.corner_radius,
                                    visuals.selection.bg_fill,
                                );
                                if state.scroll_to_selected {
                                    ui.scroll_to_rect(rect, None);
                                    state.scroll_to_selected = false;
                                }
                            } else if resp.hovered() && item.enabled {
                                ui.painter().rect_filled(
                                    rect,
                                    visuals.widgets.hovered.corner_radius,
                                    visuals.widgets.hovered.weak_bg_fill,
                                );
                            }
                            if resp.clicked() && item.enabled {
                                click_index = Some(hit.index);
                            }
                            let fg = if selected {
                                visuals.selection.stroke.color
                            } else if disabled {
                                weak.gamma_multiply(0.7)
                            } else {
                                text_color
                            };
                            let accent = if selected { fg } else { accent };
                            let body = FontId::proportional(14.0);
                            let small = FontId::proportional(12.0);
                            let mono = FontId::monospace(12.0);
                            let painter = ui.painter();
                            let mid = rect.center().y;
                            // Right edge: a shortcut column, then a category column.
                            let mut right = rect.right() - 10.0;
                            if let Some(s) = &item.shortcut {
                                let g = painter.layout_no_wrap(
                                    s.clone(),
                                    mono.clone(),
                                    if selected { fg } else { weak },
                                );
                                painter.galley(
                                    egui::pos2(right - g.size().x, mid - g.size().y / 2.0),
                                    g,
                                    Color32::PLACEHOLDER,
                                );
                            }
                            right -= SHORTCUT_COL;
                            if wide {
                                let g = painter.layout_no_wrap(
                                    item.category.to_string(),
                                    small.clone(),
                                    if selected { fg } else { weak },
                                );
                                painter.galley(
                                    egui::pos2(right - g.size().x, mid - g.size().y / 2.0),
                                    g,
                                    Color32::PLACEHOLDER,
                                );
                                right -= CATEGORY_COL;
                            }
                            // Left edge: a check mark for switched-on toggles.
                            let left = rect.left() + 28.0;
                            if item.checked == Some(true) {
                                let c = egui::pos2(rect.left() + 14.0, mid);
                                let stroke =
                                    egui::Stroke::new(1.6, if selected { fg } else { accent });
                                painter.line_segment(
                                    [c + vec2(-4.0, 0.5), c + vec2(-1.0, 3.5)],
                                    stroke,
                                );
                                painter.line_segment(
                                    [c + vec2(-1.0, 3.5), c + vec2(4.5, -3.5)],
                                    stroke,
                                );
                            }
                            let job = label_job(
                                &item.label,
                                &hit.positions,
                                body,
                                fg,
                                accent,
                                (right - left).max(40.0),
                            );
                            let g = painter.layout_job(job);
                            painter.galley(
                                egui::pos2(left, mid - g.size().y / 2.0),
                                g,
                                Color32::PLACEHOLDER,
                            );
                        }
                    });
                ui.add_space(2.0);
                ui.separator();
                ui.label(
                    egui::RichText::new(
                        "Up/Down to move, Enter to run, Esc to close, type to filter",
                    )
                    .small()
                    .weak(),
                );
            });
        });
    if let Some(i) = click_index {
        chosen = Some(i);
    }
    // A click outside closes it.
    let outside = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer
                .interact_pos()
                .is_some_and(|p| !area.response.rect.contains(p))
    });
    if chosen.is_some() || outside {
        state.close();
    }
    chosen
}

// --------------------------------------------------------------- app glue

impl OxTailApp {
    /// Opens or closes the palette. Opening refreshes what the palette
    /// shows about the system (integration state) on a worker.
    pub(crate) fn toggle_palette(&mut self) {
        if self.palette.open {
            self.palette.close();
        } else {
            self.palette.open();
            let wake = self.waker();
            self.sys.query_integration(&self.data_dir, wake, false);
        }
    }

    /// Every entry the palette can show now: all actions plus recent files,
    /// profiles and encodings.
    pub fn palette_items(&self) -> Vec<Item> {
        let mut items: Vec<Item> = Vec::new();
        for &a in Action::ALL {
            if !a.in_palette() {
                continue;
            }
            items.push(Item {
                id: a.id().to_string(),
                label: a.label().to_string(),
                category: a.category(),
                shortcut: self.keymap.shortcut_text(a),
                enabled: self.action_enabled(a),
                checked: self.action_checked(a),
                target: Target::Action(a),
            });
        }
        for p in self
            .session
            .recent_files
            .iter()
            .take(RECENT_FILES_SHOWN.min(self.settings.recent_files_limit.max(1)))
        {
            items.push(Item {
                id: format!("recent:{}", p.display()),
                label: format!(
                    "Open recent: {}",
                    crate::util::shorten(&p.display().to_string(), 80)
                ),
                category: "File",
                shortcut: None,
                enabled: true,
                checked: None,
                target: Target::Recent(p.clone()),
            });
        }
        let view = self.active_view().is_some();
        let current_profile = self
            .active_view()
            .and_then(|v| v.hl.borrow().profile.clone());
        items.push(Item {
            id: "profile:".into(),
            label: "Profile: default rules".into(),
            category: "Highlight",
            shortcut: None,
            enabled: view,
            checked: view.then_some(current_profile.is_none()),
            target: Target::Profile(None),
        });
        for name in self.profile_names() {
            items.push(Item {
                id: format!("profile:{name}"),
                label: format!("Profile: {name}"),
                category: "Highlight",
                shortcut: None,
                enabled: view,
                checked: view.then_some(current_profile.as_deref() == Some(name.as_str())),
                target: Target::Profile(Some(name)),
            });
        }
        items.push(Item {
            id: "encoding:auto".into(),
            label: "Encoding: auto-detect".into(),
            category: "View",
            shortcut: None,
            enabled: view,
            checked: None,
            target: Target::Encoding(None),
        });
        for (label, name) in ENCODINGS {
            items.push(Item {
                id: format!("encoding:{name}"),
                label: format!("Encoding: {label}"),
                category: "View",
                shortcut: None,
                enabled: view,
                checked: None,
                target: Target::Encoding(Some(name)),
            });
        }
        items
    }

    /// Runs the chosen palette entry (the same code the menus run).
    pub fn run_palette_item(&mut self, item: &Item, ctx: &Context) {
        self.palette.record_use(&item.id);
        match &item.target {
            Target::Action(a) => self.perform(*a, ctx),
            Target::Recent(p) => self.open_request(OpenRequest::file(p.clone())),
            Target::Profile(p) => self.set_profile(p.clone()),
            Target::Encoding(name) => {
                let choice = match name {
                    None => Some(EncodingChoice::Auto),
                    Some(n) => TextEncoding::from_label(n).ok().map(EncodingChoice::Fixed),
                };
                if let (Some(choice), Some(v)) = (choice, self.active_view()) {
                    v.set_encoding(choice);
                }
            }
        }
        ctx.request_repaint();
    }

    /// Draws the palette and runs what was chosen.
    pub(crate) fn palette_ui(&mut self, ctx: &Context) {
        if !self.palette.open {
            return;
        }
        let items = self.palette_items();
        let mut state = std::mem::take(&mut self.palette);
        let chosen = show(ctx, &mut state, &items);
        self.palette = state;
        if let Some(i) = chosen {
            self.run_palette_item(&items[i], ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, label: &str, category: &'static str, enabled: bool) -> Item {
        Item {
            id: id.into(),
            label: label.into(),
            category,
            shortcut: None,
            enabled,
            checked: None,
            target: Target::Action(Action::Open),
        }
    }

    fn score(q: &str, t: &str) -> i32 {
        fuzzy(q, t).unwrap_or_else(|| panic!("{q} !~ {t}")).0
    }

    #[test]
    fn subsequences_match_case_insensitively() {
        assert!(fuzzy("wrp", "Toggle line wrapping").is_some());
        assert!(fuzzy("WRAP", "Toggle line wrapping").is_some());
        assert!(fuzzy("tlw", "Toggle line wrapping").is_some());
        assert!(fuzzy("xyz", "Toggle line wrapping").is_none());
        assert!(fuzzy("gnippart", "Toggle line wrapping").is_none());
        // Order matters.
        assert!(fuzzy("pw", "wrap").is_none());
        // Too long a query cannot match a short text.
        assert!(fuzzy("wrapping", "wrap").is_none());
        // Empty query matches everything.
        assert_eq!(fuzzy("", "anything"), Some((0, vec![])));
        assert_eq!(fuzzy("   ", "anything"), Some((0, vec![])));
        // Unicode does not panic or misalign.
        assert!(fuzzy("\u{e9}", "Caf\u{c9}").is_some());
        assert_eq!(fuzzy("\u{e9}", "Caf\u{c9}").unwrap().1, vec![3]);
    }

    #[test]
    fn positions_point_at_the_best_alignment() {
        // "wrap" is found as the word, not scattered over the text.
        let (_, p) = fuzzy("wrap", "Toggle line wrapping").unwrap();
        assert_eq!(p, vec![12, 13, 14, 15]);
        let (_, p) = fuzzy("gt", "Go to time").unwrap();
        assert_eq!(p, vec![0, 3]);
    }

    #[test]
    fn word_starts_and_consecutive_characters_score_higher() {
        // Word starts beat scattered mid-word letters.
        assert!(score("gt", "Go time") > score("gt", "Big text"));
        // Consecutive beats spread out.
        assert!(score("fol", "Toggle following") > score("fol", "Find or load"));
        // Start of the text beats the middle.
        assert!(score("op", "Open file") > score("op", "Stop file"));
        // Shorter wins when all else is equal.
        assert!(score("find", "Find") > score("find", "Find in a much longer sentence"));
    }

    #[test]
    fn scattered_matches_are_rejected_but_acronyms_are_not() {
        assert!(fuzzy("wrap", "Windows-1250 (Central European)").is_none());
        assert!(fuzzy("wrap", "View Relative time: since the previous line").is_none());
        // Word-start acronyms may be spread out.
        assert!(fuzzy("cf", "Clear recent files").is_some());
        assert!(fuzzy("otnt", "Open clipboard text in a new tab").is_some());
        // Short spans are fine.
        assert!(fuzzy("tlw", "Toggle line wrapping").is_some());
    }

    #[test]
    fn several_words_all_have_to_match() {
        assert!(fuzzy("view wrap", "View wrap lines").is_some());
        assert!(fuzzy("wrap view", "View wrap lines").is_some());
        assert!(fuzzy("view zoom", "View wrap lines").is_none());
    }

    #[test]
    fn rank_prefers_the_label_and_falls_back_to_category_and_id() {
        let items = vec![
            item("view.wrap", "Toggle line wrapping", "View", true),
            item("file.open", "Open file", "File", true),
            item("theme.dark", "Theme: dark", "Theme", true),
        ];
        let h = rank(&items, "wrap", &[]);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].index, 0);
        assert!(!h[0].positions.is_empty());
        // Category words find their commands (no highlight in the label).
        let h = rank(&items, "file open", &[]);
        assert_eq!(h[0].index, 1);
        // The id works too.
        let h = rank(&items, "theme.dark", &[]);
        assert_eq!(h[0].index, 2);
        assert!(rank(&items, "zzzz", &[]).is_empty());
    }

    #[test]
    fn unavailable_commands_come_last_and_the_best_match_first() {
        let items = vec![
            item("a", "Find in tabs", "Search", false),
            item("b", "Toggle bookmark: find later", "Bookmark", true),
            item("c", "Find", "Search", true),
        ];
        let h = rank(&items, "find", &[]);
        let order: Vec<usize> = h.iter().map(|h| h.index).collect();
        assert_eq!(order, vec![2, 1, 0]);
    }

    #[test]
    fn recently_used_commands_lead_an_empty_query_and_get_a_lift() {
        let items = vec![
            item("a", "Alpha", "X", true),
            item("b", "Beta", "X", true),
            item("c", "Gamma", "X", true),
        ];
        let recent = vec!["c".to_string(), "b".to_string()];
        let order: Vec<usize> = rank(&items, "", &recent).iter().map(|h| h.index).collect();
        assert_eq!(order, vec![2, 1, 0]);
        // With a query the recent one wins ties.
        let items = vec![item("a", "Same", "X", true), item("b", "Same", "X", true)];
        let order: Vec<usize> = rank(&items, "same", &["b".to_string()])
            .iter()
            .map(|h| h.index)
            .collect();
        assert_eq!(order, vec![1, 0]);
        // But a much better match still beats a recent weak one.
        let items = vec![
            item("a", "Toggle the wrapping of long lines", "X", true),
            item("b", "Wrap", "X", true),
        ];
        let order: Vec<usize> = rank(&items, "wrap", &["a".to_string()])
            .iter()
            .map(|h| h.index)
            .collect();
        assert_eq!(order[0], 1);
    }

    #[test]
    fn recent_list_keeps_the_last_few_without_duplicates() {
        let mut s = PaletteState::default();
        for id in ["a", "b", "c", "a", "d", "e", "f"] {
            s.record_use(id);
        }
        assert_eq!(s.recent, vec!["f", "e", "d", "a", "c"]);
        assert_eq!(s.recent.len(), RECENT_KEPT);
    }

    #[test]
    fn selection_wraps_around() {
        assert_eq!(step_selection(0, 5, -1), 4);
        assert_eq!(step_selection(4, 5, 1), 0);
        assert_eq!(step_selection(2, 5, 1), 3);
        assert_eq!(step_selection(0, 0, 1), 0);
    }

    #[test]
    fn hostile_input_does_not_panic() {
        let long = "x".repeat(10_000);
        let _ = fuzzy(&long, &long);
        let _ = fuzzy("a b c d e f g h i j", "abcdefghij");
        let _ = fuzzy("\u{0}\u{1F600}", "\u{1F600}");
        let items = vec![item("a", &long, "X", true)];
        let _ = rank(&items, &long, &[]);
    }

    #[test]
    fn every_action_is_listed_once_with_its_shortcut() {
        let app = crate::app::OxTailApp::from_init(crate::startup::AppInit {
            startup: crate::startup::load_startup(oxtail_config::DataDir::in_memory("t")),
            request: OpenRequest::default(),
            external: crate::startup::ExternalOpen::disconnected(),
        });
        let items = app.palette_items();
        let ids: std::collections::HashSet<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids.len(), items.len(), "duplicate ids");
        for &a in Action::ALL {
            assert_eq!(ids.contains(a.id()), a.in_palette(), "{}", a.id());
        }
        let wrap = items.iter().find(|i| i.id == "view.wrap").unwrap();
        assert_eq!(wrap.shortcut.as_deref(), Some("Alt+Z"));
        // Nothing is open: view commands are unavailable, global ones are not.
        assert!(!wrap.enabled);
        assert!(
            items
                .iter()
                .find(|i| i.id == "app.settings")
                .unwrap()
                .enabled
        );
    }
}
