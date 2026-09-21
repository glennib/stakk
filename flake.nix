{
  description = "Bridge Jujutsu bookmarks to GitHub or Forgejo stacked pull requests";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      # `f` receives one system's package set; the result is keyed by system.
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        stakk = pkgs.callPackage ./nix/package.nix { };
        default = stakk;
      });

      # For consumers who assemble their own package set:
      #   nixpkgs.overlays = [ inputs.stakk.overlays.default ];
      overlays.default = final: _prev: {
        stakk = final.callPackage ./nix/package.nix { };
      };

      # `nix flake check` builds the package, and the package build runs the
      # unit tests — so the flake check is the whole verification.
      checks = forAllSystems (pkgs: {
        stakk = self.packages.${pkgs.system}.stakk;
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.cargo-nextest
            pkgs.clippy
            pkgs.jujutsu
            pkgs.rustc
            pkgs.rustfmt
          ];
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}
