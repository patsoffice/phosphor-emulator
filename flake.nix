{
  description = "Phosphor Emulator — cycle-accurate retro CPU emulator framework";

  inputs = {
    # Pinned to the same nixpkgs rev that shell.nix used (nixos-unstable 2026-06-06).
    # Bump with: nix flake update
    nixpkgs.url = "github:NixOS/nixpkgs/a799d3e3886da994fa307f817a6bc705ae538eeb";
    flake-utils.url = "github:numtide/flake-utils";
    flake-compat = {
      url = "github:edolstra/flake-compat";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
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
      {
        devShells.default = pkgs.mkShell {
          buildInputs = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
            pkgs.clang
            pkgs.SDL2
            pkgs.pkg-config
            pkgs.libGL
            pkgs.ast-grep # structural (AST-aware) search/replace for code mods
          ] ++ linuxPkgs;

          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath ([
            pkgs.SDL2
            pkgs.libGL
          ] ++ linuxLibs);

          shellHook = ''
            export CC="${pkgs.clang}/bin/clang"
            export CXX="${pkgs.clang}/bin/clang++"
          '';
        };
      });
}
