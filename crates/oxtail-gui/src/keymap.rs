//! The action registry and the keyboard map.
//!
//! * [`Action`] is every command the user can trigger: a stable id (used in
//!   `settings.toml` and by the command palette), a label, a category.
//! * [`Chord`] is one key combination with a text form (`Ctrl+Shift+P`,
//!   `Cmd+K`, `F3`, `/`).
//! * [`KeyMap`] is built from a preset ([`oxtail_config::Keymap`]) plus the
//!   user's overrides (`custom_keybindings`). It never fails: bad entries
//!   become warnings the user can see, everything else keeps working.
//!
//! Everything here is plain data, so it is tested without a UI.
//!
//! # Chord text
//!
//! Modifiers are joined with `+` in any order, then the key: `Ctrl`,
//! `Cmd` (`Command`, `Mod`), `Alt`, `Shift`. On Windows and Linux `Ctrl`,
//! `Cmd` and `Mod` all mean the Control key; on macOS `Cmd` is the Command
//! key and `Ctrl` the Control key. Files written by OxTail use `Mod` so a
//! settings folder on a USB stick keeps working on every system. Keys are
//! letters, digits, `F1`..`F12`, `Up`/`Down`/`Left`/`Right`, `Home`, `End`,
//! `PageUp`, `PageDown`, `Enter`, `Esc`, `Space`, `Tab`, `Backspace`,
//! `Delete`, `Insert` and the punctuation `/ ? + - = , . ; : [ ] \ ' \``.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use egui::{Key, Modifiers};
use oxtail_config::Keymap;

macro_rules! actions {
    ($( $var:ident, $id:literal, $cat:literal, $label:literal; )*) => {
        /// Something the user can trigger from the keyboard, a menu or the
        /// command palette. The doc comment of each variant is its label.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Action {
            $( #[doc = $label] $var, )*
        }

        impl Action {
            /// Every action, in the order menus and the palette list them.
            pub const ALL: &'static [Action] = &[ $( Action::$var, )* ];

            /// The stable id (`view.wrap`): the key in `custom_keybindings`.
            pub fn id(self) -> &'static str {
                match self { $( Action::$var => $id, )* }
            }

            /// The label shown in the palette and the key bindings editor.
            pub fn label(self) -> &'static str {
                match self { $( Action::$var => $label, )* }
            }

            /// The category (`File`, `View`, ...).
            pub fn category(self) -> &'static str {
                match self { $( Action::$var => $cat, )* }
            }
        }
    };
}

actions! {
    Open, "file.open", "File", "Open file\u{2026}";
    OpenFolder, "file.open_folder", "File", "Open folder\u{2026}";
    PasteAsTab, "file.paste", "File", "Open clipboard text in a new tab";
    CloseTab, "file.close_tab", "File", "Close tab";
    CloseOtherTabs, "file.close_others", "File", "Close other tabs";
    ClearRecent, "file.clear_recent", "File", "Clear recent files";
    Settings, "app.settings", "App", "Settings\u{2026}";
    Palette, "app.palette", "App", "Command palette";
    Quit, "app.quit", "App", "Quit";
    NextTab, "tab.next", "Tab", "Next tab";
    PrevTab, "tab.prev", "Tab", "Previous tab";
    Copy, "edit.copy", "Edit", "Copy";
    CopyWithNumbers, "edit.copy_numbers", "Edit", "Copy with line numbers";
    SelectAll, "edit.select_all", "Edit", "Select all lines";
    Mark, "edit.mark", "Edit", "Insert a mark after the last line";
    Find, "search.find", "Search", "Find\u{2026}";
    FindNext, "search.next", "Search", "Next match";
    FindPrev, "search.prev", "Search", "Previous match";
    Filter, "search.filter", "Search", "Filter view\u{2026}";
    FindInTabs, "search.in_tabs", "Search", "Search in all open tabs\u{2026}";
    GotoLine, "go.line", "Go", "Go to line\u{2026}";
    GotoTime, "go.time", "Go", "Go to time\u{2026}";
    ScrollTop, "go.top", "Go", "Jump to the first line";
    ScrollBottom, "go.bottom", "Go", "Jump to the last line and follow";
    PageUp, "go.page_up", "Go", "Page up";
    PageDown, "go.page_down", "Go", "Page down";
    LineUp, "go.line_up", "Go", "Scroll one line up";
    LineDown, "go.line_down", "Go", "Scroll one line down";
    ScrollLeft, "go.left", "Go", "Scroll left";
    ScrollRight, "go.right", "Go", "Scroll right";
    ToggleBookmark, "bookmark.toggle", "Bookmark", "Toggle bookmark on the current line";
    NextBookmark, "bookmark.next", "Bookmark", "Next bookmark";
    PrevBookmark, "bookmark.prev", "Bookmark", "Previous bookmark";
    ToggleFollow, "view.follow", "View", "Toggle following the end of the file";
    ToggleWrap, "view.wrap", "View", "Toggle line wrapping";
    ToggleLineNumbers, "view.line_numbers", "View", "Toggle line numbers";
    ToggleAnsi, "view.ansi", "View", "Toggle ANSI colours";
    ToggleHidden, "view.hidden_lines", "View", "Toggle showing hidden lines";
    ToggleMinimap, "view.minimap", "View", "Toggle the minimap";
    ToggleTimeGaps, "view.time_gaps", "View", "Toggle time gap separators";
    ZoomIn, "view.zoom_in", "View", "Zoom in";
    ZoomOut, "view.zoom_out", "View", "Zoom out";
    ZoomReset, "view.zoom_reset", "View", "Reset zoom";
    ToggleColumns, "view.columns", "View", "Toggle columns (table view)";
    ChooseParser, "view.choose_parser", "View", "Choose the column parser\u{2026}";
    ToggleDetail, "view.detail_pane", "View", "Toggle the detail pane";
    RelTimeOff, "view.reltime_off", "View", "Relative time: off";
    RelTimePrevious, "view.reltime_previous", "View", "Relative time: since the previous line";
    RelTimeSelected, "view.reltime_selected", "View", "Relative time: since the selected line";
    Stats, "view.stats", "View", "Column statistics\u{2026}";
    Export, "view.export", "View", "Export the view\u{2026}";
    ThemeSystem, "theme.system", "Theme", "Theme: follow the system";
    ThemeDark, "theme.dark", "Theme", "Theme: dark";
    ThemeLight, "theme.light", "Theme", "Theme: light";
    ThemeHighContrast, "theme.high_contrast", "Theme", "Theme: high contrast";
    SplitRight, "layout.split_right", "Layout", "Split right";
    SplitDown, "layout.split_down", "Layout", "Split down";
    SyncCursors, "layout.sync_cursors", "Layout", "Sync cursors between panes";
    MergeTabs, "layout.merge_tabs", "Layout", "Merge tabs by time\u{2026}";
    EditRules, "rules.edit", "Highlight", "Highlight rules\u{2026}";
    Shortcuts, "help.shortcuts", "Help", "Keyboard shortcuts";
    About, "help.about", "Help", "About OxTail";
    CheckUpdates, "help.check_updates", "Help", "Check for updates";
    IntegrateSystem, "system.integrate", "System", "Integrate with the system\u{2026}";
    RemoveIntegration, "system.remove_integration", "System", "Remove system integration";
    Escape, "app.escape", "App", "Close find bar or dialog, clear selection";
}

impl Action {
    /// Looks an action up by its stable id.
    pub fn from_id(id: &str) -> Option<Action> {
        Action::ALL.iter().copied().find(|a| a.id() == id)
    }

    /// The shorter label used in menus (a check mark shows the state of the
    /// toggles, so they need no "Toggle").
    pub fn menu_label(self) -> &'static str {
        match self {
            Action::ToggleFollow => "Follow",
            Action::ToggleWrap => "Wrap lines",
            Action::ToggleLineNumbers => "Line numbers",
            Action::ToggleAnsi => "Render ANSI colours",
            Action::ToggleHidden => "Show hidden lines",
            Action::ToggleMinimap => "Minimap",
            Action::ToggleTimeGaps => "Show time gaps",
            Action::ToggleColumns => "Show as columns",
            Action::ToggleDetail => "Detail pane",
            Action::ChooseParser => "Choose parser\u{2026}",
            Action::RelTimeOff => "Off",
            Action::RelTimePrevious => "Since the previous line",
            Action::RelTimeSelected => "Since the selected line",
            Action::Stats => "Statistics\u{2026}",
            Action::Export => "Export\u{2026}",
            Action::ThemeSystem => "Follow system",
            Action::ThemeDark => "Dark",
            Action::ThemeLight => "Light",
            Action::ThemeHighContrast => "High contrast",
            Action::SyncCursors => "Sync cursors between panes",
            Action::Mark => "Add mark",
            Action::ToggleBookmark => "Toggle bookmark",
            Action::CopyWithNumbers => "Copy with line numbers",
            Action::SelectAll => "Select all",
            Action::Filter => "Filter view\u{2026}",
            Action::FindInTabs => "Search in all tabs\u{2026}",
            Action::EditRules => "Highlight rules\u{2026}",
            other => other.label(),
        }
    }

    /// Whether the user can change the shortcut. Copy and paste arrive as
    /// clipboard events from the windowing system, not as key presses.
    pub fn remappable(self) -> bool {
        !self.event_driven()
    }

    /// Whether the action is triggered by a windowing-system event instead
    /// of a key chord (copy, paste).
    pub fn event_driven(self) -> bool {
        matches!(self, Action::Copy | Action::PasteAsTab)
    }

    /// Whether the command palette lists the action (scroll-by-one-step and
    /// Esc are keys, not commands).
    pub fn in_palette(self) -> bool {
        !matches!(
            self,
            Action::Escape
                | Action::LineUp
                | Action::LineDown
                | Action::ScrollLeft
                | Action::ScrollRight
        )
    }

    /// Whether a key repeat (holding the key down) repeats the action.
    pub fn repeats(self) -> bool {
        matches!(
            self,
            Action::LineUp
                | Action::LineDown
                | Action::PageUp
                | Action::PageDown
                | Action::ScrollLeft
                | Action::ScrollRight
                | Action::FindNext
                | Action::FindPrev
        )
    }
}

// ------------------------------------------------------------------ chords

/// Key names for [`Chord`] text: the canonical spelling first.
const KEY_NAMES: &[(Key, &str)] = &[
    (Key::A, "A"),
    (Key::B, "B"),
    (Key::C, "C"),
    (Key::D, "D"),
    (Key::E, "E"),
    (Key::F, "F"),
    (Key::G, "G"),
    (Key::H, "H"),
    (Key::I, "I"),
    (Key::J, "J"),
    (Key::K, "K"),
    (Key::L, "L"),
    (Key::M, "M"),
    (Key::N, "N"),
    (Key::O, "O"),
    (Key::P, "P"),
    (Key::Q, "Q"),
    (Key::R, "R"),
    (Key::S, "S"),
    (Key::T, "T"),
    (Key::U, "U"),
    (Key::V, "V"),
    (Key::W, "W"),
    (Key::X, "X"),
    (Key::Y, "Y"),
    (Key::Z, "Z"),
    (Key::Num0, "0"),
    (Key::Num1, "1"),
    (Key::Num2, "2"),
    (Key::Num3, "3"),
    (Key::Num4, "4"),
    (Key::Num5, "5"),
    (Key::Num6, "6"),
    (Key::Num7, "7"),
    (Key::Num8, "8"),
    (Key::Num9, "9"),
    (Key::F1, "F1"),
    (Key::F2, "F2"),
    (Key::F3, "F3"),
    (Key::F4, "F4"),
    (Key::F5, "F5"),
    (Key::F6, "F6"),
    (Key::F7, "F7"),
    (Key::F8, "F8"),
    (Key::F9, "F9"),
    (Key::F10, "F10"),
    (Key::F11, "F11"),
    (Key::F12, "F12"),
    (Key::ArrowUp, "Up"),
    (Key::ArrowDown, "Down"),
    (Key::ArrowLeft, "Left"),
    (Key::ArrowRight, "Right"),
    (Key::Home, "Home"),
    (Key::End, "End"),
    (Key::PageUp, "PageUp"),
    (Key::PageDown, "PageDown"),
    (Key::Enter, "Enter"),
    (Key::Escape, "Esc"),
    (Key::Space, "Space"),
    (Key::Tab, "Tab"),
    (Key::Backspace, "Backspace"),
    (Key::Delete, "Delete"),
    (Key::Insert, "Insert"),
    (Key::Slash, "/"),
    (Key::Questionmark, "?"),
    (Key::Plus, "+"),
    (Key::Minus, "-"),
    (Key::Equals, "="),
    (Key::Comma, ","),
    (Key::Period, "."),
    (Key::Semicolon, ";"),
    (Key::Colon, ":"),
    (Key::OpenBracket, "["),
    (Key::CloseBracket, "]"),
    (Key::Backslash, "\\"),
    (Key::Quote, "'"),
    (Key::Backtick, "`"),
];

/// Extra spellings accepted when parsing (lower case).
const KEY_ALIASES: &[(&str, Key)] = &[
    ("escape", Key::Escape),
    ("return", Key::Enter),
    ("pgup", Key::PageUp),
    ("pgdn", Key::PageDown),
    ("pagedown", Key::PageDown),
    ("pageup", Key::PageUp),
    ("arrowup", Key::ArrowUp),
    ("arrowdown", Key::ArrowDown),
    ("arrowleft", Key::ArrowLeft),
    ("arrowright", Key::ArrowRight),
    ("del", Key::Delete),
    ("ins", Key::Insert),
    ("plus", Key::Plus),
    ("minus", Key::Minus),
    ("comma", Key::Comma),
    ("period", Key::Period),
    ("dot", Key::Period),
    ("slash", Key::Slash),
    ("question", Key::Questionmark),
];

/// The keys a chord can use (for tests and the recorder).
pub fn chord_keys() -> impl Iterator<Item = Key> {
    KEY_NAMES.iter().map(|(k, _)| *k)
}

/// Whether a chord can use `key` (the recorder ignores other keys).
pub fn is_bindable(key: Key) -> bool {
    key_name(key).is_some()
}

fn key_name(key: Key) -> Option<&'static str> {
    KEY_NAMES.iter().find(|(k, _)| *k == key).map(|(_, n)| *n)
}

fn key_from_name(name: &str) -> Option<Key> {
    if let Some((k, _)) = KEY_NAMES.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)) {
        return Some(*k);
    }
    let lower = name.to_ascii_lowercase();
    KEY_ALIASES
        .iter()
        .find(|(a, _)| *a == lower)
        .map(|(_, k)| *k)
}

/// Why a chord text could not be understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChordError {
    /// The text is empty.
    Empty,
    /// Only modifiers, no key (`Ctrl+`).
    MissingKey,
    /// A modifier name that is not `Ctrl`, `Cmd`, `Alt` or `Shift`.
    UnknownModifier(String),
    /// A key name that is not known.
    UnknownKey(String),
}

impl fmt::Display for ChordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("empty key chord"),
            Self::MissingKey => f.write_str("no key after the modifiers"),
            Self::UnknownModifier(m) => write!(f, "unknown modifier '{m}'"),
            Self::UnknownKey(k) => write!(f, "unknown key '{k}'"),
        }
    }
}

impl std::error::Error for ChordError {}

/// One key combination.
///
/// `command` is the platform's shortcut modifier (Ctrl on Windows and Linux,
/// Cmd on macOS). `ctrl` is the literal Control key and only exists on
/// macOS; elsewhere it is folded into `command` (see [`Chord::normalized`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    /// The key.
    pub key: Key,
    /// Ctrl (Windows, Linux) or Cmd (macOS).
    pub command: bool,
    /// The literal Control key on macOS.
    pub ctrl: bool,
    /// Alt (Option on macOS).
    pub alt: bool,
    /// Shift.
    pub shift: bool,
}

/// The default platform: whether `Cmd` is a separate key from `Ctrl`.
pub const fn host_is_mac() -> bool {
    cfg!(target_os = "macos")
}

impl Chord {
    /// A chord with no modifiers.
    pub const fn plain(key: Key) -> Self {
        Self {
            key,
            command: false,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// The chord as this platform sees it: on non-macOS the literal Control
    /// key is the command key, and shifted symbol keys (`?`, `+`, `:`) are
    /// the symbol itself, so the chord matches however the layout produces
    /// it (Shift+/ arrives as `?` on some systems and as `/`+Shift on
    /// others).
    pub fn normalized(mut self, mac: bool) -> Self {
        if !mac {
            self.command |= self.ctrl;
            self.ctrl = false;
        }
        if self.shift {
            let symbol = match self.key {
                Key::Slash => Some(Key::Questionmark),
                Key::Equals => Some(Key::Plus),
                Key::Semicolon => Some(Key::Colon),
                _ => None,
            };
            if let Some(s) = symbol {
                self.key = s;
                self.shift = false;
            }
        }
        if matches!(self.key, Key::Plus | Key::Questionmark | Key::Colon) {
            self.shift = false;
        }
        self
    }

    /// The chord a key press is, for lookup in a [`KeyMap`].
    pub fn from_event(key: Key, mods: Modifiers, mac: bool) -> Self {
        Self {
            key,
            command: mods.command || (!mac && mods.ctrl),
            ctrl: mac && mods.ctrl,
            alt: mods.alt,
            shift: mods.shift,
        }
        .normalized(mac)
    }

    /// Parses chord text for the running platform.
    pub fn parse(text: &str) -> Result<Self, ChordError> {
        Self::parse_for(text, host_is_mac())
    }

    /// Parses chord text as it reads on macOS (`mac`) or elsewhere.
    pub fn parse_for(text: &str, mac: bool) -> Result<Self, ChordError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(ChordError::Empty);
        }
        // The key may be `+` itself: `Ctrl++`, or `+` alone.
        let (mods_text, key_text) = if text == "+" {
            ("", "+")
        } else if let Some(prefix) = text.strip_suffix("++") {
            (prefix, "+")
        } else if let Some((m, k)) = text.rsplit_once('+') {
            (m, k)
        } else {
            ("", text)
        };
        let mut chord = Self::plain(Key::A);
        for m in mods_text.split('+') {
            let m = m.trim();
            if m.is_empty() {
                continue;
            }
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => {
                    if mac {
                        chord.ctrl = true;
                    } else {
                        chord.command = true;
                    }
                }
                "cmd" | "command" | "mod" => chord.command = true,
                "alt" | "option" | "opt" => chord.alt = true,
                "shift" => chord.shift = true,
                _ => return Err(ChordError::UnknownModifier(m.to_string())),
            }
        }
        let key_text = key_text.trim();
        if key_text.is_empty() {
            return Err(ChordError::MissingKey);
        }
        chord.key = match key_from_name(key_text) {
            Some(k) => k,
            None => {
                let l = key_text.to_ascii_lowercase();
                return Err(
                    if matches!(
                        l.as_str(),
                        "ctrl" | "control" | "cmd" | "command" | "mod" | "alt" | "shift"
                    ) {
                        ChordError::MissingKey
                    } else {
                        ChordError::UnknownKey(key_text.to_string())
                    },
                );
            }
        };
        Ok(chord.normalized(mac))
    }

    /// The text shown to the user (`Ctrl+Shift+P`, `Cmd+K` on macOS).
    pub fn format(&self) -> String {
        self.format_for(host_is_mac())
    }

    /// Like [`Chord::format`] for macOS (`mac`) or another system.
    pub fn format_for(&self, mac: bool) -> String {
        self.text(mac, if mac { "Cmd" } else { "Ctrl" })
    }

    /// The text written to `settings.toml`: `Mod` for the command key, so
    /// the file means the same on every system.
    pub fn to_config(&self) -> String {
        self.text(host_is_mac(), "Mod")
    }

    fn text(&self, mac: bool, command_word: &str) -> String {
        let mut out = String::new();
        if self.ctrl && mac {
            out.push_str("Ctrl+");
        }
        if self.command || (self.ctrl && !mac) {
            out.push_str(command_word);
            out.push('+');
        }
        if self.alt {
            out.push_str("Alt+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        out.push_str(key_name(self.key).unwrap_or("?"));
        out
    }

    /// Whether a key chord may fire while a text field has the keyboard
    /// focus. Plain letters, Shift+letter, arrows and paging keys belong to
    /// the text field; function keys, Esc and Ctrl/Alt combinations do not,
    /// except the ones text fields use for editing (Ctrl+A/C/X/V/Z/Y and
    /// caret movement).
    pub fn works_in_text_fields(&self) -> bool {
        self.works_in_text_fields_for(host_is_mac())
    }

    /// [`Self::works_in_text_fields`] for macOS (`mac`) or another system.
    /// Chords that type characters count as text: Option+letter on macOS and
    /// AltGr (reported as Ctrl+Alt) on Windows and Linux.
    pub fn works_in_text_fields_for(&self, mac: bool) -> bool {
        if matches!(self.key, Key::Escape) || is_function_key(self.key) {
            return true;
        }
        if self.alt && ((mac && !self.command) || (!mac && self.command && !self.shift)) {
            return false;
        }
        if self.command || self.ctrl {
            return !matches!(
                self.key,
                Key::A
                    | Key::C
                    | Key::X
                    | Key::V
                    | Key::Z
                    | Key::Y
                    | Key::Home
                    | Key::End
                    | Key::ArrowUp
                    | Key::ArrowDown
                    | Key::ArrowLeft
                    | Key::ArrowRight
                    | Key::PageUp
                    | Key::PageDown
                    | Key::Backspace
                    | Key::Delete
                    | Key::Enter
                    | Key::Space
            );
        }
        self.alt
    }
}

fn is_function_key(key: Key) -> bool {
    matches!(
        key,
        Key::F1
            | Key::F2
            | Key::F3
            | Key::F4
            | Key::F5
            | Key::F6
            | Key::F7
            | Key::F8
            | Key::F9
            | Key::F10
            | Key::F11
            | Key::F12
    )
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format())
    }
}

// ----------------------------------------------------------------- presets

/// The chords of the standard preset, in display order (the first chord of
/// an action is the one shown in menus). `Mod` is the command key.
const STANDARD: &[(Action, &[&str])] = &[
    (Action::Open, &["Mod+O"]),
    (Action::OpenFolder, &["Mod+Shift+O"]),
    (Action::PasteAsTab, &["Mod+V"]),
    (Action::CloseTab, &["Mod+W"]),
    (Action::Settings, &["Mod+,"]),
    (Action::Palette, &["Mod+Shift+P"]),
    (Action::Quit, &["Mod+Q"]),
    (Action::NextTab, &["Ctrl+Tab"]),
    (Action::PrevTab, &["Ctrl+Shift+Tab"]),
    (Action::Copy, &["Mod+C"]),
    (Action::CopyWithNumbers, &["Mod+Shift+C"]),
    (Action::SelectAll, &["Mod+A"]),
    (Action::Mark, &["Mod+M"]),
    (Action::Find, &["Mod+F"]),
    (Action::FindNext, &["F3"]),
    (Action::FindPrev, &["Shift+F3"]),
    (Action::Filter, &["Mod+Shift+F"]),
    (Action::FindInTabs, &["Mod+Alt+F"]),
    (Action::GotoLine, &["Mod+G"]),
    (Action::GotoTime, &["Mod+Shift+G"]),
    (Action::ScrollTop, &["Home", "Mod+Home"]),
    (Action::ScrollBottom, &["End", "Mod+End"]),
    (Action::PageUp, &["PageUp"]),
    (Action::PageDown, &["PageDown"]),
    (Action::LineUp, &["Up"]),
    (Action::LineDown, &["Down"]),
    (Action::ScrollLeft, &["Left"]),
    (Action::ScrollRight, &["Right"]),
    (Action::ToggleBookmark, &["Mod+F2"]),
    (Action::NextBookmark, &["F2"]),
    (Action::PrevBookmark, &["Shift+F2"]),
    (Action::ToggleFollow, &["F"]),
    (Action::ToggleWrap, &["Alt+Z"]),
    (Action::ZoomIn, &["Mod+=", "Mod++"]),
    (Action::ZoomOut, &["Mod+-"]),
    (Action::ZoomReset, &["Mod+0"]),
    (Action::Shortcuts, &["F1"]),
    (Action::Escape, &["Esc"]),
];

/// What the `less` preset adds to the standard one. `q` is deliberately not
/// bound: it must never close anything.
const LESS: &[(Action, &[&str])] = &[
    (Action::ScrollTop, &["G"]),
    (Action::ScrollBottom, &["Shift+G"]),
    (Action::Find, &["/", "?"]),
    (Action::FindNext, &["N"]),
    (Action::FindPrev, &["Shift+N"]),
    (Action::ToggleFollow, &["Shift+F"]),
    (Action::LineDown, &["J"]),
    (Action::LineUp, &["K"]),
    (Action::PageDown, &["Space"]),
    (Action::PageUp, &["B"]),
];

fn preset_table(preset: Keymap) -> Vec<(Action, &'static [&'static str])> {
    let mut rows: Vec<(Action, &'static [&'static str])> = STANDARD.to_vec();
    if preset == Keymap::Less {
        rows.extend_from_slice(LESS);
    }
    rows
}

/// A chord bound to two different actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// The chord.
    pub chord: Chord,
    /// The actions that claim it (the first one wins).
    pub actions: Vec<Action>,
}

/// The active key bindings: preset plus the user's overrides.
#[derive(Debug, Clone)]
pub struct KeyMap {
    mac: bool,
    preset: Keymap,
    /// Chords per action, primary first.
    chords: HashMap<Action, Vec<Chord>>,
    /// Which actions each chord triggers (the first wins).
    by_chord: HashMap<Chord, Vec<Action>>,
    warnings: Vec<String>,
}

impl Default for KeyMap {
    fn default() -> Self {
        Self::build(Keymap::Standard, &BTreeMap::new())
    }
}

impl KeyMap {
    /// The chords `preset` binds to `action` (before any override).
    pub fn default_chords(preset: Keymap, action: Action, mac: bool) -> Vec<Chord> {
        let mut out: Vec<Chord> = Vec::new();
        // The last table entry for an action wins, so `less` can replace.
        let mut found: Vec<Chord> = Vec::new();
        let mut extra: Vec<Chord> = Vec::new();
        for (i, (a, texts)) in preset_table(preset).into_iter().enumerate() {
            if a != action {
                continue;
            }
            let parsed: Vec<Chord> = texts
                .iter()
                .filter_map(|t| Chord::parse_for(t, mac).ok())
                .collect();
            if i < STANDARD.len() {
                found = parsed;
            } else {
                extra.extend(parsed);
            }
        }
        out.extend(found);
        for c in extra {
            if !out.contains(&c) {
                out.push(c);
            }
        }
        out
    }

    /// Builds the map for the running platform.
    pub fn build(preset: Keymap, custom: &BTreeMap<String, String>) -> Self {
        Self::build_for(preset, custom, host_is_mac())
    }

    /// Builds the map as it would be on macOS (`mac`) or another system.
    /// Never fails: invalid overrides are skipped and listed in
    /// [`KeyMap::warnings`].
    pub fn build_for(preset: Keymap, custom: &BTreeMap<String, String>, mac: bool) -> Self {
        let mut warnings = Vec::new();
        let mut chords: HashMap<Action, Vec<Chord>> = HashMap::new();
        for &a in Action::ALL {
            let d = Self::default_chords(preset, a, mac);
            if !d.is_empty() {
                chords.insert(a, d);
            }
        }
        // The overrides, validated.
        let mut overrides: Vec<(Action, Option<Chord>)> = Vec::new();
        for (id, text) in custom {
            let Some(action) = Action::from_id(id) else {
                warnings.push(format!("Key bindings: unknown action '{id}' (ignored)"));
                continue;
            };
            if !action.remappable() {
                warnings.push(format!(
                    "Key bindings: '{}' cannot be changed (ignored)",
                    action.label()
                ));
                continue;
            }
            let t = text.trim();
            if t.is_empty() || t.eq_ignore_ascii_case("none") {
                overrides.push((action, None));
                continue;
            }
            match Chord::parse_for(t, mac) {
                Ok(c) => overrides.push((action, Some(c))),
                Err(e) => warnings.push(format!(
                    "Key bindings: '{t}' for '{id}' is not valid ({e}); the default is used"
                )),
            }
        }
        let overridden: HashSet<Action> = overrides.iter().map(|(a, _)| *a).collect();
        for a in &overridden {
            chords.remove(a);
        }
        // A user chord takes the chord away from the preset action that had it.
        let taken: HashSet<Chord> = overrides.iter().filter_map(|(_, c)| *c).collect();
        for (&a, list) in chords.iter_mut() {
            let before = list.len();
            let mut lost = Vec::new();
            list.retain(|c| {
                let keep = !taken.contains(c);
                if !keep {
                    lost.push(*c);
                }
                keep
            });
            if list.len() != before {
                let owner = overrides
                    .iter()
                    .find(|(_, c)| c.is_some_and(|c| lost.contains(&c)))
                    .map(|(o, _)| o.label())
                    .unwrap_or("another action");
                warnings.push(format!(
                    "Key bindings: {} lost {} to '{owner}'",
                    a.label(),
                    lost.iter()
                        .map(|c| c.format_for(mac))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        chords.retain(|_, l| !l.is_empty());
        for (a, c) in &overrides {
            if let Some(c) = c {
                let list = chords.entry(*a).or_default();
                if !list.contains(c) {
                    list.push(*c);
                }
            }
        }
        let mut by_chord: HashMap<Chord, Vec<Action>> = HashMap::new();
        for &a in Action::ALL {
            for c in chords.get(&a).into_iter().flatten() {
                by_chord.entry(*c).or_default().push(a);
            }
        }
        let map = Self {
            mac,
            preset,
            chords,
            by_chord,
            warnings,
        };
        let mut map = map;
        for c in map.conflicts() {
            let names: Vec<&str> = c.actions.iter().map(|a| a.label()).collect();
            map.warnings.push(format!(
                "Key bindings: {} is used by {} (the first one wins)",
                c.chord.format_for(mac),
                names.join(" and ")
            ));
        }
        map
    }

    /// The preset this map was built from.
    pub fn preset(&self) -> Keymap {
        self.preset
    }

    /// Whether the map was built for macOS.
    pub fn is_mac(&self) -> bool {
        self.mac
    }

    /// The action bound to a key press, if any.
    pub fn lookup(&self, key: Key, mods: Modifiers) -> Option<Action> {
        let c = Chord::from_event(key, mods, self.mac);
        self.by_chord.get(&c).and_then(|v| v.first().copied())
    }

    /// The chord an action is bound to and can be triggered from.
    pub fn chords_for(&self, action: Action) -> &[Chord] {
        self.chords.get(&action).map_or(&[], Vec::as_slice)
    }

    /// The main chord of an action, formatted for display (menus, palette).
    pub fn shortcut_text(&self, action: Action) -> Option<String> {
        self.chords_for(action)
            .first()
            .map(|c| c.format_for(self.mac))
    }

    /// All chords of an action, formatted, joined with `/`.
    pub fn shortcut_text_all(&self, action: Action) -> String {
        self.chords_for(action)
            .iter()
            .map(|c| c.format_for(self.mac))
            .collect::<Vec<_>>()
            .join(" / ")
    }

    /// Problems found while building the map (bad or conflicting entries).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Chords that two or more actions claim.
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut out: Vec<Conflict> = self
            .by_chord
            .iter()
            .filter(|(_, v)| v.len() > 1)
            .map(|(c, v)| Conflict {
                chord: *c,
                actions: v.clone(),
            })
            .collect();
        out.sort_by_key(|c| c.chord.format_for(self.mac));
        out
    }

    /// Whether `action` shares one of its chords with another action.
    pub fn in_conflict(&self, action: Action) -> bool {
        self.chords_for(action).iter().any(|c| {
            self.by_chord
                .get(c)
                .is_some_and(|v| v.len() > 1 && v.contains(&action))
        })
    }
}

/// Rows for the shortcuts window: `(chords, description)`, generated from
/// the key map (plus the few fixed gestures that are not actions).
pub fn shortcut_rows(map: &KeyMap) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    for &a in Action::ALL {
        let text = map.shortcut_text_all(a);
        if text.is_empty() {
            continue;
        }
        rows.push((text, a.label().to_string()));
    }
    let cmd = if map.is_mac() { "Cmd" } else { "Ctrl" };
    rows.push((
        "Enter / Shift+Enter".into(),
        "Next / previous match (in the find field)".into(),
    ));
    rows.push((format!("{cmd}+wheel"), "Zoom".into()));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers::NONE;

    fn m(command: bool, shift: bool, alt: bool) -> Modifiers {
        Modifiers {
            alt,
            ctrl: command,
            shift,
            mac_cmd: false,
            command,
        }
    }

    fn standard() -> KeyMap {
        KeyMap::build_for(Keymap::Standard, &BTreeMap::new(), false)
    }

    fn less() -> KeyMap {
        KeyMap::build_for(Keymap::Less, &BTreeMap::new(), false)
    }

    fn custom(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn action_ids_are_unique_and_well_formed() {
        let mut seen = HashSet::new();
        for &a in Action::ALL {
            assert!(seen.insert(a.id()), "duplicate id {}", a.id());
            assert!(a.id().contains('.'), "{}", a.id());
            assert!(
                a.id()
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_'),
                "{}",
                a.id()
            );
            assert!(!a.label().is_empty() && !a.category().is_empty());
            assert_eq!(Action::from_id(a.id()), Some(a));
        }
        assert_eq!(Action::from_id("nope"), None);
        assert!(Action::ALL.len() >= 60);
    }

    #[test]
    fn documented_shortcuts() {
        let k = standard();
        let cmd = m(true, false, false);
        let cmd_shift = m(true, true, false);
        let shift = m(false, true, false);
        assert_eq!(k.lookup(Key::O, cmd), Some(Action::Open));
        assert_eq!(k.lookup(Key::W, cmd), Some(Action::CloseTab));
        assert_eq!(k.lookup(Key::Tab, cmd), Some(Action::NextTab));
        assert_eq!(k.lookup(Key::Tab, cmd_shift), Some(Action::PrevTab));
        assert_eq!(k.lookup(Key::F, cmd), Some(Action::Find));
        assert_eq!(k.lookup(Key::F, cmd_shift), Some(Action::Filter));
        assert_eq!(k.lookup(Key::F3, NONE), Some(Action::FindNext));
        assert_eq!(k.lookup(Key::F3, shift), Some(Action::FindPrev));
        assert_eq!(k.lookup(Key::G, cmd), Some(Action::GotoLine));
        assert_eq!(k.lookup(Key::G, cmd_shift), Some(Action::GotoTime));
        assert_eq!(
            k.lookup(Key::F, m(true, false, true)),
            Some(Action::FindInTabs)
        );
        assert_eq!(k.lookup(Key::F2, cmd), Some(Action::ToggleBookmark));
        assert_eq!(k.lookup(Key::F2, NONE), Some(Action::NextBookmark));
        assert_eq!(k.lookup(Key::F2, shift), Some(Action::PrevBookmark));
        assert_eq!(k.lookup(Key::M, cmd), Some(Action::Mark));
        assert_eq!(k.lookup(Key::C, cmd_shift), Some(Action::CopyWithNumbers));
        assert_eq!(k.lookup(Key::A, cmd), Some(Action::SelectAll));
        assert_eq!(k.lookup(Key::Equals, cmd), Some(Action::ZoomIn));
        assert_eq!(k.lookup(Key::Plus, cmd), Some(Action::ZoomIn));
        assert_eq!(k.lookup(Key::Equals, cmd_shift), Some(Action::ZoomIn));
        assert_eq!(k.lookup(Key::Minus, cmd), Some(Action::ZoomOut));
        assert_eq!(k.lookup(Key::Num0, cmd), Some(Action::ZoomReset));
        assert_eq!(
            k.lookup(Key::Z, m(false, false, true)),
            Some(Action::ToggleWrap)
        );
        assert_eq!(k.lookup(Key::P, cmd_shift), Some(Action::Palette));
        assert_eq!(k.lookup(Key::Comma, cmd), Some(Action::Settings));
        assert_eq!(k.lookup(Key::F1, NONE), Some(Action::Shortcuts));
    }

    #[test]
    fn navigation_keys() {
        let k = standard();
        let cmd = m(true, false, false);
        assert_eq!(k.lookup(Key::F, NONE), Some(Action::ToggleFollow));
        assert_eq!(k.lookup(Key::Home, NONE), Some(Action::ScrollTop));
        assert_eq!(k.lookup(Key::Home, cmd), Some(Action::ScrollTop));
        assert_eq!(k.lookup(Key::End, NONE), Some(Action::ScrollBottom));
        assert_eq!(k.lookup(Key::End, cmd), Some(Action::ScrollBottom));
        assert_eq!(k.lookup(Key::PageUp, NONE), Some(Action::PageUp));
        assert_eq!(k.lookup(Key::PageDown, NONE), Some(Action::PageDown));
        assert_eq!(k.lookup(Key::ArrowUp, NONE), Some(Action::LineUp));
        assert_eq!(k.lookup(Key::ArrowDown, NONE), Some(Action::LineDown));
        assert_eq!(k.lookup(Key::ArrowLeft, NONE), Some(Action::ScrollLeft));
        assert_eq!(k.lookup(Key::ArrowRight, NONE), Some(Action::ScrollRight));
        assert_eq!(k.lookup(Key::Escape, NONE), Some(Action::Escape));
    }

    #[test]
    fn unrelated_chords_are_not_stolen() {
        let k = standard();
        assert_eq!(k.lookup(Key::F, m(false, true, false)), None);
        assert_eq!(k.lookup(Key::ArrowUp, m(false, true, false)), None);
        assert_eq!(k.lookup(Key::X, m(true, false, false)), None);
        assert_eq!(k.lookup(Key::G, NONE), None);
        assert_eq!(k.lookup(Key::W, m(true, true, false)), None);
        // The standard preset has no plain-letter bindings besides F.
        assert_eq!(k.lookup(Key::J, NONE), None);
        assert_eq!(k.lookup(Key::Space, NONE), None);
    }

    #[test]
    fn the_standard_preset_is_conflict_free_and_warning_free() {
        for mac in [false, true] {
            for preset in [Keymap::Standard, Keymap::Less] {
                let k = KeyMap::build_for(preset, &BTreeMap::new(), mac);
                assert!(k.conflicts().is_empty(), "{:?}", k.conflicts());
                assert!(k.warnings().is_empty(), "{:?}", k.warnings());
            }
        }
    }

    #[test]
    fn less_preset_adds_the_less_keys_and_keeps_standard_ones() {
        let k = less();
        let shift = m(false, true, false);
        assert_eq!(k.lookup(Key::G, NONE), Some(Action::ScrollTop));
        assert_eq!(k.lookup(Key::G, shift), Some(Action::ScrollBottom));
        assert_eq!(k.lookup(Key::Slash, NONE), Some(Action::Find));
        assert_eq!(k.lookup(Key::Questionmark, NONE), Some(Action::Find));
        // Shift+/ is `?` however the system reports it.
        assert_eq!(k.lookup(Key::Slash, shift), Some(Action::Find));
        assert_eq!(k.lookup(Key::Questionmark, shift), Some(Action::Find));
        assert_eq!(k.lookup(Key::N, NONE), Some(Action::FindNext));
        assert_eq!(k.lookup(Key::N, shift), Some(Action::FindPrev));
        assert_eq!(k.lookup(Key::F, shift), Some(Action::ToggleFollow));
        assert_eq!(k.lookup(Key::F, NONE), Some(Action::ToggleFollow));
        assert_eq!(k.lookup(Key::J, NONE), Some(Action::LineDown));
        assert_eq!(k.lookup(Key::K, NONE), Some(Action::LineUp));
        assert_eq!(k.lookup(Key::Space, NONE), Some(Action::PageDown));
        assert_eq!(k.lookup(Key::B, NONE), Some(Action::PageUp));
        // Standard chords still work.
        assert_eq!(k.lookup(Key::F, m(true, false, false)), Some(Action::Find));
        assert_eq!(k.lookup(Key::F3, NONE), Some(Action::FindNext));
        assert_eq!(k.lookup(Key::PageDown, NONE), Some(Action::PageDown));
        // `q` must never do anything, least of all quit.
        assert_eq!(k.lookup(Key::Q, NONE), None);
        assert_eq!(k.lookup(Key::Q, shift), None);
    }

    #[test]
    fn less_keys_do_not_fire_in_text_fields_but_ctrl_chords_do() {
        let k = less();
        for (key, mods) in [
            (Key::G, NONE),
            (Key::J, NONE),
            (Key::Space, NONE),
            (Key::Slash, NONE),
            (Key::N, m(false, true, false)),
        ] {
            let c = Chord::from_event(key, mods, false);
            assert!(k.lookup(key, mods).is_some());
            assert!(!c.works_in_text_fields(), "{c}");
        }
        for (key, mods) in [
            (Key::F, m(true, false, false)),
            (Key::F3, NONE),
            (Key::Escape, NONE),
            (Key::G, m(true, false, false)),
            (Key::Z, m(false, false, true)),
            (Key::P, m(true, true, false)),
        ] {
            assert!(
                Chord::from_event(key, mods, false).works_in_text_fields(),
                "{key:?}"
            );
        }
        // Editing chords and caret keys belong to the text field.
        for (key, mods) in [
            (Key::A, m(true, false, false)),
            (Key::C, m(true, false, false)),
            (Key::Home, m(true, false, false)),
            (Key::ArrowUp, NONE),
            (Key::PageDown, NONE),
            (Key::F, NONE),
        ] {
            assert!(
                !Chord::from_event(key, mods, false).works_in_text_fields(),
                "{key:?}"
            );
        }
    }

    #[test]
    fn chords_that_type_characters_are_text_in_text_fields() {
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        let ctrl_alt = Modifiers {
            alt: true,
            ctrl: true,
            command: true,
            ..Modifiers::NONE
        };
        let cmd_alt = Modifiers {
            alt: true,
            command: true,
            ..Modifiers::NONE
        };
        // macOS: Option+Z types a character, Cmd+Option+F is a command.
        assert!(!Chord::from_event(Key::Z, alt, true).works_in_text_fields_for(true));
        assert!(Chord::from_event(Key::F, cmd_alt, true).works_in_text_fields_for(true));
        // Elsewhere: Alt+Z is a command, AltGr (Ctrl+Alt) types characters.
        assert!(Chord::from_event(Key::Z, alt, false).works_in_text_fields_for(false));
        assert!(!Chord::from_event(Key::F, ctrl_alt, false).works_in_text_fields_for(false));
    }

    #[test]
    fn overrides_replace_the_preset_chords_of_an_action() {
        let k = KeyMap::build_for(
            Keymap::Standard,
            &custom(&[("search.find", "Ctrl+Shift+K")]),
            false,
        );
        assert_eq!(k.lookup(Key::K, m(true, true, false)), Some(Action::Find));
        // The default chord is gone (the override replaces, not adds).
        assert_eq!(k.lookup(Key::F, m(true, false, false)), None);
        assert_eq!(
            k.shortcut_text(Action::Find).as_deref(),
            Some("Ctrl+Shift+K")
        );
        assert!(k.warnings().is_empty());
    }

    #[test]
    fn an_empty_override_unbinds_and_none_too() {
        let k = KeyMap::build_for(
            Keymap::Standard,
            &custom(&[("edit.mark", ""), ("search.filter", "none")]),
            false,
        );
        assert!(k.chords_for(Action::Mark).is_empty());
        assert!(k.chords_for(Action::Filter).is_empty());
        assert_eq!(k.lookup(Key::M, m(true, false, false)), None);
        assert!(k.warnings().is_empty());
    }

    #[test]
    fn overrides_win_over_the_preset_and_the_loser_is_reported() {
        // Ctrl+F now goes to "Go to line": Find loses its chord.
        let k = KeyMap::build_for(Keymap::Standard, &custom(&[("go.line", "Ctrl+F")]), false);
        assert_eq!(
            k.lookup(Key::F, m(true, false, false)),
            Some(Action::GotoLine)
        );
        assert!(k.chords_for(Action::Find).is_empty());
        assert!(
            k.warnings()
                .iter()
                .any(|w| w.contains("Find") && w.contains("Ctrl+F")),
            "{:?}",
            k.warnings()
        );
        assert!(k.conflicts().is_empty());
    }

    #[test]
    fn two_overrides_on_one_chord_are_a_reported_conflict() {
        let k = KeyMap::build_for(
            Keymap::Standard,
            &custom(&[("edit.mark", "Ctrl+K"), ("search.filter", "Ctrl+K")]),
            false,
        );
        let c = k.conflicts();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].chord.format_for(false), "Ctrl+K");
        assert_eq!(c[0].actions.len(), 2);
        assert!(k.in_conflict(Action::Mark) && k.in_conflict(Action::Filter));
        assert!(!k.in_conflict(Action::Find));
        // Deterministic: the action listed first wins.
        assert_eq!(
            k.lookup(Key::K, m(true, false, false)),
            Some(Action::Mark.min(Action::Filter))
        );
        assert!(k.warnings().iter().any(|w| w.contains("Ctrl+K")));
    }

    #[test]
    fn invalid_overrides_become_warnings_not_panics() {
        let k = KeyMap::build_for(
            Keymap::Standard,
            &custom(&[
                ("nope.nothing", "Ctrl+X"),
                ("search.find", "Ctrl+Banana"),
                ("edit.mark", "Hyper+M"),
                ("go.line", "Ctrl+"),
                ("edit.copy", "Ctrl+Shift+Y"),
            ]),
            false,
        );
        assert_eq!(k.warnings().len(), 5, "{:?}", k.warnings());
        // Everything falls back to the defaults.
        assert_eq!(k.lookup(Key::F, m(true, false, false)), Some(Action::Find));
        assert_eq!(k.lookup(Key::M, m(true, false, false)), Some(Action::Mark));
        assert_eq!(
            k.lookup(Key::G, m(true, false, false)),
            Some(Action::GotoLine)
        );
    }

    #[test]
    fn mac_uses_cmd_for_the_command_key_and_ctrl_for_tabs() {
        let k = KeyMap::build_for(Keymap::Standard, &BTreeMap::new(), true);
        let cmd = Modifiers {
            mac_cmd: true,
            command: true,
            ..Modifiers::NONE
        };
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        assert_eq!(k.lookup(Key::O, cmd), Some(Action::Open));
        assert_eq!(k.lookup(Key::O, ctrl), None);
        assert_eq!(k.lookup(Key::Tab, ctrl), Some(Action::NextTab));
        assert_eq!(k.lookup(Key::Tab, cmd), None);
        assert_eq!(k.shortcut_text(Action::Open).as_deref(), Some("Cmd+O"));
        assert_eq!(
            k.shortcut_text(Action::NextTab).as_deref(),
            Some("Ctrl+Tab")
        );
        assert_eq!(
            k.shortcut_text(Action::Palette).as_deref(),
            Some("Cmd+Shift+P")
        );
        // On other systems both are the Control key.
        let w = standard();
        assert_eq!(w.shortcut_text(Action::Open).as_deref(), Some("Ctrl+O"));
        assert_eq!(
            w.shortcut_text(Action::NextTab).as_deref(),
            Some("Ctrl+Tab")
        );
        assert_eq!(
            w.lookup(
                Key::Tab,
                Modifiers {
                    ctrl: true,
                    ..Modifiers::NONE
                }
            ),
            Some(Action::NextTab)
        );
    }

    #[test]
    fn chord_parse_and_format() {
        let p = |s: &str| Chord::parse_for(s, false).unwrap();
        assert_eq!(p("Ctrl+Shift+P").format_for(false), "Ctrl+Shift+P");
        assert_eq!(p("shift+ctrl+p").format_for(false), "Ctrl+Shift+P");
        assert_eq!(p("Cmd+K").format_for(false), "Ctrl+K");
        assert_eq!(p("Cmd+K").format_for(true), "Cmd+K");
        assert_eq!(p("Mod+K").to_config().len(), 5);
        assert_eq!(p("f5").format_for(false), "F5");
        assert_eq!(p("F12"), Chord::plain(Key::F12));
        assert_eq!(p("Alt+Left").format_for(false), "Alt+Left");
        assert_eq!(p("Ctrl+PgDn").format_for(false), "Ctrl+PageDown");
        assert_eq!(p("Escape").format_for(false), "Esc");
        assert_eq!(p("/").format_for(false), "/");
        assert_eq!(p("?").format_for(false), "?");
        assert_eq!(p("Ctrl++").format_for(false), "Ctrl++");
        assert_eq!(p("+"), Chord::plain(Key::Plus));
        assert_eq!(p("Ctrl+-").format_for(false), "Ctrl+-");
        assert_eq!(p("Ctrl+,").format_for(false), "Ctrl+,");
        assert_eq!(p("Shift+G").format_for(false), "Shift+G");
        assert_eq!(p("g"), Chord::plain(Key::G));
        assert_eq!(p("1"), Chord::plain(Key::Num1));
        // On macOS Ctrl is the literal Control key, distinct from Cmd.
        let c = Chord::parse_for("Ctrl+Tab", true).unwrap();
        assert!(c.ctrl && !c.command);
        assert_eq!(c.format_for(true), "Ctrl+Tab");
        // Shift+/ is the question mark.
        assert_eq!(p("Shift+/"), Chord::plain(Key::Questionmark));
    }

    #[test]
    fn chord_parse_errors() {
        let e = |s: &str| Chord::parse_for(s, false).unwrap_err();
        assert_eq!(e(""), ChordError::Empty);
        assert_eq!(e("  "), ChordError::Empty);
        assert_eq!(e("Ctrl+"), ChordError::MissingKey);
        assert_eq!(e("Ctrl"), ChordError::MissingKey);
        assert_eq!(e("Ctrl+Shift"), ChordError::MissingKey);
        assert!(matches!(e("Hyper+X"), ChordError::UnknownModifier(_)));
        assert!(matches!(e("Ctrl+Banana"), ChordError::UnknownKey(_)));
        assert!(matches!(e("Ctrl+F99"), ChordError::UnknownKey(_)));
        assert!(!e("Ctrl+Banana").to_string().is_empty());
    }

    #[test]
    fn every_key_round_trips_with_every_modifier_set() {
        for mac in [false, true] {
            for key in chord_keys() {
                for bits in 0..16u8 {
                    let c = Chord {
                        key,
                        command: bits & 1 != 0,
                        ctrl: bits & 2 != 0,
                        alt: bits & 4 != 0,
                        shift: bits & 8 != 0,
                    }
                    .normalized(mac);
                    let text = c.format_for(mac);
                    let back = Chord::parse_for(&text, mac)
                        .unwrap_or_else(|e| panic!("{text}: {e}"))
                        .normalized(mac);
                    assert_eq!(back, c, "{text}");
                }
            }
        }
    }

    #[test]
    fn config_text_round_trips_across_systems() {
        // Written as `Mod+..`, so it means the command key everywhere.
        let c = Chord::parse_for("Ctrl+Shift+P", false).unwrap();
        let text = c.text(false, "Mod");
        assert_eq!(text, "Mod+Shift+P");
        let on_mac = Chord::parse_for(&text, true).unwrap();
        assert!(on_mac.command && !on_mac.ctrl);
        assert_eq!(Chord::parse_for(&text, false).unwrap(), c);
    }

    #[test]
    fn hostile_chord_text_never_panics() {
        for s in [
            "+++",
            "++",
            "Ctrl+++",
            "+Ctrl",
            "\u{0}",
            "Ctrl+\u{1F600}",
            "a+b+c+d+e",
            " + ",
            "Ctrl+ +",
            "\u{2028}",
            "F0",
            "F13",
        ] {
            let _ = Chord::parse_for(s, false);
            let _ = Chord::parse_for(s, true);
        }
    }

    #[test]
    fn shortcut_rows_are_generated_from_the_map() {
        let k = KeyMap::build_for(
            Keymap::Less,
            &custom(&[("edit.mark", "Ctrl+Shift+K")]),
            false,
        );
        let rows = shortcut_rows(&k);
        let find = rows
            .iter()
            .find(|(_, d)| d == Action::Find.label())
            .expect("find row");
        assert_eq!(find.0, "Ctrl+F / / / ?");
        let mark = rows
            .iter()
            .find(|(_, d)| d == Action::Mark.label())
            .expect("mark row");
        assert_eq!(mark.0, "Ctrl+Shift+K");
        assert!(rows.iter().any(|(k, _)| k == "Ctrl+Q"));
        // Actions without a shortcut are not listed.
        let unbound = KeyMap::build_for(Keymap::Standard, &custom(&[("app.quit", "")]), false);
        assert!(
            !shortcut_rows(&unbound)
                .iter()
                .any(|(_, d)| d == Action::Quit.label())
        );
    }

    proptest::proptest! {
        #[test]
        fn chord_text_round_trips(
            idx in 0usize..KEY_NAMES.len(),
            command in proptest::bool::ANY,
            ctrl in proptest::bool::ANY,
            alt in proptest::bool::ANY,
            shift in proptest::bool::ANY,
            mac in proptest::bool::ANY,
        ) {
            let c = Chord { key: KEY_NAMES[idx].0, command, ctrl, alt, shift }.normalized(mac);
            let text = c.format_for(mac);
            let back = Chord::parse_for(&text, mac).unwrap().normalized(mac);
            proptest::prop_assert_eq!(back, c);
        }

        #[test]
        fn parse_never_panics(s in "\\PC{0,24}") {
            let _ = Chord::parse_for(&s, false);
            let _ = Chord::parse_for(&s, true);
        }

        #[test]
        fn building_with_any_overrides_never_panics(
            entries in proptest::collection::btree_map("[a-z._]{0,20}", "\\PC{0,16}", 0..6),
        ) {
            let k = KeyMap::build_for(Keymap::Less, &entries, false);
            let _ = k.conflicts();
            let _ = k.lookup(Key::A, Modifiers::NONE);
        }
    }
}
