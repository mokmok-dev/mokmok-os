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
