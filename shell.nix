{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  buildInputs = [
    pkgs.cargo
    pkgs.rustc
    pkgs.clang
    pkgs.SDL2
    pkgs.pkg-config
    pkgs.libGL
    pkgs.wayland
    pkgs.wayland-protocols
    pkgs.libxkbcommon
  ];

  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
    pkgs.SDL2
    pkgs.libGL
    pkgs.wayland
    pkgs.libxkbcommon
  ];

  shellHook = ''
    export CC="${pkgs.clang}/bin/clang"
    export CXX="${pkgs.clang}/bin/clang++"
  '';
}
