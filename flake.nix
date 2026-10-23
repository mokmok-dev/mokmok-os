{
  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.2605.1015867";
    flake-parts.url = "https://flakehub.com/f/hercules-ci/flake-parts/0.1.497";
    git-hooks.url = "https://flakehub.com/f/cachix/git-hooks.nix/0.1.1252";
    treefmt-nix.url = "https://flakehub.com/f/numtide/treefmt-nix/0.1.682";
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
        {
          _module.args = {
            pkgs = import inputs.nixpkgs {
              inherit system;
            };
          };

          checks =
            let
              src = ./.;
              dependencySrc = pkgs.lib.fileset.toSource {
                root = ./.;
                fileset = pkgs.lib.fileset.unions [
                  ./package.json
                  ./pnpm-lock.yaml
                  ./pnpm-workspace.yaml
                  (pkgs.lib.fileset.fileFilter (file: file.name == "package.json") ./.)
                ];
              };
              pnpmDeps = pkgs.fetchPnpmDeps {
                pname = "mokmok-os";
                src = dependencySrc;
                inherit (pkgs) pnpm;
                fetcherVersion = 4;
                hash = "sha256-VxuWidXjBa5iEzNsBJ2zbyc2ScUmxjrsQXu6mYnnxRo=";
              };
              runPnpm =
                name: command:
                pkgs.stdenvNoCC.mkDerivation {
                  pname = "mokmok-os-${name}";
                  version = "0.0.0";
                  inherit src pnpmDeps;
                  nativeBuildInputs = with pkgs; [
                    nodejs
                    pnpm
                    pnpmConfigHook
                    typescript
                  ];
                  buildPhase = command;
                  installPhase = "mkdir -p $out";
                };
            in
            {
              typecheck = runPnpm "typecheck" "pnpm typecheck";
              test = runPnpm "test" "pnpm test";
            };

          devShells.default = pkgs.mkShellNoCC {
            inputsFrom = [ config.pre-commit.devShell ];

            packages = with pkgs; [
              ni
              nodejs
              pnpm
              typescript
            ];
          };

          pre-commit.settings = {
            hooks = {
              actionlint.enable = true;
              deadnix.enable = true;
              oxlint.enable = true;
              statix.enable = true;
            };
          };

          treefmt = {
            projectRootFile = "flake.nix";
            programs = {
              nixfmt.enable = true;
              oxfmt.enable = true;
            };
          };
        };

      systems = [
        "x86_64-linux"
      ];
    };
}
