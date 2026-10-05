{
  description = "Phomemo PAPPL Printer Application";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (
        _: pkgs: {
          default = pkgs.callPackage ./package.nix { };
        }
      );

      # Building the package also runs the Rust tests.
      checks = forAllSystems (
        system: _: {
          package = self.packages.${system}.default;
        }
      );

      formatter = forAllSystems (_: pkgs: pkgs.nixfmt);

      # The package's toolchain, plus what `make check` adds.
      devShells = forAllSystems (
        system: pkgs: {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.default ];
            packages = with pkgs; [
              clippy
              rustfmt
            ];
          };
        }
      );
    };
}
