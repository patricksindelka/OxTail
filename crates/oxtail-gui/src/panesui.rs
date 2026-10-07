//! Drawing the parts of the application that involve several tabs: the tab
//! strips (one per pane), the split layout with its draggable dividers, the
//! merged tab, the "Merge tabs" dialog and the search across tabs.

use std::time::Instant;

use egui::{
    Align, Align2, Color32, Context, CornerRadius, Id, Layout, Pos2, Rect, RichText, Sense, Ui,
    UiBuilder, vec2,
};
use oxtail_config::SplitDirection;

use crate::app::OxTailApp;
use crate::cross::CrossTarget;
use crate::logview::ViewEnv;
use crate::mergepaint::{self, badge_color};
use crate::mergeview::{MergedView, badge_letter};
use crate::panels::{
    FilterPanelOptions, Icon, a11y_label, a11y_role, case_chip, filter_panel_ui, follow_chip,
    icon_button, toggle_chip,
};
use crate::panes::{Edge, PaneId, edge_at, extent_of, ratio_at};
use crate::tab::{TabContent, drop_index, move_item};
use crate::util::{fmt_bytes, fmt_count};

/// Approximate height of a tab strip (its panel included), used to tell a
/// drop on a strip from a drop on a pane's body.
pub const STRIP_H: f32 = 34.0;

/// What a tab's context menu asked for.
enum TabAction {
    Split(SplitDirection),
    MergeWith(u64),
    Close(usize),
    CloseOthers(usize),
}

impl OxTailApp {
    // ------------------------------------------------------------- tab strip

    /// Draws the tab strip of `pane`.
    pub(crate) fn tabs_strip(&mut self, ui: &mut Ui, pane: PaneId) {
        let colors = self.colors.clone();
        let strip: Vec<usize> = self.tabs_in_pane(pane);
        let active = self.active_index_in_pane(pane);
        let mut select: Option<usize> = None;
        let mut close: Option<usize> = None;
        let mut actions: Vec<(usize, TabAction)> = Vec::new();
        let mut drag_started: Option<u64> = None;
        let mut stopped: Option<(usize, Option<Pos2>)> = None;
        let mut centers: Vec<f32> = Vec::new();
        egui::ScrollArea::horizontal()
            .id_salt(("tab-scroll", pane))
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    // The strip is a tab list for screen readers; the tabs
                    // below become its children.
                    ui.ctx().accesskit_node_builder(ui.unique_id(), |b| {
                        b.set_role(egui::accesskit::Role::TabList);
                        b.set_label("Open files".to_owned());
                    });
                    for &i in &strip {
                        let tab = &self.tabs[i];
                        let selected = Some(i) == active;
                        let font = egui::FontId::proportional(13.0);
                        let galley = ui.painter().layout_no_wrap(
                            tab.label(),
                            font,
                            if selected {
                                colors.text
                            } else {
                                colors.status_text
                            },
                        );
                        let close_w = 18.0;
                        let size = vec2(galley.size().x + 22.0 + close_w, 26.0);
                        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
                        centers.push(rect.center().x);
                        let tab_label = tab.label();
                        resp.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &tab_label)
                        });
                        a11y_role(&resp, egui::accesskit::Role::Tab);
                        ui.ctx().accesskit_node_builder(resp.id, |b| {
                            b.set_selected(selected);
                            if tab.merged().is_some() {
                                b.set_description("Merged view".to_owned());
                            } else if let Some(p) = &tab.path {
                                b.set_description(p.display().to_string());
                            }
                        });
                        if resp.has_focus() {
                            ui.painter().rect_stroke(
                                rect,
                                CornerRadius::same(4),
                                egui::Stroke::new(2.0, colors.accent),
                                egui::StrokeKind::Inside,
                            );
                        }
                        let bg = if selected {
                            colors.background
                        } else if resp.hovered() {
                            colors.current_line_bg
                        } else {
                            colors.gutter_bg
                        };
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius {
                                nw: 4,
                                ne: 4,
                                sw: 0,
                                se: 0,
                            },
                            bg,
                        );
                        if selected {
                            ui.painter().hline(
                                rect.x_range(),
                                rect.top() + 1.0,
                                egui::Stroke::new(2.0, colors.accent),
                            );
                        }
                        if tab.merged().is_some() {
                            // A small marker for merged tabs.
                            ui.painter().circle_filled(
                                egui::pos2(rect.left() + 6.0, rect.center().y),
                                2.5,
                                colors.accent,
                            );
                        }
                        let text_pos =
                            egui::pos2(rect.left() + 10.0, rect.center().y - galley.size().y * 0.5);
                        ui.painter().galley(text_pos, galley, colors.text);
                        // Close button: an x drawn with two lines.
                        let close_rect = Rect::from_center_size(
                            egui::pos2(rect.right() - close_w * 0.5 - 4.0, rect.center().y),
                            vec2(16.0, 16.0),
                        );
                        let close_resp =
                            ui.interact(close_rect, Id::new(("tab-close", tab.id)), Sense::click());
                        close_resp.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                true,
                                format!("Close {tab_label}"),
                            )
                        });
                        let c = if close_resp.hovered() {
                            colors.text
                        } else {
                            colors.gutter_text
                        };
                        if close_resp.hovered() {
                            ui.painter().rect_filled(
                                close_rect,
                                CornerRadius::same(3),
                                colors.selection_bg,
                            );
                        }
                        let m = 4.5;
                        let ctr = close_rect.center();
                        let stroke = egui::Stroke::new(1.3, c);
                        ui.painter()
                            .line_segment([ctr + vec2(-m, -m), ctr + vec2(m, m)], stroke);
                        ui.painter()
                            .line_segment([ctr + vec2(-m, m), ctr + vec2(m, -m)], stroke);
                        if close_resp.clicked() || resp.middle_clicked() {
                            close = Some(i);
                        } else if resp.clicked() {
                            select = Some(i);
                        }
                        if resp.drag_started() {
                            select = Some(i);
                            drag_started = Some(tab.id);
                        }
                        if resp.drag_stopped() {
                            stopped = Some((i, ui.input(|i| i.pointer.interact_pos())));
                        }
                        let tab_id = tab.id;
                        let has_path = tab.path.is_some();
                        let multi_tab = strip.len() > 1;
                        let resp = match &tab.path {
                            Some(p) => resp.on_hover_text(p.display().to_string()),
                            None => resp,
                        };
                        resp.context_menu(|ui| {
                            if ui
                                .add_enabled(has_path, egui::Button::new("Split right"))
                                .on_disabled_hover_text("This tab has no file to show twice")
                                .clicked()
                            {
                                actions.push((i, TabAction::Split(SplitDirection::Horizontal)));
                                ui.close();
                            }
                            if ui
                                .add_enabled(has_path, egui::Button::new("Split down"))
                                .on_disabled_hover_text("This tab has no file to show twice")
                                .clicked()
                            {
                                actions.push((i, TabAction::Split(SplitDirection::Vertical)));
                                ui.close();
                            }
                            if ui.button("Merge with\u{2026}").clicked() {
                                actions.push((i, TabAction::MergeWith(tab_id)));
                                ui.close();
                            }
                            ui.separator();
                            if ui.button("Close").clicked() {
                                actions.push((i, TabAction::Close(i)));
                                ui.close();
                            }
                            if ui
                                .add_enabled(multi_tab, egui::Button::new("Close other tabs"))
                                .clicked()
                            {
                                actions.push((i, TabAction::CloseOthers(i)));
                                ui.close();
                            }
                        });
                    }
                    let plus = ui
                        .add(egui::Button::new("+").frame(false))
                        .on_hover_text("Open a file (Ctrl+O)");
                    a11y_label(&plus, "Open a file");
                    if plus.clicked() {
                        self.file_dialog_requested = true;
                    }
                });
            });
        if let Some(id) = drag_started {
            self.dragging_tab = Some(id);
        }
        if let Some(i) = select {
            self.select_tab(i);
        }
        if let Some((i, p)) = stopped {
            self.finish_tab_drag(i, p, pane, &strip, &centers);
        }
        for (i, a) in actions {
            self.select_tab(i);
            match a {
                TabAction::Split(d) => self.split_active(d),
                TabAction::MergeWith(id) => {
                    self.windows.merge = Some(crate::appmerge::MergeDialog { selected: vec![id] });
                }
                TabAction::Close(i) => close = Some(i),
                TabAction::CloseOthers(keep) => {
                    let Some(keep_id) = self.tabs.get(keep).map(|t| t.id) else {
                        continue;
                    };
                    let others: Vec<u64> = strip
                        .iter()
                        .filter_map(|&j| self.tabs.get(j).map(|t| t.id))
                        .filter(|id| *id != keep_id)
                        .collect();
                    for id in others {
                        if let Some(j) = self.tabs.iter().position(|t| t.id == id) {
                            self.close_tab(j);
                        }
                    }
                }
            }
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
        if std::mem::take(&mut self.file_dialog_requested) {
            self.pick_files();
        }
    }

    /// A tab was released after a drag: reorder it in its strip, move it to
    /// another pane's strip, or split a pane at the edge it was dropped on.
    fn finish_tab_drag(
        &mut self,
        from: usize,
        pointer: Option<Pos2>,
        pane: PaneId,
        strip: &[usize],
        centers: &[f32],
    ) {
        self.dragging_tab = None;
        let Some(p) = pointer else { return };
        let Some(tab_id) = self.tabs.get(from).map(|t| t.id) else {
            return;
        };
        let target = self
            .pane_rects
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(id, r)| (*id, *r));
        match target {
            Some((target_pane, rect)) if self.pane_count() > 1 => {
                let in_strip = p.y < rect.top() + STRIP_H;
                if in_strip && target_pane == pane {
                    self.reorder_in_strip(from, p.x, strip, centers);
                } else if in_strip {
                    self.move_tab_to_pane(tab_id, target_pane);
                } else {
                    let body =
                        Rect::from_min_max(pos2f(rect.left(), rect.top() + STRIP_H), rect.max);
                    match edge_at(body, p) {
                        Some(edge) => self.move_tab_to_new_pane(tab_id, target_pane, edge),
                        None if target_pane != pane => self.move_tab_to_pane(tab_id, target_pane),
                        None => {}
                    }
                }
            }
            Some((target_pane, rect)) => {
                // A single pane: only its body counts (the strip is above it).
                if let Some(edge) = edge_at(rect, p) {
                    self.move_tab_to_new_pane(tab_id, target_pane, edge);
                }
            }
            None => self.reorder_in_strip(from, p.x, strip, centers),
        }
    }

    fn reorder_in_strip(&mut self, from: usize, x: f32, strip: &[usize], centers: &[f32]) {
        let to = drop_index(centers, x);
        // Map the position among the strip's tabs to the global list.
        let to_global = match strip.get(to) {
            Some(&g) => g,
            None => strip.last().map_or(0, |&g| g + 1),
        };
        let new_index = move_item(&mut self.tabs, from, to_global);
        if self.active == from {
            self.active = new_index;
        } else if from < self.active && new_index >= self.active {
            self.active -= 1;
        } else if from > self.active && new_index <= self.active {
            self.active += 1;
        }
    }

    /// While a tab is dragged: highlights the edge of the pane under the
    /// pointer that a drop would split.
    pub(crate) fn drop_overlay_for_tabs(&mut self, ctx: &Context) {
        if self.dragging_tab.is_none() {
            return;
        }
        if !ctx.input(|i| i.pointer.any_down()) {
            self.dragging_tab = None;
            return;
        }
        let Some(p) = ctx.input(|i| i.pointer.interact_pos()) else {
            return;
        };
        let Some((_, rect)) = self.pane_rects.iter().find(|(_, r)| r.contains(p)) else {
            return;
        };
        let body = if self.pane_count() > 1 {
            Rect::from_min_max(pos2f(rect.left(), rect.top() + STRIP_H), rect.max)
        } else {
            *rect
        };
        let Some(edge) = edge_at(body, p) else { return };
        let zone = match edge {
            Edge::Left => Rect::from_min_size(body.min, vec2(body.width() * 0.5, body.height())),
            Edge::Right => Rect::from_min_max(pos2f(body.center().x, body.top()), body.max),
            Edge::Top => Rect::from_min_size(body.min, vec2(body.width(), body.height() * 0.5)),
            Edge::Bottom => Rect::from_min_max(pos2f(body.left(), body.center().y), body.max),
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            Id::new("tab-drop-overlay"),
        ));
        painter.rect_filled(
            zone,
            CornerRadius::same(4),
            self.colors.accent.gamma_multiply(0.25),
        );
        painter.rect_stroke(
            zone,
            CornerRadius::same(4),
            egui::Stroke::new(2.0, self.colors.accent),
            egui::StrokeKind::Inside,
        );
        ctx.request_repaint();
    }

    // ---------------------------------------------------------------- panes

    /// Draws every pane of a split layout with its dividers.
    pub(crate) fn body_panes(&mut self, ui: &mut Ui) {
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        let layout = self.panes.layout(rect);
        self.pane_rects = layout.panes.clone();
        for s in &layout.splitters {
            let resp = ui.interact(
                s.rect,
                Id::new(("splitter", s.path.clone())),
                Sense::click_and_drag(),
            );
            let icon = match s.direction {
                SplitDirection::Horizontal => egui::CursorIcon::ResizeHorizontal,
                SplitDirection::Vertical => egui::CursorIcon::ResizeVertical,
            };
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(icon);
            }
            let color = if resp.hovered() || resp.dragged() {
                self.colors.accent
            } else {
                self.colors.border
            };
            ui.painter().rect_filled(s.rect, CornerRadius::ZERO, color);
            if resp.dragged()
                && let Some(p) = resp.interact_pointer_pos()
            {
                let ratio = ratio_at(s, p);
                self.panes.set_ratio(&s.path, ratio, extent_of(s));
            }
        }
        for (pane, prect) in layout.panes {
            self.pane_ui(ui, pane, prect);
        }
    }

    fn pane_ui(&mut self, ui: &mut Ui, pane: PaneId, rect: Rect) {
        let ctx = ui.ctx().clone();
        // A press inside the pane gives it the focus.
        if self.focused_pane != pane
            && ctx.input(|i| i.pointer.any_pressed())
            && ctx
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|p| rect.contains(p))
        {
            self.focus_pane(pane);
        }
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::Min)),
        );
        child.set_clip_rect(rect);
        egui::Panel::top(Id::new(("pane-tabs", pane)))
            .show(&mut child, |ui| self.tabs_strip(ui, pane));
        match self.active_index_in_pane(pane) {
            Some(i) => self.tab_body(&mut child, i),
            None => {
                egui::CentralPanel::default().show(&mut child, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new("Drop a tab here").weak());
                    });
                });
            }
        }
        if self.focused_pane == pane {
            ui.painter().rect_stroke(
                rect.shrink(0.5),
                CornerRadius::ZERO,
                egui::Stroke::new(1.0, self.colors.accent.gamma_multiply(0.6)),
                egui::StrokeKind::Inside,
            );
        }
    }

    // ---------------------------------------------------------- merged tabs

    /// Draws a merged tab.
    pub(crate) fn merged_ui(&mut self, ui: &mut Ui, index: usize) {
        let now = Instant::now();
        let colors = self.colors.clone();
        let env_epoch = self.style_epoch;
        let pacer = std::sync::Arc::clone(&self.pacer);
        let (font_size, line_height, line_numbers, gap) = (
            self.settings.font_size,
            self.settings.line_height,
            self.settings.line_numbers,
            self.settings.time_gap_threshold_secs,
        );
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        let tab_id = tab.id;
        let TabContent::Merged(mv) = &mut tab.content else {
            return;
        };
        if mv.filter.open {
            egui::Panel::top(Id::new(("mfilter", tab_id))).show(ui, |ui| {
                let opts = FilterPanelOptions {
                    has_hide: false,
                    queries: false,
                    context: false,
                    summary: mv.filter_progress().map(|(n, done)| (n as u64, done)),
                };
                if let Some(enabled) =
                    filter_panel_ui(ui, tab_id, &mut mv.filter, &opts, &colors, now)
                {
                    mv.set_filter_view(enabled);
                }
            });
        }
        if mv.find.open {
            egui::Panel::top(Id::new(("mfind", tab_id))).show(ui, |ui| {
                merged_find_bar(ui, tab_id, mv, now);
            });
        }
        egui::Panel::bottom(Id::new(("mstatus", tab_id))).show(ui, |ui| {
            merged_status(ui, mv, &colors);
        });
        let env = ViewEnv {
            colors: &colors,
            font_size,
            line_height,
            line_numbers,
            minimap: false,
            style_epoch: env_epoch,
            gap_secs: gap,
            pacer: Some(&pacer),
        };
        egui::CentralPanel::no_frame().show(ui, |ui| {
            mergepaint::show(ui, Id::new(("merged", tab_id)), mv, &env);
        });
    }

    // -------------------------------------------------------------- windows

    /// The "Merge tabs" dialog.
    pub(crate) fn merge_window(&mut self, ctx: &Context) {
        let Some(mut dlg) = self.windows.merge.take() else {
            return;
        };
        let candidates = self.merge_candidates();
        let mut open = true;
        let mut go = false;
        egui::Window::new("Merge tabs")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, [0.0, 90.0])
            .show(ctx, |ui| {
                ui.label("Interleave the lines of these files by timestamp:");
                for (id, title) in &candidates {
                    let mut on = dlg.selected.contains(id);
                    if ui.checkbox(&mut on, title).changed() {
                        if on {
                            dlg.selected.push(*id);
                        } else {
                            dlg.selected.retain(|x| x != id);
                        }
                    }
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(dlg.selected.len() >= 2, egui::Button::new("Merge"))
                        .clicked()
                    {
                        go = true;
                    }
                    ui.label(
                        RichText::new("The files stay open in their own tabs.")
                            .weak()
                            .small(),
                    );
                });
            });
        if go {
            // Keep the tab order, not the order of ticking.
            let ids: Vec<u64> = candidates
                .iter()
                .map(|c| c.0)
                .filter(|id| dlg.selected.contains(id))
                .collect();
            self.merge_tabs(&ids);
            return;
        }
        if open {
            self.windows.merge = Some(dlg);
        }
    }

    /// The "Search in all tabs" window.
    pub(crate) fn cross_window(&mut self, ctx: &Context) {
        if !self.windows.cross.open {
            return;
        }
        let mut open = true;
        let mut start = false;
        let mut jump: Option<(u64, u64)> = None;
        let wake = self.waker();
        let targets: Vec<CrossTarget> = self
            .tabs
            .iter()
            .filter_map(|t| {
                t.view().map(|v| CrossTarget {
                    tab_id: t.id,
                    title: t.title.clone(),
                    doc: std::sync::Arc::clone(&v.doc),
                })
            })
            .collect();
        let st = &mut self.windows.cross;
        egui::Window::new("Search in all tabs")
            .open(&mut open)
            .default_width(520.0)
            .anchor(Align2::CENTER_TOP, [0.0, 90.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut st.text)
                            .hint_text("text or regex")
                            .desired_width(300.0),
                    );
                    a11y_label(&resp, "Search in all tabs");
                    if st.focus {
                        resp.request_focus();
                        st.focus = false;
                    }
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        start = true;
                    }
                    toggle_chip(
                        ui,
                        &mut st.regex,
                        ".*",
                        "Regular expression",
                        "Regular expression: on treats the text as a pattern",
                    );
                    case_chip(ui, &mut st.case, "");
                    if ui.button("Search").clicked() {
                        start = true;
                    }
                    if st.running {
                        ui.spinner();
                    }
                });
                if let Some(e) = &st.error {
                    ui.label(RichText::new(e).color(Color32::LIGHT_RED));
                }
                if !st.results.is_empty() || st.running {
                    ui.label(
                        RichText::new(format!(
                            "{} matching lines in {} tabs{}",
                            fmt_count(st.total()),
                            st.results.len(),
                            if st.running { " so far" } else { "" }
                        ))
                        .weak(),
                    );
                }
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .id_salt("cross-results")
                    .show(ui, |ui| {
                        for r in &st.results {
                            let header = format!("{}  ({})", r.title, fmt_count(r.total));
                            egui::CollapsingHeader::new(RichText::new(header).strong())
                                .id_salt(("cross-tab", r.tab_id))
                                .default_open(r.total > 0 && r.total <= 50)
                                .show(ui, |ui| {
                                    for h in &r.hits {
                                        let label = format!(
                                            "{:>7}  {}",
                                            h.line + 1,
                                            crate::util::shorten(&h.text, 120)
                                        );
                                        if ui
                                            .add(
                                                egui::Label::new(RichText::new(label).monospace())
                                                    .sense(Sense::click()),
                                            )
                                            .on_hover_text("Jump to this line")
                                            .clicked()
                                        {
                                            jump = Some((r.tab_id, h.offset));
                                        }
                                    }
                                    if r.total as usize > r.hits.len() {
                                        ui.label(
                                            RichText::new(format!(
                                                "\u{2026} and {} more (search inside the tab)",
                                                fmt_count(r.total - r.hits.len() as u64)
                                            ))
                                            .weak(),
                                        );
                                    }
                                });
                        }
                    });
            });
        if start {
            self.windows.cross.start(targets, wake);
        }
        if let Some((tab_id, offset)) = jump {
            self.jump_to_hit(tab_id, offset);
        }
        if !open {
            self.windows.cross.job = None;
        }
        self.windows.cross.open = open;
    }

    /// The open tabs, in order.
    pub fn tab_list(&self) -> &[crate::tab::Tab] {
        &self.tabs
    }

    /// Where the panes were drawn in the last frame.
    pub fn pane_rects(&self) -> &[(PaneId, Rect)] {
        &self.pane_rects
    }

    /// Searches all open tabs for `text` (literal, smart case), like the
    /// "Search in all tabs" window does.
    pub fn start_cross_search(&mut self, text: &str) {
        let wake = self.waker();
        let targets: Vec<CrossTarget> = self
            .tabs
            .iter()
            .filter_map(|t| {
                t.view().map(|v| CrossTarget {
                    tab_id: t.id,
                    title: t.title.clone(),
                    doc: std::sync::Arc::clone(&v.doc),
                })
            })
            .collect();
        self.windows.cross.open = true;
        self.windows.cross.text = text.to_string();
        self.windows.cross.start(targets, wake);
    }

    /// Whether the "Search in all tabs" window is open.
    pub fn cross_window_open(&self) -> bool {
        self.windows.cross.open
    }

    /// The results of the search across tabs.
    pub fn cross_results(&self) -> &[crate::cross::TabResult] {
        &self.windows.cross.results
    }

    /// Whether the search across tabs is still running.
    pub fn cross_running(&self) -> bool {
        self.windows.cross.running
    }

    /// Jumps to a hit of the search across tabs.
    pub fn jump_to_hit(&mut self, tab_id: u64, offset: u64) {
        if let Some(i) = self.tabs.iter().position(|t| t.id == tab_id) {
            self.select_tab(i);
            if let Some(v) = self.tabs[i].view_mut() {
                v.jump_offset(offset, true);
            }
        }
    }

    /// The split and merge entries of the View menu.
    pub(crate) fn layout_menu(&mut self, ui: &mut Ui) {
        let has_tab = !self.tabs.is_empty();
        if ui
            .add_enabled(has_tab, egui::Button::new("Split right"))
            .on_hover_text("Show this file again in a pane to the right")
            .clicked()
        {
            self.split_active(SplitDirection::Horizontal);
            ui.close();
        }
        if ui
            .add_enabled(has_tab, egui::Button::new("Split down"))
            .on_hover_text("Show this file again in a pane below")
            .clicked()
        {
            self.split_active(SplitDirection::Vertical);
            ui.close();
        }
        let mut sync = self.sync_cursor();
        if ui
            .add_enabled(
                self.pane_count() > 1,
                egui::Checkbox::new(&mut sync, "Sync cursors between panes"),
            )
            .on_hover_text("Panes showing the same file follow each other's selected line")
            .changed()
        {
            self.set_sync_cursor(sync);
        }
        ui.separator();
        if ui
            .add_enabled(
                self.merge_candidates().len() >= 2,
                egui::Button::new("Merge tabs\u{2026}"),
            )
            .clicked()
        {
            self.windows.merge = Some(crate::appmerge::MergeDialog::default());
            ui.close();
        }
    }
}

fn pos2f(x: f32, y: f32) -> Pos2 {
    Pos2::new(x, y)
}

/// The find bar of a merged tab.
fn merged_find_bar(ui: &mut Ui, tab_id: u64, mv: &mut MergedView, now: Instant) {
    ui.horizontal(|ui| {
        ui.label("Find");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut mv.find.text)
                .id(Id::new(("mfind-text", tab_id)))
                .hint_text("text or regex (searches the merged view)")
                .desired_width(300.0),
        );
        a11y_label(&resp, "Find text");
        if mv.find.focus {
            resp.request_focus();
            mv.find.focus = false;
        }
        if resp.changed() {
            mv.find_changed(now);
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let shift = ui.input(|i| i.modifiers.shift);
            mv.find_step(!shift);
            resp.request_focus();
        }
        if toggle_chip(
            ui,
            &mut mv.find.regex,
            ".*",
            "Regular expression",
            "Regular expression: on treats the text as a pattern",
        )
        .changed()
        {
            mv.find_changed(now);
        }
        if case_chip(ui, &mut mv.find.case, "") {
            mv.find_changed(now);
        }
        if icon_button(ui, Icon::Up, "Previous match", "Previous match (Shift+F3)").clicked() {
            mv.find_step(false);
        }
        if icon_button(ui, Icon::Down, "Next match", "Next match (F3)").clicked() {
            mv.find_step(true);
        }
        if let Some(e) = &mv.find.problem {
            ui.label(RichText::new(e).color(Color32::LIGHT_RED));
        } else if let Some(s) = &mv.find.search {
            let scanning = (s.scanned() as usize) < mv.len();
            if scanning {
                ui.spinner();
            }
            ui.label(format!(
                "{}{} matches{}",
                if scanning { "\u{2265} " } else { "" },
                fmt_count(s.count() as u64),
                if mv.filter_view_active() {
                    " (all lines; hidden ones are skipped)"
                } else {
                    ""
                }
            ));
        }
        if icon_button(ui, Icon::Close, "Close find bar", "Close (Esc)").clicked() {
            mv.find.open = false;
        }
    });
}

/// The status bar of a merged tab: follow state, the sources with their
/// badges and line counts, and the size of the merged order.
fn merged_status(ui: &mut Ui, mv: &mut MergedView, colors: &crate::colors::Colors) {
    ui.horizontal_wrapped(|ui| {
        if follow_chip(ui, mv.follow, colors).clicked() {
            mv.toggle_follow();
        }
        ui.separator();
        for (i, s) in mv.sources.iter().enumerate() {
            let lines = s.doc.snapshot().lines.estimated_total;
            ui.label(
                RichText::new(format!(" {} ", badge_letter(i)))
                    .background_color(badge_color(i))
                    .color(Color32::WHITE)
                    .monospace(),
            );
            ui.label(format!("{} ({})", s.name, fmt_count(lines)));
        }
        ui.separator();
        let count_label = match mv.filter_progress() {
            Some((n, done)) => format!(
                "{} of {} lines{}",
                fmt_count(n as u64),
                fmt_count(mv.len() as u64),
                if mv.filter_truncated() {
                    " (too many lines: the rest is not filtered)"
                } else if done {
                    ""
                } else {
                    " so far"
                }
            ),
            None => format!("{} merged lines", fmt_count(mv.len() as u64)),
        };
        ui.label(count_label).on_hover_text(format!(
            "{} bytes per merged line: the merged order takes {}",
            MergedView::BYTES_PER_LINE,
            fmt_bytes(mv.store.bytes() as u64)
        ));
        ui.label(RichText::new(fmt_bytes(mv.store.bytes() as u64)).weak());
        if mv.loading() {
            ui.spinner();
            ui.label("Merging\u{2026}");
        }
        if let Some((msg, _)) = &mv.toast {
            ui.separator();
            ui.label(RichText::new(msg).strong());
        }
    });
}
