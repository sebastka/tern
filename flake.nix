{
  description = "Tern, a mail client for IMAP and SMTP with offline support and OpenPGP";

  # A stable release: only fixes between lock updates, and the same Qt/WebEngine
  # as most NixOS systems. Needs Rust >= 1.89 and Qt >= 6.8 (26.05: 1.95, 6.11).
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        tern = pkgs.callPackage ./packaging/nix/package.nix { };
        default = tern;
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.tern ];
          packages = with pkgs; [
            clippy
            rustfmt
            rust-analyzer
          ];
        };
      });
    };
}
