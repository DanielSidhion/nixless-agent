{ attic, nixless-agent-module, pkgs }:
let
  atticModule = attic.nixosModules.atticd;
  atticClient = attic.packages.x86_64-linux.attic-client;
  nixosLib = import (pkgs.path + "/nixos/lib") { };
in
nixosLib.runTest {
  name = "normal";
  hostPkgs = pkgs;

  nodes = {
    binary_cache = {
      imports = [
        (import ./binary_cache_machine.nix { inherit atticModule atticClient; })
      ];
    };

    test_machine = {
      imports = [
        (import ./test_machine.nix { inherit nixless-agent-module; })
      ];
    };
  };

  testScript = ''
    import re

    binary_cache.start()
    binary_cache.wait_for_unit("atticd.service")

    token = binary_cache.succeed("atticd-atticadm make-token --sub test --validity 1y --push '*' --pull '*' --create-cache '*' --configure-cache '*' --delete '*' --configure-cache-retention '*' --destroy-cache '*'")
    token = token.strip()
    binary_cache.succeed(f"attic login default http://127.0.0.1:8090 {token}")
    binary_cache.succeed("attic cache create --public test-cache")

    # For some reason `attic cache info` outputs to stderr, so we need to redirect here to capture its output.
    cache_public_key = binary_cache.succeed("attic cache info test-cache 2>&1")
    match = re.search(r"Public Key: (test-cache:.*)$", cache_public_key, flags=re.MULTILINE)
    assert match is not None
    cache_public_key = match[1]

    test_machine.start()
    test_machine.wait_for_unit("nixless-agent.service")
  '';
}
