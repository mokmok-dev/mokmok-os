{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs";
    flake-parts.url = "github:hercules-ci/flake-parts";
    flake-parts.inputs.nixpkgs-lib.follows = "nixpkgs";
    git-hooks.url = "github:cachix/git-hooks.nix";
    git-hooks.inputs.nixpkgs.follows = "nixpkgs";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    inputs:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        inputs.git-hooks.flakeModule
        inputs.treefmt-nix.flakeModule
      ];

      perSystem =
        {
          config,
          pkgs,
          system,
          ...
        }:
        let
          rustToolchain = pkgs.rust-bin.stable.latest.default;
          craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchain;
          src = pkgs.lib.cleanSourceWith { src = ./.; };
          commonArgs = {
            inherit src;
            strictDeps = true;
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          linkerPackages = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux (
            with pkgs;
            [
              clang
              mold
            ]
          );
          linkerEnv = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
            export RUSTFLAGS="-C link-arg=-fuse-ld=mold"
            export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="clang"
            export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="clang"
          '';
          kani = pkgs.callPackage ./nix/kani.nix { };
        in
        {
          _module.args = {
            pkgs = import inputs.nixpkgs {
              inherit system;
              overlays = [ inputs.rust-overlay.overlays.default ];
            };
          };

          checks = {
            clippy = craneLib.cargoClippy (
              commonArgs
              // {
                inherit
                  src
                  cargoArtifacts
                  ;
                cargoClippyExtraArgs = "--all-targets --all-features -- --deny warnings";
              }
            );
            test = craneLib.cargoTest (
              commonArgs
              // {
                inherit
                  src
                  cargoArtifacts
                  ;
              }
            );
          }
          // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            mutants = craneLib.mkCargoDerivation (
              commonArgs
              // {
                inherit
                  src
                  cargoArtifacts
                  ;
                pname = "mokmok-os-mutants";
                version = "2026.11.1";
                nativeBuildInputs = [
                  pkgs.cargo-mutants
                  pkgs.cargo-nextest
                ];
                buildPhaseCargoCommand = "cargo mutants --workspace --jobs 4 --timeout 60 --no-shuffle";
                doInstallCargoArtifacts = false;
              }
            );
          }
          // pkgs.lib.optionalAttrs (system == "x86_64-linux") {
            kani = craneLib.mkCargoDerivation (
              commonArgs
              // {
                inherit
                  src
                  cargoArtifacts
                  ;
                pname = "mokmok-os-kani";
                version = "2026.11.1";
                nativeBuildInputs = [ kani ];
                buildPhaseCargoCommand = "PATH=${kani}/bin:$PATH kani-driver crates/mokmok-proofs/src/lib.rs";
                doInstallCargoArtifacts = false;
              }
            );
          };

          packages.default = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              meta.mainProgram = "mokmok-controld";
            }
          );

          devShells.default = pkgs.mkShellNoCC {
            inputsFrom = [ config.pre-commit.devShell ];

            packages = with pkgs; [
              cargo-mutants
              cargo-nextest
              linkerPackages
              rustToolchain
              sccache
            ];

            shellHook = ''
              export RUSTC_WRAPPER="${pkgs.sccache}/bin/sccache"
              ${linkerEnv}
            '';
          };

          pre-commit.settings = {
            hooks = {
              actionlint.enable = true;
              deadnix.enable = true;
              statix.enable = true;
            };
          };

          treefmt = {
            projectRootFile = "flake.nix";
            programs = {
              nixfmt.enable = true;
              rustfmt.enable = true;
              rustfmt.package = rustToolchain;
              taplo.enable = true;
            };
          };
        };

      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
    };
}
