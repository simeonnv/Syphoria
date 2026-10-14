{
  lib,
  rustPlatform,
  callPackage,
  lld,
  _3270font,
  zip,
}: let
  cargo = lib.importTOML ../Cargo.toml;
  addonId = "syphoria@extension";
  firefox = "{ec8030f7-c20a-464f-9b0e-13a3a9e97384}";
  wasm-bindgen-cli = callPackage ./wasm-bindgen-cli.nix {};
in
  rustPlatform.buildRustPackage {
    pname = "syphoria-extension";
    inherit (cargo.workspace.package) version;

    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../Cargo.toml
        ../Cargo.lock
        ../crates
      ];
    };

    cargoLock.lockFile = ../Cargo.lock;

    nativeBuildInputs = [lld wasm-bindgen-cli zip];

    buildPhase = ''
      runHook preBuild
      cargo build --release --offline -p syphoria-extension --target wasm32-unknown-unknown
      mkdir -p dist
      cp -r crates/syphoria-extension/static/. dist/
      install -Dm644 ${_3270font}/share/fonts/truetype/3270-Regular.ttf dist/fonts/3270-Regular.ttf
      wasm-bindgen --target web --no-typescript --out-dir dist/pkg \
        "''${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/release/syphoria_extension.wasm"
      runHook postBuild
    '';

    installPhase = ''
      runHook preInstall
      mkdir -p $out/share/syphoria-extension "$out/share/mozilla/extensions/${firefox}"
      cp -r dist/. $out/share/syphoria-extension/
      (cd dist && zip -qrX "$out/share/mozilla/extensions/${firefox}/${addonId}.xpi" .)
      runHook postInstall
    '';

    doCheck = false;

    passthru = {inherit addonId;};

    meta = {
      description = "Browser extension for the syphoria vault";
      license = lib.licenses.agpl3Only;
      platforms = lib.platforms.all;
    };
  }
