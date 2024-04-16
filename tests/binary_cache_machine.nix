{ atticModule, atticClient }:
{ modulesPath, ... }:
{
  imports = [
    atticModule
    (modulesPath + "/profiles/minimal.nix")
    (modulesPath + "/profiles/headless.nix")
  ];

  nix.enable = false;

  environment.systemPackages = [ atticClient ];

  # The attic module has an assertion that prevents us from simply passing a Nix package path to its `credentialsFile` attribute, so we have to create a credentials file like this. The simpler method would be to just use `pkgs.writeText`.
  environment.etc.atticCredentials = {
    text = "ATTIC_SERVER_TOKEN_HS256_SECRET_BASE64=\"qRx8WSv9OVv1/zCesASUUGMvtUY1uu9TjXEGbsyv1QTUEqFDxDlevqbQBD0nMedc6R76iCqwLZeKCXSphnzQ/Q==\"";
  };

  networking.firewall.allowedTCPPorts = [ 8090 ];

  services.atticd = {
    enable = true;
    credentialsFile = "/etc/atticCredentials";
    settings.listen = "0.0.0.0:8090";

    settings.chunking = {
      # The minimum NAR size to trigger chunking
      #
      # If 0, chunking is disabled entirely for newly-uploaded NARs.
      # If 1, all NARs are chunked.
      nar-size-threshold = 0; # 64 KiB

      # The preferred minimum size of a chunk, in bytes
      min-size = 16 * 1024; # 16 KiB

      # The preferred average size of a chunk, in bytes
      avg-size = 64 * 1024; # 64 KiB

      # The preferred maximum size of a chunk, in bytes
      max-size = 256 * 1024; # 256 KiB
    };
  };
}
