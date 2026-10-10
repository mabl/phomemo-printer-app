{
  lib,
  stdenv,
  rustPlatform,
  cargo,
  rustc,
  rust-cbindgen,
  pkg-config,
  pappl,
  python3,
}:

stdenv.mkDerivation (finalAttrs: {
  pname = "phomemo-printer-app";
  version = (lib.importTOML ./phomemo-pappl/Cargo.toml).package.version;

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.lock
      ./Cargo.toml
      ./LICENSE
      ./Makefile
      ./c
      ./docs/models.md # read by a test that keeps it current
      ./phomemo-pappl
      ./phomemo-protocol
      ./scripts/test_packaged_runtime.py
      ./systemd
    ];
  };

  cargoDeps = rustPlatform.importCargoLock { lockFile = ./Cargo.lock; };

  strictDeps = true;

  nativeBuildInputs = [
    cargo
    rustc
    rustPlatform.cargoSetupHook
    rust-cbindgen
    pkg-config
    python3
  ];

  buildInputs = [ pappl ]; # PAPPL 1.x: the driver uses the 1.4 callback signatures

  makeFlags = [ "PREFIX=${placeholder "out"}" ];

  # The release profile, so that the tests reuse the dependencies just built.
  doCheck = stdenv.buildPlatform.canExecute stdenv.hostPlatform;
  checkPhase = ''
    runHook preCheck
    cargo test --release --workspace --locked

    # Keep the normal binary's native paths. Compile only the service variant
    # in scratch space, reusing this build's Rust library and generated header.
    service_directory="$TMPDIR/service"
    service_binary="$TMPDIR/service-binary"
    make all BIN="$service_binary" \
      "CPPFLAGS=-DSERVICE_DIRECTORY=\\\"$service_directory\\\""
    python3 "$PWD/scripts/test_packaged_runtime.py" \
      --binary "$PWD/phomemo-printer-app" \
      --service-binary "$service_binary" \
      --service-directory "$service_directory"
    runHook postCheck
  '';

  # The unit reads its configuration from /etc/default, outside the store.
  installTargets = [
    "install"
    "install-unit"
  ];

  postInstall = ''
    install -D -m 0644 -t $out/share/doc/phomemo-printer-app \
      LICENSE docs/models.md systemd/phomemo-printer-app.env.example
  '';

  meta = {
    description = "PAPPL printer application for Phomemo Bluetooth label printers";
    homepage = "https://github.com/mabl/phomemo-printer-app";
    license = lib.licenses.asl20;
    platforms = lib.platforms.linux;
    mainProgram = finalAttrs.pname;
  };
})
