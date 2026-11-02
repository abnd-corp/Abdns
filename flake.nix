{
  description = "abdns — encrypted DNS for your computer: your own DoH resolver first, private fallbacks, network DNS only for captive portals";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (s: f nixpkgs.legacyPackages.${s});
    in {
      packages = forAll (pkgs: { default = pkgs.callPackage ./nix/package.nix { }; });
      darwinModules.default = import ./nix/darwin.nix self;
    };
}
