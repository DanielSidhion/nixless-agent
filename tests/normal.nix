{ nix-serve-ng, nixless-agent-module, pkgs }:
let
  nixServeNgModule = nix-serve-ng.nixosModules.default;
  nixosLib = import (pkgs.path + "/nixos/lib") { };

  newTestMachineTopLevel = testToRun.nodes.new_test_machine.system.build.toplevel;
  newTestMachineClosure = pkgs.writeClosure [ newTestMachineTopLevel ];
  newTestMachineClosureSorted = pkgs.runCommand "new-test-machine-closure" { } ''
    echo ${newTestMachineTopLevel} >> $out
    ${pkgs.lib.getExe pkgs.gnugrep} -v '${newTestMachineTopLevel}' ${newTestMachineClosure} >> $out
    ${pkgs.lib.getExe pkgs.gnused} -i 's/\/nix\/store\///g' $out
  '';

  newFileContents = "This proves the machine got updated to the new configuration.";

  testToRun = nixosLib.runTest {
    name = "normal";
    hostPkgs = pkgs;
    globalTimeout = 120;

    nodes = {
      binary_cache = {
        imports = [
          (import ./binary_cache_machine.nix { inherit nixServeNgModule; })
        ];

        virtualisation.additionalPaths = [ "${newTestMachineClosureSorted}" ];
      };

      test_machine = {
        imports = [
          (import ./test_machine.nix { inherit nixless-agent-module; })
        ];
      };

      # This machine will never be started, we just have it here so we can build its system configuration. It's the configuration that we'll update `test_machine` to.
      new_test_machine = {
        imports = [
          (import ./test_machine.nix { inherit nixless-agent-module; })
          ({
            environment.etc.new-test-machine-tracker.text = newFileContents;
          })
        ];
      };
    };

    includeTestScriptReferences = false; # If this is left at the default of `true`, the test machine will end up with a local copy of the new configuration already, because it uses its own Nix store and the testing infrastructure will put the closure of the test script inside that Nix store.
    testScript = ''
      binary_cache.start()
      binary_cache.wait_for_unit("nix-serve.service")

      test_machine.start()
      test_machine.wait_for_unit("nixless-agent.service")

      binary_cache.succeed("curl -i -X POST --data-binary @${newTestMachineClosureSorted} http://test_machine:56321/new-configuration")
      test_machine.wait_for_file("/etc/new-test-machine-tracker", 20000)
      file_contents = test_machine.succeed("cat /etc/new-test-machine-tracker")

      assert file_contents == "${newFileContents}"
    '';
  };
in
testToRun
