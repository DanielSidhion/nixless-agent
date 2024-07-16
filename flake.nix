{
  description = "NixOS agent.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    nix-serve-ng = {
      url = "github:aristanetworks/nix-serve-ng";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    nil = {
      url = "github:oxalica/nil";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, nix-serve-ng, nil, ... }:
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
            clang

            dbus.dev
            systemdLibs.dev
            pkg-config

            # Both of these used with VSCode.
            nixpkgs-fmt
            nil.packages.${system}.default
          ];

          hardeningDisable = [ "fortify" ];

          env = {
            RUST_BACKTRACE = "full";
            RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";
            LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
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

            # TODO: Some incompatibility with libseccomp which is built by the `foundations` crate, should look into this to re-enable this setting in the future.
            hardeningDisable = [ "fortify" ];

            # TODO: remove.
            buildType = "debug";

            nativeBuildInputs = [ pkgs.pkg-config pkgs.rustPlatform.bindgenHook ];
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

          nixless-request-signer = pkgs.rustPlatform.buildRustPackage {
            pname = "nixless-request-signer";
            version = "0.1.0";

            src = ./.;
            buildAndTestSubdir = "nixless-request-signer";
            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            meta = {
              description = "nixless-agent request signer";
              mainProgram = "nixless-request-signer";
              maintainers = with pkgs.lib.maintainers; [ danielsidhion ];
            };
          };
        };

      checks.x86_64-linux =
        let
          # Run `nix build .#.checks.x86_64-linux.<test_name>.driverInteractive` to build an interactive version of the check so you can inspect it if it fails.
          # Inside the interactive session, you can either run the function `test_script()` to run the entire test, or call things individually. It works like a Python REPL. To log into a machine, run `machine_name.shell_interactive()`.
          nixless-agent-tests = pkgs.callPackage ./tests/default.nix {
            inherit nix-serve-ng;
            inherit (self.packages.x86_64-linux) nixless-request-signer;
            nixless-agent-module = import ./service.nix
              {
                inherit (self.packages.x86_64-linux) nixless-agent system-switch-tracker;
              };
          };
        in
        nixless-agent-tests;
    };
}
