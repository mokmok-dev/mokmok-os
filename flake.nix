{
  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.2605.1015867";
    flake-parts.url = "https://flakehub.com/f/hercules-ci/flake-parts/0.1.497";
    git-hooks.url = "https://flakehub.com/f/cachix/git-hooks.nix/0.1.1252";
    treefmt-nix.url = "https://flakehub.com/f/numtide/treefmt-nix/0.1.682";
    rust-overlay.url = "https://flakehub.com/f/oxalica/rust-overlay/0.1.2330";
    crane.url = "https://flakehub.com/f/ipetkov/crane/0.24.0";
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
          lib,
          pkgs,
          system,
          ...
        }:
        let
          rustToolchain = pkgs.rust-bin.stable.latest.default;
          craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchain;
        in
        {
          _module.args = {
            pkgs = import inputs.nixpkgs {
              inherit system;
              overlays = [ inputs.rust-overlay.overlays.default ];
            };
          };

          checks =
            let
              src = pkgs.lib.cleanSourceWith { src = ./.; };

              # cargo
              cargoCommonArgs = {
                inherit src;
                strictDeps = true;
              };
              cargoArtifacts = craneLib.buildDepsOnly cargoCommonArgs;
            in
            {
              cargoClippy = craneLib.cargoClippy (
                cargoCommonArgs
                // {
                  inherit
                    src
                    cargoArtifacts
                    ;
                  cargoClippyExtraArgs = "--all-targets --all-features -- --deny warnings";
                }
              );
              cargoDocTest = craneLib.cargoDocTest (
                cargoCommonArgs
                // {
                  inherit
                    src
                    cargoArtifacts
                    ;
                }
              );
              cargoTest = craneLib.cargoNextest (
                cargoCommonArgs
                // {
                  inherit
                    src
                    cargoArtifacts
                    ;
                }
              );
            };

          devShells.default = pkgs.mkShellNoCC {
            inputsFrom = [ config.pre-commit.devShell ];
            packages =
              with pkgs;
              [
                cargo-mutants
                rustToolchain
                sccache
              ]
              ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux (
                with pkgs;
                [
                  mold
                ]
              );
            shellHook = ''
              ${lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
                export RUSTFLAGS="-C link-arg=-fuse-ld=mold";
              ''}
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
        "x86_64-linux"
      ];
    };
}
