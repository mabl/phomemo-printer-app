{
  description = "Phomemo PAPPL Printer Application";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${system});
    in
    {
      overlays.default = final: _prev: {
        phomemo-printer-app = final.callPackage ./package.nix { };
      };

      # services.phomemo-printer-app; README.md describes it.
      nixosModules.default = ./nixos/module.nix;

      packages = forAllSystems (
        _: pkgs: rec {
          phomemo-printer-app = pkgs.callPackage ./package.nix { };
          default = phomemo-printer-app;
        }
      );

      checks = forAllSystems (
        system: pkgs:
        let
          package = self.packages.${system}.default;

          # A Makefile target, run on the package's sources with its
          # toolchain and Cargo dependencies, plus `inputs`.
          makeCheck =
            name: inputs: target:
            package.overrideAttrs (old: {
              pname = "${old.pname}-${name}";
              nativeBuildInputs = old.nativeBuildInputs ++ inputs;
              buildFlags = [ target ];
              doCheck = false;
              installPhase = "touch $out";
              dontFixup = true;
            });
        in
        {
          # Building the package also runs the Rust tests.
          inherit package;

          # The Nix files with `nix fmt`'s formatter, the Rust ones with rustfmt.
          formatting =
            pkgs.runCommand "phomemo-printer-app-formatting"
              {
                src = lib.fileset.toSource {
                  root = ./.;
                  fileset = lib.fileset.unions [
                    (lib.fileset.fileFilter (file: file.hasExt "nix" || file.hasExt "rs") ./.)
                    (lib.fileset.fileFilter (file: file.name == "Cargo.toml") ./.)
                    ./Cargo.lock
                    ./Makefile
                  ];
                };
                nativeBuildInputs = [
                  self.formatter.${system}
                  pkgs.cargo
                  pkgs.rustfmt
                ];
              }
              ''
                cp -r --no-preserve=mode "$src" source && cd source
                export HOME=$TMPDIR
                treefmt --ci
                make fmt-check
                touch $out
              '';

          clippy = makeCheck "clippy" [ pkgs.clippy ] "lint";

          # The C sources with the build's flags and -Werror.
          c-lint = makeCheck "c-lint" [ ] "c-lint";

          module-eval = import ./nixos/module-eval.nix {
            inherit lib pkgs;
            module = self.nixosModules.default;
          };

          nixos = pkgs.testers.runNixOSTest {
            imports = [ ./nixos/test.nix ];
            defaults.imports = [ self.nixosModules.default ];
          };

          devShell = self.devShells.${system}.default;
        }
      );

      # `nix fmt` formats every Nix file of the flake, wherever it runs in it.
      formatter = forAllSystems (
        _: pkgs: pkgs.nixfmt-tree.override { settings.tree-root-file = "flake.nix"; }
      );

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
