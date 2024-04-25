{ nixServeNgModule }:
{ modulesPath, pkgs, ... }:
{
  imports = [
    nixServeNgModule
    (modulesPath + "/profiles/minimal.nix")
    (modulesPath + "/profiles/headless.nix")
  ];

  virtualisation.graphics = false;

  environment.etc.nixServeSecretKey.text = "test-cache:a9OHZ2CtyxOaAVJSIgvBa7QgW/ejKh2QLvXC1oXMlK+NaFGRIiOn87UsbjRx5RiaW/a0gOia+RS323buii8wFQ==";

  services.nix-serve = {
    enable = true;
    port = 8090;
    openFirewall = true;

    secretKeyFile = "/etc/nixServeSecretKey";
  };
}
