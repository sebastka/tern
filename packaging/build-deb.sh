#!/bin/sh
# Build a .deb for Debian 13 "trixie" or newer (Qt >= 6.8), for the
# architecture of the machine it runs on. Meant for a clean container:
#
#   docker run --rm -v "$PWD:/src" -w /src debian:trixie packaging/build-deb.sh
#
# Installs the build dependencies (needs root), a current Rust through rustup
# when the distribution's is older than Tern's MSRV, builds with CMake, and
# packages the result with dpkg-deb. The .deb ends up in packaging/out/.
set -eu
top=$(cd "$(dirname "$0")/.." && pwd)
cd "$top"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
revision=${DEB_REVISION:-1}
arch=$(dpkg --print-architecture)
msrv=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml | head -1)

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
    build-essential cmake ninja-build pkg-config git curl ca-certificates file dpkg-dev \
    qt6-base-dev qt6-webengine-dev libgl-dev gnupg >/dev/null

# Rust: the distribution's if new enough, else rustup (build-time only).
if ! command -v cargo >/dev/null || ! printf '%s\n%s\n' "$msrv" "$(rustc -V | cut -d' ' -f2)" | sort -V -C; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
    . "$HOME/.cargo/env"
fi

build=build-deb
cmake -B "$build" -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr
cmake --build "$build"
stage=$(mktemp -d)
DESTDIR="$stage" cmake --install "$build" >/dev/null
strip --strip-unneeded "$stage/usr/bin/tern"
install -Dm644 LICENSE-MIT "$stage/usr/share/doc/tern/LICENSE-MIT"
install -Dm644 LICENSE-APACHE "$stage/usr/share/doc/tern/LICENSE-APACHE"
install -Dm644 README.md "$stage/usr/share/doc/tern/README.md"

# Shared-library dependencies, the Debian way (dpkg-shlibdeps wants a
# debian/control next to it).
work=$(mktemp -d)
mkdir -p "$work/debian"
printf 'Source: tern\n\nPackage: tern\nArchitecture: any\n' > "$work/debian/control"
shlibs=$(cd "$work" && dpkg-shlibdeps -O "$stage/usr/bin/tern" 2>/dev/null | sed -n 's/^shlibs:Depends=//p')
# The SVG app icon needs Qt's SVG plugins; OpenPGP needs gpg.
svg_pkg=$(apt-cache show qt6-svg-plugins >/dev/null 2>&1 && echo qt6-svg-plugins || echo libqt6svg6)

mkdir -p "$stage/DEBIAN"
cat > "$stage/DEBIAN/control" <<EOF
Package: tern
Version: $version-$revision
Architecture: $arch
Maintainer: Sebastian Karlsen <sebastian@karlsen.fr>
Installed-Size: $(du -sk "$stage" | cut -f1)
Depends: $shlibs, $svg_pkg, gnupg
Recommends: xdg-desktop-portal
Section: mail
Priority: optional
Homepage: https://github.com/sebastka/tern
Description: Mail client for IMAP and SMTP with offline support and OpenPGP
 Tern is a desktop mail client that only speaks standard protocols (IMAP and
 SMTP). It keeps a full offline copy of your mail, is configured through
 plain TOML files, and uses the system gpg for OpenPGP.
EOF

mkdir -p packaging/out
deb="packaging/out/tern_${version}-${revision}_${arch}.deb"
dpkg-deb --root-owner-group --build "$stage" "$deb" >/dev/null
rm -rf "$stage" "$work"
ls -l "$deb"
