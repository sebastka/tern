#!/bin/sh
# Create the RPM sources in packaging/out/: the source tarball and the
# vendored Rust dependencies.
set -eu
top=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^Version: *//p' "$top/packaging/tern.spec")
out="$top/packaging/out"
mkdir -p "$out"
tar -czf "$out/tern-$version.tar.gz" -C "$top/.." \
    --exclude=target --exclude=build --exclude=.git --exclude=packaging/out --exclude=testenv/.demo \
    --transform "s|^$(basename "$top")|tern-$version|" "$(basename "$top")"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
(cd "$top" && cargo vendor --locked --versioned-dirs "$work/vendor" >/dev/null)
tar -czf "$out/tern-$version-vendor.tar.gz" -C "$work" vendor
ls -la "$out"
