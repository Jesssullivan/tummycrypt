{
  description = "tummycrypt/tcfs - FOSS self-hosted odrive replacement";

  nixConfig = {
    extra-substituters = [
      "https://nix-cache.fuzzy-dev.tinyland.dev/main"
    ];
    extra-trusted-public-keys = [
      "main:NKRk1XYo/dfd9fcDqgotUJg2DTDHWp5ny+Ba7WzRjgE="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, crane, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
          targets = [
            "x86_64-unknown-linux-gnu"
            "aarch64-unknown-linux-gnu"
            "x86_64-apple-darwin"
            "aarch64-apple-darwin"
          ];
        };
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Common build inputs for all crates
        commonBuildInputs = with pkgs; [
          protobuf
          pkg-config
          openssl
        ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
          fuse3
          rocksdb
        ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
          # macOS: use apple_sdk_12_3 (apple_sdk_11_0 alias removed in nixpkgs-unstable)
          darwin.apple_sdk_12_3.frameworks.Security
          darwin.apple_sdk_12_3.frameworks.SystemConfiguration
        ];

        # Source filter: include .proto files alongside standard Cargo sources
        src = let
          protoFilter = path: _type: builtins.match ".*\\.proto$" path != null;
          filter = path: type:
            (protoFilter path type) || (craneLib.filterCargoSources path type);
        in pkgs.lib.cleanSourceWith {
          src = craneLib.path ./.;
          inherit filter;
        };

        # Common args shared by all crate builds
        commonArgs = {
          inherit src;
          buildInputs = commonBuildInputs;
          nativeBuildInputs = with pkgs; [ pkg-config protobuf ];
        } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          ROCKSDB_INCLUDE_DIR = "${pkgs.rocksdb}/include";
          ROCKSDB_LIB_DIR = "${pkgs.rocksdb}/lib";
        };

        # Pre-build workspace deps (shared across all crate builds for caching)
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Build individual crates as separate derivations
        tcfsd = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "tcfsd";
          cargoExtraArgs = "-p tcfsd";
          meta.mainProgram = "tcfsd";
        });

        tcfs-cli = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "tcfs-cli";
          cargoExtraArgs = "-p tcfs-cli";
          meta.mainProgram = "tcfs";
        });

        tcfs-tui = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "tcfs-tui";
          cargoExtraArgs = "-p tcfs-tui";
          meta.mainProgram = "tcfs-tui";
        });

      in {
        packages = {
          default = tcfsd;
          inherit tcfsd tcfs-cli tcfs-tui;
        };

        devShells.default = pkgs.mkShell {
          buildInputs = commonBuildInputs ++ (with pkgs; [
            rustToolchain

            # Proto codegen
            protobuf

            # Security tooling
            age
            sops

            # Infrastructure
            opentofu
            kubectl
            kubernetes-helm
            kustomize

            # Build tooling
            go-task
            cargo-watch
            cargo-deny
            cargo-audit

            # NATS
            natscli

            # Dev tools
            git
            yq-go
          ]);

          shellHook = ''
            echo "tcfs devShell (tummycrypt monorepo)"
            echo "  task --list      # show available tasks"
            echo "  cargo build      # build workspace"
            echo "  task dev         # start local stack + watch"
          '';

        } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          ROCKSDB_INCLUDE_DIR = "${pkgs.rocksdb}/include";
          ROCKSDB_LIB_DIR = "${pkgs.rocksdb}/lib";
        };
      }
    ) // {
      # NixOS modules (system-level)
      nixosModules.tcfsd = import ./nix/modules/tcfs-daemon.nix;

      # Home Manager modules (user-level)
      homeManagerModules.tcfs = import ./nix/modules/tcfs-user.nix;
    };
}
