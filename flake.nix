{
  description = "BTC Map proxy for Blink federation";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
        flake-utils.follows = "flake-utils";
      };
    };

    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    rust-overlay,
    crane,
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      overlays = [(import rust-overlay)];
      pkgs = import nixpkgs {inherit overlays system;};
      rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

      craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

      commonArgs = {
        src = craneLib.cleanCargoSource ./.;
        strictDeps = true;
        buildInputs =
          pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
          ];
      };

      cargoArtifacts = craneLib.buildDepsOnly commonArgs;

      btcmap-proxy = craneLib.buildPackage (commonArgs
        // {
          inherit cargoArtifacts;
          cargoExtraArgs = "--bin btcmap-proxy";
        });

      nativeBuildInputs = with pkgs; [
        rustToolchain
        cargo-watch
        gnumake
        vendir
        rover
        docker-compose
        alejandra
        jq
      ];
    in {
      packages = {
        default = btcmap-proxy;
        inherit btcmap-proxy;
      };

      devShells.default = pkgs.mkShell {
        inherit nativeBuildInputs;
        shellHook = ''
          export HOST_PROJECT_PATH="$(pwd)"
        '';
      };

      formatter = pkgs.alejandra;
    });
}
