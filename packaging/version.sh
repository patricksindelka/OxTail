#!/usr/bin/env bash
# Resolves the version being built and exports VERSION and NUMERIC_VERSION to
# $GITHUB_ENV (or prints them outside Actions). On a tag push the tag must
# match the oxtail-cli crate version.
set -euo pipefail

cargo_version=$(cargo metadata --no-deps --format-version 1 --locked \
  | jq -r '.packages[] | select(.name == "oxtail-cli") | .version')

if [ "${GITHUB_REF_TYPE:-}" = tag ]; then
  version="${GITHUB_REF_NAME#v}"
  if [ "$version" != "$cargo_version" ]; then
    echo "::error::tag ${GITHUB_REF_NAME} does not match the crate version $cargo_version"
    exit 1
  fi
else
  version="$cargo_version"
fi

# MSI and Info.plist need x.y.z without a pre-release suffix.
numeric="${version%%-*}"
if [ -n "${GITHUB_ENV:-}" ]; then
  { echo "VERSION=$version"; echo "NUMERIC_VERSION=$numeric"; } >> "$GITHUB_ENV"
fi
echo "Building OxTail $version"
