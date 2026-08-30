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
            # Alfred Arnold's macro assembler, and the p2bin that turns its code
            # file into a raw image. Assembles the synthetic conformance ROMs in
            # machines/tests/roms, which are our own source rather than arcade
            # ROMs and so can be committed and rebuilt. Chosen over the 6809-only
            # lwtools, which nixpkgs does not carry: asl targets 6502, Z80, 68000
            # and the rest, so a conformance ROM for a second board needs no new
            # assembler here.
            pkgs.asl
            # RustSec advisory check over Cargo.lock: `cargo audit`. The tree is
            # mostly inert for an offline emulator, but `zip` and `flate2` parse
            # ROM archives from wherever the user got them, so that path is worth
            # watching. Lives here so the check runs in the same shell as the
            # build rather than needing an ad-hoc `cargo install`.
            pkgs.cargo-audit
            # Renders the netlists in docs/schematics to the SVGs committed
            # beside them, via docs/schematics/render.sh. Auto-places and
            # auto-routes through ELK, so a circuit excerpt is written as nets
            # and never as coordinates. Chosen over KiCad, whose schematic side
            # has no scripting API and whose file format would put every symbol
            # placement and wire segment in the diff by hand.
            pkgs.netlistsvg
          ] ++ linuxPkgs;

          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath ([
            pkgs.SDL2
            pkgs.libGL
          ] ++ linuxLibs);

          shellHook = ''
            export CC="${pkgs.clang}/bin/clang"
            export CXX="${pkgs.clang}/bin/clang++"
            # Tells the conformance-ROM drift guard that an assembler is
            # supposed to be on PATH here, so a missing one is a failure rather
            # than a skip. Without this the guard reports green whenever the
            # toolchain is absent, which is exactly how it reported green while
            # no assembler existed anywhere. CI sets nothing and still skips.
            export PHOSPHOR_ASM=1
          '';
        };
      });
}
