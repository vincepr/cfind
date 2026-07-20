use std::{
    collections::HashSet,
    env,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};

pub const ROOT_ENV: &str = "CFIND_ROOT";
pub const LANGUAGES_ENV: &str = "CFIND_LANGUAGES";
pub const INDEX_ENV: &str = "CFIND_INDEX";
pub const STALE_AFTER_HOURS_ENV: &str = "CFIND_STALE_AFTER_HOURS";
const DEFAULT_STALE_AFTER_HOURS: u64 = 6;
const AUTOMATIC_REBUILD_MULTIPLIER: u32 = 3;
const FETCH_STALE_MULTIPLIER: u32 = 12;
#[cfg(not(target_os = "windows"))]
const ROOT_REQUIRED_MESSAGE: &str = "CFIND_ROOT is required; set it to the directory containing your repositories, for example: export CFIND_ROOT=\"$HOME/code\"";
#[cfg(target_os = "windows")]
const ROOT_REQUIRED_MESSAGE: &str = "CFIND_ROOT is required; set it to the directory containing your repositories, for example: $env:CFIND_ROOT=\"C:\\path\\to\\code\"";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub const DATA_HOME_ENV: &str = "XDG_DATA_HOME";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SupportedLanguage {
    Rust,
    JavaScript,
    TypeScript,
    CSharp,
}

impl SupportedLanguage {
    pub const ALL: [Self; 4] = [Self::Rust, Self::JavaScript, Self::TypeScript, Self::CSharp];

    pub fn parse(value: &str) -> Option<Self> {
        crate::language::language_from_alias(value)
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        crate::language::language_from_path(path)
    }

    pub(crate) fn from_extension(extension: &str) -> Option<Self> {
        crate::language::language_from_extension(extension)
    }

    pub fn as_str(self) -> &'static str {
        crate::language::language_name(self)
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub index_path: PathBuf,
    pub languages: HashSet<SupportedLanguage>,
    pub stale_after: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let root = env::var_os(ROOT_ENV)
            .filter(|root| !root.is_empty())
            .map(PathBuf::from)
            .context(ROOT_REQUIRED_MESSAGE)?;
        let root = root
            .canonicalize()
            .with_context(|| format!("search root does not exist: {}", root.display()))?;
        if !root.is_dir() {
            bail!("search root is not a directory: {}", root.display());
        }

        let language_value = env::var(LANGUAGES_ENV)
            .unwrap_or_else(|_| "rust,javascript,typescript,csharp".to_owned());
        let mut languages = HashSet::new();
        for value in language_value.split(',') {
            let Some(language) = SupportedLanguage::parse(value) else {
                bail!("unsupported language in {LANGUAGES_ENV}: {value}");
            };
            languages.insert(language);
        }
        if languages.is_empty() {
            bail!("{LANGUAGES_ENV} must contain at least one language");
        }

        let index_path = match env::var_os(INDEX_ENV) {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            Some(_) => bail!("{INDEX_ENV} must not be empty"),
            None => default_index_path(&root)?,
        };
        let stale_after_hours = env::var(STALE_AFTER_HOURS_ENV)
            .map(|value| {
                value.parse::<u64>().with_context(|| {
                    format!("{STALE_AFTER_HOURS_ENV} must be a non-negative number of hours")
                })
            })
            .unwrap_or(Ok(DEFAULT_STALE_AFTER_HOURS))?;
        let stale_after_seconds = stale_after_hours.checked_mul(60 * 60).with_context(|| {
            format!("{STALE_AFTER_HOURS_ENV} is too large to represent as a duration")
        })?;

        Ok(Self {
            root,
            index_path,
            languages,
            stale_after: Duration::from_secs(stale_after_seconds),
        })
    }

    /// Returns the index age that triggers an automatic rebuild.
    pub fn automatic_rebuild_after(&self) -> Duration {
        self.stale_after
            .saturating_mul(AUTOMATIC_REBUILD_MULTIPLIER)
    }

    /// Returns the cached Git fetch age that produces a stale-state annotation.
    pub fn fetch_stale_after(&self) -> Duration {
        self.stale_after.saturating_mul(FETCH_STALE_MULTIPLIER)
    }
}

fn default_index_path(root: &Path) -> Result<PathBuf> {
    Ok(platform_data_home()?
        .join("cfind/indexes")
        .join(format!("{:032x}.sqlite", namespace_hash(root))))
}

#[cfg(target_os = "windows")]
fn platform_data_home() -> Result<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .context("LOCALAPPDATA is not set; set CFIND_INDEX to choose an index location")
}

#[cfg(target_os = "macos")]
fn platform_data_home() -> Result<PathBuf> {
    let home = env::var_os("HOME")
        .context("HOME is not set; set CFIND_INDEX to choose an index location")?;
    Ok(PathBuf::from(home).join("Library/Application Support"))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn platform_data_home() -> Result<PathBuf> {
    match env::var_os(DATA_HOME_ENV) {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ => {
            let home = env::var_os("HOME")
                .context("HOME is not set; set CFIND_INDEX to choose an index location")?;
            Ok(PathBuf::from(home).join(".local/share"))
        }
    }
}

// A stable hash keeps indexes for separate roots independent without putting the
// absolute workspace path (which may contain separators) in a file name.
fn namespace_hash(root: &Path) -> u128 {
    root.as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0x6c62272e07bb014262b821756295c58d, |hash, byte| {
            (hash ^ u128::from(*byte)).wrapping_mul(0x0000000001000000000000000000013b)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_supported_extensions() {
        assert_eq!(
            SupportedLanguage::from_path(Path::new("src/lib.rs")),
            Some(SupportedLanguage::Rust)
        );
        assert_eq!(
            SupportedLanguage::from_path(Path::new("web/view.tsx")),
            Some(SupportedLanguage::TypeScript)
        );
        assert_eq!(SupportedLanguage::from_path(Path::new("README.md")), None);
    }

    #[test]
    fn index_namespaces_are_stable_and_independent() {
        let first = default_index_path(Path::new("/work/first")).unwrap();
        let first_again = default_index_path(Path::new("/work/first")).unwrap();
        let second = default_index_path(Path::new("/work/second")).unwrap();

        assert_eq!(first, first_again);
        assert_ne!(first, second);
        assert_eq!(first.parent().unwrap(), second.parent().unwrap());
        assert_eq!(first.extension().unwrap(), "sqlite");
    }
}
