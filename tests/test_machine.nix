{ nixless-agent-module, testPublicKey }:
{ modulesPath, ... }:
{
  imports = [
    nixless-agent-module
    (modulesPath + "/profiles/minimal.nix")
    (modulesPath + "/profiles/headless.nix")
  ];

  system.stateVersion = "24.05";
  nix.enable = false;

  networking.firewall.allowedTCPPorts = [ 56321 ];
  services.nixless-agent = {
    enable = true;
    cacheUrl = "http://binary_cache:8090";
    cachePublicKey = testPublicKey;
    # We reuse the same cache key here because it doesn't really matter in this test scenario - but in production this should never be the case!
    updatePublicKey = testPublicKey;
    port = 56321;
  };

  # We don't want the host nix store to be made available to this guest, since we want to take control of it with nixless-agent.
  virtualisation.useNixStoreImage = true;
  virtualisation.writableStore = true;
  virtualisation.graphics = false;
  virtualisation.useBootLoader = true;
}
