//! Typestate-validated inputs.
//!
//! The discipline: every value that crosses the dispatch boundary
//! into the spawned `claude` subprocess must first pass through one
//! of the constructors below. Construction is the validation —
//! downstream code can rely on every [`Prompt`], [`AllowedCwd`], and
//! [`Model`] being safe to hand to a shell.
//!
//! - [`Prompt`] — bounded length, non-empty.
//! - [`AllowedCwd`] — only constructable against a configured
//!   allow-list of root directories. Refuses any path that escapes
//!   the listed roots. The adapter is therefore impossible to drive
//!   into operating in directories the operator hasn't whitelisted.
//! - [`Model`] — an enum, not a free string. Each variant maps to a
//!   `--model` argument shape Claude Code understands.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Errors raised by the typestate input constructors.
#[derive(Debug, Error)]
pub enum InputError {
    /// The prompt was empty or exceeded the byte limit.
    #[error("prompt invalid: {0}")]
    Prompt(String),
    /// The session id was not a valid lowercase UUID.
    #[error("session_id is not a valid UUID: {0}")]
    SessionId(String),
    /// The supplied cwd is not absolute, does not exist, or escapes
    /// the configured allow-list.
    #[error("cwd `{path}` is not permitted: {reason}")]
    Cwd {
        /// The path the caller supplied.
        path: String,
        /// Human-readable reason.
        reason: String,
    },
    /// The supplied model alias is not in the allow-list of accepted
    /// model aliases.
    #[error("model `{0}` is not permitted by this adapter's allow-list")]
    ModelNotAllowed(String),
}

/// A user-supplied prompt, validated.
///
/// Construction enforces non-empty + a generous byte cap. The cap is
/// not a security boundary (the model side has its own context
/// limits); it exists so a runaway client can't blow up the adapter
/// with a multi-MB string passed as a process argument.
#[derive(Debug, Clone)]
pub struct Prompt(String);

const MAX_PROMPT_BYTES: usize = 256 * 1024;

impl Prompt {
    /// Parse and validate.
    pub fn parse(s: impl Into<String>) -> Result<Self, InputError> {
        let s = s.into();
        if s.trim().is_empty() {
            return Err(InputError::Prompt("empty prompt".into()));
        }
        if s.len() > MAX_PROMPT_BYTES {
            return Err(InputError::Prompt(format!(
                "prompt is {} bytes, max is {MAX_PROMPT_BYTES}",
                s.len()
            )));
        }
        Ok(Self(s))
    }

    /// Borrow the prompt as a `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A lowercase hex UUID for `claude --session-id` / `claude -r`.
///
/// Claude Code requires session ids to be UUID-shaped; this newtype
/// catches "foo123" at the adapter boundary instead of after the
/// child process rejects it.
#[derive(Debug, Clone)]
pub struct SessionId(String);

impl SessionId {
    /// Parse and validate. Accepts the canonical `8-4-4-4-12` hex
    /// form, case-insensitive; normalises to lowercase.
    pub fn parse(s: impl Into<String>) -> Result<Self, InputError> {
        let s = s.into();
        if s.len() != 36 {
            return Err(InputError::SessionId(s));
        }
        let bytes = s.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            let want_dash = matches!(i, 8 | 13 | 18 | 23);
            let ok = if want_dash {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            };
            if !ok {
                return Err(InputError::SessionId(s));
            }
        }
        Ok(Self(s.to_ascii_lowercase()))
    }

    /// Borrow as `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A working directory that has been proven safe to `cd` into.
///
/// Construction requires:
/// 1. The path is absolute and canonicalises to an existing directory.
/// 2. The canonicalised path is contained in (or equal to) at least
///    one of the operator-configured roots in `allowed_roots`.
///
/// Adapters expose an [`AllowedRoots`] allow-list at startup; the
/// dispatch layer only ever calls [`AllowedCwd::parse`] against
/// that list. Bypassing it requires either patching the adapter or
/// constructing a value through the private field — neither is
/// possible from a client.
#[derive(Debug, Clone)]
pub struct AllowedCwd(PathBuf);

impl AllowedCwd {
    /// Validate a user-supplied cwd against `allowed_roots`.
    pub fn parse(
        user_path: &str,
        allowed_roots: &AllowedRoots,
    ) -> Result<Self, InputError> {
        let p = Path::new(user_path);
        if !p.is_absolute() {
            return Err(InputError::Cwd {
                path: user_path.into(),
                reason: "not an absolute path".into(),
            });
        }
        let canon = p.canonicalize().map_err(|e| InputError::Cwd {
            path: user_path.into(),
            reason: format!("canonicalize: {e}"),
        })?;
        if !canon.is_dir() {
            return Err(InputError::Cwd {
                path: user_path.into(),
                reason: "not a directory".into(),
            });
        }
        if !allowed_roots.contains(&canon) {
            return Err(InputError::Cwd {
                path: user_path.into(),
                reason: "outside the configured allowed-cwd roots".into(),
            });
        }
        Ok(Self(canon))
    }

    /// Borrow as a `Path`.
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// The set of root directories the adapter is permitted to launch
/// `claude` inside.
///
/// Empty by default: an empty allow-list rejects every cwd. The
/// operator opts in by listing roots at startup.
#[derive(Debug, Clone, Default)]
pub struct AllowedRoots {
    roots: Vec<PathBuf>,
}

impl AllowedRoots {
    /// Empty allow-list. Every [`AllowedCwd::parse`] against this
    /// will fail. Useful when the operator wants to forbid cwd
    /// overrides entirely and always use the adapter's own cwd.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Add a root. The root is canonicalised; non-existent or
    /// non-directory roots are rejected so misconfiguration is loud.
    pub fn with_root(mut self, path: impl AsRef<Path>) -> Result<Self, InputError> {
        let p = path.as_ref();
        if !p.is_absolute() {
            return Err(InputError::Cwd {
                path: p.display().to_string(),
                reason: "allowed root must be absolute".into(),
            });
        }
        let canon = p.canonicalize().map_err(|e| InputError::Cwd {
            path: p.display().to_string(),
            reason: format!("canonicalize: {e}"),
        })?;
        if !canon.is_dir() {
            return Err(InputError::Cwd {
                path: p.display().to_string(),
                reason: "allowed root is not a directory".into(),
            });
        }
        self.roots.push(canon);
        Ok(self)
    }

    /// Whether `canonical_path` is inside one of the configured roots
    /// (or equal to it).
    fn contains(&self, canonical_path: &Path) -> bool {
        self.roots.iter().any(|r| canonical_path.starts_with(r))
    }

    /// The roots, for diagnostics.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
}

/// The model alias to pass to `claude --model`.
///
/// Enum, not a free string, so the adapter never propagates an
/// arbitrary `--model` value from a client. The `Custom` variant
/// is gated behind an operator allow-list (see
/// [`ModelAllowList`]) so even custom names go through a check.
#[derive(Debug, Clone)]
pub enum Model {
    /// The current Opus alias.
    Opus,
    /// The current Sonnet alias.
    Sonnet,
    /// The current Haiku alias.
    Haiku,
    /// An operator-allow-listed custom alias (full model id, e.g.
    /// `claude-sonnet-4-6`).
    Custom(String),
}

impl Model {
    /// Render as the `--model <X>` argument shape.
    pub fn as_arg(&self) -> &str {
        match self {
            Model::Opus => "opus",
            Model::Sonnet => "sonnet",
            Model::Haiku => "haiku",
            Model::Custom(s) => s.as_str(),
        }
    }

    /// Parse a client-supplied alias against an operator allow-list.
    pub fn parse(alias: &str, allow: &ModelAllowList) -> Result<Self, InputError> {
        match alias {
            "opus" if allow.allows("opus") => Ok(Model::Opus),
            "sonnet" if allow.allows("sonnet") => Ok(Model::Sonnet),
            "haiku" if allow.allows("haiku") => Ok(Model::Haiku),
            other if allow.allows(other) => Ok(Model::Custom(other.to_string())),
            other => Err(InputError::ModelNotAllowed(other.to_string())),
        }
    }
}

/// Operator-configured allow-list of model aliases clients may select.
#[derive(Debug, Clone, Default)]
pub struct ModelAllowList {
    aliases: Vec<String>,
}

impl ModelAllowList {
    /// Empty allow-list. Every [`Model::parse`] against this will
    /// reject — use [`with_alias`](Self::with_alias) to opt in.
    pub fn empty() -> Self {
        Self::default()
    }

    /// A sensible default allow-list for general use: the three
    /// official aliases (`opus`, `sonnet`, `haiku`).
    pub fn standard() -> Self {
        Self {
            aliases: vec!["opus".into(), "sonnet".into(), "haiku".into()],
        }
    }

    /// Add an alias to the allow-list.
    #[must_use]
    pub fn with_alias(mut self, alias: impl Into<String>) -> Self {
        self.aliases.push(alias.into());
        self
    }

    /// Whether `alias` is in the list.
    fn allows(&self, alias: &str) -> bool {
        self.aliases.iter().any(|a| a == alias)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn prompt_rejects_empty_and_oversize() {
        assert!(Prompt::parse("").is_err());
        assert!(Prompt::parse("   \t\n  ").is_err());
        assert!(Prompt::parse("ok").is_ok());
        let huge = "x".repeat(MAX_PROMPT_BYTES + 1);
        assert!(Prompt::parse(huge).is_err());
    }

    #[test]
    fn session_id_accepts_lowercase_and_uppercase_uuids() {
        let lower = "d739e4e7-efe6-49d4-a597-1c3e71de2fd0";
        let upper = "D739E4E7-EFE6-49D4-A597-1C3E71DE2FD0";
        let lower_id = SessionId::parse(lower).unwrap();
        let upper_id = SessionId::parse(upper).unwrap();
        assert_eq!(lower_id.as_str(), lower);
        assert_eq!(upper_id.as_str(), lower);
    }

    #[test]
    fn session_id_rejects_garbage() {
        for bad in [
            "",
            "not-a-uuid",
            // Wrong dash positions:
            "d739e4e7efe6-49d4-a597-1c3e71de2fd0",
            // Non-hex character:
            "d739e4e7-efe6-49d4-a597-1c3e71de2fdZ",
            // Length off-by-one:
            "d739e4e7-efe6-49d4-a597-1c3e71de2fd00",
        ] {
            assert!(SessionId::parse(bad).is_err(), "must reject `{bad}`");
        }
    }

    #[test]
    fn allowed_cwd_accepts_path_inside_root_and_rejects_outside() {
        let root = TempDir::new().unwrap();
        let inside = root.path().join("sub");
        std::fs::create_dir(&inside).unwrap();
        let outside = TempDir::new().unwrap();

        let allow = AllowedRoots::empty()
            .with_root(root.path())
            .unwrap();

        // Inside is accepted.
        let cwd = AllowedCwd::parse(inside.to_str().unwrap(), &allow).unwrap();
        assert!(cwd.as_path().starts_with(root.path()));

        // Outside is rejected.
        let err = AllowedCwd::parse(outside.path().to_str().unwrap(), &allow).unwrap_err();
        assert!(matches!(err, InputError::Cwd { .. }));
    }

    #[test]
    fn allowed_cwd_rejects_relative_paths() {
        let allow = AllowedRoots::empty();
        assert!(AllowedCwd::parse("relative/path", &allow).is_err());
    }

    #[test]
    fn empty_allow_list_rejects_everything() {
        let allow = AllowedRoots::empty();
        let root = TempDir::new().unwrap();
        assert!(AllowedCwd::parse(root.path().to_str().unwrap(), &allow).is_err());
    }

    #[test]
    fn model_parse_honours_allow_list() {
        let allow = ModelAllowList::standard();
        assert!(matches!(Model::parse("opus", &allow), Ok(Model::Opus)));
        assert!(matches!(Model::parse("sonnet", &allow), Ok(Model::Sonnet)));
        assert!(matches!(Model::parse("haiku", &allow), Ok(Model::Haiku)));
        assert!(Model::parse("claude-opus-4-7", &allow).is_err());

        let extended = ModelAllowList::standard().with_alias("claude-opus-4-7");
        assert!(matches!(
            Model::parse("claude-opus-4-7", &extended),
            Ok(Model::Custom(s)) if s == "claude-opus-4-7"
        ));

        let empty = ModelAllowList::empty();
        assert!(Model::parse("opus", &empty).is_err());
    }
}
