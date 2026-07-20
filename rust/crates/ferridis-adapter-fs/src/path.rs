//! Path-traversal protection.
//!
//! The discipline: every intent that touches the filesystem must first
//! resolve its user-supplied path through [`Root::resolve`], which:
//!
//! 1. Treats the input as relative — leading `/` is stripped to prevent
//!    absolute-path escape.
//! 2. Resolves `.` and `..` segments by accumulating, refusing any
//!    sequence that would pop above the root.
//! 3. Returns a [`RelPath`] (a private newtype) that the call site can
//!    join against the [`Root`] to obtain a final absolute path.
//!
//! There is no way to construct a [`RelPath`] that escapes its [`Root`]
//! — *illegal states unrepresentable* applied to the directory tree.

use std::path::{Component, Path, PathBuf};

use thiserror::Error;

/// Errors raised when resolving user-supplied paths.
#[derive(Debug, Error)]
pub enum PathError {
    /// The root was not absolute or did not exist as a directory.
    #[error("root must be an existing absolute directory: {0}")]
    BadRoot(String),

    /// The resolved path would escape the configured root.
    #[error("path `{0}` escapes the configured root")]
    Escapes(String),
}

/// A verified absolute root directory.
///
/// Construction via [`Root::new`] enforces that the path is absolute and
/// exists as a directory.
#[derive(Debug, Clone)]
pub struct Root {
    path: PathBuf,
}

impl Root {
    /// Build a root from an absolute, existing directory path.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, PathError> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(PathError::BadRoot(format!("not absolute: {path:?}")));
        }
        let canon = path
            .canonicalize()
            .map_err(|e| PathError::BadRoot(format!("{path:?}: {e}")))?;
        if !canon.is_dir() {
            return Err(PathError::BadRoot(format!("not a directory: {canon:?}")));
        }
        Ok(Self { path: canon })
    }

    /// The root as a [`Path`].
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// Resolve a user-supplied path under this root.
    ///
    /// The input is treated as relative regardless of leading separators.
    /// `..` components that would escape the root cause [`PathError::Escapes`].
    pub fn resolve(&self, user_path: &str) -> Result<RelPath, PathError> {
        let stripped = user_path.trim_start_matches('/');
        let path = Path::new(stripped);
        let mut accum: Vec<&std::ffi::OsStr> = Vec::new();
        for comp in path.components() {
            match comp {
                Component::CurDir => {}
                Component::Normal(seg) => accum.push(seg),
                Component::ParentDir => {
                    if accum.pop().is_none() {
                        return Err(PathError::Escapes(user_path.into()));
                    }
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(PathError::Escapes(user_path.into()));
                }
            }
        }
        let mut buf = PathBuf::new();
        for seg in &accum {
            buf.push(seg);
        }
        Ok(RelPath { inner: buf })
    }

    /// Join a [`RelPath`] with this [`Root`] to produce an absolute path.
    pub fn join(&self, rel: &RelPath) -> PathBuf {
        self.path.join(&rel.inner)
    }
}

/// A relative path that has been proven not to escape its [`Root`].
///
/// Constructible only via [`Root::resolve`]. Downstream code can join it
/// against the root with confidence.
#[derive(Debug, Clone)]
pub struct RelPath {
    inner: PathBuf,
}

impl RelPath {
    /// The relative path as a [`Path`].
    pub fn as_path(&self) -> &Path {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn root() -> (TempDir, Root) {
        let dir = TempDir::new().unwrap();
        let root = Root::new(dir.path()).unwrap();
        (dir, root)
    }

    #[test]
    fn rejects_non_absolute_root() {
        assert!(matches!(
            Root::new("relative/path"),
            Err(PathError::BadRoot(_))
        ));
    }

    #[test]
    fn resolves_simple_relative_path() {
        let (_d, r) = root();
        let p = r.resolve("a/b/c").unwrap();
        assert_eq!(p.as_path(), Path::new("a/b/c"));
    }

    #[test]
    fn strips_leading_slash() {
        let (_d, r) = root();
        let p = r.resolve("/etc/passwd").unwrap();
        assert_eq!(p.as_path(), Path::new("etc/passwd"));
    }

    #[test]
    fn collapses_curdir() {
        let (_d, r) = root();
        let p = r.resolve("./a/./b").unwrap();
        assert_eq!(p.as_path(), Path::new("a/b"));
    }

    #[test]
    fn rejects_escape_via_parent_dir() {
        let (_d, r) = root();
        assert!(matches!(r.resolve("../etc"), Err(PathError::Escapes(_))));
        assert!(matches!(
            r.resolve("a/../../etc"),
            Err(PathError::Escapes(_))
        ));
    }

    #[test]
    fn allows_parent_dir_within_bounds() {
        let (_d, r) = root();
        // a/b/../c == a/c, which is still under the root.
        let p = r.resolve("a/b/../c").unwrap();
        assert_eq!(p.as_path(), Path::new("a/c"));
    }
}
