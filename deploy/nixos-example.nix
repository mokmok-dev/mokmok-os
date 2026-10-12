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
# The values below are examples. Both are absolute paths that have to exist at
# run time; the module passes them to the daemon and writes no file itself.
{
  services.mokmokd = {
    enable = true;
    allowTool = [ "/bin/sh" ];
    allowPath = [ "/nix/store" ];
  };
}
