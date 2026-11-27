use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
pub use winrust_common::journal::{Entry, parse_all};

pub fn state_dir() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir())
        .join("gp-pc-boost")
}

pub fn default_journal_path() -> PathBuf {
    state_dir().join("journal.log")
}

pub fn read_entries(path: &Path) -> Result<Vec<Entry>, String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(vec![]);
    };
    parse_all(&text).map_err(|line| format!("corrupt journal entry at line {line} in {}", path.display()))
}

/// Appends `entry` only if no existing entry targets the same registry value or service,
/// preserving the original pre-modification baseline across repeated applies.
pub fn record_if_new(path: &Path, entry: &Entry) -> Result<bool, String> {
    let line = entry
        .line()
        .ok_or_else(|| format!("entry cannot be faithfully serialized to journal: {entry:?}"))?;
    let existing = read_entries(path)?;
    if existing.iter().any(|e| e.same(entry)) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    writeln!(f, "{line}").map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(true)
}

pub fn clear_journal(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_file(path).map_err(|e| format!("remove {}: {e}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduplicates_and_round_trips_original_values() {
        let dir = env::temp_dir().join("gp-pc-boost-journal-test");
        let _ = fs::remove_dir_all(&dir);
        let p = dir.join("journal.log");

        let e1 = Entry::Reg {
            key: r"HKLM\SYSTEM\CurrentControlSet\Control\FileSystem".into(),
            name: "NtfsDisable8dot3NameCreation".into(),
            old: Some(("REG_DWORD".into(), "2".into())),
        };
        let e1_later = Entry::Reg {
            key: r"hklm\system\currentcontrolset\control\filesystem".into(),
            name: "ntfsdisable8dot3namecreation".into(),
            old: Some(("REG_DWORD".into(), "1".into())),
        };
        let e2 = Entry::Svc {
            name: "DiagTrack".into(),
            mode: "auto".into(),
        };

        assert!(record_if_new(&p, &e1).unwrap());
        assert!(!record_if_new(&p, &e1_later).unwrap(), "must keep original baseline");
        assert!(record_if_new(&p, &e2).unwrap());

        let loaded = read_entries(&p).unwrap();
        assert_eq!(loaded, vec![e1, e2]);

        clear_journal(&p).unwrap();
        assert!(read_entries(&p).unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
