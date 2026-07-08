//! Bounded filesystem walker for local provider logs.
//!
//! Stack-based, with an mtime cutoff and a file cap, so a large Codex sqlite or
//! huge history dir can't stall the CLI.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Visit every file with the given extension under `dir` (recursively),
/// newest-first-ish by directory order, skipping files older than `cutoff` and
/// stopping after `max_files` visits. `visit` is called per file.
pub fn recent_files(
    dir: &Path,
    extension: &str,
    cutoff: Option<SystemTime>,
    max_files: u32,
    visit: &mut impl FnMut(&Path),
) {
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    let mut count = 0u32;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            let Ok(ft) = ent.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() && p.extension().and_then(|e| e.to_str()) == Some(extension) {
                if count >= max_files {
                    return;
                }
                count += 1;
                if let Some(c) = cutoff {
                    if let Ok(m) = ent.metadata() {
                        if let Ok(mt) = m.modified() {
                            if mt < c {
                                continue;
                            }
                        }
                    }
                }
                visit(&p);
            }
        }
    }
}

/// Convenience: `recent_files` for `*.jsonl` logs.
pub fn recent_jsonl(
    dir: &Path,
    cutoff: Option<SystemTime>,
    max_files: u32,
    visit: &mut impl FnMut(&Path),
) {
    recent_files(dir, "jsonl", cutoff, max_files, visit);
}
