{
  description = "Phomemo PAPPL Printer Application";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
    in {
      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          # Rust toolchain
          cargo
          rustc
          rust-cbindgen

          # C toolchain
          clang
          pkg-config
          gnumake

          # PAPPL framework
          pappl

          # Bluetooth (BlueZ headers + libs for SDP/RFCOMM)
          bluez

          # For bindgen if we add it later
          llvmPackages.libclang
        ];

        LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

        shellHook = ''
          echo "Phomemo Printer App — dev shell"
          echo "  make        — build everything"
          echo "  make test   — run Rust tests"
          echo "  make clean  — clean all artifacts"
        '';
      };
    };
}
