# Themes

OxTail has three built-in themes, **dark**, **light** and **high-contrast**, and
follows the operating system's dark or light setting unless you choose one. Pick
a theme in **File > Settings...** (**Theme**) or with `theme` in
[`settings.toml`](settings.md). `theme = "system"` uses `dark` or `light`
depending on the system and switches when the system does.

A theme controls the colours of the window (the log area, the gutter, the
selection, the status bar) and the colours behind the **semantic names** that
[highlight rules](profiles.md#styles) use (`error`, `warn`, `accent1` ...).

## Your own theme

1. Create `<data folder>/themes/mytheme.toml`. See
   [Installation and portable mode](install.md) for the data folder.
2. Set `theme = "custom"` and `custom_theme = "mytheme"` in the settings, or pick
   **Custom** and the theme's name in the Settings window.

The `themes` folder is watched, so a saved change is applied immediately. A file
that is not valid TOML, or has no `name`, is skipped with a notice. A theme with
exactly the same `name` as a built-in theme (`dark`, `light`, `high-contrast`)
replaces it.

## Theme file reference

```toml
name = "dark"          # required; the name used by custom_theme
dark = true            # true for a dark theme: selects the base widget style

[ui]                   # colours of the window; each key is optional
background = "#1e1f22"
foreground = "#d4d4d4"
gutter_background = "#25262a"
gutter_foreground = "#6b6f76"
selection_background = "#264f78"
current_line_background = "#2a2d33"
search_match_background = "#5c4a00"
border = "#3a3d42"
accent = "#4d9de0"
status_bar_background = "#2b2d31"
status_bar_foreground = "#b8bcc4"

[semantic]             # colours for the names used by highlight rules
fatal = "#ff6b81"
"fatal.subtle" = "#4a1d26"
error = "#f7768e"
"error.subtle" = "#3d1f27"
warn = "#e0af68"
"warn.subtle" = "#3a3020"
info = "#7aa2f7"
"info.subtle" = "#1f2a44"
success = "#9ece6a"
debug = "#8b93a1"
trace = "#6b7280"
muted = "#7a808c"
accent1 = "#4d9de0"
accent2 = "#bb9af7"
accent3 = "#2ac3de"
accent4 = "#ff9e64"
```

Colours are written `#rrggbb` (`#rgb` also works). Any key you leave out
takes the value of the dark theme (for `[ui]`) or of OxTail's built-in palette
(for `[semantic]`), so a small file is enough:

```toml
name = "my-dark"
dark = true

[ui]
background = "#101014"
accent = "#ff8800"

[semantic]
error = "#ff5555"
```

### The `[ui]` keys

| Key | Colours |
|---|---|
| `background`, `foreground` | The log text area and its default text. |
| `gutter_background`, `gutter_foreground` | The line number gutter. |
| `selection_background` | Selected lines. |
| `current_line_background` | The current or hovered line. |
| `search_match_background` | Search matches (the current match is drawn with a stronger version of this colour). |
| `border` | Borders and separators. |
| `accent` | Focus, the active tab, bookmarks. |
| `status_bar_background`, `status_bar_foreground` | The status bar. |

### The `[semantic]` keys

A rule's `fg`, `bg` and `minimap` colours name an entry here. The names are
`error`, `warn`, `info`, `debug`, `trace`, `success`, `muted` and `accent1` to
`accent8`, each with an optional `.subtle` variant for backgrounds (write it as a
quoted key: `"error.subtle"`). `fatal` and `fatal.subtle` exist in the built-in
themes, but rules that say `fatal` are currently drawn like `error`. A name your
theme does not define comes from OxTail's built-in palette for the light, dark or
high-contrast look (a `.subtle` colour there is a faint version of the solid
colour). A rule can also use a fixed `#rrggbb`, which is the same on every theme.

OxTail's built-in palettes are tested for readable contrast (WCAG). If you write
your own theme, check that error and warning text stays readable on `background`,
and that `.subtle` backgrounds do not swallow the text colour.
