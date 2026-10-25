use std::env;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use llmroller_store::crypto::MasterKey;

pub(crate) const DATA_DIR_ENV: &str = "LLMROLLER_DATA_DIR";
pub(crate) const DEFAULT_DATA_DIR: &str = "/var/lib/llmroller";

pub(crate) fn resolve_data_dir(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| env::var_os(DATA_DIR_ENV).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR))
}

pub(crate) fn acquire_lock(data_dir: &Path) -> Result<File, CliFailure> {
    let path = data_dir.join("llmroller.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| CliFailure::new(13, format!("cannot open {}: {error}", path.display())))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(CliFailure::new(
            4,
            format!(
                "{} is held by another process; stop serve before rotating",
                path.display()
            ),
        )),
        Err(TryLockError::Error(error)) => Err(CliFailure::new(
            13,
            format!("cannot lock {}: {error}", path.display()),
        )),
    }
}

pub(crate) fn read_key_from_env(variable: &str, role: &str) -> Result<MasterKey, CliFailure> {
    let encoded = match env::var(variable) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => {
            return Err(CliFailure::new(
                3,
                format!("{role} master key environment variable {variable:?} is not set"),
            ));
        }
        Err(env::VarError::NotUnicode(_)) => {
            return Err(CliFailure::new(
                3,
                format!("{role} master key environment variable {variable:?} is not UTF-8"),
            ));
        }
    };
    MasterKey::from_base64(&encoded).map_err(|error| CliFailure::new(3, error.to_string()))
}

#[derive(Debug)]
pub(crate) struct CliFailure {
    pub(crate) code: u8,
    pub(crate) message: String,
}

impl CliFailure {
    pub(crate) fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
