{
  description = "NixOS agent.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nixos-generators = {
      url = "github:nix-community/nixos-generators";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # Nix language server used with VSCode.
    nil = {
      url = "github:oxalica/nil";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, nixos-generators, nil, ... }:
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

      packages.x86_64-linux = {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "nix-tree-sizes";
          version = "0.1.0";

          src = ./.;
          cargoHash = "sha256-9qTWAimy+GUVHqiPQ3jgvIUYUoOdVsWTQMbkPO8UfgM=";
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

                  nix.enable = false;

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
