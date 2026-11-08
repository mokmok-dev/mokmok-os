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
          kaniToolchain = pkgs.rust-bin.nightly."2026-08-21".default.override {
            extensions = [
              "rustc-dev"
              "rust-src"
            ];
          };
          # The bundle's kani-compiler links librustc_driver-59857489213b2a36.so from
          # rustc 1.100.0-nightly (8925ea358 2026-08-20); that nightly is the toolchain here.
          kani = pkgs.stdenv.mkDerivation {
            pname = "kani";
            version = "0.68.0";
            src = pkgs.fetchurl {
              url = "https://github.com/model-checking/kani/releases/download/kani-0.68.0/kani-0.68.0-x86_64-unknown-linux-gnu.tar.gz";
              hash = "sha256-MuK0hNc+3gu/ZKLPCHnEJZQiSX2K7E7mfUSN6a54Q9M=";
            };
            nativeBuildInputs = [
              pkgs.autoPatchelfHook
              pkgs.makeWrapper
            ];
            buildInputs = [
              pkgs.stdenv.cc.cc.lib
              pkgs.zlib
            ];
            autoPatchelfIgnoreMissingDeps = true;
            unpackPhase = ''
              mkdir -p source
              tar -xzf $src -C source --strip-components=1
            '';
            installPhase = ''
              mkdir -p $out
              cp -r source/. $out/
              ln -s ${kaniToolchain} $out/toolchain
              for bin in kani-driver kani-compiler cbmc goto-cc goto-instrument goto-analyzer kani-cov kissat; do
                if [ -e "$out/bin/$bin" ]; then
                  wrapProgram "$out/bin/$bin" --prefix LD_LIBRARY_PATH : "$out/lib/rustlib/x86_64-unknown-linux-gnu/lib:$out/toolchain/lib:$out/toolchain/lib/rustlib/x86_64-unknown-linux-gnu/lib:${pkgs.stdenv.cc.cc.lib}/lib:${pkgs.zlib}/lib"
                fi
              done
              ln -s $out/bin/kani-driver $out/bin/kani
              ln -s $out/bin/kani-driver $out/bin/cargo-kani
            '';
            dontStrip = true;
          };
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
