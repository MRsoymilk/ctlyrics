use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct AppPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    state_dir: PathBuf,
}

impl AppPaths {
    fn discover() -> io::Result<Self> {
        Ok(Self {
            config_dir: app_dir(dirs::config_dir(), "configuration")?,
            data_dir: app_dir(dirs::data_dir(), "data")?,
            state_dir: app_dir(dirs::state_dir(), "state")?,
        })
    }

    #[cfg(test)]
    fn new(config_dir: PathBuf, data_dir: PathBuf, state_dir: PathBuf) -> Self {
        Self {
            config_dir,
            data_dir,
            state_dir,
        }
    }

    pub fn mapping_file(&self) -> PathBuf {
        self.config_dir.join("mappings.json")
    }

    pub fn language_file(&self) -> PathBuf {
        self.config_dir.join("language")
    }

    pub fn bubble_font_size_file(&self) -> PathBuf {
        self.config_dir.join("bubble-font-size")
    }

    pub fn lyrics_dir(&self) -> PathBuf {
        self.data_dir.join("lyrics")
    }

    pub fn log_dir(&self) -> &Path {
        &self.state_dir
    }

    fn prepare(&self, legacy_root: &Path) -> io::Result<()> {
        fs::create_dir_all(&self.config_dir)?;
        fs::create_dir_all(self.lyrics_dir())?;
        fs::create_dir_all(&self.state_dir)?;
        copy_missing(&legacy_root.join("config"), &self.config_dir)?;
        copy_missing(&legacy_root.join("lyrics"), &self.lyrics_dir())?;
        Ok(())
    }
}

fn app_dir(base: Option<PathBuf>, kind: &str) -> io::Result<PathBuf> {
    base.map(|path| path.join("ctlyrics")).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("unable to determine the user {kind} directory"),
        )
    })
}

fn copy_missing(source: &Path, destination: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(source) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            fs::create_dir_all(&target)?;
            copy_missing(&entry.path(), &target)?;
        } else if file_type.is_file() && !target.exists() {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

static PATHS: OnceLock<AppPaths> = OnceLock::new();

pub fn get() -> &'static AppPaths {
    PATHS.get_or_init(|| AppPaths::discover().expect("unable to determine user directories"))
}

pub fn prepare_user_dirs() -> io::Result<()> {
    get().prepare(&std::env::current_dir()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_root() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ctlyrics-paths-{id}"))
    }

    #[test]
    fn prepares_xdg_directories_and_copies_legacy_data() {
        let root = temporary_root();
        let legacy = root.join("legacy");
        fs::create_dir_all(legacy.join("config")).unwrap();
        fs::create_dir_all(legacy.join("lyrics")).unwrap();
        fs::write(legacy.join("config/mappings.json"), "legacy").unwrap();
        fs::write(legacy.join("lyrics/song.lrc"), "lyrics").unwrap();
        let paths = AppPaths::new(
            root.join("config/ctlyrics"),
            root.join("data/ctlyrics"),
            root.join("state/ctlyrics"),
        );

        paths.prepare(&legacy).unwrap();

        assert_eq!(fs::read_to_string(paths.mapping_file()).unwrap(), "legacy");
        assert_eq!(
            fs::read_to_string(paths.lyrics_dir().join("song.lrc")).unwrap(),
            "lyrics"
        );
        assert!(paths.log_dir().is_dir());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_does_not_overwrite_existing_data() {
        let root = temporary_root();
        let legacy = root.join("legacy");
        let paths = AppPaths::new(
            root.join("config/ctlyrics"),
            root.join("data/ctlyrics"),
            root.join("state/ctlyrics"),
        );
        fs::create_dir_all(legacy.join("config")).unwrap();
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(legacy.join("config/mappings.json"), "legacy").unwrap();
        fs::write(paths.mapping_file(), "current").unwrap();

        paths.prepare(&legacy).unwrap();

        assert_eq!(fs::read_to_string(paths.mapping_file()).unwrap(), "current");
        fs::remove_dir_all(root).unwrap();
    }
}
