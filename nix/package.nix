{ lib, rustPlatform, installShellFiles }:
rustPlatform.buildRustPackage {
  pname = "abdns";
  version = "0.2.0";
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../src ../config.example.toml ../man ../completions ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  nativeBuildInputs = [ installShellFiles ];
  postInstall = ''
    installManPage man/abdns.1 man/abdnsd.8
    installShellCompletion --bash --name abdns completions/abdns.bash
    installShellCompletion --zsh completions/_abdns
    installShellCompletion --fish completions/abdns.fish
    install -Dm644 config.example.toml $out/share/abdns/config.example.toml
  '';
  meta = {
    description = "Encrypted DNS for your computer: your own DoH resolver first, ordered encrypted fallbacks, network DNS only for captive portals";
    homepage = "https://github.com/abnd-corp/Abdns";
    mainProgram = "abdns";
    license = lib.licenses.agpl3Only;
  };
}
