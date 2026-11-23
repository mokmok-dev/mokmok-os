{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs";
    flake-parts.url = "github:hercules-ci/flake-parts";
    flake-parts.inputs.nixpkgs-lib.follows = "nixpkgs";
    git-hooks.url = "github:cachix/git-hooks.nix";
    git-hooks.inputs.nixpkgs.follows = "nixpkgs";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";
    nix-bun.url = "github:ryoppippi/nix-bun";
    nix-bun.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      nixpkgs,
      flake-parts,
      git-hooks,
      treefmt-nix,
      nix-bun,
      ...
    }@inputs:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        git-hooks.flakeModule
        treefmt-nix.flakeModule
      ];

      perSystem =
        {
          pkgs,
          system,
          ...
        }:
        let
          depsInputs = pkgs.lib.fileset.unions [
            ./bun.lock
            ./bunfig.toml
            ./package.json
          ];
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              depsInputs
              ./tsconfig.base.json
              ./tsconfig.json
              (pkgs.lib.fileset.maybeMissing ./apps)
              (pkgs.lib.fileset.maybeMissing ./packages)
            ];
          };
          bunDeps = pkgs.stdenvNoCC.mkDerivation {
            pname = "mokmok-os-deps";
            inherit (pkgs.lib.importJSON ./package.json) version;
            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = depsInputs;
            };
            nativeBuildInputs = [ pkgs.bun ];
            dontFixup = true;
            buildPhase = ''
              export HOME=$TMPDIR
              bun install --frozen-lockfile --linker hoisted
            '';
            installPhase = ''
              mkdir -p $out
              cp -r node_modules $out/node_modules
            '';
            outputHashMode = "recursive";
            outputHash = "sha256-56l5JMGpvGSugB3u7j20Dh3ghpN0DWrTgg3CtE6AYX0=";
          };
          bunRun =
            name: command:
            pkgs.stdenvNoCC.mkDerivation {
              inherit name src;
              nativeBuildInputs = [
                pkgs.bun
                pkgs.git
              ];
              buildPhase = ''
                export HOME=$TMPDIR
                cp -r ${bunDeps}/node_modules ./node_modules
                chmod -R u+w node_modules
                ${command}
              '';
              installPhase = "touch $out";
            };
        in
        {
          _module.args = {
            pkgs = import nixpkgs {
              inherit system;
              overlays = [ nix-bun.overlays.default ];
            };
          };

          apps = { };

          checks = {
            fmt = bunRun "fmt" "bun fmt --check";
            test = bunRun "test" "bun test --pass-with-no-tests";
            types = bunRun "types" "bun check -b";
          };

          devShells.default = pkgs.mkShellNoCC {
            packages = with pkgs; [
              bun
              ni
              skills
            ];
          };

          packages = { };

          pre-commit.settings = {
            hooks = {
              actionlint.enable = true;
              deadnix.enable = true;
              statix.enable = true;
            };
            package = pkgs.prek;
          };

          treefmt = {
            projectRootFile = "flake.nix";
            programs = {
              nixfmt.enable = true;
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
