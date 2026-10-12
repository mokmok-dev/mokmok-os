{
  config,
  lib,
  ...
}:

let
  cfg = config.services.mokmokd;

  # nixpkgs defines a normal user's `group` and `isSystemUser` with `lib.mkDefault`, so a plain
  # `lib.mkDefault` here ties with nixpkgs and fails the merge. 1100 sits between the option's
  # own default and `lib.mkDefault`, so the module fills in an account it creates and yields to
  # nixpkgs or the operator for one they define.
  accountDefault = lib.mkOverride 1100;

  # systemd parses ExecStart with its own rules, not a shell's. It expands `$VAR` and `%`
  # specifiers and processes C escapes even inside single quotes, which only stop word
  # splitting there. `lib.escapeShellArgs` alone therefore loses an operator's `$FOO` as a
  # removed word and turns `%p` into a specifier, so each operator value is neutralised
  # first. The module's own `%S` and `%t` fragments are passed through untouched.
  systemdArg = value: lib.replaceStrings [ "%" "$" "\\" ] [ "%%" "$$" "\\\\" ] value;
in
{
  options.services.mokmokd = {
    enable = lib.mkEnableOption "the mokmok kernel daemon";

    package = lib.mkOption {
      type = lib.types.package;
      description = ''
        The mokmok package. It must provide `bin/mokmokd` and the confiner
        beside it, because the daemon defaults to the confiner next to its own
        executable. This file sets no default, because a plain NixOS module
        cannot know which package the operator built. The flake's
        `nixosModules.default` sets the default to its own package. An import of
        this file directly has to set it.
      '';
    };

    directory = lib.mkOption {
      type = lib.types.strMatching "[A-Za-z0-9][A-Za-z0-9_.-]*";
      default = "mokmokd";
      description = ''
        Name of the systemd state and runtime directory. The unit passes
        `%S/` plus this name as the data directory and `%t/` plus this name plus
        `/bus.sock` as the socket, and declares the same name in
        `StateDirectory` and `RuntimeDirectory`, so systemd owns everything the
        daemon writes. The name must start with a letter or digit and may hold
        only letters, digits, `_`, `.`, and `-`, because systemd rejects a
        directory name with a `/` and the unit would otherwise hand a `%`, `$`,
        or `\` to the parser that reads the command line.
      '';
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "mokmokd";
      description = ''
        User the daemon runs as. The module declares `users.users.<user>` to
        fill in `isSystemUser`, `group`, and a description when nothing else
        defines them, so an account the operator declares keeps every attribute
        it sets, including the `group` and `isSystemUser` that nixpkgs derives
        for an `isNormalUser` account. The module defines nothing else about the
        account. Set it to `root` to run as root, in which case the module
        declares no user at all.
      '';
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "mokmokd";
      description = ''
        Group the daemon runs as. The module declares `users.groups.<group>`
        when this is not `root` and defines nothing else about it, so the
        operator's own definition of that group survives. Set it to `root` to
        run in the root group, in which case the module declares no group.
      '';
    };

    allowTool = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = ''
        Tool programs the agent principal may invoke. Each value is an absolute
        path that has to exist at run time; the module passes it to the command
        line and copies no file into the store. Nothing is allowed by default,
        and the daemon refuses a program it cannot resolve.
      '';
    };

    allowPath = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = ''
        Directories a workload may read and execute in. Each value is an
        absolute path that has to exist at run time; the module passes it to the
        command line and copies no file into the store. Nothing is allowed by
        default, and the daemon refuses a path it cannot resolve.
      '';
    };

    allowWrite = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = ''
        Directories a workload may also write in. Each value is an absolute path
        that has to exist at run time; the module passes it to the command line
        and copies no file into the store. Nothing is allowed by default.
      '';
    };

    watchdogSec = lib.mkOption {
      type = lib.types.ints.positive;
      default = 30;
      description = ''
        Seconds the daemon has to answer the systemd watchdog before systemd
        restarts it.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    users.users = lib.mkIf (cfg.user != "root") {
      ${cfg.user} = {
        isSystemUser = accountDefault true;
        group = accountDefault cfg.group;
        description = accountDefault "mokmok OS kernel daemon";
      };
    };

    users.groups = lib.mkIf (cfg.group != "root") {
      ${cfg.group} = { };
    };

    systemd.services.mokmokd = {
      description = "mokmok OS kernel daemon";
      wantedBy = [ "multi-user.target" ];
      # deploy/mokmokd.service records why the directives it names are the whole set: add no
      # RestrictNamespaces=, because mokmok-sandbox plans unshare(CLONE_NEWUSER | CLONE_NEWPID) as
      # its containment, no ProtectSystem=/ProtectHome=, which make an --allow-path under /home
      # unreachable, and no RestrictSUIDSGID=, which fails openat2 with ENOSYS where the sandbox
      # resolves a workload's path. Read that file for the rest of the why; this serviceConfig
      # mirrors it.
      startLimitIntervalSec = 60;
      startLimitBurst = 5;
      serviceConfig = {
        Type = "notify";
        NotifyAccess = "main";
        WatchdogSec = "${toString cfg.watchdogSec}s";
        TimeoutStartSec = "30s";
        TimeoutStopSec = "20s";
        Restart = "always";
        RestartSec = "1s";
        StateDirectory = cfg.directory;
        RuntimeDirectory = cfg.directory;
        User = cfg.user;
        Group = cfg.group;
        UMask = "0077";
        NoNewPrivileges = true;
        CapabilityBoundingSet = "";
        AmbientCapabilities = "";
        LockPersonality = true;
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "invisible";
        RestrictRealtime = true;
        SystemCallArchitectures = "native";
        # clap global options go before the subcommand and subcommand options after it, so
        # --data-dir and the allow options precede `serve` and --socket follows it.
        ExecStart = lib.escapeShellArgs (
          [
            "${cfg.package}/bin/mokmokd"
            "--data-dir"
            "%S/${cfg.directory}"
          ]
          ++ lib.concatMap (value: [
            "--allow-tool"
            (systemdArg value)
          ]) cfg.allowTool
          ++ lib.concatMap (value: [
            "--allow-path"
            (systemdArg value)
          ]) cfg.allowPath
          ++ lib.concatMap (value: [
            "--allow-write"
            (systemdArg value)
          ]) cfg.allowWrite
          ++ [
            "serve"
            "--socket"
            "%t/${cfg.directory}/bus.sock"
          ]
        );
      };
    };
  };
}
