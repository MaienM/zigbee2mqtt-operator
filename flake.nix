{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";

    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";

    flake-utils.url = "github:numtide/flake-utils";
  };
  outputs = { nixpkgs, fenix, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        devToolchain = fenix.packages.${system}.stable;
        devToolchainUnstable = fenix.packages."${system}".latest;
      in
      {
        defaultPackage = fenix.packages.x86_64-linux.minimal.toolchain;
        devShell = pkgs.mkShell {
          buildInputs = [
            (devToolchain.withComponents [
              "cargo"
              "rust-src"
              "rustc"
            ])
            (devToolchainUnstable.withComponents [
              "rustfmt"
            ])
            fenix.packages.${system}.rust-analyzer

            pkgs.kubectl
          ];
        };
      });
}
