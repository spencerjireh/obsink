use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Suffix of the temp files `write_atomic` stages next to the target. The
/// hasher skips these so a leftover from a crash is never treated as a note.
pub const TEMP_SUFFIX: &str = ".obsink-tmp";

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` through a temp file in the same directory plus a
/// rename, so a crash mid-write never leaves a truncated target behind. On
/// APFS (macOS/iOS, the only targets) `rename` replaces the destination
/// atomically. Missing parent directories are created.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let temp = parent.join(format!(
        ".{file_name}.{}-{}{TEMP_SUFFIX}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let result = (|| {
        let mut file = fs::File::create(&temp)?;
        io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{write_atomic, TEMP_SUFFIX};

    #[test]
    fn write_atomic_leaves_no_temp_on_success() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("note.md");

        write_atomic(&target, b"first").unwrap();
        write_atomic(&target, b"second").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "second");
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["note.md".to_string()]);
        assert!(!names.iter().any(|name| name.ends_with(TEMP_SUFFIX)));
    }

    #[test]
    fn write_atomic_creates_parents() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("a/b/c.md");

        write_atomic(&target, b"deep").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "deep");
    }
}
