{ pkgs, agentCheck }:
pkgs.mkShell {
  packages = [
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
