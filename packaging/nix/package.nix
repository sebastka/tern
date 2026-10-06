# Tern for Nix: `nix build github:sebastka/tern`, or
# `nix profile install github:sebastka/tern`.
{
  lib,
  stdenv,
  rustPlatform,
  cargo,
  rustc,
  cmake,
  ninja,
  pkg-config,
  corrosion,
  qt6,
  libcanberra,
  gnupg,
}:

let
  root = ../..;
in
stdenv.mkDerivation {
  pname = "tern";
  version = (lib.importTOML (root + "/Cargo.toml")).workspace.package.version;

  # The source tree without build output.
  src = lib.cleanSourceWith {
    src = lib.cleanSource root;
    filter =
      path: _type:
      let
        rel = lib.removePrefix (toString root + "/") (toString path);
      in
      !(lib.elem rel [
        "target"
        "build"
        "packaging/out"
        "testenv/.demo"
        "result"
      ]);
  };

  # Rust dependencies from Cargo.lock (fetched by Nix, used offline).
  cargoDeps = rustPlatform.importCargoLock { lockFile = root + "/Cargo.lock"; };

  nativeBuildInputs = [
    cmake
    ninja
    pkg-config
    cargo
    rustc
    rustPlatform.cargoSetupHook
    corrosion
    qt6.wrapQtAppsHook
  ];

  buildInputs = [
    qt6.qtbase
    qt6.qtwebengine
    qt6.qtsvg
    libcanberra
  ];

  cmakeFlags = [ (lib.cmakeFeature "CMAKE_BUILD_TYPE" "Release") ];

  # OpenPGP goes through the gpg binary (ARCHITECTURE.md §11).
  qtWrapperArgs = [ "--prefix PATH : ${lib.makeBinPath [ gnupg ]}" ];

  meta = {
    description = "Mail client for IMAP and SMTP with offline support and OpenPGP";
    homepage = "https://github.com/sebastka/tern";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "tern";
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
    ];
  };
}
