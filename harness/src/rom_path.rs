//! ROM path resolution: loads a [`RomSet`] from a MAME-style rompath,
//! a direct ZIP file, or a directory of loose ROM files.

use phosphor_machines::rom_loader::{RomLoadError, RomSet};
use std::path::Path;

/// Resolve a ROM-set path into a [`RomSet`].
///
/// Resolution order:
/// 1. `path` ends with `.zip` → load that archive.
/// 2. `path` is a directory containing `{rom_name}.zip` (any provided name) → load it.
/// 3. `path` is a directory of loose files → [`RomSet::from_directory`].
pub fn load_rom_set(path: &str, rom_names: &[&str]) -> Result<RomSet, RomLoadError> {
    let p = Path::new(path);

    if p.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
    {
        return load_from_zip(p);
    }

    if p.is_dir() {
        for name in rom_names {
            let zip_path = p.join(format!("{name}.zip"));
            if zip_path.exists() {
                return load_from_zip(&zip_path);
            }
        }
        return RomSet::from_directory(p);
    }

    Err(RomLoadError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("ROM path not found: {}", p.display()),
    )))
}

/// Extract every file from a ZIP archive into a [`RomSet`].
fn load_from_zip(path: &Path) -> Result<RomSet, RomLoadError> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut archive = zip::ZipArchive::new(reader).map_err(zip_err)?;

    let mut entries = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(zip_err)?;
        if entry.is_dir() {
            continue;
        }
        // Zip 9 reports an undecodable entry name as an error instead of
        // lossy-converting it. MAME sets are ASCII, so a bad name means a
        // stranger archive: skip that entry with a warning rather than
        // failing the whole load on one filename.
        let Some(name) = entry_name(i, path, entry.name()) else {
            continue;
        };
        let mut data = Vec::with_capacity(entry.size() as usize);
        std::io::Read::read_to_end(&mut entry, &mut data)?;
        entries.push((name, data));
    }
    Ok(RomSet::from_entries(entries))
}

/// Decide what to do with one archive entry's name: `Some` loads it, `None`
/// skips it with a warning. Zip 9 surfaces undecodable names as an error;
/// those entries are strangers in the archive, not missing ROMs, so the
/// load carries on without them.
fn entry_name(
    index: usize,
    archive: &Path,
    raw: Result<std::borrow::Cow<'_, str>, zip::result::ZipError>,
) -> Option<String> {
    match raw {
        Ok(name) => Some(name.into_owned()),
        Err(e) => {
            log::warn!("skipping entry {index} in {}: {e}", archive.display());
            None
        }
    }
}

fn zip_err(e: zip::result::ZipError) -> RomLoadError {
    RomLoadError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("ZIP error: {e}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    fn create_test_zip(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> std::path::PathBuf {
        let zip_path = dir.join(name);
        let file = File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (fname, data) in files {
            zip.start_file(*fname, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        zip_path
    }

    #[test]
    fn resolve_zip_file_directly() {
        let dir = std::env::temp_dir().join("phosphor_rompath_test_zip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let zip_path = create_test_zip(&dir, "joust.zip", &[("rom.bin", &[0xAA; 16])]);

        let rom_set = load_rom_set(zip_path.to_str().unwrap(), &["joust"]).unwrap();
        assert_eq!(rom_set.get("rom.bin"), Some(&[0xAA; 16][..]));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_zip_from_rompath_directory() {
        let dir = std::env::temp_dir().join("phosphor_rompath_test_dir");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        create_test_zip(&dir, "joust.zip", &[("rom.bin", &[0xBB; 8])]);

        let rom_set = load_rom_set(dir.to_str().unwrap(), &["joust"]).unwrap();
        assert_eq!(rom_set.get("rom.bin"), Some(&[0xBB; 8][..]));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An undecodable entry name skips that entry instead of failing the
    /// whole archive. Zip 9's own writer lossy-converts exotic names on the
    /// way in, so no public writer API produces the error case; this pins
    /// the decision directly.
    #[test]
    fn undecodable_entry_names_are_skipped() {
        let bad = zip::result::ZipError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Cannot translate path from cp437",
        ));
        assert_eq!(
            entry_name(3, Path::new("mame.zip"), Err(bad)),
            None,
            "a bad name skips the entry, it does not fail the load"
        );
        assert_eq!(
            entry_name(3, Path::new("mame.zip"), Ok("a.bin".into())),
            Some("a.bin".to_string())
        );
    }

    #[test]
    fn resolve_loose_directory_fallback() {
        let dir = std::env::temp_dir().join("phosphor_rompath_test_loose");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("test.rom"), [0xCC; 4]).unwrap();

        let rom_set = load_rom_set(dir.to_str().unwrap(), &["joust"]).unwrap();
        assert_eq!(rom_set.get("test.rom"), Some(&[0xCC; 4][..]));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
