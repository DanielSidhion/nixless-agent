{
  description = "NixOS agent.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nixos-generators = {
      url = "github:nix-community/nixos-generators";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    attic = {
      url = "github:zhaofengli/attic";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    nil = {
      url = "github:oxalica/nil";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, nixos-generators, attic, nil, ... }:
    let
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
    in
    {
      devShells.x86_64-linux = {
        default = pkgs.mkShell {
          packages = with pkgs; [
            git
            cargo
            rustc
            rust-analyzer
            rustfmt
            just

            dbus.dev
            systemdLibs.dev
            pkg-config

            # Both of these used with VSCode.
            nixpkgs-fmt
            nil.packages.${system}.default
          ];

          env = {
            RUST_BACKTRACE = "full";
            RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";
          };
        };
      };

      packages.x86_64-linux =
        let
          nixless-agent-pkg = pkgs.rustPlatform.buildRustPackage {
            pname = "nixless-agent";
            version = "0.1.0";

            src = ./.;
            buildAndTestSubdir = "nixless-agent";
            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            # TODO: remove.
            buildType = "debug";

            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = [ pkgs.dbus.dev pkgs.systemdLibs.dev ];

            meta = {
              description = "nixless-agent";
              mainProgram = "nixless-agent";
              maintainers = with pkgs.lib.maintainers; [ danielsidhion ];
            };
          };
        in
        {
          default = nixless-agent-pkg;
          nixless-agent = nixless-agent-pkg;

          system-switch-tracker = pkgs.rustPlatform.buildRustPackage {
            pname = "system-switch-tracker";
            version = "0.1.0";

            src = ./.;
            buildAndTestSubdir = "system-switch-tracker";
            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            meta = {
              description = "nixless-agent system switch tracker";
              mainProgram = "system-switch-tracker";
              maintainers = with pkgs.lib.maintainers; [ danielsidhion ];
            };
          };
        };

      checks.x86_64-linux = {
        # Run `nix build .#.checks.x86_64-linux.normal.driverInteractive` to build an interactive version of the check so you can inspect it if it fails.
        # Inside the interactive session, you can either run the function `test_script()` to run the entire test, or call things individually. It works like a Python REPL. To log into a machine, run `machine_name.shell_interactive()`.
        normal = pkgs.callPackage ./tests/normal.nix {
          inherit attic;
          nixless-agent-module = import ./service.nix
            {
              inherit (self.packages.x86_64-linux) nixless-agent system-switch-tracker;
            };
        };
      };

      systems = {
        nixless =
          let
            builtSystem = nixpkgs.lib.nixosSystem {
              system = "x86_64-linux";
              modules = [
                # nixos-generators.nixosModules.qcow-efi
                nixos-generators.nixosModules.qcow
                (nixpkgs.outPath + "/nixos/modules/profiles/minimal.nix")
                (nixpkgs.outPath + "/nixos/modules/profiles/headless.nix")
                # (nixpkgs.outPath + "/nixos/modules/profiles/perlless.nix")
                ({ lib, pkgs, ... }: {
                  # boot.loader.systemd-boot.enable = true;

                  nix. enable = false;

                  services.openssh.enable = true;
                  services.openssh.settings.PermitRootLogin = "yes";
                  users.users.root.password = "123456";
                })
              ];
            };
          in
          {
            inherit (builtSystem.config.system.build) toplevel qcow;
            inherit (builtSystem) options config;
          };
      };
    };
}
