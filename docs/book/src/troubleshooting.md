# Troubleshooting

OxTail writes its own diagnostics to `oxtail.log` in the
[data folder](install.md#where-oxtail-keeps-its-data) (rotated at 1 MB to
`oxtail.log.1`) and to standard error. Set the environment variable
`OXTAIL_LOG=debug` for more detail. When you report a problem, that file is the
most useful thing to attach. You can even open it in OxTail.

## The window does not open, or is blank

OxTail draws with the GPU. On start-up it chooses a renderer:

1. **`auto`** (the default) probes for a GPU adapter for `wgpu` (Vulkan, Metal or
   DirectX 12) before any window exists. If there is none, it uses OpenGL. If
   `wgpu` is chosen but then fails to create the window, OxTail restarts itself with
   OpenGL (`--renderer glow`) once.
2. **`wgpu`** forces the `wgpu` renderer; **`glow`** forces OpenGL. Neither falls
   back.

Choose one with `--renderer glow` on the command line, or `renderer = "glow"` in
`settings.toml`. Try `glow` first when:

* you connect over **Remote Desktop**, VNC or X forwarding, or run in a virtual
  machine without a GPU;
* the window opens but stays black or white, or flickers;
* `oxtail.log` mentions a `wgpu` failure at start.

There is no separate software renderer. On a machine without a GPU, the operating
system's or Mesa's software OpenGL (for example `llvmpipe` on Linux) is what
makes `glow` work, so check that your system has OpenGL libraries installed. On Linux a system with neither
an X11 nor a Wayland session cannot show the window at all. OxTail is a desktop
program.

## Files on network shares

* **New lines appear late, or not at all.** File change notifications are not
  reliable on network file systems (SMB, NFS, sshfs), and their size information can be
  stale. OxTail always polls as well (every 250 ms while the file changes, up to
  about a second when it is quiet), so lines arrive, with a small delay. If the
  server caches file attributes for a long time, the delay is the server's.
* **Errors reading.** A dropped connection can make reads fail. The error is
  shown in a banner above the log (with a time in UTC) and OxTail keeps polling
  the file.
* OxTail opens files read-only and shared, so it never stops the machine that
  writes the log from appending or rotating.

## Read-only folders and no place for settings

If OxTail cannot write its data folder (an executable on a read-only share or
DVD, a locked-down account, or, on macOS, an app running from Downloads that macOS
has translocated to a temporary read-only location) it runs with **settings in
memory only**: everything works, but nothing is saved, and a notice says why.
Fixes:

* start with `--data-dir` pointing at a writable folder (a `--data-dir` on a USB
  stick works well);
* on macOS, move `OxTail.app` out of Downloads (to Applications, for example) and
  start it again;
* remove the `portable` marker or the `oxtail-data` folder next to the executable
  if you want the per-user folder instead.

The Settings window shows `Data folder: none (settings are not saved)` when this
is the case.

## Settings or profiles are ignored

* A profile or theme file that is not valid TOML, or has no `name`, is skipped, and
  a notice at start-up (and `oxtail.log`) says which file and why.
* A profile that never seems to apply: check `match_files` (a pattern without `/`
  is compared with the file name only) and `match_content` (a regular expression
  tried on the first lines), or pick it in the status bar.
* A rule that never highlights: a `regex` is case sensitive by default (add `(?i)`),
  and a column rule needs the file to be shown with a parser. See
  [Highlighting and profiles](profiles.md).
* Settings that say "stored, not used" in [Settings reference](settings.md) have no
  effect yet.

## A file looks wrong

* **Odd characters.** Pick the encoding in the status bar. Auto-detection looks at
  the first 64 KB and can be fooled by a file that is ASCII at the start.
* **A very long line is cut.** Lines longer than `max_display_line_length`
  characters (10,000 by default) are cut for display.
* **Line numbers with a `≈`.** The index is still being built; they become exact
  when the status bar stops saying `Indexing`.
* **Nothing shows for an empty file.** OxTail keeps following; lines appear when
  they are written.

## Memory and speed

Memory use does not grow with the file size, apart from the line index (about
2.5 MB for 10 GB) and the sets of matching lines of a search or filter. If OxTail
uses more than expected, lower `cache_size_mb`. Searching a large file for the
first time is limited by the speed of your disk; a second search is faster because
the operating system has the file cached.

## Standard input

`some_command | oxtail -` needs a place for its temporary spool file. It uses
`.oxtail-spool/` in the data folder, or the system temporary folder when the data
folder is not writable. A very long-running pipe grows that file, so watch the
free disk space of the folder.
