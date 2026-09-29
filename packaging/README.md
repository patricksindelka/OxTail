# Packaging and releases

Everything needed to turn a tag into portable archives, installers and package
manager manifests (PLAN.md 3.3, M6). Layout:

| Path | Purpose |
|---|---|
| `../.github/workflows/ci.yml` | Builds, tests and packages every push; publishes on `v*` tags |
| `version.sh` | Resolves the version (and checks it against the tag) for the workflow |
| `icons/` | App icon: `oxtail.svg`, PNGs, `oxtail.ico`, `oxtail.icns`. `make_icons.py` regenerates the raster files from the geometry in `crates/oxtail-gui/src/icon.rs` (there is no other icon source) |
| `linux/` | `.desktop` file, AppStream metainfo, `nfpm.yaml` (deb and rpm) |
| `windows/` | `oxtail.wxs` (WiX 3 MSI source), `sign.ps1` (signtool wrapper) |
| `macos/Info.plist` | Bundle template (`@VERSION@` is filled in by the workflow) |
| `flatpak/` | Manifest for a later Flathub submission (not built in CI) |
| `winget/`, `homebrew/`, `scoop/` | Manifest templates with `@PLACEHOLDERS@` |
| `render-manifests.py` | Fills the templates from a release's `SHA256SUMS` |

The application id (bundle id, Flatpak id, desktop file name, AppStream id) is
`io.github.patricksindelka.OxTail`, the form Flathub accepts for GitHub-hosted projects.

## Cutting a release

1. Bump `version` in the root `Cargo.toml` (`[workspace.package]`) and add a
   `<release version="X.Y.Z" date="...">` entry to
   `linux/io.github.patricksindelka.OxTail.metainfo.xml`. Commit.
2. Start a full CI run first: Actions, "CI", "Run workflow", keep "full" ticked.
   It tests on all three platforms and builds every package as workflow artifacts
   (`oxtail-Linux`, `oxtail-Windows`, `oxtail-macOS`) without publishing. Ordinary
   pushes only run Linux checks, to save Actions minutes (macOS minutes count 10x,
   Windows 2x on private repositories).
3. Tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`. CI runs in full on
   the tag (`version.sh` fails if the tag differs from the crate version), then
   the `publish` job creates the GitHub Release (pre-release if the version
   contains `-`) with all packages, `SHA256SUMS` and the rendered manifests
   (`oxtail-X.Y.Z-manifests.tar.gz`). Only tags publish.
4. Submit the package manager manifests by hand from that archive (see below).

## Artifacts

| Platform | Files |
|---|---|
| Linux x86_64 (glibc 2.28 via `cargo zigbuild`) | `oxtail-V-x86_64-linux.tar.gz` (binary + `portable` marker + README), `oxtail-V-x86_64.AppImage`, `oxtail_V_amd64.deb`, `oxtail-V-1.x86_64.rpm` |
| Windows x86_64 (MSVC, static CRT) | `oxtail-V-x86_64-windows.zip` (exe + `portable` marker + README), `oxtail-V-x86_64.msi` |
| macOS (arm64 + x86_64 via `lipo`, deployment target 11.0) | `oxtail-V-macos-universal.zip` (`OxTail.app`), `oxtail-V-macos-universal.dmg` |
| all | `SHA256SUMS` |

Every job runs `xtask check-deps --binary` on the built binary before packaging
(macOS: on each thin binary, because `check-deps` does not read fat Mach-O files).
The Linux job also fails if the binary references a glibc symbol newer than 2.28.

Data location of each artifact (PLAN.md 3.2):

- Portable `.zip` / `.tar.gz` contain a `portable` marker, so settings live in
  `oxtail-data/` next to the executable.
- MSI, `.deb`, `.rpm`, AppImage, Flatpak and the macOS app have no marker: installed
  mode (`%APPDATA%\OxTail`, `~/.config/oxtail`, `~/Library/Application Support/OxTail`).
  The AppImage and the `.app` are (signed) read-only bundles, so a marker cannot
  be placed next to their executable; use `--data-dir` for a portable data folder.

### Installer rules (PLAN.md 3.1)

- The MSI installs `oxtail.exe` and a Start menu shortcut and nothing else: no file
  associations, no context menu entries, no PATH change, and no registry writes other
  than Windows Installer's own bookkeeping. Integration is an opt-in action in the app.
- The MSI is per-machine (`InstallScope="perMachine"`, needs elevation, installs to
  `Program Files\OxTail`). The Windows Installer public property `ALLUSERS` is
  therefore fixed to 1 by the package; there is no per-user MSI. Per-user, no-admin
  installs are served by the portable `.zip` and Scoop. Use
  `msiexec /i oxtail-V-x86_64.msi INSTALLFOLDER="D:\Tools\OxTail"` to change the folder.
- The `.desktop` file lists `MimeType=text/x-log;text/plain;` so OxTail appears in
  "Open With"; nothing sets it as the default handler.

## Signing and notarization

Steps are conditional on secrets (Settings, Secrets and variables, Actions). Without
them the jobs succeed and produce unsigned artifacts (with a warning annotation);
on macOS the app is then only ad-hoc signed, which is required for arm64 to run.

| Secret | Content | Used for |
|---|---|---|
| `WINDOWS_CERT_PFX_BASE64` | Base64 of the code-signing `.pfx` (`base64 -w0 cert.pfx`) | signtool signs `oxtail.exe` (before zip and MSI) and the MSI |
| `WINDOWS_CERT_PASSWORD` | Password of the `.pfx` | same |
| `MACOS_CERT_P12_BASE64` | Base64 of the "Developer ID Application" certificate exported as `.p12` | codesign of the app (hardened runtime) and the DMG |
| `MACOS_CERT_PASSWORD` | Password of the `.p12` | same |
| `MACOS_SIGN_IDENTITY` | Identity name, e.g. `Developer ID Application: Name (TEAMID)` | codesign |
| `APPLE_ID` | Apple ID e-mail | `notarytool submit` |
| `APPLE_TEAM_ID` | 10 character team id | `notarytool submit` |
| `APPLE_APP_PASSWORD` | App-specific password for that Apple ID | `notarytool submit` |

The optional environment variable `WINDOWS_TIMESTAMP_URL` defaults to DigiCert's
RFC 3161 server. Notarization needs all three `APPLE_*` secrets plus the
certificate; with only the certificate the app and DMG are signed but not
notarized. The app is notarized and stapled before the zip and DMG are made from it.
The `.deb`, `.rpm`, AppImage and tarball are not signed (publish `SHA256SUMS`;
GPG-signing them is an open decision).

## Package manager manifests

On a tag, the `publish` job runs `render-manifests.py` and attaches the result to
the release as `oxtail-X.Y.Z-manifests.tar.gz`. Submitting is manual:

- **winget**: copy `winget/*.yaml` into a `winget-pkgs` fork at
  `manifests/o/OxTail/OxTail/<version>/` (or run `wingetcreate update OxTail.OxTail
  --version <v> --urls <msi url>`) and open a PR. The MSI `ProductCode` is read from
  the built MSI by the Windows job; it changes with every build (`Product Id="*"`).
- **Homebrew**: put `homebrew/oxtail.rb` into a tap (`homebrew-oxtail`) as
  `Casks/oxtail.rb`, or submit to `homebrew-cask`. It installs the DMG and links the
  `oxtail` command.
- **Scoop**: put `scoop/oxtail.json` into a bucket. It installs the portable zip, so
  the `portable` marker is active and `"persist": "oxtail-data"` keeps settings
  across updates (Scoop moves `oxtail-data` to `persist/oxtail` and re-links it).
- **Flathub**: see the header of `flatpak/io.github.patricksindelka.OxTail.yml`
  (needs `cargo-sources.json` and screenshots in the metainfo).

Render locally from a downloaded release:

```sh
python3 packaging/render-manifests.py --version 0.1.0 --sums SHA256SUMS --out out \
  --product-code '{GUID-from-the-built-msi}'
```

## Testing locally

```sh
# .desktop and AppStream
desktop-file-validate packaging/linux/io.github.patricksindelka.OxTail.desktop
appstreamcli validate --no-net packaging/linux/io.github.patricksindelka.OxTail.metainfo.xml

# .deb / .rpm from any built oxtail binary (go install github.com/goreleaser/nfpm/v2/cmd/nfpm@v2.41.3)
mkdir -p dist/stage && cp target/release/oxtail dist/stage/oxtail
VERSION=0.1.0 nfpm package -f packaging/linux/nfpm.yaml -p deb -t dist/
dpkg-deb -c dist/oxtail_0.1.0_amd64.deb

# Manifests
python3 packaging/render-manifests.py --help

# macOS bundle (on a Mac, after cargo build --release)
mkdir -p dist/OxTail.app/Contents/{MacOS,Resources}
cp target/release/oxtail dist/OxTail.app/Contents/MacOS/
sed s/@VERSION@/0.1.0/ packaging/macos/Info.plist > dist/OxTail.app/Contents/Info.plist
cp packaging/icons/oxtail.icns dist/OxTail.app/Contents/Resources/OxTail.icns
codesign --force --sign - dist/OxTail.app
```

The MSI (WiX 3: `candle -arch x64 -dVersion=0.1.0 -dBinDir=<dir> -dIconDir=packaging\icons
packaging\windows\oxtail.wxs`, then `light`) can only be built on Windows. The workflow
is the reference for the exact commands.
