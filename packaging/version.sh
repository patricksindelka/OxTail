#!/usr/bin/env bash
# Resolves the version being built and exports VERSION and NUMERIC_VERSION to
# $GITHUB_ENV (or prints them outside Actions).
#
# The release tag is the only place a version is set. The repository keeps
# 0.0.0-dev; on a `vX.Y.Z` tag push this script writes X.Y.Z into Cargo.toml
# ([workspace.package]), Cargo.lock (the workspace crates) and the AppStream
# metainfo (<release> entry) before anything is built, so the binaries, the
# MSI, Info.plist and the packages all carry the tag's version. Nothing is
# committed back. Any other build reports the repository's 0.0.0-dev.
set -euo pipefail

metainfo=packaging/linux/io.github.patricksindelka.OxTail.metainfo.xml

crate_version() {
  cargo metadata --no-deps --format-version 1 --locked \
    | jq -r '.packages[] | select(.name == "oxtail-cli") | .version'
}

# Rewrites $1 with the awk program $2 (portable: no `sed -i`, which differs
# between GNU and BSD). The programs drop a trailing CR first: a Windows
# checkout may have CRLF line endings, which exact-line matches would miss.
rewrite() {
  local file=$1 tmp
  shift
  tmp=$(mktemp)
  awk "$@" "$file" > "$tmp"
  mv "$tmp" "$file"
}

if [ "${GITHUB_REF_TYPE:-}" = tag ]; then
  version="${GITHUB_REF_NAME#v}"
  if ! [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
    echo "::error::tag ${GITHUB_REF_NAME} is not vMAJOR.MINOR.PATCH (optionally -prerelease)"
    exit 1
  fi
  old=$(crate_version)
  # Cargo.toml: the `version` line of [workspace.package].
  rewrite Cargo.toml -v v="$version" '
    { sub(/\r$/, "") }
    /^\[/ { in_pkg = ($0 == "[workspace.package]") }
    in_pkg && /^version[[:space:]]*=/ { print "version = \"" v "\""; next }
    { print }'
  # Cargo.lock: the workspace crates are the packages at the old version
  # without a `source` (registry and git packages have one).
  rewrite Cargo.lock -v old="$old" -v v="$version" '
    function flush() {
      for (i = 1; i <= n; i++) {
        if (i == vline && !has_source) print "version = \"" v "\""
        else print buf[i]
      }
      n = 0; vline = 0; has_source = 0
    }
    { sub(/\r$/, "") }
    /^\[\[package\]\]$/ { flush() }
    { buf[++n] = $0 }
    $0 == "version = \"" old "\"" { vline = n }
    /^source = / { has_source = 1 }
    END { flush() }'
  # AppStream wants a <release> for the shipped version; newest first.
  if ! grep -q "<release version=\"$version\"" "$metainfo"; then
    rewrite "$metainfo" -v v="$version" -v d="$(date -u +%Y-%m-%d)" '
      { print }
      /<releases>/ && !done { print "    <release version=\"" v "\" date=\"" d "\"/>"; done = 1 }'
  fi
  # A full resolve checks Cargo.lock too (--no-deps would not).
  stamped=$(crate_version)
  if [ "$stamped" != "$version" ] || ! cargo metadata --format-version 1 --locked > /dev/null; then
    echo "::error::writing $version into Cargo.toml/Cargo.lock failed (cargo reports $stamped)"
    exit 1
  fi
else
  version=$(crate_version)
fi

# MSI and Info.plist need x.y.z without a pre-release suffix.
numeric="${version%%-*}"
if [ -n "${GITHUB_ENV:-}" ]; then
  { echo "VERSION=$version"; echo "NUMERIC_VERSION=$numeric"; } >> "$GITHUB_ENV"
fi
echo "Building OxTail $version"
