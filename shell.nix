{ pkgs ? import <nixpkgs> {} }:

let
  isLinux = pkgs.stdenv.isLinux;
  linuxPkgs = pkgs.lib.optionals isLinux [
    pkgs.wayland
    pkgs.wayland-protocols
    pkgs.libxkbcommon
  ];
  linuxLibs = pkgs.lib.optionals isLinux [
    pkgs.wayland
    pkgs.libxkbcommon
  ];
in
pkgs.mkShell {
  buildInputs = [
    pkgs.cargo
    pkgs.rustc
    pkgs.clang
    pkgs.SDL2
    pkgs.pkg-config
    pkgs.libGL
  ] ++ linuxPkgs;

  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath ([
    pkgs.SDL2
    pkgs.libGL
  ] ++ linuxLibs);

  shellHook = ''
    export CC="${pkgs.clang}/bin/clang"
    export CXX="${pkgs.clang}/bin/clang++"
  '';
}
