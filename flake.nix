{
  description = "tummycrypt/tcfs - FOSS self-hosted odrive replacement";

  # Public Attic read endpoint used by local dev and CI.
  # CI/release workflows push with `attic login` separately.
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
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, crane, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        rustVersion = "1.93.0";
        rustTargets = [
          "x86_64-unknown-linux-gnu"
          "aarch64-unknown-linux-gnu"
          "x86_64-apple-darwin"
          "aarch64-apple-darwin"
          "aarch64-apple-ios"
          "aarch64-apple-ios-sim"
        ];
        rustToolchain = pkgs.rust-bin.stable.${rustVersion}.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
          targets = rustTargets;
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
          apple-sdk
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
          nativeBuildInputs = with pkgs; [ pkg-config protobuf perl ];
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
          # Vendor OpenSSL on macOS to avoid dyld Team ID mismatch
          # when launchd loads the binary (Nix store openssl has different
          # code signature than the daemon binary).
          cargoExtraArgs = "-p tcfsd"
            + pkgs.lib.optionalString pkgs.stdenv.isDarwin " --features openssl-vendored";
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

        tcfs-mcp = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "tcfs-mcp";
          cargoExtraArgs = "-p tcfs-mcp";
          meta.mainProgram = "tcfs-mcp";
        });

        # FileProvider FFI static library (pure Rust build).
        # Header: $out/include/tcfs_file_provider.h
        # Library: $out/lib/libtcfs_file_provider.a
        # Used by: swift/fileprovider/build.sh (impure, needs system swiftc)
        tcfs-file-provider-staticlib = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "tcfs-file-provider-staticlib";
          cargoExtraArgs = "-p tcfs-file-provider --no-default-features --features grpc";
          postInstall = ''
            mkdir -p $out/lib $out/include
            find target -name "libtcfs_file_provider.a" -exec cp {} $out/lib/ \;
            find target -name "tcfs_file_provider.h" -exec cp {} $out/include/ \;
          '';
        });

        # macOS .app bundle for TCC persistence.
        # TCC grants (Full Disk Access, etc.) are tied to bundle ID + CDHash.
        # Bare binaries in /nix/store/ lose grants on every rebuild.
        # This bundle provides a stable identity (io.tinyland.tcfsd).
        tcfsd-app = pkgs.lib.optionalAttrs pkgs.stdenv.isDarwin (
          pkgs.stdenv.mkDerivation {
            pname = "tcfsd-app";
            version = tcfsd.version or "0.12.0";
            dontUnpack = true;
            buildInputs = [ pkgs.darwin.sigtool ];
            installPhase = ''
              mkdir -p $out/Applications/TCFSDaemon.app/Contents/MacOS
              cp ${tcfsd}/bin/tcfsd $out/Applications/TCFSDaemon.app/Contents/MacOS/tcfsd
              cp ${./swift/daemon/resources/Info.plist} $out/Applications/TCFSDaemon.app/Contents/Info.plist
              # Ad-hoc sign for local use; Developer ID signing done out-of-band
              codesign -f -s - --options runtime $out/Applications/TCFSDaemon.app || true
            '';
          }
        );

      in {
        packages = {
          default = tcfsd;
          inherit tcfsd tcfs-cli tcfs-tui tcfs-mcp tcfs-file-provider-staticlib;
        } // pkgs.lib.optionalAttrs pkgs.stdenv.isDarwin {
          inherit tcfsd-app;
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

            # Docs tooling
            lychee
            pandoc
            tectonic
            mermaid-cli

            # Dev tools
            git
            just
            yq-go
          ]);
          TCFS_RUST_TOOLCHAIN = rustVersion;

          shellHook = ''
            echo "tcfs devShell (tummycrypt monorepo)"
            echo "  rustc --version  # pinned toolchain should report ${rustVersion}"
            echo "  just --list      # show available recipes"
            echo "  task --list      # show go-task tasks"
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
