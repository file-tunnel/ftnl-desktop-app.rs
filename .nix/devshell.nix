{ pkgs, agentCheck }:
pkgs.mkShell {
  packages = [
    # encrypted env files — env/enc/*.env.enc, see env/README.md
    pkgs.sops
    pkgs.age
    pkgs.python3
    pkgs.just
    agentCheck
    pkgs.actionlint
    pkgs.cargo
    pkgs.clippy
    pkgs.git
    pkgs.nixfmt
    pkgs.python312
    pkgs.ripgrep
    pkgs.rustc
    pkgs.rustfmt
    pkgs.shellcheck
    pkgs.shfmt
    pkgs.tlaplus
  ]
  ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
    pkgs.libGL
    pkgs.libxkbcommon
    pkgs.pkg-config
    pkgs.wayland
    pkgs.xorg.libX11
    pkgs.xorg.libXi
    pkgs.xorg.libXrandr
  ];

  RUST_BACKTRACE = "1";
}
