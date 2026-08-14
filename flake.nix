{
  description = "Reproducible File Tunnel Rust desktop development shell";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      mkAgentCheck =
        pkgs:
        pkgs.writeShellApplication {
          name = "agent-check";
          runtimeInputs = [
            pkgs.actionlint
            pkgs.bash
            pkgs.cargo
            pkgs.clippy
            pkgs.coreutils
            pkgs.git
            pkgs.python312
            pkgs.rustc
            pkgs.rustfmt
            pkgs.shellcheck
            pkgs.shfmt
            pkgs.tlaplus
          ];
          text = builtins.readFile ./.nix/agent-check.sh;
        };
    in
    {
      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
      packages = forAllSystems (
        system:
        let
          agentCheck = mkAgentCheck nixpkgs.legacyPackages.${system};
        in
        {
          agent-check = agentCheck;
          default = agentCheck;
        }
      );
      checks = forAllSystems (system: {
        agent-check = mkAgentCheck nixpkgs.legacyPackages.${system};
      });
      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = import ./.nix/devshell.nix {
            inherit pkgs;
            agentCheck = mkAgentCheck pkgs;
          };
        }
      );
    };
}
