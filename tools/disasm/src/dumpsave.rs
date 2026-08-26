//! `disasm dump-save`: print the chunk tree of a save file.
//!
//! The dump is produced by *loading* the file into a bare machine of the id its
//! header names, with the reader recording every chunk it enters. That matters:
//! a save body interleaves inline scalars with framed components, so nothing
//! about the bytes says where a chunk starts. Walking them speculatively would
//! be guesswork that reads plausibly and is wrong. Only the machine's own
//! `load_state` knows its layout, so the tool asks it.
//!
//! The consequence worth knowing: a file that fails to load still prints
//! everything read before it stopped, and the last line is where it stopped.
//! That is usually the answer.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::Path;

use phosphor_core::core::save_state::{
    CHUNK_HEADER_LEN, ChunkTrace, MIN_SUPPORTED_SAVE_VERSION, SAVE_MAGIC, SAVE_VERSION, crc32,
};
use phosphor_machines::registry;

fn unknown_machine(name: &str) -> String {
    format!(
        "no registered machine named '{name}'; \
         pass --machine to name the one that wrote this file \
         (`disasm machines` lists them)"
    )
}

/// What a save file's header says about itself, read without a machine.
#[derive(Debug)]
struct Header {
    version: u32,
    machine_id: String,
    /// Byte offset of the first chunk.
    body_start: usize,
}

fn parse_header(data: &[u8]) -> Result<Header, String> {
    if data.len() < 16 {
        return Err(format!(
            "too short to be a save file ({} bytes)",
            data.len()
        ));
    }
    if &data[..4] != SAVE_MAGIC {
        return Err(format!(
            "bad magic {:02X?}, expected {:02X?}",
            &data[..4],
            SAVE_MAGIC
        ));
    }
    let version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let id_len = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
    let end = 12 + id_len;
    if end > data.len() {
        return Err(format!("machine id length {id_len} runs past the file"));
    }
    let machine_id = String::from_utf8(data[12..end].to_vec())
        .map_err(|_| "machine id is not UTF-8".to_string())?;
    Ok(Header {
        version,
        machine_id,
        body_start: end,
    })
}

pub fn run(
    file: Option<&Path>,
    machine_override: Option<&str>,
    max_depth: Option<usize>,
) -> Result<String, String> {
    let (source, data) = match (file, machine_override) {
        (Some(f), _) => {
            let data = std::fs::read(f).map_err(|e| format!("reading {}: {e}", f.display()))?;
            (f.display().to_string(), data)
        }
        // No file: save a bare machine and dump that, which is the layout this
        // build expects a file to have.
        (None, Some(name)) => {
            let entry = registry::find(name).ok_or_else(|| unknown_machine(name))?;
            let data = (entry.create_bare)()
                .save_state()
                .ok_or_else(|| format!("machine '{name}' does not support save states"))?;
            (format!("<{name}, freshly built>"), data)
        }
        (None, None) => return Err("give a save file, or --machine to dump a layout".into()),
    };
    let header = parse_header(&data)?;

    let mut out = String::new();
    let _ = writeln!(out, "{source}");
    let _ = writeln!(out, "  size          {} bytes", data.len());
    let _ = writeln!(
        out,
        "  envelope      version {} (this build reads {}..={})",
        header.version, MIN_SUPPORTED_SAVE_VERSION, SAVE_VERSION
    );
    let _ = writeln!(out, "  machine id    {}", header.machine_id);

    // Checksum, reported rather than enforced: a corrupt file is exactly the
    // one worth walking as far as it goes.
    if data.len() >= 4 {
        let (body, trailer) = data.split_at(data.len() - 4);
        let stored = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
        let computed = crc32(body);
        let verdict = if stored == computed {
            "ok".to_string()
        } else {
            format!("MISMATCH (computed {computed:#010x})")
        };
        let _ = writeln!(out, "  checksum      {stored:#010x}  {verdict}");
    }

    if header.version < MIN_SUPPORTED_SAVE_VERSION || header.version > SAVE_VERSION {
        let _ = writeln!(
            out,
            "\nthis build cannot read envelope version {}, so there is no chunk tree to show.",
            header.version
        );
        return Ok(out);
    }

    let wanted = machine_override.unwrap_or(&header.machine_id);
    let entry = registry::find(wanted).ok_or_else(|| unknown_machine(wanted))?;

    // A bare machine: real devices, zero-filled ROM. Nothing here runs the
    // game, and the layout of the state does not depend on ROM contents.
    let mut machine = (entry.create_bare)();
    let trace = RefCell::new(ChunkTrace::new());
    let result = machine.load_state_traced(&data, &trace);
    let trace = trace.borrow();

    let _ = writeln!(
        out,
        "\n  tag   length     offset  component (names from the reader, not from the bytes)"
    );
    let mut top_framed = 0usize;
    let mut top_level = 0usize;
    let mut elided = 0usize;
    for e in trace.events() {
        if e.depth == 0 {
            top_framed += e.len as usize + CHUNK_HEADER_LEN;
            top_level += 1;
        }
        if max_depth.is_some_and(|d| e.depth > d) {
            elided += 1;
            continue;
        }
        let indent = "    ".repeat(e.depth);
        let note = if e.skipped {
            "   [in the file, not in this build's configuration: skipped]"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "  {:>3}  {:>7}  {:#09x}  {indent}{}{note}",
            e.tag, e.len, e.offset, e.name
        );
    }
    if trace.events().is_empty() {
        let _ = writeln!(out, "  (none)");
    }
    // Never let a depth limit read as "that is all there was".
    if elided > 0 {
        let _ = writeln!(
            out,
            "  ... {elided} chunks deeper than --max-depth not shown"
        );
    }

    let body_len = data.len() - header.body_start - 4;
    let _ = writeln!(
        out,
        "\n  {} chunks, {top_level} at the top level covering {top_framed} of {body_len} body \
         bytes. The rest are inline scalars and blobs, which carry no framing.",
        trace.events().len()
    );
    match result {
        Ok(()) => {
            let _ = writeln!(out, "  load: ok");
        }
        Err(e) => {
            let _ = writeln!(out, "  load: FAILED after the chunks above");
            let _ = writeln!(out, "        {e}");
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_file_is_reported_not_panicked_on() {
        let err = parse_header(b"PHOS").unwrap_err();
        assert!(err.contains("too short"), "{err}");
    }

    #[test]
    fn bad_magic_is_reported() {
        let err = parse_header(b"NOPE\x0d\x00\x00\x00\x05\x00\x00\x00joust").unwrap_err();
        assert!(err.contains("bad magic"), "{err}");
    }

    #[test]
    fn the_header_reads_back_what_save_machine_wrote() {
        let mut data = Vec::new();
        data.extend_from_slice(SAVE_MAGIC);
        data.extend_from_slice(&SAVE_VERSION.to_le_bytes());
        data.extend_from_slice(&5u32.to_le_bytes());
        data.extend_from_slice(b"joust");
        data.extend_from_slice(&[0; 8]);

        let h = parse_header(&data).unwrap();
        assert_eq!(h.version, SAVE_VERSION);
        assert_eq!(h.machine_id, "joust");
        assert_eq!(h.body_start, 17);
    }
}
