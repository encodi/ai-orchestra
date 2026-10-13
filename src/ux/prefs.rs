//! Saved user preferences: the starting point of every new interactive session.
//!
//! One small JSON file next to the memory database, owner-only. It holds the settings the user chose
//! to save with `/save` and nothing else: no history, no feedback, no learned values. Feedback never
//! turns into preferences by itself.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{OrchestraError, Result};

use super::settings::Settings;

/// Where preferences live: beside the memory database (`preferences.json`).
pub fn path_for(config: &Config) -> Option<PathBuf> {
    let memory = config.resolved_memory_path()?;
    Some(memory.parent()?.join("preferences.json"))
}

pub fn load(path: &Path) -> Result<Settings> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            OrchestraError::Config(format!(
                "the preferences file {} is damaged ({e}); fix it or remove it",
                path.display()
            ))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(e.into()),
    }
}

pub fn save(path: &Path, settings: &Settings) -> Result<()> {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    let text = serde_json::to_string_pretty(settings)
        .map_err(|e| OrchestraError::Config(e.to_string()))?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn clear(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ux::settings::Key;

    #[test]
    fn preferences_round_trip_privately_and_a_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("preferences.json");
        assert!(load(&path).unwrap().is_empty());
        let mut s = Settings::default();
        s.set(Key::Strategy, "balanced").unwrap();
        s.set(Key::Web, "off").unwrap();
        save(&path, &s).unwrap();
        assert_eq!(load(&path).unwrap(), s);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        clear(&path).unwrap();
        assert!(load(&path).unwrap().is_empty());
        std::fs::write(&path, "not json").unwrap();
        assert!(load(&path).unwrap_err().to_string().contains("damaged"));
    }
}
