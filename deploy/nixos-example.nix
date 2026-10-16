# Copy this file into your configuration.
#
# The flake input:
#
#   # flake.nix
#   inputs.mokmok-os.url = "github:mokmok-dev/mokmok-os";
#
# The import line, next to the module the flake publishes:
#
#   nixpkgs.lib.nixosSystem {
#     modules = [
#       inputs.mokmok-os.nixosModules.default
#       ./nixos-example.nix
#     ];
#   };
#
# The values below are examples. Each is an absolute path that has to exist at
# run time; the module passes them to the daemon and writes no file itself. A
# tool a workload runs lives in the store, and /tmp is where one may write.
{
  services.mokmokd = {
    enable = true;
    allowTool = [ "/bin/sh" ];
    allowPath = [ "/nix/store" ];
    allowWrite = [ "/tmp" ];
  };
}

# Running the agent is an invocation, not the unit. `mokmokd agent run` drives one session against
# the same data directory the service serves, so it takes its own grants, and it runs as whoever
# invokes it rather than as the service user:
#
#   mokmokd --data-dir /var/lib/mokmokd \
#     --allow-tool /bin/sh --allow-path /nix/store --allow-write /srv/checkout \
#     agent run --session "$(cat /proc/sys/kernel/random/uuid)" \
#       --goal 'make the failing test in src/parse.rs pass' \
#       --checkout /srv/checkout --max-turns 24 --token-budget 400000 \
#       --provider ollama --model kimi-k2.7-code \
#       --provider-credential-file /run/secrets/ollama
#
# Two things about that command are deliberate. The credential is a file, never an environment
# variable: the daemon's environment is inherited by nothing today, but a file cannot reach a
# confined tool by accident, and a file is what a secret manager can hand over. And the tool's
# environment is exactly `PATH` plus every `--tool-env NAME=VALUE`, so a command sees no variable
# the operator did not name. Add `--tool-env CARGO_HOME=/srv/cargo` and grant `/srv/cargo` the same
# way as the checkout if the task builds Rust.
#
# The provider, the model, the endpoint, the script, the credential path, and the answer ceiling can
# instead live in `$XDG_CONFIG_HOME/mokmok/config.toml`, which an invocation reads when no flag names
# them: a flag beats the file, and the file beats the built-in default. A value in that file may name
# a variable of the daemon's environment, so the configuration file can name the credential path
# through a variable, and the environment says where the secret lives and never what it is. A
# variable must reach the daemon's own environment to be answered, so with the shipped unit that
# means an `Environment=` line or `systemctl --user set-environment`, and a variable exported in an
# interactive shell does not arrive.
#
# A tool cannot reach the network: the sandbox denies TCP `bind` and `connect` to every workload,
# which needs Linux 6.7 or newer. The model call is the daemon's own, validated against the
# configured destination, and `--endpoint http://127.0.0.1:11434/v1` keeps the checkout on the host.
# `examples/coding-agent/run.sh` runs one task end to end and asserts the file it produced.
