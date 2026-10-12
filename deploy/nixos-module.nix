{
  config,
  lib,
  ...
}:

let
  cfg = config.services.mokmokd;
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
      type = lib.types.str;
      default = "mokmokd";
      description = ''
        Name of the systemd state and runtime directory. The unit passes
        `%S/` plus this name as the data directory and `%t/` plus this name plus
        `/bus.sock` as the socket, and declares the same name in
        `StateDirectory` and `RuntimeDirectory`, so systemd owns everything the
        daemon writes.
      '';
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "mokmokd";
      description = ''
        User the daemon runs as. Set it to `root` to run as root, in which case
        this module creates no user.
      '';
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "mokmokd";
      description = ''
        Group the daemon runs as. Set it to `root` to run in the root group, in
        which case this module creates no group.
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
    users = lib.mkIf (cfg.user != "root") {
      users.${cfg.user} = {
        isSystemUser = true;
        inherit (cfg) group;
        description = "mokmok OS kernel daemon";
      };
      groups.${cfg.group} = { };
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
            value
          ]) cfg.allowTool
          ++ lib.concatMap (value: [
            "--allow-path"
            value
          ]) cfg.allowPath
          ++ lib.concatMap (value: [
            "--allow-write"
            value
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
