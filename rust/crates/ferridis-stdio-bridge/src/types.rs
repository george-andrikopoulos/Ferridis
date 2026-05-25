use std::fmt;
use std::process::Stdio;

use thiserror::Error;

// ── Command ───────────────────────────────────────────────────────────────────

/// Validated command string for the stdio MCP process to spawn.
///
/// Non-empty; may be a bare binary name (resolved via `PATH`) or an
/// absolute path.
#[derive(Debug, Clone)]
pub struct Command(String);

/// Errors from [`Command::new`].
#[derive(Debug, Error)]
pub enum CommandError {
    /// The command string was empty.
    #[error("command must not be empty")]
    Empty,
}

impl Command {
    /// Parse and validate a command string.
    pub fn new(s: impl Into<String>) -> Result<Self, CommandError> {
        let s = s.into();
        if s.is_empty() {
            return Err(CommandError::Empty);
        }
        Ok(Self(s))
    }

    /// The command as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ── SessionId ─────────────────────────────────────────────────────────────────

/// Opaque identifier for one SSE session and its associated child process.
///
/// Generated as UUID v4 at session creation; surfaced as the `sessionId`
/// query parameter in `POST /messages?sessionId=<id>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(uuid::Uuid);

impl SessionId {
    /// Generate a fresh random session identifier.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    /// Construct from a raw [`uuid::Uuid`].
    pub fn from_raw(u: uuid::Uuid) -> Self {
        Self(u)
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── SpawnConfig ───────────────────────────────────────────────────────────────

/// Everything needed to spawn one stdio MCP child process.
///
/// Cloned for each new SSE session. Owns the spawning logic via
/// [`SpawnConfig::spawn`] so callers never touch [`std::process::Command`]
/// directly.
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    command: Command,
    args: Vec<String>,
    env: Vec<(String, String)>,
}

impl SpawnConfig {
    /// Construct with a validated command and no args or extra env.
    pub fn new(command: Command) -> Self {
        Self { command, args: Vec::new(), env: Vec::new() }
    }

    /// Append a command-line argument.
    pub fn with_arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Inject an environment variable into the child's environment.
    pub fn with_env(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.env.push((key.into(), val.into()));
        self
    }

    /// Spawn the configured child process.
    ///
    /// Captures stdin, stdout, and stderr. The caller consumes the result
    /// via [`SpawnedChild::into_parts`].
    pub fn spawn(&self) -> Result<SpawnedChild, BridgeError> {
        let mut cmd = tokio::process::Command::new(self.command.as_str());
        cmd.args(&self.args);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| BridgeError::Spawn {
            command: self.command.0.clone(), // clone: String into owned error — not Copy
            source: e,
        })?;

        let stdin = child.stdin.take().ok_or(BridgeError::MissingStream { stream: "stdin" })?;
        let stdout =
            child.stdout.take().ok_or(BridgeError::MissingStream { stream: "stdout" })?;
        let stderr = child.stderr.take();

        Ok(SpawnedChild { child, stdin, stdout, stderr })
    }
}

// ── SpawnedChild ──────────────────────────────────────────────────────────────

/// A freshly-spawned child process with captured I/O streams.
///
/// Consumed once via [`SpawnedChild::into_parts`].
pub struct SpawnedChild {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
}

impl SpawnedChild {
    /// Destructure into the raw process handle and I/O streams.
    pub fn into_parts(
        self,
    ) -> (
        tokio::process::Child,
        tokio::process::ChildStdin,
        tokio::process::ChildStdout,
        Option<tokio::process::ChildStderr>,
    ) {
        (self.child, self.stdin, self.stdout, self.stderr)
    }
}

// ── BridgeError ───────────────────────────────────────────────────────────────

/// Errors from bridge operations.
#[derive(Debug, Error)]
pub enum BridgeError {
    /// The stdio child process failed to spawn.
    #[error("failed to spawn `{command}`: {source}")]
    Spawn {
        /// The command that was attempted.
        command: String,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// The child process had no stdin/stdout (internal invariant violation).
    #[error("child process missing {stream}")]
    MissingStream {
        /// Which stream was missing.
        stream: &'static str,
    },
}
