#!/usr/bin/env bash
# Sets the package version in Cargo.toml and the matching Cargo.lock entry.
# Usage: set-version.sh X.Y.Z
set -euo pipefail
next="$1"
[[ "${next}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "bad version: ${next}" >&2; exit 1; }
# First `version = "x.y.z"` line is the [package] version.
sed -i -E "0,/^version = \"[0-9]+\.[0-9]+\.[0-9]+\"/s//version = \"${next}\"/" Cargo.toml
if [ -f Cargo.lock ]; then
  awk -v nv="${next}" '
    /^name = "ferrocord"$/ { print; getline; if ($0 ~ /^version = /) { print "version = \"" nv "\""; next } }
    { print }' Cargo.lock > Cargo.lock.new
  mv Cargo.lock.new Cargo.lock
fi
