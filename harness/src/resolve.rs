//! One place that turns a machine plus a ROM path into a booted machine.
//!
//! Every boot path (the frontend, [`Harness::build`](crate::Harness::build),
//! the disasm CLIs, script sessions, the golden suite) resolves through here,
//! so the digest always describes the set the machine was actually built from
//! and every caller agrees on which revision that was.
//!
//! A revision is only ever built from its own archives: the set's ZIP, then
//! its aliases. An explicit choice is strict: it builds that revision or
//! errors, never silently a neighbour. With no choice the revisions are
//! tried in declaration order and the first build wins; when the winner is
//! not the default, that fallback is logged with the reason the default was
//! skipped.
//!
//! Three source shapes:
//!
//! * Pointed straight at an archive there is nothing to choose between: it is
//!   loaded and, when its stem names an accepted archive, that revision is
//!   built; otherwise each revision is tried against it in order.
//! * Pointed at a directory, each revision's archives are tried in order and
//!   the first build wins. The first candidate's error is the one reported,
//!   because it is the dump the old behaviour would have chosen and so the
//!   one a reader is most likely asking about.
//! * A directory holding no archive at all falls back to its loose files, the
//!   way it always has.
//!
//! Trying a candidate costs decompressing it, which is why resolution stops
//! at the first success rather than scoring them all.
//!
//! **The machine is the judge, not the filesystem.** Loading the first
//! archive present is not the same question as which archive satisfies a
//! revision's ROM entries (Donkey Kong Jr. declares two names but its entries
//! name the members of only one dump), so every candidate is built, not just
//! located.

use std::path::Path;

use phosphor_core::core::machine::FrontendMachine;
use phosphor_machines::registry::MachineEntry;
use phosphor_machines::rom_loader::{RomLoadError, RomSet};

use crate::movie::{Movie, rom_digest};
use crate::rom_path::load_rom_set;

/// Where a running machine's ROMs came from: the entry, which of its
/// revisions booted, and the digest of the set it was built from.
pub struct RomSource {
    /// The resolved registry entry.
    pub entry: &'static MachineEntry,
    /// Index into [`entry`](Self::entry) revisions of the booted revision.
    pub revision: usize,
    /// Digest of the set the machine was built from.
    pub digest: [u8; 32],
}

impl RomSource {
    /// Canonical MAME set name of the booted revision.
    pub fn set(&self) -> &'static str {
        self.entry.revisions[self.revision].set()
    }

    /// NVRAM filename stem: the revision's override, else the machine name.
    pub fn nvram_group(&self) -> &'static str {
        self.entry.revisions[self.revision]
            .nvram_group
            .unwrap_or(self.entry.name)
    }
}

/// A resolved boot: what was chosen plus the machine built from it.
pub struct Resolved {
    /// Where the ROMs came from.
    pub source: RomSource,
    /// The machine built from them (not yet reset).
    pub machine: Box<dyn FrontendMachine>,
}

/// Boot `entry` from `path`, optionally pinned to one ROM set.
///
/// `rom_set` names a set or alias and is strict: it builds that revision or
/// errors, and an unknown name lists what the machine accepts. `None` tries
/// the revisions in declaration order with the fallback logging the module
/// docs describe, except a direct archive whose stem names an accepted set
/// pins that revision before any loader runs.
pub fn resolve(
    entry: &'static MachineEntry,
    path: &str,
    rom_set: Option<&str>,
) -> Result<Resolved, String> {
    match rom_set {
        Some(name) => resolve_explicit(entry, path, name),
        None => resolve_default(entry, path),
    }
}

/// Revisions that could load from `path`: those with at least one archive
/// present, or every revision when `path` names no archives at all (a direct
/// ZIP or a loose-file directory), where any revision may match the bytes.
pub fn present_revisions(entry: &MachineEntry, path: &str) -> Vec<usize> {
    if !Path::new(path).is_dir() {
        return (0..entry.revisions.len()).collect();
    }
    let present: Vec<usize> = entry
        .revisions
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r.names
                .iter()
                .any(|n| Path::new(path).join(format!("{n}.zip")).exists())
        })
        .map(|(i, _)| i)
        .collect();
    if present.is_empty() {
        // No archives anywhere: a loose-file directory, where any revision
        // may match the bytes.
        (0..entry.revisions.len()).collect()
    } else {
        present
    }
}

/// Load revision `rev`'s set without building a machine, for the disasm
/// region and gfx commands, which assemble bytes rather than boot.
pub fn load_revision_set(entry: &MachineEntry, path: &str, rev: usize) -> Result<RomSet, String> {
    let revision = entry
        .revisions
        .get(rev)
        .ok_or_else(|| format!("'{}' has no revision {rev}", entry.name))?;
    if !Path::new(path).is_dir() {
        // Not a directory: the names are not consulted, exactly as in
        // `load_rom_set`.
        return load_rom_set(path, revision.names)
            .map_err(|e| format!("loading ROM set {path}: {e}"));
    }
    for &name in revision.names {
        if Path::new(path).join(format!("{name}.zip")).exists() {
            return load_rom_set(path, &[name])
                .map_err(|e| format!("loading ROM set {name}.zip: {e}"));
        }
    }
    // No archive of this revision: the loose-file fallback, as in `resolve`.
    load_rom_set(path, revision.names).map_err(|e| format!("loading ROM set {path}: {e}"))
}

// -- Movie replay boot selection -------------------------------------------------
//
// A replay boots what the recording booted: the movie's recorded set (v3), or
// the revision its digest identifies (v2 inference). An explicit `--rom-set`
// still wins over both — the callers apply it before calling here.

/// First archive of revision `rev` present in `path`, in preference order.
///
/// A directory names the archive to load. Anything else (a direct ZIP, a
/// loose-file directory) loads by path rather than by name, so the revision's
/// canonical set is returned and only selects which `create` runs.
pub fn present_archive(entry: &MachineEntry, path: &str, rev: usize) -> Option<&'static str> {
    let revision = entry.revisions.get(rev)?;
    if !Path::new(path).is_dir() {
        return Some(revision.set());
    }
    revision
        .names
        .iter()
        .find(|n| Path::new(path).join(format!("{n}.zip")).exists())
        .copied()
}

/// Revision named by a direct archive's stem, when the stem names an archive
/// the entry accepts. A directory is never consulted: its archives name their
/// revisions one by one already.
pub fn stem_revision(entry: &MachineEntry, path: &str) -> Option<usize> {
    if Path::new(path).is_dir() {
        return None;
    }
    let stem = Path::new(path).file_stem()?.to_str()?;
    entry.find_revision(stem)
}

/// Which revision a version 2 movie names by digest: the first archive on
/// disk, in declaration order, whose dump fingerprints to `digest`.
///
/// A tie — two revisions digesting identically — goes to the earlier revision,
/// matching default order. Pointed at anything but a directory there is only
/// one dump to compare and the default boot already checks it, so this
/// declines and the caller boots the default.
pub fn infer_rom_set(entry: &MachineEntry, path: &str, digest: &[u8; 32]) -> Option<&'static str> {
    if !Path::new(path).is_dir() {
        return None;
    }
    for revision in entry.revisions {
        for &name in revision.names {
            if !Path::new(path).join(format!("{name}.zip")).exists() {
                continue;
            }
            let Ok(set) = load_rom_set(path, &[name]) else {
                continue;
            };
            if rom_digest(&set) == *digest {
                return Some(name);
            }
        }
    }
    None
}

/// Which set a movie replay boots, and why (for mismatch diagnostics).
pub enum ReplayBoot {
    /// The v3 movie's recorded set, present on disk — possibly under an alias
    /// archive, which names the same revision.
    Recorded(&'static str),
    /// The v2 movie's digest matched this set's dump.
    Inferred(&'static str),
    /// The movie says nothing usable: boot the default and let the digest
    /// check report the mismatch.
    Default,
}

/// The set a movie replay should boot: the movie's recorded revision (v3), or
/// the revision its digest identifies (v2 inference).
///
/// Errors only when the movie names a set this machine does not know or that
/// is not on disk — both are strict failures, never a silent neighbouring
/// revision. A v2 digest matching nothing returns [`ReplayBoot::Default`]: the
/// default boot plus the digest check reports that better than this function
/// can.
pub fn replay_boot(entry: &MachineEntry, path: &str, movie: &Movie) -> Result<ReplayBoot, String> {
    if !movie.header.rom_set.is_empty() {
        let rev = entry.find_revision(&movie.header.rom_set).ok_or_else(|| {
            format!(
                "movie names unknown ROM set '{}' for '{}'; known sets: {}",
                movie.header.rom_set,
                entry.name,
                entry.archive_names().join(", ")
            )
        })?;
        let name = present_archive(entry, path, rev).ok_or_else(|| {
            format!(
                "ROM set '{}' for '{}' not found in {path}",
                movie.header.rom_set, entry.name
            )
        })?;
        return Ok(ReplayBoot::Recorded(name));
    }
    Ok(match infer_rom_set(entry, path, &movie.header.rom_digest) {
        Some(name) => ReplayBoot::Inferred(name),
        None => ReplayBoot::Default,
    })
}

/// [`replay_boot`] by machine name, for callers without the entry to hand. The
/// owned name feeds [`resolve`] or [`Harness::build`](crate::Harness::build)
/// directly; `None` means boot the default.
pub fn movie_boot_set(machine: &str, path: &str, movie: &Movie) -> Result<Option<String>, String> {
    let entry = phosphor_machines::registry::find(machine)
        .ok_or_else(|| format!("unknown machine '{machine}'"))?;
    match replay_boot(entry, path, movie)? {
        ReplayBoot::Recorded(name) | ReplayBoot::Inferred(name) => Ok(Some(name.to_string())),
        ReplayBoot::Default => Ok(None),
    }
}

fn resolve_explicit(
    entry: &'static MachineEntry,
    path: &str,
    name: &str,
) -> Result<Resolved, String> {
    let rev = entry.find_revision(name).ok_or_else(|| {
        format!(
            "unknown ROM set '{name}' for '{}'; known sets: {}",
            entry.name,
            entry.archive_names().join(", ")
        )
    })?;
    // An explicit choice names a set, and sets come in aliases: a directory
    // loads the canonical archive first, then each alias in turn. Loose
    // files have no set identity, so a directory holding none of the
    // revision's archives is simply missing it, and the error lists every
    // archive tried. (A direct ZIP path still loads whatever it points at.)
    let revision = &entry.revisions[rev];
    let archive = if Path::new(path).is_dir() {
        revision
            .names
            .iter()
            .find(|n| Path::new(path).join(format!("{n}.zip")).exists())
            .copied()
            .ok_or_else(|| {
                let tried = revision
                    .names
                    .iter()
                    .map(|n| format!("{n}.zip"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "ROM set '{name}' for '{}' not found in {path} (tried {tried})",
                    entry.name
                )
            })?
    } else {
        name
    };
    let set =
        load_rom_set(path, &[archive]).map_err(|e| format!("loading ROM set {archive}: {e}"))?;
    let machine = (entry.create)(&set, rev).map_err(|e| {
        format!(
            "creating machine '{}' from ROM set '{archive}': {e}",
            entry.name
        )
    })?;
    Ok(Resolved {
        source: RomSource {
            entry,
            revision: rev,
            digest: rom_digest(&set),
        },
        machine,
    })
}

fn resolve_default(entry: &'static MachineEntry, path: &str) -> Result<Resolved, String> {
    // Pointed straight at an archive there is nothing to choose between.
    if !Path::new(path).is_dir() {
        let set = load_rom_set(path, &entry.archive_names())
            .map_err(|e| format!("loading ROM set {path}: {e}"))?;
        // A stem naming an accepted archive pins the revision before any
        // loader runs: `mhavocp.zip` boots the prototype even where an
        // earlier loader would also accept its bytes. A stem that names
        // nothing falls through to matching below.
        if let Some(revision) = stem_revision(entry, path) {
            let machine = (entry.create)(&set, revision)
                .map_err(|e| format!("creating machine '{}': {e}", entry.name))?;
            return Ok(Resolved {
                source: RomSource {
                    entry,
                    revision,
                    digest: rom_digest(&set),
                },
                machine,
            });
        }
        let (revision, machine) = first_accepting(entry, &set)
            .map_err(|e| format!("creating machine '{}': {e}", entry.name))?;
        return Ok(Resolved {
            source: RomSource {
                entry,
                revision,
                digest: rom_digest(&set),
            },
            machine,
        });
    }

    let mut first_error = None;
    let mut default_reason: Option<String> = None;
    for (rev, revision) in entry.revisions.iter().enumerate() {
        for &name in revision.names {
            if !Path::new(path).join(format!("{name}.zip")).exists() {
                continue;
            }
            let set = match load_rom_set(path, &[name]) {
                Ok(set) => set,
                Err(e) => {
                    first_error.get_or_insert(format!("loading ROM set {name}.zip: {e}"));
                    if rev == 0 {
                        default_reason.get_or_insert(format!("loading {name}.zip failed: {e}"));
                    }
                    continue;
                }
            };
            match (entry.create)(&set, rev) {
                Ok(machine) => {
                    if rev > 0 {
                        log::warn!(
                            "{}: default ROM set '{}' unavailable ({}); using '{}'",
                            entry.name,
                            entry.default_revision().set(),
                            default_reason.as_deref().unwrap_or("no archive present"),
                            revision.set()
                        );
                    }
                    return Ok(Resolved {
                        source: RomSource {
                            entry,
                            revision: rev,
                            digest: rom_digest(&set),
                        },
                        machine,
                    });
                }
                Err(e) => {
                    first_error.get_or_insert(format!("creating machine '{}': {e}", entry.name));
                    if rev == 0 {
                        default_reason.get_or_insert(format!("{name}.zip did not build: {e}"));
                    }
                }
            }
        }
    }

    // No candidate archive worked. Fall back so a loose-file directory, which
    // names no archive at all, still resolves the way it always has.
    match first_error {
        Some(e) => Err(e),
        None => {
            let set = load_rom_set(path, &entry.archive_names())
                .map_err(|e| format!("loading ROM set {path}: {e}"))?;
            let (revision, machine) = first_accepting(entry, &set)
                .map_err(|e| format!("creating machine '{}': {e}", entry.name))?;
            Ok(Resolved {
                source: RomSource {
                    entry,
                    revision,
                    digest: rom_digest(&set),
                },
                machine,
            })
        }
    }
}

/// Build `entry` from `set`, trying each revision in declaration order.
fn first_accepting(
    entry: &MachineEntry,
    set: &RomSet,
) -> Result<(usize, Box<dyn FrontendMachine>), RomLoadError> {
    let mut last_err = None;
    for rev in 0..entry.revisions.len() {
        match (entry.create)(set, rev) {
            Ok(machine) => return Ok((rev, machine)),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("an entry always declares a revision"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};

    use phosphor_machines::registry;

    static TOY_REVISIONS: &[phosphor_machines::registry::Revision] = &[
        phosphor_machines::registry::Revision {
            names: &["rev0a", "rev0alias"],
            nvram_group: None,
        },
        phosphor_machines::registry::Revision {
            names: &["rev1a", "rev1alias"],
            nvram_group: None,
        },
    ];

    /// A toy entry whose revisions accept disjoint marker members. The built
    /// box is a real bare machine — resolution never runs it, so any box
    /// will do.
    static TOY: MachineEntry = MachineEntry::new(
        "toy",
        TOY_REVISIONS,
        toy_create,
        toy_bare,
        toy_bare_revision,
        &[],
    );

    fn toy_create(set: &RomSet, rev: usize) -> Result<Box<dyn FrontendMachine>, RomLoadError> {
        let marker = ["rev0.bin", "rev1.bin"][rev];
        if set.get(marker).is_none() {
            return Err(RomLoadError::MissingFile(marker.to_string()));
        }
        Ok(toy_bare())
    }

    fn toy_bare() -> Box<dyn FrontendMachine> {
        (registry::find("joust").unwrap().create_bare)()
    }

    fn toy_bare_revision(_rev: usize) -> Box<dyn FrontendMachine> {
        toy_bare()
    }

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn scratch() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "phosphor-resolve-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_zip(dir: &Path, name: &str, members: &[(&str, &[u8])]) {
        let file = std::fs::File::create(dir.join(format!("{name}.zip"))).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (member, bytes) in members {
            zip.start_file(*member, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn default_revision_wins_when_present() {
        let dir = scratch();
        write_zip(&dir, "rev0a", &[("rev0.bin", &[1, 2, 3])]);
        write_zip(&dir, "rev1a", &[("rev1.bin", &[4, 5, 6])]);
        let resolved = resolve(&TOY, dir.to_str().unwrap(), None).unwrap();
        assert_eq!(resolved.source.revision, 0);
        assert_eq!(resolved.source.set(), "rev0a");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_default_falls_back_to_next_present() {
        let dir = scratch();
        write_zip(&dir, "rev1a", &[("rev1.bin", &[4, 5, 6])]);
        let resolved = resolve(&TOY, dir.to_str().unwrap(), None).unwrap();
        assert_eq!(resolved.source.revision, 1);
        assert_eq!(resolved.source.set(), "rev1a");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn explicit_choice_never_falls_back() {
        let dir = scratch();
        write_zip(&dir, "rev1a", &[("rev1.bin", &[4, 5, 6])]);
        // Only rev1 is here; asking for rev0 must error naming the set and
        // the path, not quietly boot rev1.
        let Err(err) = resolve(&TOY, dir.to_str().unwrap(), Some("rev0a")) else {
            panic!("explicit choice for a missing set must fail");
        };
        assert!(
            err.contains("rev0a") && err.contains(dir.to_str().unwrap()),
            "unexpected error: {err}"
        );
        let resolved = resolve(&TOY, dir.to_str().unwrap(), Some("rev1a")).unwrap();
        assert_eq!(resolved.source.revision, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_set_lists_what_the_machine_accepts() {
        let dir = scratch();
        let Err(err) = resolve(&TOY, dir.to_str().unwrap(), Some("nope")) else {
            panic!("unknown set must fail");
        };
        assert!(
            err.contains("nope") && err.contains("rev0a") && err.contains("rev1a"),
            "unexpected error: {err}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn alias_selects_its_revision() {
        let dir = scratch();
        write_zip(&dir, "rev0alias", &[("rev0.bin", &[1])]);
        let resolved = resolve(&TOY, dir.to_str().unwrap(), Some("rev0alias")).unwrap();
        assert_eq!(resolved.source.revision, 0);
        // And in default order the alias archive satisfies rev0.
        let resolved = resolve(&TOY, dir.to_str().unwrap(), None).unwrap();
        assert_eq!(resolved.source.revision, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn direct_zip_builds_whichever_revision_matches() {
        let dir = scratch();
        write_zip(&dir, "rev1a", &[("rev1.bin", &[7])]);
        let zip = dir.join("rev1a.zip");
        let resolved = resolve(&TOY, zip.to_str().unwrap(), None).unwrap();
        assert_eq!(resolved.source.revision, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn explicit_canonical_loads_from_an_alias_archive() {
        // Only the alias archive is on disk; asking for the canonical set
        // still boots the revision it names.
        let dir = scratch();
        write_zip(&dir, "rev0alias", &[("rev0.bin", &[1])]);
        let resolved = resolve(&TOY, dir.to_str().unwrap(), Some("rev0a")).unwrap();
        assert_eq!(resolved.source.revision, 0);
        assert_eq!(resolved.source.set(), "rev0a");
        assert_eq!(
            resolved.source.digest,
            digest_of(&dir, "rev0alias"),
            "the bytes came from the alias archive"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn explicit_alias_prefers_the_canonical_archive() {
        // Both archives hold a buildable dump; the canonical one wins even
        // when the choice named the alias.
        let dir = scratch();
        write_zip(&dir, "rev0a", &[("rev0.bin", &[1])]);
        write_zip(&dir, "rev0alias", &[("rev0.bin", &[2])]);
        let resolved = resolve(&TOY, dir.to_str().unwrap(), Some("rev0alias")).unwrap();
        assert_eq!(resolved.source.revision, 0);
        assert_eq!(
            resolved.source.digest,
            digest_of(&dir, "rev0a"),
            "the bytes came from the canonical archive"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn explicit_missing_set_lists_the_archives_tried() {
        let dir = scratch();
        write_zip(&dir, "rev1a", &[("rev1.bin", &[4, 5, 6])]);
        let Err(err) = resolve(&TOY, dir.to_str().unwrap(), Some("rev0a")) else {
            panic!("explicit choice for a missing set must fail");
        };
        assert!(
            err.contains("rev0a")
                && err.contains("rev0a.zip")
                && err.contains("rev0alias.zip")
                && err.contains(dir.to_str().unwrap()),
            "unexpected error: {err}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn direct_zip_stem_names_the_revision_before_matching() {
        // The dump would satisfy rev0 first, but the stem names rev1's
        // alias, so rev1 boots.
        let dir = scratch();
        write_zip(&dir, "rev1alias", &[("rev0.bin", &[1]), ("rev1.bin", &[7])]);
        let zip = dir.join("rev1alias.zip");
        let resolved = resolve(&TOY, zip.to_str().unwrap(), None).unwrap();
        assert_eq!(resolved.source.revision, 1);
        assert_eq!(resolved.source.set(), "rev1a");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn direct_zip_stem_pins_the_revision_strictly() {
        // The stem names rev1 but the dump only satisfies rev0: the stem
        // wins and the build fails, rather than silently booting rev0.
        let dir = scratch();
        write_zip(&dir, "rev1a", &[("rev0.bin", &[1])]);
        let zip = dir.join("rev1a.zip");
        let Err(err) = resolve(&TOY, zip.to_str().unwrap(), None) else {
            panic!("a stem-named revision the bytes cannot build must fail");
        };
        assert!(err.contains("creating machine"), "unexpected error: {err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn direct_zip_unknown_stem_still_matches_by_bytes() {
        // A stem naming no archive keeps the old behavior: the bytes pick.
        let dir = scratch();
        write_zip(&dir, "renamed", &[("rev1.bin", &[7])]);
        let zip = dir.join("renamed.zip");
        let resolved = resolve(&TOY, zip.to_str().unwrap(), None).unwrap();
        assert_eq!(resolved.source.revision, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn loose_directory_resolves_like_before() {
        let dir = scratch();
        std::fs::write(dir.join("rev1.bin"), [9, 9]).unwrap();
        let resolved = resolve(&TOY, dir.to_str().unwrap(), None).unwrap();
        assert_eq!(resolved.source.revision, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn digest_describes_the_booted_set() {
        let dir = scratch();
        write_zip(&dir, "rev1a", &[("rev1.bin", &[4, 5, 6])]);
        let resolved = resolve(&TOY, dir.to_str().unwrap(), None).unwrap();
        let expected = rom_digest(&load_rom_set(dir.to_str().unwrap(), &["rev1a"]).unwrap());
        assert_eq!(resolved.source.digest, expected);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn movie_with(rom_set: &str, digest: [u8; 32]) -> Movie {
        Movie {
            header: crate::movie::MovieHeader {
                machine: "toy".into(),
                rom_set: rom_set.into(),
                rom_digest: digest,
                controls: Vec::new(),
                dip: Vec::new(),
                nvram: None,
                host_sample_rate: 44_100,
                frames: 0,
            },
            records: Vec::new(),
        }
    }

    fn digest_of(dir: &Path, name: &str) -> [u8; 32] {
        rom_digest(&load_rom_set(dir.to_str().unwrap(), &[name]).unwrap())
    }

    #[test]
    fn inference_finds_the_revision_whose_dump_matches() {
        let dir = scratch();
        write_zip(&dir, "rev0a", &[("rev0.bin", &[1, 2, 3])]);
        write_zip(&dir, "rev1a", &[("rev1.bin", &[4, 5, 6])]);
        let path = dir.to_str().unwrap();
        assert_eq!(
            infer_rom_set(&TOY, path, &digest_of(&dir, "rev1a")),
            Some("rev1a")
        );
        assert_eq!(
            infer_rom_set(&TOY, path, &digest_of(&dir, "rev0a")),
            Some("rev0a")
        );
        assert_eq!(infer_rom_set(&TOY, path, &[0xEE; 32]), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inference_tie_goes_to_the_earlier_revision() {
        // Two revisions whose dumps are byte-identical: inference cannot tell
        // them apart, so default order decides.
        let dir = scratch();
        write_zip(&dir, "rev0a", &[("shared.bin", &[1, 2])]);
        write_zip(&dir, "rev1a", &[("shared.bin", &[1, 2])]);
        let found = infer_rom_set(&TOY, dir.to_str().unwrap(), &digest_of(&dir, "rev1a"));
        assert_eq!(found, Some("rev0a"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inference_declines_outside_directories() {
        // One dump, no choice to make: the default boot plus the digest check
        // already covers it.
        let dir = scratch();
        write_zip(&dir, "rev1a", &[("rev1.bin", &[7])]);
        let zip = dir.join("rev1a.zip");
        let digest = digest_of(&dir, "rev1a");
        assert_eq!(infer_rom_set(&TOY, zip.to_str().unwrap(), &digest), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replay_boot_uses_the_recorded_set() {
        let dir = scratch();
        write_zip(&dir, "rev1a", &[("rev1.bin", &[4, 5, 6])]);
        let path = dir.to_str().unwrap();
        let boot = replay_boot(&TOY, path, &movie_with("rev1a", [0; 32])).unwrap();
        assert!(matches!(boot, ReplayBoot::Recorded("rev1a")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replay_boot_accepts_an_alias_archive_for_the_recorded_set() {
        // Recorded against rev0's canonical set, replayed from a collection
        // holding only its alias: same revision, so it boots.
        let dir = scratch();
        write_zip(&dir, "rev0alias", &[("rev0.bin", &[1])]);
        let path = dir.to_str().unwrap();
        let boot = replay_boot(&TOY, path, &movie_with("rev0a", [0; 32])).unwrap();
        assert!(matches!(boot, ReplayBoot::Recorded("rev0alias")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replay_boot_rejects_unknown_and_missing_sets() {
        let dir = scratch();
        write_zip(&dir, "rev1a", &[("rev1.bin", &[4, 5, 6])]);
        let path = dir.to_str().unwrap();
        let Err(err) = replay_boot(&TOY, path, &movie_with("nope", [0; 32])) else {
            panic!("unknown recorded set must fail");
        };
        assert!(
            err.contains("unknown ROM set") && err.contains("rev0a"),
            "unexpected error: {err}"
        );
        let Err(err) = replay_boot(&TOY, path, &movie_with("rev0a", [0; 32])) else {
            panic!("missing recorded set must fail");
        };
        assert!(err.contains("not found in"), "unexpected error: {err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replay_boot_falls_back_to_default_when_nothing_matches() {
        let dir = scratch();
        write_zip(&dir, "rev1a", &[("rev1.bin", &[4, 5, 6])]);
        let path = dir.to_str().unwrap();
        // v2 digest matching nothing on disk: boot the default and let the
        // digest check report it.
        let boot = replay_boot(&TOY, path, &movie_with("", [0xEE; 32])).unwrap();
        assert!(matches!(boot, ReplayBoot::Default));
        // ...while a matching digest infers instead of defaulting.
        let boot = replay_boot(&TOY, path, &movie_with("", digest_of(&dir, "rev1a"))).unwrap();
        assert!(matches!(boot, ReplayBoot::Inferred("rev1a")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
