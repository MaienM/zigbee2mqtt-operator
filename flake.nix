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
              "clippy"
              "rust-src"
              "rustc"
            ])
            (devToolchainUnstable.withComponents [
              "rustfmt"
            ])
            fenix.packages.${system}.rust-analyzer
            pkgs.cargo-nextest

            pkgs.act
            pkgs.kubectl
          ];

          shellHook = ''
            source .env
            (
              real_version="$(rustc --version | cut -d' ' -f2)"
              if [ "$RUST_VERSION" != "$real_version" ]; then
                >&2 echo "WARNING: Rust version $RUST_VERSION is specified in .env, but the installed version is $real_version."
              fi
            )
          '';
        };
      });
}
