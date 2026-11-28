{
  description = "rusuna: a Hasura-style GraphQL engine as a Rust library";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    leanfran = {
      url = "github:conao3/lean-leanfran/420a7dce66dda3cbfd03b884d453cb8de63a0fc6";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs =
    inputs:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-darwin"
      ];
      imports = [ inputs.treefmt-nix.flakeModule ];
      perSystem =
        { pkgs, system, ... }:
        let
          leanfran = inputs.leanfran.packages.${system}.leanfran;
        in
        {
          treefmt = {
            projectRootFile = "flake.nix";
            programs.nixfmt.enable = true;
            programs.rustfmt.enable = true;
            programs.shfmt.enable = true;
          };
          devShells.default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.rustc
              pkgs.clippy
              pkgs.rustfmt
              pkgs.rust-analyzer
              pkgs.lean4
              pkgs.gnumake
              pkgs.postgresql_17
              leanfran
            ];
            LEANFRAN_CLI = "${leanfran}/bin/leanfran";
          };
          packages.default = pkgs.rustPlatform.buildRustPackage {
            pname = "rusuna";
            version = "0.3.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            nativeCheckInputs = [ pkgs.postgresql_17 ];
            LEANFRAN_OFFLINE = "1";
            checkPhase = "bash tools/with-postgres.sh tests/fixtures/schema.sql tests/fixtures/seed.sql -- cargo test --workspace";
            meta.mainProgram = "rusuna";
            meta.license = pkgs.lib.licenses.asl20;
          };
        };
    };
}
