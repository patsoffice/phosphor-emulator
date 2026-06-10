{
  pkgs ? import (fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/a799d3e3886da994fa307f817a6bc705ae538eeb.tar.gz";
    sha256 = "11mhk782xy1n58518f86k6fcvxjaaim3mk9nmhx68fg5i2jg9ayx";
  }) {}
}:

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
