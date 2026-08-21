{
  description = "Phosphor Emulator — cycle-accurate retro CPU emulator framework";

  inputs = {
    # Tracks the nixos-unstable branch rather than a fixed rev. The exact commit
    # is still pinned — in flake.lock — so builds stay reproducible; the branch
    # is what lets `nix flake update` actually move the toolchain forward. A rev
    # here would have made that command a no-op for nixpkgs.
    # Bump with: nix flake update
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
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
        isLinux = pkgs.stdenv.hostPlatform.isLinux;
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
            # RustSec advisory check over Cargo.lock: `cargo audit`. The tree is
            # mostly inert for an offline emulator, but `zip` and `flate2` parse
            # ROM archives from wherever the user got them, so that path is worth
            # watching. Lives here so the check runs in the same shell as the
            # build rather than needing an ad-hoc `cargo install`.
            pkgs.cargo-audit
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
