use std::env;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub static PATH_TO_CARGO_MANIFEST_DIR: LazyLock<Result<PathBuf, env::VarError>> =
    LazyLock::new(|| env::var("CARGO_MANIFEST_DIR").map(|dir| Path::new(&dir).into()));

// TODO(Tsabary/ Arni): consolidate with other get_absolute_path functions.
/// Returns the absolute path from the project root.
pub fn get_absolute_path(relative_path: &str) -> PathBuf {
    let base_dir = PATH_TO_CARGO_MANIFEST_DIR.clone()
        // Attempt to get the `CARGO_MANIFEST_DIR` environment variable and convert it to `PathBuf`.
        // Ascend two directories ("../..") to get to the project root.
        .map(|dir| dir.join("../.."))
        // If `CARGO_MANIFEST_DIR` isn't set, fall back to the current working directory
        .unwrap_or_else(|_| env::current_dir().expect("Failed to get current directory"));
    base_dir.join(relative_path)
}
