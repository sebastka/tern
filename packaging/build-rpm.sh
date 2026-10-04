#!/bin/sh
# Build the Fedora RPM in a container, for x86_64 or (emulated) aarch64.
#
#   packaging/build-rpm.sh [x86_64|aarch64]     (default: the host's arch)
#
# Needs Docker; aarch64 on an x86_64 host also needs qemu-user-static.
# Emulated builds skip the test suite (%check), which would compile the
# workspace again under emulation; set TERN_RPM_CHECK=1 to run it anyway.
# If an image tern-build:f44-<arch> exists (Fedora 44 with the build
# dependencies already installed, made with `docker commit`), it is used and
# the slow dependency installation is skipped. Results go to packaging/out/.
set -eu
top=$(cd "$(dirname "$0")/.." && pwd)
arch=${1:-$(uname -m)}
case $arch in
    x86_64) platform=linux/amd64 ;;
    aarch64) platform=linux/arm64 ;;
    *) echo "unsupported arch: $arch" >&2; exit 1 ;;
esac

"$top/packaging/make-sources.sh" >/dev/null

check=
if [ "$arch" != "$(uname -m)" ] && [ "${TERN_RPM_CHECK:-0}" != 1 ]; then
    check=--nocheck
fi

image=tern-build:f44-$arch
docker image inspect "$image" >/dev/null 2>&1 || image=fedora:44

docker run --rm --platform "$platform" --network host \
    -v "$top/packaging:/pkg:ro" -v "$top/packaging/out:/out" "$image" bash -c "
set -e
rm -rf ~/rpmbuild
mkdir -p ~/rpmbuild/SOURCES
cp /out/*.tar.gz ~/rpmbuild/SOURCES/
command -v rpmbuild >/dev/null || dnf -y -q install rpm-build dnf-plugins-core
dnf -y -q builddep /pkg/tern.spec
rpmbuild -ba $check /pkg/tern.spec > /out/rpmbuild-$arch.log 2>&1 || { tail -40 /out/rpmbuild-$arch.log; exit 1; }
cp ~/rpmbuild/RPMS/*/*.rpm /out/
"
ls -l "$top"/packaging/out/*."$arch".rpm
