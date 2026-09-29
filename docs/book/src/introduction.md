# Introduction

OxTail is a fast, portable, real-time log viewer for Windows, macOS and Linux, a
modern successor to BareTail. It opens very large files without loading them,
follows them as they grow, searches and filters them in the background,
highlights them with rules, and can show structured logs as columns.

This guide describes what OxTail does today. Pages that list options (settings,
profiles, command line, shortcuts) were written from the code, and the profile
gallery is generated from the profile files themselves.

## What it does

* **Opens big files instantly.** Only the part you look at is read. Memory does
  not grow with the size of the file; a sparse index of one checkpoint per 64 KB
  is all that is kept.
* **Follows files.** New lines appear as they are written. Truncation and
  rotation are detected, and OxTail never locks the file, so the program that
  writes the log is never blocked. See [Opening and following files](opening.md).
* **Searches and filters.** Incremental find with a match minimap, and stacked
  include and exclude filters that keep following. See [Search](search.md) and
  [Filters](filters.md).
* **Highlights.** Rules per line, match, capture group or column, grouped into
  profiles that are picked automatically by file name or content. See
  [Highlighting and profiles](profiles.md) and the [profile gallery](gallery/index.md).
* **Understands structure.** CSV, regex, JSON Lines, logfmt, syslog, Apache and
  Nginx, IIS (W3C), log4j patterns and fixed width become columns, with a query
  language, statistics and export. See [Columns](columns.md).
* **Handles time.** Go to a time, show gaps, relative times, and merge several
  files into one view ordered by timestamp. See [Time and multiple files](time.md).
* **Stays out of the way.** One executable, no installer needed, no registry
  writes, no files outside its data folder. See
  [Installation and portable mode](install.md).

The interface is a native window drawn with `egui`. It cannot be shown on a
server without a display; see [Troubleshooting](troubleshooting.md) for remote
desktops and machines without a GPU.

## Where to go next

* New here: [Installation and portable mode](install.md), then
  [Opening and following files](opening.md).
* Looking for a shortcut: [Keyboard shortcuts](shortcuts.md).
* Teaching OxTail about your log format: [Highlighting and profiles](profiles.md)
  and the [profile gallery](gallery/index.md).
* Developing OxTail: [docs/dev.md](https://github.com/patricksindelka/OxTail/blob/main/docs/dev.md)
  in the repository.

## Coming in 1.0

Some things in the project plan are not built yet. They are listed here; other
pages only point back to this list (the settings reference marks the keys that
are stored but not used yet). Nothing else in this guide describes a feature that
is missing.

* Signed installers and package manager entries (MSI, DMG, deb, rpm, AppImage,
  Flatpak, winget, Homebrew, Scoop). Portable builds and building from source are
  what this guide describes.
* Importing and exporting all settings as one archive, and importing BareTail
  highlight configuration.
* Custom fonts (`font_family` is stored but not used), a hex view for binary
  data, and showing whitespace and control characters.
* Remote sources (SSH, containers, journald), compressed files, a histogram
  timeline and the other ideas of the plan's post-1.0 list.
