//! Typed reader for the repository version manifest, `versions/manifest.toml`.
//!
//! The manifest is the one place every pinned version, hash and URL lives; this crate is the body's
//! only reader of it. Deserialisation is strict: every key is required, so a missing key or a value
//! of the wrong type fails with an error that names the file and the key.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Manifest path, relative to the repository root.
const MANIFEST_RELATIVE: &str = "versions/manifest.toml";

/// Everything `versions/manifest.toml` pins.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub minecraft: Minecraft,
    pub azalea: Azalea,
    pub rust: Rust,
}

/// The pinned Minecraft version, its protocol number and the two jar hashes.
#[derive(Debug, Clone, Deserialize)]
pub struct Minecraft {
    /// Version id as Mojang names it, for example `26.3`.
    pub version: String,
    /// Protocol number the forwarder accepts and nothing else.
    pub protocol: u32,
    /// SHA-1 of the server jar.
    pub server_jar_sha1: String,
    /// Size of the server jar in bytes.
    pub server_jar_size: u64,
    /// SHA-1 of the client jar the launcher installs.
    pub client_jar_sha1: String,
    /// Mojang's version manifest index, the first hop of the server jar download.
    pub version_manifest_url: String,
}

/// The pinned Azalea source and the patches the spikes apply to it.
#[derive(Debug, Clone, Deserialize)]
pub struct Azalea {
    /// Git URL the spike clone comes from.
    pub repo: String,
    /// Commit the clone is checked out at.
    pub commit: String,
    /// Every patch matching this glob is applied in sorted order; relative to the repository root.
    pub patches_glob: String,
}

/// The pinned Rust toolchain and host.
#[derive(Debug, Clone, Deserialize)]
pub struct Rust {
    /// rustup channel, for example `nightly-2026-08-20`.
    pub toolchain: String,
    /// Host triple; Windows has no MSVC, hence the GNU target.
    pub windows_target: String,
    /// rustup-init download URL; `{triple}` and `{suffix}` are filled in by the caller.
    pub rustup_dist_url: String,
}

/// Why a manifest could not be loaded.
#[derive(Debug)]
pub enum ManifestError {
    /// The file could not be read.
    Unreadable { path: PathBuf, source: io::Error },
    /// The file is not TOML, a required key is missing, or a value has the wrong type.
    Invalid {
        path: PathBuf,
        source: toml::de::Error,
    },
    /// No `versions/manifest.toml` in the starting directory or any parent.
    RootNotFound { start: PathBuf },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            Self::Invalid { path, source } => write!(f, "invalid {}: {source}", path.display()),
            Self::RootNotFound { start } => write!(
                f,
                "no {MANIFEST_RELATIVE} in {} or any parent directory",
                start.display()
            ),
        }
    }
}

impl Error for ManifestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Unreadable { source, .. } => Some(source),
            Self::Invalid { source, .. } => Some(source),
            Self::RootNotFound { .. } => None,
        }
    }
}

impl Manifest {
    /// Read and validate the manifest at `path`.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|source| ManifestError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ManifestError::Invalid {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Load the manifest of the repository this build belongs to: walk up from the crate directory
    /// until a directory holds `versions/manifest.toml`.
    pub fn load_default() -> Result<Self, ManifestError> {
        let start = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut dir = Some(start);
        while let Some(current) = dir {
            let candidate = current.join(MANIFEST_RELATIVE);
            if candidate.is_file() {
                return Self::load(candidate);
            }
            dir = current.parent();
        }
        Err(ManifestError::RootNotFound {
            start: start.to_path_buf(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deliberately broken manifest to feed `Manifest::load`.
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name)
    }

    #[test]
    fn loads_the_repository_manifest() {
        let manifest = Manifest::load_default().expect("versions/manifest.toml loads");
        assert_eq!(manifest.minecraft.protocol, 777);
        assert_eq!(manifest.minecraft.version, "26.3");
    }

    #[test]
    fn rejects_a_missing_key() {
        let error = Manifest::load(fixture("missing-key.toml")).expect_err("a missing key must fail");
        let message = error.to_string();
        assert!(
            message.contains("protocol"),
            "the error should name the missing key, got: {message}"
        );
    }

    #[test]
    fn rejects_a_wrong_type() {
        let error = Manifest::load(fixture("wrong-type.toml")).expect_err("a wrong type must fail");
        let message = error.to_string();
        assert!(
            message.contains("protocol"),
            "the error should name the offending key, got: {message}"
        );
    }
}
