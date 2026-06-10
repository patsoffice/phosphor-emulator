# Compatibility shim so `nix-shell` keeps working without flakes enabled.
# The real definition lives in flake.nix (devShells.default); this re-exports it
# via flake-compat, reading the pinned revision straight from flake.lock.
(import
  (
    let lock = builtins.fromJSON (builtins.readFile ./flake.lock);
    in fetchTarball {
      url = "https://github.com/edolstra/flake-compat/archive/${lock.nodes.flake-compat.locked.rev}.tar.gz";
      sha256 = lock.nodes.flake-compat.locked.narHash;
    }
  )
  { src = ./.; }
).shellNix
