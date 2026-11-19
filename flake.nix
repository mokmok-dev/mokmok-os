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
          # crane keeps only Rust and cargo files, so the files the code reads with
          # `include_str!` are added back here.
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              ./ARCHITECTURE.md
              (pkgs.lib.fileset.fileFilter (
                file:
                pkgs.lib.any (extension: file.hasExt extension) [
                  "css"
                  "svg"
                ]
              ) ./crates/mokmok-web/src/view)
            ];
          };
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
          inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;

          mokmok-controld = craneLib.buildPackage (
            commonArgs
            // {
              inherit
                cargoArtifacts
                version
                ;
              pname = "mokmok-controld";
              cargoExtraArgs = "-p mokmok-controld";
              doCheck = false;
              meta.mainProgram = "mokmok-controld";
            }
          );
        in
        {
          _module.args = {
            pkgs = import inputs.nixpkgs {
              inherit system;
              overlays = [ inputs.rust-overlay.overlays.default ];
            };
          };

          checks = {
            inherit mokmok-controld;

            clippy = craneLib.cargoClippy (
              commonArgs
              // {
                inherit cargoArtifacts;
                cargoClippyExtraArgs = "--all-targets --all-features -- --deny warnings";
              }
            );
            test = craneLib.cargoTest (
              commonArgs
              // {
                inherit cargoArtifacts;
              }
            );
            doc = craneLib.cargoDoc (
              commonArgs
              // {
                inherit cargoArtifacts;
                env.RUSTDOCFLAGS = "--deny warnings";
              }
            );
          }
          // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            mutants = craneLib.mkCargoDerivation (
              commonArgs
              // {
                inherit
                  cargoArtifacts
                  version
                  ;
                pname = "mokmok-os-mutants";
                nativeBuildInputs = [ pkgs.cargo-mutants ];
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
                  cargoArtifacts
                  version
                  ;
                pname = "mokmok-os-kani";
                nativeBuildInputs = [ kani ];
                buildPhaseCargoCommand = "PATH=${kani}/bin:$PATH kani-driver crates/mokmok-proofs/src/lib.rs";
                doInstallCargoArtifacts = false;
              }
            );
          };

          # The daemon is the only thing shipped. A bare workspace build also
          # installs the test agents, and a consumer would depend on both.
          packages.default = mokmok-controld;

          devShells.default = pkgs.mkShellNoCC {
            inputsFrom = [ config.pre-commit.devShell ];

            packages = with pkgs; [
              cargo-mutants
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
