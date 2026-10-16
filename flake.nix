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
    let
      # The published module. `inputs.self` is this flake's outputs, so the package default is the
      # one this flake builds for the evaluated system.
      nixosModule =
        { lib, pkgs, ... }:
        {
          imports = [ ./deploy/nixos-module.nix ];
          services.mokmokd.package =
            lib.mkDefault
              inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        };
    in
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        inputs.git-hooks.flakeModule
        inputs.treefmt-nix.flakeModule
      ];

      flake.nixosModules.default = nixosModule;

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

          src = pkgs.lib.cleanSourceWith { src = ./.; };

          cargoCommonArgs = {
            inherit src;
            strictDeps = true;
          };
          cargoArtifacts = craneLib.buildDepsOnly cargoCommonArgs;
        in
        {
          _module.args = {
            pkgs = import inputs.nixpkgs {
              inherit system;
              overlays = [ inputs.rust-overlay.overlays.default ];
            };
          };

          packages.default = craneLib.buildPackage (
            cargoCommonArgs
            // {
              inherit cargoArtifacts;
            }
          );

          checks = {
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
                cargoNextestExtraArgs = "--no-tests=pass";
              }
            );

            # `mokmok-os-architecture.md` section 9 asks CI to enforce these dependency rules.
            dependencyRules =
              pkgs.runCommand "dependency-rules"
                {
                  nativeBuildInputs = with pkgs; [
                    bash
                    coreutils
                    gawk
                    gnugrep
                  ];
                }
                ''
                  bash ${src}/docs/spec/references/attesters/dependency-rules.sh check ${src} > $out
                '';

            mokmokdUnit =
              let
                mokmokd = config.packages.default;
                unit = pkgs.replaceVars ./deploy/mokmokd.service {
                  mokmokd = "${mokmokd}/bin/mokmokd";
                };
              in
              pkgs.runCommand "mokmokd-unit" { } ''
                # The daemon defaults --confiner to the sibling of its own executable and refuses
                # a run it cannot resolve, so the ExecStart path has to land beside the confiner.
                # `current_exe` resolves a symlink, so the pair has to be siblings in the crane
                # output the ExecStart path names.
                test -x ${mokmokd}/bin/mokmokd
                test -x ${mokmokd}/bin/mokmok-sandbox-confine

                install -Dm644 ${unit} $out/mokmokd.service

                # `replaceVars` is what fails when a placeholder has no value, and this is what
                # fails when the placeholder moved out of the ExecStart line while still
                # substituting cleanly.
                grep -qF "ExecStart=${mokmokd}/bin/mokmokd " $out/mokmokd.service

                # One directory for the operator: the unit, and the binaries it starts.
                install -d $out/bin
                ln -s ${mokmokd}/bin/mokmokd $out/bin/mokmokd
                ln -s ${mokmokd}/bin/mokmok-sandbox-confine $out/bin/mokmok-sandbox-confine
              '';

            nixosModule =
              let
                evaluated =
                  extra:
                  inputs.nixpkgs.lib.nixosSystem {
                    inherit system;
                    modules = [ nixosModule ] ++ extra;
                  };

                # nixpkgs renders a service into `systemd.units`; `services.<name>` has no `unit`
                # attribute in this nixpkgs.
                unitOf = config: config.config.systemd.units."mokmokd.service".text;

                # The defaults with the sample.
                defaults = evaluated [ ./deploy/nixos-example.nix ];

                # Every option off its default, with an account the operator declares, so the
                # merge the module joins is exercised rather than assumed.
                custom = evaluated [
                  {
                    services.mokmokd = {
                      enable = true;
                      directory = "custom";
                      user = "custom";
                      group = "custom";
                      watchdogSec = 45;
                      allowWrite = [ "/tmp/w" ];
                    };
                    users.users.custom = {
                      isNormalUser = true;
                      group = "users";
                    };
                  }
                ];

                # Values that reach the systemd parser, so the escaping has a regression test.
                adversarial = evaluated [
                  {
                    services.mokmokd = {
                      enable = true;
                      allowTool = [
                        "$FOO"
                        "a\\b"
                      ];
                      allowPath = [ "/nix/store/%p" ];
                    };
                  }
                ];

                # A unit render never raises a validation error, so the two option types are
                # forced here: a value outside its type only shows as a failed evaluation.
                typeCheck = expr: (builtins.tryEval expr).success;
                newlineToolSuccess = typeCheck (
                  unitOf (evaluated [
                    {
                      services.mokmokd = {
                        enable = true;
                        allowTool = [ "a\nb" ];
                      };
                    }
                  ])
                );
                slashDirectorySuccess = typeCheck (
                  unitOf (evaluated [
                    {
                      services.mokmokd = {
                        enable = true;
                        directory = "a/b";
                      };
                    }
                  ])
                );
                tabToolSuccess = typeCheck (
                  unitOf (evaluated [
                    {
                      services.mokmokd = {
                        enable = true;
                        allowTool = [ "a\tb" ];
                      };
                    }
                  ])
                );
              in
              pkgs.runCommand "mokmokd-nixos-unit" { } ''
                install -Dm644 ${pkgs.writeText "mokmokd.service" (unitOf defaults)} $out/mokmokd.service
                install -Dm644 ${pkgs.writeText "mokmokd-custom.service" (unitOf custom)} $out/custom.service
                install -Dm644 ${pkgs.writeText "mokmokd-adversarial.service" (unitOf adversarial)} $out/adversarial.service

                # The unit text never reads the account, so a weakened merge of it would stay
                # latent until activation. Writing the resolved options here, and asserting them,
                # is what raises that merge error inside this check.
                install -Dm644 ${pkgs.writeText "mokmokd-custom-account.txt" ''
                  isSystemUser = ${lib.boolToString custom.config.users.users.custom.isSystemUser}
                  isNormalUser = ${lib.boolToString custom.config.users.users.custom.isNormalUser}
                  group = ${custom.config.users.users.custom.group}
                ''} $out/custom-account.txt

                # `tryEval` reports whether forcing the option value threw, which is where a
                # type outside the restriction surfaces; grep the results so a widened type
                # fails this check rather than passing unrendered.
                install -Dm644 ${pkgs.writeText "mokmokd-type-checks.txt" ''
                  allowToolNewlineSuccess = ${lib.boolToString newlineToolSuccess}
                  directorySlashSuccess = ${lib.boolToString slashDirectorySuccess}
                  allowToolTabSuccess = ${lib.boolToString tabToolSuccess}
                ''} $out/type-checks.txt

                # The defaults with the sample.
                grep -qF 'Type=notify' $out/mokmokd.service
                grep -qF 'NotifyAccess=main' $out/mokmokd.service
                grep -qF 'WatchdogSec=30s' $out/mokmokd.service
                grep -qF 'TimeoutStartSec=30s' $out/mokmokd.service
                grep -qF 'TimeoutStopSec=20s' $out/mokmokd.service
                grep -qF 'Restart=always' $out/mokmokd.service
                grep -qF 'RestartSec=1s' $out/mokmokd.service
                grep -qF 'UMask=0077' $out/mokmokd.service
                grep -qF 'User=mokmokd' $out/mokmokd.service
                grep -qF 'Group=mokmokd' $out/mokmokd.service
                grep -qF 'StateDirectory=mokmokd' $out/mokmokd.service
                grep -qF 'RuntimeDirectory=mokmokd' $out/mokmokd.service
                grep -qF 'NoNewPrivileges=true' $out/mokmokd.service
                # The hardening directives are the containment the unit records, and each is
                # named so that dropping one from serviceConfig fails here.
                grep -qF 'LockPersonality=true' $out/mokmokd.service
                grep -qF 'ProtectClock=true' $out/mokmokd.service
                grep -qF 'ProtectControlGroups=true' $out/mokmokd.service
                grep -qF 'ProtectKernelLogs=true' $out/mokmokd.service
                grep -qF 'ProtectKernelModules=true' $out/mokmokd.service
                grep -qF 'ProtectKernelTunables=true' $out/mokmokd.service
                grep -qF 'ProtectProc=invisible' $out/mokmokd.service
                grep -qF 'RestrictRealtime=true' $out/mokmokd.service
                grep -qF 'SystemCallArchitectures=native' $out/mokmokd.service
                # An empty bounding set is what keeps a service that can run as root from
                # holding any capability, and WantedBy is what enables the unit at boot.
                grep -qE '^CapabilityBoundingSet=$' $out/mokmokd.service
                grep -qE '^AmbientCapabilities=$' $out/mokmokd.service
                grep -qF 'WantedBy=multi-user.target' $out/mokmokd.service
                grep -qF 'StartLimitIntervalSec=60' $out/mokmokd.service
                grep -qF 'StartLimitBurst=5' $out/mokmokd.service

                # The ExecStart line is one string, so dropping an argument leaves the rest
                # looking right, and no assertion on the whole line notices. Every argument
                # is named on its own. --data-dir matters most: without it the daemon falls
                # back to its relative `mokmok-data` default and writes outside the directory
                # systemd created and owns.
                grep -qF 'ExecStart=' $out/mokmokd.service
                grep -qF -- '--data-dir %S/mokmokd' $out/mokmokd.service
                grep -qF -- '--allow-tool /bin/sh' $out/mokmokd.service
                grep -qF -- '--allow-path /nix/store' $out/mokmokd.service
                grep -qF -- '--allow-write /tmp' $out/mokmokd.service
                grep -qF -- '--socket %t/mokmokd/bus.sock' $out/mokmokd.service
                grep -qE '^ExecStart=[^ ]+ --data-dir %S/mokmokd( --allow-tool [^ ]+)*( --allow-path [^ ]+)*( --allow-write [^ ]+)* serve --socket %t/mokmokd/bus\.sock$' $out/mokmokd.service

                # Every option off its default. A directive that stopped reading its option,
                # or one hardcoded to the default name, is only visible here.
                grep -qF 'StateDirectory=custom' $out/custom.service
                grep -qF 'RuntimeDirectory=custom' $out/custom.service
                grep -qF 'User=custom' $out/custom.service
                grep -qF 'Group=custom' $out/custom.service
                grep -qF 'WatchdogSec=45s' $out/custom.service
                grep -qF -- '--data-dir %S/custom' $out/custom.service
                grep -qF -- '--allow-write /tmp/w' $out/custom.service
                grep -qF -- '--socket %t/custom/bus.sock' $out/custom.service
                grep -qE '^ExecStart=[^ ]+ --data-dir %S/custom( --allow-tool [^ ]+)*( --allow-path [^ ]+)*( --allow-write [^ ]+)* serve --socket %t/custom/bus\.sock$' $out/custom.service

                # The account the operator declared keeps nixpkgs' normal-user values rather
                # than the module's defaults.
                grep -qF 'isSystemUser = false' $out/custom-account.txt
                grep -qF 'isNormalUser = true' $out/custom-account.txt
                grep -qF 'group = users' $out/custom-account.txt

                # The values the parser rewrites. A dropped `systemdArg` is only visible here:
                # without it `$FOO` leaves the line, `%p` becomes a specifier, and a backslash
                # reaches systemd as an escape it turns into a control byte.
                grep -qF -- "--allow-tool '\$\$FOO'" $out/adversarial.service
                grep -qF -- "--allow-tool 'a\\\\b'" $out/adversarial.service
                grep -qF -- '--allow-path /nix/store/%%p' $out/adversarial.service
                grep -qF -- '--data-dir %S/mokmokd' $out/adversarial.service
                grep -qF -- '--socket %t/mokmokd/bus.sock' $out/adversarial.service

                grep -qF 'allowToolNewlineSuccess = false' $out/type-checks.txt
                grep -qF 'directorySlashSuccess = false' $out/type-checks.txt
                grep -qF 'allowToolTabSuccess = true' $out/type-checks.txt
              '';
          };

          devShells.default = pkgs.mkShellNoCC {
            inputsFrom = [ config.pre-commit.devShell ];
            packages =
              with pkgs;
              [
                cargo-mutants
                jq
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
