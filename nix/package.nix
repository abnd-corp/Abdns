{ lib, rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "abdns";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../src ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  meta = {
    description = "Encrypted DNS for your computer: your own DoH resolver first, private fallbacks, network DNS only for captive portals";
    homepage = "https://github.com/abnd-corp/Abdns";
    mainProgram = "abdns";
  };
}
