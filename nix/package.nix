{
  lib,
  rustPlatform,
  perl,
}: let
  cargo = lib.importTOML ../Cargo.toml;
in
  rustPlatform.buildRustPackage {
    pname = "syphoria";
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
    cargoBuildFlags = ["-p" "syphoria"];

    nativeBuildInputs = [perl];

    doCheck = false;

    meta = {
      description = "Mail and password vault over SSH, backed by Stalwart";
      license = lib.licenses.agpl3Only;
      mainProgram = "syphoria";
      platforms = lib.platforms.unix;
    };
  }
