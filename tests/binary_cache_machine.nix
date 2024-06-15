{ nixServeNgModule, testPrivateKey }:
{ modulesPath, pkgs, ... }:
{
  imports = [
    nixServeNgModule
    (modulesPath + "/profiles/minimal.nix")
    (modulesPath + "/profiles/headless.nix")
  ];

  virtualisation.graphics = false;
  environment.etc.nixServeSecretKey.text = testPrivateKey;

  services.nix-serve = {
    enable = true;
    port = 8090;
    openFirewall = true;

    secretKeyFile = "/etc/nixServeSecretKey";
  };
}
