//! Keyed placement file.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const HEADER: &str = "win-taskbar-host placement 1";

/// Validates a persistence key and returns its file path under
/// `%LOCALAPPDATA%\win-taskbar-host\`.
pub(crate) fn store_path(key: &str) -> Result<PathBuf, String> {
    let valid = !key.is_empty()
        && key.len() <= 100
        && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !key.starts_with('.');
    if !valid {
        return Err(format!(
            "placement key '{key}' must be 1-100 ASCII letters, digits, '.', '_' or '-', not starting with '.'"
        ));
    }
    let base = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(base).join("win-taskbar-host").join(format!("{key}.placement")))
}

/// Reads a saved position. A missing file is `Ok(None)`.
pub(crate) fn load(path: &PathBuf) -> Result<Option<f64>, String> {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text).map(Some).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Reads the `position` line of a record. Records from older versions carry
/// more lines, which are ignored.
fn parse(text: &str) -> Result<f64, String> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(HEADER) {
        return Err(format!("placement record must start with '{HEADER}'"));
    }
    lines
        .find_map(|line| line.strip_prefix("position "))
        .and_then(|value| value.trim().parse().ok())
        .filter(|position| (0.0..=1.0).contains(position))
        .ok_or_else(|| "placement record needs a position between 0 and 1".into())
}

/// Writes the record to a temporary file of this process, flushes it and
/// renames it over the previous record, so a crash never leaves a partial file.
pub(crate) fn save(path: &Path, position: f64) -> Result<(), String> {
    let temp = path.with_extension(format!("placement.{}.tmp", std::process::id()));
    let result = (|| {
        fs::create_dir_all(path.parent().unwrap_or(path))?;
        let mut file = fs::File::create(&temp)?;
        file.write_all(format!("{HEADER}\nposition {position:.6}\n").as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result.map_err(|e| format!("save {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_current_and_older_records() {
        assert_eq!(parse(&format!("{HEADER}\nposition 0.4375\n")), Ok(0.4375));
        let older = format!("{HEADER}\nposition 0.500000\nedge bottom\nmonitor \\\\?\\DISPLAY#1\n");
        assert_eq!(parse(&older), Ok(0.5));
    }

    #[test]
    fn rejects_foreign_or_out_of_range_records() {
        let header = format!("{HEADER}\n");
        for record in
            ["position 0.5".into(), header.clone() + "position 1.5", header + "edge bottom"]
        {
            assert!(parse(&record).is_err(), "{record}");
        }
    }

    #[test]
    fn validates_keys() {
        assert!(store_path("example.counter").is_ok());
        for key in ["", "..", "../x", "a/b", "a b", ".hidden"] {
            assert!(store_path(key).is_err(), "{key}");
        }
    }

    #[test]
    fn save_replaces_existing_record() {
        let dir = std::env::temp_dir().join(format!("wth-test-{}", std::process::id()));
        let path = dir.join("k.placement");
        save(&path, 0.25).unwrap();
        save(&path, 0.75).unwrap();
        assert_eq!(load(&path), Ok(Some(0.75)));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(load(&path), Ok(None));
    }
}
