//! GitHub reference adapter for Ferridis.
//!
//! The canonical developer-tool demonstration adapter. Holds a GitHub personal
//! access token on the adapter side (local trust — the Ferridis connection uses
//! `auth: none`) and exposes six intents:
//!
//! - `list-repos`         — list repositories for a user or organisation
//! - `get-file`           — read a file from a repository (base64-decoded)
//! - `list-issues`        — list issues for a repository
//! - `create-issue`       — open a new issue
//! - `list-pull-requests` — list pull requests for a repository
//! - `search-code`        — search code across GitHub
//!
//! # Auth model
//!
//! The GitHub PAT lives on the adapter's side of the boundary. The Ferridis
//! client that calls this adapter needs no token — it speaks to the adapter
//! over unauthenticated HTTP (local trust). The adapter then forwards requests
//! to GitHub using its internally-held token. This mirrors the filesystem
//! adapter's approach: the Ferridis layer does not need to see the underlying
//! service credentials.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod capability;
pub mod client;
pub mod types;

pub use capability::GitHubCapability;
pub use types::{GitHubError, GitHubToken, RepoName, RepoOwner};
