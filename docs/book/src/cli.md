# Command line

```text
oxtail [OPTIONS] [FILE|-]...
```

`FILE` is a log file to open in a tab. `-` reads standard input. With no
arguments OxTail starts with its session (or an empty window).

```sh
oxtail app.log                          # open and follow
oxtail -n 500 app.log                   # start 500 lines from the end
oxtail --profile nginx access.log
oxtail --filter ERROR *.log              # only lines containing ERROR
oxtail --filter '?level:ERROR' *.log     # a column query
oxtail --merge worker-1.log worker-2.log
some_cmd | oxtail -                     # view standard input
```

## Options

| Option | Meaning |
|---|---|
| `-n N`, `--lines N` | Start with the view `N` lines from the end of each file, instead of the default tail view. Following is paused until you scroll to the end. `+N` is accepted as `N`. |
| `--profile NAME` | Use a highlight profile for the opened files. `NAME` is resolved against your profiles and the built-in ones: the exact name, the name ignoring case, the file name without `.toml` (`nginx-access`), or a unique prefix of the name, a word in it or the file name (`nginx`). If several profiles match, or none, OxTail prints the matching or available profile names and exits with status 2 before opening a window. See [Highlighting and profiles](profiles.md). |
| `--filter QUERY` | Filter the opened files. `QUERY` is one filter row in the [filter string grammar](filters.md#the-filter-string-grammar): plain text keeps lines containing it, `-text` excludes, `/regex/r` is a regular expression, and a leading `?` makes it a column query such as `'?level:ERROR status>=500'`. |
| `--merge` | Open all the given files as one merged tab, interleaved by timestamp (needs at least two files; with one, it opens normally). See [Time and multiple files](time.md#merged-view). |
| `--data-dir PATH` | Use `PATH` as the data folder (settings, profiles, themes, session). See [Installation and portable mode](install.md). |
| `--renderer R` | `auto` (default), `wgpu` or `glow` (`opengl` and `gl` are accepted for `glow`). Overrides the `renderer` setting. See [Troubleshooting](troubleshooting.md#the-window-does-not-open-or-is-blank). |
| `--new-instance` | Do not hand the files to a running instance; start a new window even if one is running for the same data folder. |
| `--integrate` | Register OxTail with the system ("Open with" entry, Start menu shortcut or desktop entry), print what was done and exit. See [Integration and updates](install.md#integration-and-updates). |
| `--remove-integration` | Undo `--integrate` exactly, print what was removed and exit. Does nothing if OxTail is not integrated. |
| `--check-update` | Ask GitHub whether a newer release exists (needs `curl`), print the result (newer, up to date, or no release published yet) and exit. |
| `-V`, `--version` | Print the version (for example `oxtail 1.2.0`; `0.0.0-dev` for a build not made from a release) and exit. |
| `-h`, `--help` | Print the usage text and exit. |

Values can be given as `--lines 500`, `--lines=500` or, for short options, `-n500`.
Relative file names are made absolute from the current folder before they are
handed on. A file that does not exist opens as a tab with an error message.

`--profile`, `--filter` and `-n` apply to every file given in that command.

## Single instance

If OxTail is already running with the same data folder, the files (and `--profile`,
`--filter`, `-n`, `--merge`) are sent to it and the new process exits at once with
status 0. The running window gets the tabs and comes to the front. A run that reads
standard input (`-`) never does this: it owns the input, so it always starts its
own window. `--new-instance` forces a separate window for file arguments too.

Because the local socket name comes from the data folder, copies with different
data folders (for example two portable copies) never interfere.

## Exit status

| Status | Meaning |
|---|---|
| 0 | Normal exit, `--help`, `--version`, or files handed to a running instance. |
| 1 | The program could not start (for example no window could be created), or `--integrate`, `--remove-integration` or `--check-update` failed. The error is printed to standard error. `--check-update` exits 0 whether or not a newer release exists. |
| 2 | Invalid command line. The message and `Try 'oxtail --help'` are printed to standard error. |

On Windows, release builds are GUI-subsystem programs, so when started from a
terminal `--help`, `--version`, error messages and the text of `--integrate`,
`--remove-integration` and `--check-update` may not be printed there. Redirect the
output to see it (`oxtail --check-update | more`, `oxtail --integrate > out.txt`) or
rely on the exit status; the Settings window has buttons for integration.
`--integrate`, `--remove-integration` and `--check-update` cannot be combined
with file arguments (status 2).

## Environment

| Variable | Meaning |
|---|---|
| `OXTAIL_LOG` | Log filter for OxTail's own diagnostics, in `tracing` `EnvFilter` syntax (`debug`, `oxtail_core=trace`). `RUST_LOG` is used if it is not set; the default is `info`. The log goes to standard error and to `oxtail.log` in the data folder. |
