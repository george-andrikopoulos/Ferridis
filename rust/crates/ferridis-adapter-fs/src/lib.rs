//! Filesystem reference adapter for Ferridis.
//!
//! The canonical end-to-end demonstration adapter, modeled on MCP's
//! `filesystem-server`. Mounts a single capability that exposes a
//! sandboxed directory tree to a Ferridis client via these intents:
//!
//! - `read-file`
//! - `write-file`
//! - `list-dir`
//! - `search-files`
//! - `move-file`
//!
//! # Path-traversal protection
//!
//! [`Root`] is a verified absolute root. [`RelPath`] holds a relative
//! path that has been resolved against a [`Root`] and proven not to
//! escape it. Every intent in [`FilesystemCapability`] funnels through
//! [`Root::resolve`], so there is no path in the adapter that touches
//! the filesystem without first becoming a [`RelPath`]. *Illegal states
//! unrepresentable* applied to the directory tree.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod capability;
pub mod path;

pub use capability::FilesystemCapability;
pub use path::{PathError, RelPath, Root};
