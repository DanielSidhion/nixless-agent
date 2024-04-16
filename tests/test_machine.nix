{ nixless-agent-module }:
{ modulesPath, ... }:
{
  imports = [
    nixless-agent-module
    (modulesPath + "/profiles/minimal.nix")
    (modulesPath + "/profiles/headless.nix")
  ];

  # boot.loader.systemd-boot.enable = true;

  nix.enable = false;

  networking.firewall.allowedTCPPorts = [ 56321 ];
  services.nixless-agent = {
    enable = true;
    cacheUrl = "http://binary_cache:8090/test-cache";
    port = 56321;
  };

  # We don't want the host nix store to be made available to this guest, since we want to take control of it with nixless-agent.
  virtualisation.useNixStoreImage = true;
  virtualisation.writableStore = true;
}
