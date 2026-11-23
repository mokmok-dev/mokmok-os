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
            (pkgs.lib.fileset.maybeMissing ./apps)
            (pkgs.lib.fileset.maybeMissing ./packages)
          ];
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              depsInputs
              ./tsconfig.base.json
              ./tsconfig.json
              ./mutator.config.ts
              (pkgs.lib.fileset.maybeMissing ./mutator.accepted.json)
            ];
          };
          depsSrc = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = depsInputs;
          };
          bunDeps = pkgs.stdenvNoCC.mkDerivation {
            pname = "mokmok-os-deps";
            inherit (pkgs.lib.importJSON ./package.json) version;
            src = depsSrc;
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
            outputHash = "sha256-yKA1Ox9Q/VnoO0jYwWXq+IvWtulJ5T1k1wYoo6BER4w=";
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
          # `@mizchi/mutator` is a Node CLI that drives `bun test`, so both are
          # needed. Its quality gate is the check itself, not a wrapper script.
          mutantRun = bunRun "mutants" ''
            ${pkgs.nodejs_24}/bin/node node_modules/.bin/mutator
          '';
          # The npm workerd binary is linked against a generic glibc on Linux.
          # NixOS hosts without `nix-ld` cannot exec it, and `wrangler dev` needs
          # it, so the binary is repackaged with its interpreter patched and the
          # dev shell points `MINIFLARE_WORKERD_PATH` at it. On Darwin the npm
          # binary links against system frameworks and needs no patching.
          workerd = pkgs.stdenvNoCC.mkDerivation {
            pname = "workerd";
            inherit (pkgs.lib.importJSON ./package.json) version;
            dontUnpack = true;
            nativeBuildInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.autoPatchelfHook ];
            buildInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.stdenv.cc.cc.lib ];
            installPhase = ''
              install -Dm755 ${bunDeps}/node_modules/workerd/bin/workerd $out/bin/workerd
            '';
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
            mutants = mutantRun;
          };

          packages = {
            inherit workerd;
            default = workerd;
          };

          devShells.default = pkgs.mkShellNoCC {
            packages = with pkgs; [
              bun
              nodejs_24
              ni
              skills
            ];
            env.MINIFLARE_WORKERD_PATH = "${workerd}/bin/workerd";
          };

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
