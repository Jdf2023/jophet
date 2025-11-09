// src/config.rs
//! Handles loading and parsing of the `Jophet.toml` project configuration file.
//!
//! This module defines the data structures that mirror the structure of the TOML
//! configuration file, using `serde` for deserialization. It also provides the
//! crucial `find_project_root` function, which allows the compiler to be run
//! from any subdirectory within a project.

use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

/// Represents the `[package]` table in `Jophet.toml`.
#[derive(Deserialize, Debug)]
pub struct PackageConfig {
    /// The name of the package.
    pub name: String,
    /// The version of the package.
    pub version: String,
    /// The type of the package, either "bin" (executable) or "lib" (library).
    pub r#type: String,
}

/// Represents a single profile's settings (e.g., `[profile.dev]`).
#[derive(Deserialize, Debug)]
pub struct Profile {
    /// The optimization level for the C compiler (0-3).
    #[serde(rename = "opt-level")]
    pub opt_level: Option<u32>,
    /// Whether to include debug information in the final binary.
    pub debug: Option<bool>,
    /// The compiler backend to use for this profile.
    pub backend: Option<String>,
    /// Whether to perform a static build for this profile.
    #[serde(rename = "static")]
    pub static_build: Option<bool>,
    /// Whether to keep intermediate files for this profile.
    #[serde(rename = "keep-intermediate")]
    pub keep_intermediate: Option<bool>,
    /// The default target triple for this profile. Can be overridden with the --target flag.
    pub target: Option<String>,
}

/// Represents the `[profile]` table containing both `dev` and `release` settings.
#[derive(Deserialize, Debug)]
pub struct Profiles {
    #[serde(default)]
    pub dev: Profile,
    #[serde(default)]
    pub release: Profile,
}

// Default implementations for Profile and Profiles to handle cases
// where the user doesn't specify these sections in their TOML.
impl Default for Profile {
    fn default() -> Self {
        Self {
            opt_level: Some(0),
            debug: Some(true),
            backend: None,
            static_build: None,
            keep_intermediate: None,
            target: None,
        }
    }
}

impl Default for Profiles {
    fn default() -> Self {
        Self {
            dev: Profile {
                opt_level: Some(0),
                debug: Some(true),
                backend: None,
                static_build: None,
                keep_intermediate: None,
                target: None,
            },
            release: Profile {
                opt_level: Some(3),
                debug: Some(false),
                backend: None,
                static_build: None,
                keep_intermediate: None,
                target: None,
            },
        }
    }
}

/// Represents the root of the `Jophet.toml` configuration file.
#[derive(Deserialize, Debug)]
pub struct ProjectConfig {
    /// The package configuration section.
    pub package: PackageConfig,
    /// The dependencies section. `#[serde(default)]` makes this section optional.
    #[serde(default)]
    pub dependencies: HashMap<String, String>,
    /// The build profiles section. `#[serde(default)]` makes this section optional.
    #[serde(default)]
    pub profile: Profiles,
}

/// Searches for the project root by looking for a `Jophet.toml` file.
///
/// It starts from the current working directory and traverses upwards through
/// parent directories until it finds a `Jophet.toml` file. This allows CLI
/// commands to be run from anywhere inside a project's directory tree.
///
/// # Returns
/// `Some(PathBuf)` containing the path to the project root if found, otherwise `None`.
pub fn find_project_root() -> Option<PathBuf> {
    let mut current_dir = env::current_dir().ok()?;
    loop {
        if current_dir.join("Jophet.toml").is_file() {
            return Some(current_dir);
        }
        // Move to the parent directory. If there is no parent, stop.
        if !current_dir.pop() {
            return None;
        }
    }
}

/// Finds, reads, and parses the `Jophet.toml` file for the current project.
///
/// This function combines `find_project_root` with file reading and TOML parsing
/// to provide the project's configuration and root path in a single call.
///
/// # Returns
/// A `Result` containing a tuple of `(ProjectConfig, PathBuf)` on success,
/// or a boxed error if the root cannot be found or the file is invalid.
pub fn load_config() -> Result<(ProjectConfig, PathBuf), Box<dyn Error>> {
    let root_path = find_project_root().ok_or_else(|| {
        "Could not find Jophet.toml in the current directory or any parent. Are you in a Jophet project?".to_string()
    })?;

    let config_path = root_path.join("Jophet.toml");
    let content = fs::read_to_string(config_path)?;
    let config: ProjectConfig = toml::from_str(&content)?;

    Ok((config, root_path))
}

/// Reads and parses the `Jophet.toml` file from a specific project path.
///
/// This version is used for loading the configuration of local dependencies.
///
/// # Returns
/// A `Result` containing the `ProjectConfig` on success, or a boxed error.
pub fn load_config_for_path(project_path: &Path) -> Result<ProjectConfig, Box<dyn Error>> {
    let config_path = project_path.join("Jophet.toml");
    if !config_path.exists() {
        return Err(format!(
            "Could not find Jophet.toml in dependency path '{}'",
            project_path.display()
        )
        .into());
    }
    let content = fs::read_to_string(config_path)?;
    let config: ProjectConfig = toml::from_str(&content)?;
    Ok(config)
}