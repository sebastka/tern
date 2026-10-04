# Fedora package for Tern. Rust dependencies are vendored (Source1), so the
# build works offline: run packaging/make-sources.sh to create both tarballs.
Name:           tern
Version:        0.1.0
Release:        2%{?dist}
Summary:        Mail client for IMAP and SMTP with offline support and OpenPGP
License:        MIT OR Apache-2.0
URL:            https://github.com/sebastka/tern
Source0:        %{name}-%{version}.tar.gz
Source1:        %{name}-%{version}-vendor.tar.gz

BuildRequires:  cmake >= 3.24
BuildRequires:  ninja-build
BuildRequires:  gcc-c++ >= 14
BuildRequires:  cargo
BuildRequires:  rust >= 1.89
BuildRequires:  corrosion
BuildRequires:  cmake(Qt6Core) >= 6.8
BuildRequires:  cmake(Qt6DBus)
BuildRequires:  cmake(Qt6Widgets)
BuildRequires:  cmake(Qt6WebEngineCore)
BuildRequires:  cmake(Qt6WebEngineWidgets)
BuildRequires:  desktop-file-utils

# OpenPGP goes through the system gpg (ARCHITECTURE.md §11).
Requires:       gnupg2
# The app icon is SVG.
Requires:       qt6-qtsvg
Recommends:     xdg-desktop-portal

# QtWebEngine exists only on these.
ExclusiveArch:  x86_64 aarch64

%description
Tern is a desktop mail client that only speaks standard protocols (IMAP and
SMTP). It keeps a full offline copy of your mail, is configured through
plain TOML files, and uses the system gpg for OpenPGP.

%prep
%autosetup -n %{name}-%{version}
tar -xzf %{SOURCE1}
mkdir -p .cargo
cat > .cargo/config.toml <<'CARGO'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"

[net]
offline = true
CARGO

%build
%cmake -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo
%cmake_build

%install
%cmake_install

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/fr.karlsen.Tern.desktop
# Unit tests; the server integration tests skip themselves without servers.
# Reuse what the CMake build compiled: same target directory and the same
# explicit --target as Corrosion, so mostly the test code itself is compiled.
target_dir=$(ls -d %{__cmake_builddir}/cargo/*/ | head -1)
triple=$(rustc -vV | sed -n 's/^host: //p')
cargo test --workspace --offline --release --target "$triple" --target-dir "$target_dir"

%files
%license LICENSE-MIT LICENSE-APACHE
%doc README.md ARCHITECTURE.md DECISIONS.md
%{_bindir}/tern
%{_datadir}/applications/fr.karlsen.Tern.desktop
%{_datadir}/icons/hicolor/scalable/apps/fr.karlsen.Tern.svg

%changelog
* Sun Oct 04 2026 Sebastian Karlsen <sebastian@karlsen.fr> - 0.1.0-2
- Plain text, Markdown and HTML composing; signatures
- Configurable archive folders; opening attachments
- New application icon

* Sun Oct 04 2026 Sebastian Karlsen <sebastian@karlsen.fr> - 0.1.0-1
- First package
