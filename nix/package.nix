{
  lib,
  rustPlatform,
  perl,
}: let
  cargo = lib.importTOML ../Cargo.toml;
in
  rustPlatform.buildRustPackage {
    pname = cargo.package.name;
    inherit (cargo.package) version;

    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../Cargo.toml
        ../Cargo.lock
        ../src
      ];
    };

    cargoLock.lockFile = ../Cargo.lock;

    nativeBuildInputs = [perl];

    meta = {
      description = "Mail and password vault over SSH, backed by Stalwart";
      license = lib.licenses.agpl3Only;
      mainProgram = "syphoria";
      platforms = lib.platforms.unix;
    };
  }
