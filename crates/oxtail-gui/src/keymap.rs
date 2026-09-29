//! Keyboard shortcuts as a pure mapping from (key, modifiers) to an
//! [`Action`], so the table can be unit-tested without a UI.

use egui::{Key, Modifiers};

/// Something the user can trigger from the keyboard (or a menu).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Open a file (Ctrl+O).
    Open,
    /// Close the current tab (Ctrl+W).
    CloseTab,
    /// Next tab (Ctrl+Tab).
    NextTab,
    /// Previous tab (Ctrl+Shift+Tab).
    PrevTab,
    /// Show the find bar (Ctrl+F).
    Find,
    /// Next match (F3).
    FindNext,
    /// Previous match (Shift+F3).
    FindPrev,
    /// Show the filter panel (Ctrl+Shift+F).
    Filter,
    /// Go to line (Ctrl+G).
    GotoLine,
    /// Toggle a bookmark on the current line (Ctrl+F2).
    ToggleBookmark,
    /// Next bookmark (F2).
    NextBookmark,
    /// Previous bookmark (Shift+F2).
    PrevBookmark,
    /// Insert a visual mark after the last line (Ctrl+M).
    Mark,
    /// Copy the selected lines (Ctrl+C).
    Copy,
    /// Copy the selected lines with line numbers (Ctrl+Shift+C).
    CopyWithNumbers,
    /// Select all lines (Ctrl+A).
    SelectAll,
    /// Bigger font (Ctrl+= / Ctrl++).
    ZoomIn,
    /// Smaller font (Ctrl+-).
    ZoomOut,
    /// Default font size (Ctrl+0).
    ZoomReset,
    /// Toggle following the end of the file (F).
    ToggleFollow,
    /// Jump to the first line (Home, Ctrl+Home).
    ScrollTop,
    /// Jump to the last line and follow (End, Ctrl+End).
    ScrollBottom,
    /// One page up (PageUp).
    PageUp,
    /// One page down (PageDown).
    PageDown,
    /// One line up (Up).
    LineUp,
    /// One line down (Down).
    LineDown,
    /// Scroll left (Left).
    ScrollLeft,
    /// Scroll right (Right).
    ScrollRight,
    /// Close the find bar, dialogs and clear the selection (Esc).
    Escape,
    /// Toggle line wrapping (Alt+Z).
    ToggleWrap,
}

impl Action {
    /// Whether the shortcut may fire while a text field has keyboard focus
    /// (chords with Ctrl/Alt and function keys can; plain letters, arrows and
    /// paging keys belong to the text field then).
    pub fn works_in_text_fields(self) -> bool {
        matches!(
            self,
            Action::Open
                | Action::CloseTab
                | Action::NextTab
                | Action::PrevTab
                | Action::Find
                | Action::FindNext
                | Action::FindPrev
                | Action::Filter
                | Action::GotoLine
                | Action::ToggleBookmark
                | Action::NextBookmark
                | Action::PrevBookmark
                | Action::Mark
                | Action::ZoomIn
                | Action::ZoomOut
                | Action::ZoomReset
                | Action::ToggleWrap
                | Action::Escape
        )
    }
}

/// Maps a key press to an action.
pub fn map(key: Key, mods: Modifiers) -> Option<Action> {
    let cmd = mods.command;
    let shift = mods.shift;
    let alt = mods.alt;
    let plain = !cmd && !shift && !alt && !mods.ctrl;
    let action = match key {
        Key::O if cmd && !shift && !alt => Action::Open,
        Key::W if cmd && !shift && !alt => Action::CloseTab,
        Key::Tab if mods.ctrl && !alt => {
            if shift {
                Action::PrevTab
            } else {
                Action::NextTab
            }
        }
        Key::F if cmd && shift && !alt => Action::Filter,
        Key::F if cmd && !shift && !alt => Action::Find,
        Key::F3 if !cmd && !alt => {
            if shift {
                Action::FindPrev
            } else {
                Action::FindNext
            }
        }
        Key::G if cmd && !shift && !alt => Action::GotoLine,
        Key::F2 if cmd && !alt => Action::ToggleBookmark,
        Key::F2 if !cmd && !alt => {
            if shift {
                Action::PrevBookmark
            } else {
                Action::NextBookmark
            }
        }
        Key::M if cmd && !shift && !alt => Action::Mark,
        Key::C if cmd && shift && !alt => Action::CopyWithNumbers,
        Key::A if cmd && !shift && !alt => Action::SelectAll,
        Key::Equals | Key::Plus if cmd && !alt => Action::ZoomIn,
        Key::Minus if cmd && !alt => Action::ZoomOut,
        Key::Num0 if cmd && !shift && !alt => Action::ZoomReset,
        Key::Z if alt && !cmd && !shift => Action::ToggleWrap,
        Key::F if plain => Action::ToggleFollow,
        Key::Home if !alt && !shift => Action::ScrollTop,
        Key::End if !alt && !shift => Action::ScrollBottom,
        Key::PageUp if plain => Action::PageUp,
        Key::PageDown if plain => Action::PageDown,
        Key::ArrowUp if plain => Action::LineUp,
        Key::ArrowDown if plain => Action::LineDown,
        Key::ArrowLeft if plain => Action::ScrollLeft,
        Key::ArrowRight if plain => Action::ScrollRight,
        Key::Escape if plain => Action::Escape,
        _ => return None,
    };
    Some(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(command: bool, shift: bool, alt: bool) -> Modifiers {
        Modifiers {
            alt,
            ctrl: command,
            shift,
            mac_cmd: false,
            command,
        }
    }

    const NONE: Modifiers = Modifiers::NONE;

    #[test]
    fn documented_shortcuts() {
        let cmd = m(true, false, false);
        let cmd_shift = m(true, true, false);
        let shift = m(false, true, false);
        assert_eq!(map(Key::O, cmd), Some(Action::Open));
        assert_eq!(map(Key::W, cmd), Some(Action::CloseTab));
        assert_eq!(map(Key::Tab, cmd), Some(Action::NextTab));
        assert_eq!(map(Key::Tab, cmd_shift), Some(Action::PrevTab));
        assert_eq!(map(Key::F, cmd), Some(Action::Find));
        assert_eq!(map(Key::F, cmd_shift), Some(Action::Filter));
        assert_eq!(map(Key::F3, NONE), Some(Action::FindNext));
        assert_eq!(map(Key::F3, shift), Some(Action::FindPrev));
        assert_eq!(map(Key::G, cmd), Some(Action::GotoLine));
        assert_eq!(map(Key::F2, cmd), Some(Action::ToggleBookmark));
        assert_eq!(map(Key::F2, NONE), Some(Action::NextBookmark));
        assert_eq!(map(Key::F2, shift), Some(Action::PrevBookmark));
        assert_eq!(map(Key::M, cmd), Some(Action::Mark));
        assert_eq!(map(Key::C, cmd_shift), Some(Action::CopyWithNumbers));
        assert_eq!(map(Key::A, cmd), Some(Action::SelectAll));
        assert_eq!(map(Key::Equals, cmd), Some(Action::ZoomIn));
        assert_eq!(map(Key::Plus, cmd), Some(Action::ZoomIn));
        assert_eq!(map(Key::Minus, cmd), Some(Action::ZoomOut));
        assert_eq!(map(Key::Num0, cmd), Some(Action::ZoomReset));
        assert_eq!(map(Key::Z, m(false, false, true)), Some(Action::ToggleWrap));
    }

    #[test]
    fn navigation_keys() {
        assert_eq!(map(Key::F, NONE), Some(Action::ToggleFollow));
        assert_eq!(map(Key::Home, NONE), Some(Action::ScrollTop));
        assert_eq!(
            map(Key::Home, m(true, false, false)),
            Some(Action::ScrollTop)
        );
        assert_eq!(map(Key::End, NONE), Some(Action::ScrollBottom));
        assert_eq!(
            map(Key::End, m(true, false, false)),
            Some(Action::ScrollBottom)
        );
        assert_eq!(map(Key::PageUp, NONE), Some(Action::PageUp));
        assert_eq!(map(Key::PageDown, NONE), Some(Action::PageDown));
        assert_eq!(map(Key::ArrowUp, NONE), Some(Action::LineUp));
        assert_eq!(map(Key::ArrowDown, NONE), Some(Action::LineDown));
        assert_eq!(map(Key::ArrowLeft, NONE), Some(Action::ScrollLeft));
        assert_eq!(map(Key::ArrowRight, NONE), Some(Action::ScrollRight));
        assert_eq!(map(Key::Escape, NONE), Some(Action::Escape));
    }

    #[test]
    fn unrelated_chords_are_not_stolen() {
        assert_eq!(map(Key::F, m(false, true, false)), None);
        assert_eq!(map(Key::ArrowUp, m(false, true, false)), None);
        assert_eq!(map(Key::X, m(true, false, false)), None);
        assert_eq!(map(Key::G, NONE), None);
        assert_eq!(map(Key::W, m(true, true, false)), None);
    }

    #[test]
    fn plain_keys_are_left_to_text_fields() {
        assert!(!Action::ToggleFollow.works_in_text_fields());
        assert!(!Action::PageUp.works_in_text_fields());
        assert!(!Action::LineDown.works_in_text_fields());
        assert!(!Action::ScrollBottom.works_in_text_fields());
        assert!(Action::Find.works_in_text_fields());
        assert!(Action::FindNext.works_in_text_fields());
        assert!(Action::Escape.works_in_text_fields());
    }
}
