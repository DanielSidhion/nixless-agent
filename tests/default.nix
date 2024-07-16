{ nix-serve-ng, nixless-agent-module, nixless-request-signer, pkgs, lib }:
let
  testPrivateKey = "test-cache:a9OHZ2CtyxOaAVJSIgvBa7QgW/ejKh2QLvXC1oXMlK+NaFGRIiOn87UsbjRx5RiaW/a0gOia+RS323buii8wFQ==";
  testPublicKey = "test-cache:jWhRkSIjp/O1LG40ceUYmlv2tIDomvkUt9t27oovMBU=";

  nixServeNgModule = nix-serve-ng.nixosModules.default;
  nixosLib = import (pkgs.path + "/nixos/lib") { };
  # We'll use this to build system configurations that we'll update the test machine to. The NixOS testing infrastructure adds a bunch of modules and other things to the machine config, and we need these same modules in the system configurations we'll update the test machine to. If we skipped all of this and directly evaluated a config (by using `pkgs.path + "/nixos/lib/eval-config.nix"`), we'd lose networking settings and some other settings that come with the NixOS testing infrastructure. Because the test machine is running under the NixOS testing infrastructure, we declare a fake NixOS test here to make sure it'll add all those modules and configs to the machine config we were given.
  evalTestMachine = machineConfig:
    let
      unused-test = nixosLib.runTest {
        name = "unused-test";
        hostPkgs = pkgs;

        # The machine to eval must be alphabetically always in the same position in all tests, because the NixOS testing infrastructure assigns ip addresses based on the alphabetical list of nodes, and we can't change the ip addresses from the config - they're marked as read-only. Additionally, the configuration of the test machine will depend on the other nodes listed in the test - for example, the `/etc/hosts` file will have contents that depend on the naming of the nodes and also their ip addresses, so the node names must be the same as in the other tests. The config of the other machines doesn't matter, so we just reuse the same config for every machine here.
        nodes = {
          binary_cache = machineConfig;
          test_machine = machineConfig;
        };
      };
    in
    unused-test.nodes.test_machine;

  generateSystemConfigRequestFile = machine:
    let
      machineTopLevel = machine.system.build.toplevel;
      machineName = machine.system.name;
      machineClosure = pkgs.writeClosure [ machineTopLevel ];
    in
    pkgs.runCommand "${machineName}-closure" { } ''
      # The system top level package must be the first line.
      echo ${machineTopLevel} >> $out
      # Removing the system top level package from the closure list.
      ${pkgs.lib.getExe pkgs.gnugrep} -v '${machineTopLevel}' ${machineClosure} >> $out
      ${pkgs.lib.getExe pkgs.gnused} -i 's/\/nix\/store\///g' $out
      ${lib.getExe nixless-request-signer} sign --private-key-encoded '${testPrivateKey}' --file-path $out >> $out
    '';

  getSystemPackageId = machine:
    let
      machineTopLevel = machine.system.build.toplevel;
      nixStoreLength = builtins.stringLength "/nix/store/";
    in
    builtins.substring nixStoreLength (-1) "${machineTopLevel}";

  binaryCacheNode = {
    imports = [
      (import ./binary_cache_machine.nix { inherit nixServeNgModule testPrivateKey; })
    ];

    virtualisation.additionalPaths = [ "${pkgs.jq}" "${newTestMachineRequest}" "${secondNewTestMachineRequest}" "${thirdNewTestMachineRequest}" ];
  };

  testMachineNode = import ./test_machine.nix { inherit nixless-agent-module testPublicKey; };

  newFileContents = "This proves the machine got updated to the new configuration.";
  newTestMachine = evalTestMachine {
    imports = [
      testMachineNode
      ({
        environment.etc.new-test-machine-tracker.text = newFileContents;
      })
    ];
  };
  newTestMachineRequest = generateSystemConfigRequestFile newTestMachine;

  secondNewFileContents = "This proves the machine got updated to the second new configuration.";
  secondNewTestMachine = evalTestMachine {
    imports = [
      testMachineNode
      ({
        environment.etc.second-new-test-machine-tracker.text = secondNewFileContents;
        services.nixless-agent.maxSystemHistoryCount = 1;
      })
    ];
  };
  secondNewTestMachineRequest = generateSystemConfigRequestFile secondNewTestMachine;

  thirdNewFileContents = "This proves the machine got updated to the third new configuration.";
  thirdNewTestMachine = evalTestMachine {
    imports = [
      testMachineNode
      ({
        environment.etc.third-new-test-machine-tracker.text = thirdNewFileContents;
      })
    ];
  };
  thirdNewTestMachineRequest = generateSystemConfigRequestFile thirdNewTestMachine;
in
{
  normal = nixosLib.runTest {
    name = "normal";
    hostPkgs = pkgs;
    globalTimeout = 120;

    nodes = {
      binary_cache = binaryCacheNode;
      test_machine = testMachineNode;
    };

    includeTestScriptReferences = false; # If this is left at the default of `true`, the test machine will end up with a local copy of the new configuration already, because it uses its own Nix store and the testing infrastructure will put the closure of the test script inside that Nix store.
    testScript = ''
      binary_cache.start()
      binary_cache.wait_for_unit("nix-serve.service")

      test_machine.start(True)
      test_machine.wait_for_unit("nixless-agent.service")

      binary_cache.wait_until_succeeds("curl -N http://test_machine:56321/summary | ${lib.getExe pkgs.jq} -e '.status == \"standby\"'", 20000)
      binary_cache.succeed("curl -N http://test_machine:56432/metrics | grep -q 'nixless_agent_system_version 0'")

      binary_cache.succeed("curl -i --fail-with-body -X POST --data-binary @${newTestMachineRequest} http://test_machine:56321/new-configuration")
      test_machine.wait_for_file("/etc/new-test-machine-tracker", 20000)
      file_contents = test_machine.succeed("cat /etc/new-test-machine-tracker")
      assert file_contents == "${newFileContents}"

      binary_cache.wait_until_succeeds("curl -N http://test_machine:56321/summary | ${lib.getExe pkgs.jq} -e '.status == \"standby\" and .current_config.system_package_id == \"${getSystemPackageId newTestMachine}\"'", 20000)
      binary_cache.succeed("curl -N http://test_machine:56432/metrics | grep -q 'nixless_agent_system_version 1' -")
      binary_cache.succeed("curl -N http://test_machine:56432/metrics | grep -q 'nixless_agent_requests_summary [[:digit:]]\+' -")
      binary_cache.succeed("curl -N http://test_machine:56432/metrics | grep -q 'nixless_agent_requests_new_configuration 1' -")
      binary_cache.succeed("curl -N http://test_machine:56432/metrics | grep -q 'nixless_agent_system_configuration_download_duration_count{system_package_id=\"${getSystemPackageId newTestMachine}\"} 1' -")
      binary_cache.succeed("curl -N http://test_machine:56432/metrics | grep -q 'nixless_agent_system_configuration_setup_duration_count{system_package_id=\"${getSystemPackageId newTestMachine}\"} 1' -")
      binary_cache.succeed("curl -N http://test_machine:56432/metrics | grep -q 'nixless_agent_system_configuration_switch_duration_count{system_package_id=\"${getSystemPackageId newTestMachine}\"} 1' -")
    '';
  };

  cleanOldConfiguration = nixosLib.runTest {
    name = "clean-old-configuration";
    hostPkgs = pkgs;
    globalTimeout = 120;

    nodes = {
      binary_cache = binaryCacheNode;
      test_machine = testMachineNode;
    };

    includeTestScriptReferences = false; # If this is left at the default of `true`, the test machine will end up with a local copy of the new configuration already, because it uses its own Nix store and the testing infrastructure will put the closure of the test script inside that Nix store.
    testScript = ''
      binary_cache.start()
      binary_cache.wait_for_unit("nix-serve.service")

      test_machine.start(True)
      test_machine.wait_for_unit("nixless-agent.service")

      binary_cache.wait_until_succeeds("curl -N http://test_machine:56321/summary | ${lib.getExe pkgs.jq} -e '.status == \"standby\"'", 20000)

      binary_cache.succeed("curl -i --fail-with-body -X POST --data-binary @${newTestMachineRequest} http://test_machine:56321/new-configuration")
      test_machine.wait_for_file("/etc/new-test-machine-tracker", 20000)

      binary_cache.wait_until_succeeds("curl -N http://test_machine:56321/summary | ${lib.getExe pkgs.jq} -e '.status == \"standby\"'", 20000)

      binary_cache.succeed("curl -i --fail-with-body -X POST --data-binary @${secondNewTestMachineRequest} http://test_machine:56321/new-configuration")
      test_machine.wait_for_file("/etc/second-new-test-machine-tracker", 20000)

      file_contents = test_machine.succeed("cat /etc/second-new-test-machine-tracker")
      assert file_contents == "${secondNewFileContents}"

      binary_cache.wait_until_succeeds("curl -N http://test_machine:56321/summary | ${lib.getExe pkgs.jq} -e '.status == \"standby\" and .current_config.system_package_id == \"${getSystemPackageId secondNewTestMachine}\"'", 20000)
      binary_cache.succeed("curl -N http://test_machine:56432/metrics | grep -q 'nixless_agent_system_version 2' -")

      # Should've been cleaned up.
      test_machine.fail("ls -l /etc/new-test-machine-tracker")
    '';
  };

  rollbackFromStandbyToLatest = nixosLib.runTest {
    name = "rollback-from-standby-to-latest";
    hostPkgs = pkgs;
    globalTimeout = 120;

    nodes = {
      binary_cache = binaryCacheNode;
      test_machine = testMachineNode;
    };

    includeTestScriptReferences = false; # If this is left at the default of `true`, the test machine will end up with a local copy of the new configuration already, because it uses its own Nix store and the testing infrastructure will put the closure of the test script inside that Nix store.
    testScript = ''
      binary_cache.start()
      binary_cache.wait_for_unit("nix-serve.service")

      test_machine.start(True)
      test_machine.wait_for_unit("nixless-agent.service")

      binary_cache.wait_until_succeeds("curl -N http://test_machine:56321/summary | ${lib.getExe pkgs.jq} -e '.status == \"standby\"'", 20000)

      binary_cache.succeed("curl -i --fail-with-body -X POST --data-binary @${newTestMachineRequest} http://test_machine:56321/new-configuration")
      test_machine.wait_for_file("/etc/new-test-machine-tracker", 20000)

      binary_cache.wait_until_succeeds("curl -N http://test_machine:56321/summary | ${lib.getExe pkgs.jq} -e '.status == \"standby\"'", 20000)

      binary_cache.succeed("curl -i --fail-with-body -X POST --data-binary @${thirdNewTestMachineRequest} http://test_machine:56321/new-configuration")
      test_machine.wait_for_file("/etc/third-new-test-machine-tracker", 20000)

      binary_cache.wait_until_succeeds("curl -N http://test_machine:56321/summary | ${lib.getExe pkgs.jq} -e '.status == \"standby\"'", 20000)

      binary_cache.succeed("curl -i --fail-with-body -X POST http://test_machine:56321/rollback-configuration")
      test_machine.wait_for_file("/etc/new-test-machine-tracker", 20000)

      binary_cache.wait_until_succeeds("curl -N http://test_machine:56321/summary | ${lib.getExe pkgs.jq} -e '.status == \"standby\" and .current_config.system_package_id == \"${getSystemPackageId newTestMachine}\"'", 20000)
      binary_cache.succeed("curl -N http://test_machine:56432/metrics | grep -q 'nixless_agent_system_version 3' -")
      binary_cache.succeed("curl -N http://test_machine:56432/metrics | grep -q 'nixless_agent_requests_rollback 1' -")

      # Should've been cleaned up.
      test_machine.fail("ls -l /etc/third-new-test-machine-tracker")
    '';
  };
}
