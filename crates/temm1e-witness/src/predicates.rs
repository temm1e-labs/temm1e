//! Tier 0 predicate checkers.
//!
//! Every primitive in this file is deterministic and LLM-free. They inspect
//! real world state (file system, processes, network, git) and return
//! `PredicateCheckResult { outcome, detail, latency_ms }`.
//!
//! See `tems_lab/witness/RESEARCH_PAPER.md` §6.2 and
//! `tems_lab/witness/IMPLEMENTATION_DETAILS.md` §6 for the full catalog.

use crate::error::WitnessError;
use crate::types::{GitScope, OutputStream, Predicate, VerdictOutcome};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::process::Command as TokioCommand;

/// Context passed to every predicate check.
#[derive(Debug, Clone)]
pub struct CheckContext {
    pub workspace_root: PathBuf,
    pub env: BTreeMap<String, String>,
    pub started_at: DateTime<Utc>,
    /// Arbitrary markers set during execution; used by `ElapsedUnder`.
    pub markers: BTreeMap<String, DateTime<Utc>>,
}

impl CheckContext {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            env: BTreeMap::new(),
            started_at: Utc::now(),
            markers: BTreeMap::new(),
        }
    }

    pub fn with_marker(mut self, name: impl Into<String>, at: DateTime<Utc>) -> Self {
        self.markers.insert(name.into(), at);
        self
    }
}

/// Result of checking a single predicate.
#[derive(Debug, Clone)]
pub struct PredicateCheckResult {
    pub outcome: VerdictOutcome,
    pub detail: String,
    pub latency_ms: u64,
}

impl PredicateCheckResult {
    pub fn pass(detail: impl Into<String>, latency_ms: u64) -> Self {
        Self {
            outcome: VerdictOutcome::Pass,
            detail: detail.into(),
            latency_ms,
        }
    }

    pub fn fail(detail: impl Into<String>, latency_ms: u64) -> Self {
        Self {
            outcome: VerdictOutcome::Fail,
            detail: detail.into(),
            latency_ms,
        }
    }

    pub fn inconclusive(detail: impl Into<String>, latency_ms: u64) -> Self {
        Self {
            outcome: VerdictOutcome::Inconclusive,
            detail: detail.into(),
            latency_ms,
        }
    }
}

// ===========================================================================
// Main dispatch
// ===========================================================================

/// Check a Tier 0 predicate. Returns `WitnessError::InvalidPredicate` for
/// Tier 1/2 predicates (those must be dispatched through the Witness runtime).
pub async fn check_tier0(
    predicate: &Predicate,
    ctx: &CheckContext,
) -> Result<PredicateCheckResult, WitnessError> {
    let start = Instant::now();
    let result = match predicate {
        // File system
        Predicate::FileExists { path } => check_file_exists(&resolve(path, ctx)).await,
        Predicate::FileAbsent { path } => check_file_absent(&resolve(path, ctx)).await,
        Predicate::DirectoryExists { path } => check_directory_exists(&resolve(path, ctx)).await,
        Predicate::FileContains { path, regex } => {
            check_file_contains(&resolve(path, ctx), regex).await
        }
        Predicate::FileDoesNotContain { path, regex } => {
            check_file_does_not_contain(&resolve(path, ctx), regex).await
        }
        Predicate::FileHashEquals { path, sha256_hex } => {
            check_file_hash_equals(&resolve(path, ctx), sha256_hex).await
        }
        Predicate::FileSizeInRange {
            path,
            min_bytes,
            max_bytes,
        } => check_file_size_in_range(&resolve(path, ctx), *min_bytes, *max_bytes).await,
        Predicate::FileModifiedWithin {
            path,
            duration_secs,
        } => check_file_modified_within(&resolve(path, ctx), *duration_secs).await,

        // Command
        Predicate::CommandExits {
            cmd,
            args,
            expected_code,
            cwd,
            timeout_ms,
        } => {
            check_command_exits(
                cmd,
                args,
                *expected_code,
                cwd.as_deref().map(|p| resolve(p, ctx)).as_deref(),
                *timeout_ms,
            )
            .await
        }
        Predicate::CommandOutputContains {
            cmd,
            args,
            regex,
            stream,
            cwd,
            timeout_ms,
        } => {
            check_command_output_contains(
                cmd,
                args,
                regex,
                *stream,
                cwd.as_deref().map(|p| resolve(p, ctx)).as_deref(),
                *timeout_ms,
            )
            .await
        }
        Predicate::CommandOutputAbsent {
            cmd,
            args,
            regex,
            stream,
            cwd,
            timeout_ms,
        } => {
            check_command_output_absent(
                cmd,
                args,
                regex,
                *stream,
                cwd.as_deref().map(|p| resolve(p, ctx)).as_deref(),
                *timeout_ms,
            )
            .await
        }
        Predicate::CommandDurationUnder {
            cmd,
            args,
            max_ms,
            cwd,
        } => {
            check_command_duration_under(
                cmd,
                args,
                *max_ms,
                cwd.as_deref().map(|p| resolve(p, ctx)).as_deref(),
            )
            .await
        }

        // Process
        Predicate::ProcessAlive { name_or_pid } => check_process_alive(name_or_pid).await,
        Predicate::PortListening { port, interface } => {
            check_port_listening(*port, interface.as_deref()).await
        }

        // Network
        Predicate::HttpStatus {
            url,
            method,
            expected_status,
        } => check_http_status(url, method, *expected_status).await,
        Predicate::HttpBodyContains { url, method, regex } => {
            check_http_body_contains(url, method, regex).await
        }

        // VCS
        Predicate::GitFileInDiff {
            path,
            include_staged,
            include_unstaged,
        } => {
            check_git_file_in_diff(
                &resolve(path, ctx),
                *include_staged,
                *include_unstaged,
                &ctx.workspace_root,
            )
            .await
        }
        Predicate::GitDiffLineCountAtMost { max, scope } => {
            check_git_diff_line_count_at_most(*max, *scope, &ctx.workspace_root).await
        }
        Predicate::GitNewFilesMatch { glob } => {
            check_git_new_files_match(glob, &ctx.workspace_root).await
        }
        Predicate::GitCommitMessageMatches {
            regex,
            commits_back,
        } => check_git_commit_message_matches(regex, *commits_back, &ctx.workspace_root).await,

        // Text search
        Predicate::GrepPresent { pattern, path_glob } => {
            check_grep_present(pattern, path_glob, &ctx.workspace_root).await
        }
        Predicate::GrepAbsent { pattern, path_glob } => {
            check_grep_absent(pattern, path_glob, &ctx.workspace_root).await
        }
        Predicate::GrepCountAtLeast {
            pattern,
            path_glob,
            n,
        } => check_grep_count_at_least(pattern, path_glob, *n, &ctx.workspace_root).await,

        // Time
        Predicate::ElapsedUnder {
            start_marker,
            max_secs,
        } => check_elapsed_under(start_marker, *max_secs, ctx).await,

        // Composites
        Predicate::AllOf { predicates } => check_all_of(predicates, ctx).await,
        Predicate::AnyOf { predicates } => check_any_of(predicates, ctx).await,
        Predicate::NotOf { predicate } => check_not_of(predicate, ctx).await,

        // Tier 1/2 — not handled here
        Predicate::AspectVerifier { .. } | Predicate::AdversarialJudge { .. } => {
            return Err(WitnessError::InvalidPredicate(
                "Tier 1/2 predicate dispatched to Tier 0 checker".to_string(),
            ));
        }
    };

    let latency = start.elapsed().as_millis() as u64;
    result.map(|mut r| {
        r.latency_ms = latency;
        r
    })
}

fn resolve(p: &Path, ctx: &CheckContext) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        ctx.workspace_root.join(p)
    }
}

// ===========================================================================
// File system (8 checkers)
// ===========================================================================

async fn check_file_exists(path: &Path) -> Result<PredicateCheckResult, WitnessError> {
    match tokio::fs::metadata(path).await {
        Ok(m) if m.is_file() => Ok(PredicateCheckResult::pass(
            format!("file exists: {}", path.display()),
            0,
        )),
        Ok(_) => Ok(PredicateCheckResult::fail(
            format!("path is not a regular file: {}", path.display()),
            0,
        )),
        Err(_) => Ok(PredicateCheckResult::fail(
            format!("file does not exist: {}", path.display()),
            0,
        )),
    }
}

async fn check_file_absent(path: &Path) -> Result<PredicateCheckResult, WitnessError> {
    match tokio::fs::metadata(path).await {
        Ok(_) => Ok(PredicateCheckResult::fail(
            format!("file still exists: {}", path.display()),
            0,
        )),
        Err(_) => Ok(PredicateCheckResult::pass(
            format!("file absent: {}", path.display()),
            0,
        )),
    }
}

async fn check_directory_exists(path: &Path) -> Result<PredicateCheckResult, WitnessError> {
    match tokio::fs::metadata(path).await {
        Ok(m) if m.is_dir() => Ok(PredicateCheckResult::pass(
            format!("directory exists: {}", path.display()),
            0,
        )),
        Ok(_) => Ok(PredicateCheckResult::fail(
            format!("path is not a directory: {}", path.display()),
            0,
        )),
        Err(_) => Ok(PredicateCheckResult::fail(
            format!("directory does not exist: {}", path.display()),
            0,
        )),
    }
}

const MAX_FILE_READ_BYTES: u64 = 10 * 1024 * 1024; // 10 MB

async fn check_file_contains(
    path: &Path,
    regex_str: &str,
) -> Result<PredicateCheckResult, WitnessError> {
    let meta = match tokio::fs::metadata(path).await {
        Ok(m) => m,
        Err(e) => {
            return Ok(PredicateCheckResult::fail(
                format!("cannot stat {}: {}", path.display(), e),
                0,
            ));
        }
    };
    if meta.len() > MAX_FILE_READ_BYTES {
        return Ok(PredicateCheckResult::inconclusive(
            format!(
                "file too large to scan ({} bytes > {} limit): {}",
                meta.len(),
                MAX_FILE_READ_BYTES,
                path.display()
            ),
            0,
        ));
    }
    let contents = tokio::fs::read_to_string(path).await?;
    let re = regex::Regex::new(regex_str)?;
    if re.is_match(&contents) {
        Ok(PredicateCheckResult::pass(
            format!("pattern `{}` found in {}", regex_str, path.display()),
            0,
        ))
    } else {
        Ok(PredicateCheckResult::fail(
            format!("pattern `{}` NOT found in {}", regex_str, path.display()),
            0,
        ))
    }
}

async fn check_file_does_not_contain(
    path: &Path,
    regex_str: &str,
) -> Result<PredicateCheckResult, WitnessError> {
    let res = check_file_contains(path, regex_str).await?;
    Ok(PredicateCheckResult {
        outcome: match res.outcome {
            VerdictOutcome::Pass => VerdictOutcome::Fail,
            VerdictOutcome::Fail => VerdictOutcome::Pass,
            VerdictOutcome::Inconclusive => VerdictOutcome::Inconclusive,
        },
        detail: match res.outcome {
            VerdictOutcome::Pass => format!(
                "anti-pattern `{}` FOUND in {} (predicate fails)",
                regex_str,
                path.display()
            ),
            VerdictOutcome::Fail => format!(
                "anti-pattern `{}` not found in {} (predicate passes)",
                regex_str,
                path.display()
            ),
            VerdictOutcome::Inconclusive => res.detail,
        },
        latency_ms: 0,
    })
}

async fn check_file_hash_equals(
    path: &Path,
    expected_hex: &str,
) -> Result<PredicateCheckResult, WitnessError> {
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            return Ok(PredicateCheckResult::fail(
                format!("cannot read {}: {}", path.display(), e),
                0,
            ));
        }
    };
    let mut h = Sha256::new();
    h.update(&bytes);
    let actual = hex::encode(h.finalize());
    if actual.eq_ignore_ascii_case(expected_hex) {
        Ok(PredicateCheckResult::pass(
            format!("hash matches for {}", path.display()),
            0,
        ))
    } else {
        Ok(PredicateCheckResult::fail(
            format!(
                "hash mismatch for {}: expected {}, got {}",
                path.display(),
                expected_hex,
                actual
            ),
            0,
        ))
    }
}

async fn check_file_size_in_range(
    path: &Path,
    min_bytes: u64,
    max_bytes: u64,
) -> Result<PredicateCheckResult, WitnessError> {
    let meta = match tokio::fs::metadata(path).await {
        Ok(m) => m,
        Err(e) => {
            return Ok(PredicateCheckResult::fail(
                format!("cannot stat {}: {}", path.display(), e),
                0,
            ));
        }
    };
    let size = meta.len();
    if size >= min_bytes && size <= max_bytes {
        Ok(PredicateCheckResult::pass(
            format!("size {} is within [{}, {}]", size, min_bytes, max_bytes),
            0,
        ))
    } else {
        Ok(PredicateCheckResult::fail(
            format!("size {} is outside [{}, {}]", size, min_bytes, max_bytes),
            0,
        ))
    }
}

async fn check_file_modified_within(
    path: &Path,
    duration_secs: u64,
) -> Result<PredicateCheckResult, WitnessError> {
    let meta = match tokio::fs::metadata(path).await {
        Ok(m) => m,
        Err(e) => {
            return Ok(PredicateCheckResult::fail(
                format!("cannot stat {}: {}", path.display(), e),
                0,
            ));
        }
    };
    let modified = meta.modified()?;
    let now = std::time::SystemTime::now();
    match now.duration_since(modified) {
        Ok(elapsed) if elapsed.as_secs() <= duration_secs => Ok(PredicateCheckResult::pass(
            format!(
                "file modified {}s ago (within {})",
                elapsed.as_secs(),
                duration_secs
            ),
            0,
        )),
        Ok(elapsed) => Ok(PredicateCheckResult::fail(
            format!(
                "file last modified {}s ago, exceeds window {}",
                elapsed.as_secs(),
                duration_secs
            ),
            0,
        )),
        Err(_) => Ok(PredicateCheckResult::inconclusive(
            "clock skew: modified time is in the future".to_string(),
            0,
        )),
    }
}

// ===========================================================================
// Command execution (4 checkers)
// ===========================================================================

async fn run_command(
    cmd: &str,
    args: &[String],
    cwd: Option<&Path>,
    timeout_ms: u64,
) -> Result<(std::process::Output, u64), WitnessError> {
    let mut c = TokioCommand::new(cmd);
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    c.stdin(std::process::Stdio::null());
    let start = Instant::now();
    let fut = c.output();
    let output = match tokio::time::timeout(Duration::from_millis(timeout_ms.max(1)), fut).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return Err(WitnessError::PredicateCheck(format!(
                "cmd `{}` failed to spawn: {}",
                cmd, e
            )));
        }
        Err(_) => {
            return Err(WitnessError::Timeout(timeout_ms));
        }
    };
    let elapsed = start.elapsed().as_millis() as u64;
    Ok((output, elapsed))
}

async fn check_command_exits(
    cmd: &str,
    args: &[String],
    expected_code: i32,
    cwd: Option<&Path>,
    timeout_ms: u64,
) -> Result<PredicateCheckResult, WitnessError> {
    match run_command(cmd, args, cwd, timeout_ms).await {
        Ok((output, elapsed)) => {
            let actual = output.status.code();
            if actual == Some(expected_code) {
                Ok(PredicateCheckResult::pass(
                    format!(
                        "`{} {}` exited {} in {}ms",
                        cmd,
                        args.join(" "),
                        expected_code,
                        elapsed
                    ),
                    0,
                ))
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let snippet: String = stderr.chars().take(300).collect();
                Ok(PredicateCheckResult::fail(
                    format!(
                        "`{} {}` exited {:?}, expected {}. stderr: {}",
                        cmd,
                        args.join(" "),
                        actual,
                        expected_code,
                        snippet
                    ),
                    0,
                ))
            }
        }
        Err(WitnessError::Timeout(ms)) => Ok(PredicateCheckResult::inconclusive(
            format!("`{} {}` timed out after {}ms", cmd, args.join(" "), ms),
            0,
        )),
        Err(e) => Ok(PredicateCheckResult::fail(
            format!("command error: {}", e),
            0,
        )),
    }
}

fn combined_output(out: &std::process::Output, stream: OutputStream) -> String {
    match stream {
        OutputStream::Stdout => String::from_utf8_lossy(&out.stdout).into_owned(),
        OutputStream::Stderr => String::from_utf8_lossy(&out.stderr).into_owned(),
        OutputStream::Either => {
            format!(
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        }
    }
}

async fn check_command_output_contains(
    cmd: &str,
    args: &[String],
    regex_str: &str,
    stream: OutputStream,
    cwd: Option<&Path>,
    timeout_ms: u64,
) -> Result<PredicateCheckResult, WitnessError> {
    match run_command(cmd, args, cwd, timeout_ms).await {
        Ok((output, _elapsed)) => {
            let combined = combined_output(&output, stream);
            let re = regex::Regex::new(regex_str)?;
            if re.is_match(&combined) {
                Ok(PredicateCheckResult::pass(
                    format!("pattern `{}` found in {:?} of `{}`", regex_str, stream, cmd),
                    0,
                ))
            } else {
                Ok(PredicateCheckResult::fail(
                    format!(
                        "pattern `{}` NOT found in {:?} of `{}`",
                        regex_str, stream, cmd
                    ),
                    0,
                ))
            }
        }
        Err(WitnessError::Timeout(ms)) => Ok(PredicateCheckResult::inconclusive(
            format!("`{}` timed out after {}ms", cmd, ms),
            0,
        )),
        Err(e) => Ok(PredicateCheckResult::fail(
            format!("command error: {}", e),
            0,
        )),
    }
}

async fn check_command_output_absent(
    cmd: &str,
    args: &[String],
    regex_str: &str,
    stream: OutputStream,
    cwd: Option<&Path>,
    timeout_ms: u64,
) -> Result<PredicateCheckResult, WitnessError> {
    let res = check_command_output_contains(cmd, args, regex_str, stream, cwd, timeout_ms).await?;
    Ok(PredicateCheckResult {
        outcome: match res.outcome {
            VerdictOutcome::Pass => VerdictOutcome::Fail,
            VerdictOutcome::Fail => VerdictOutcome::Pass,
            VerdictOutcome::Inconclusive => VerdictOutcome::Inconclusive,
        },
        detail: res.detail,
        latency_ms: 0,
    })
}

async fn check_command_duration_under(
    cmd: &str,
    args: &[String],
    max_ms: u64,
    cwd: Option<&Path>,
) -> Result<PredicateCheckResult, WitnessError> {
    match run_command(cmd, args, cwd, max_ms * 2).await {
        Ok((_output, elapsed)) => {
            if elapsed <= max_ms {
                Ok(PredicateCheckResult::pass(
                    format!("`{}` ran in {}ms (under {})", cmd, elapsed, max_ms),
                    0,
                ))
            } else {
                Ok(PredicateCheckResult::fail(
                    format!("`{}` ran in {}ms (exceeded {})", cmd, elapsed, max_ms),
                    0,
                ))
            }
        }
        Err(e) => Ok(PredicateCheckResult::fail(
            format!("command error: {}", e),
            0,
        )),
    }
}

// ===========================================================================
// Process and system (2 checkers)
// ===========================================================================

#[cfg(unix)]
async fn check_process_alive(name_or_pid: &str) -> Result<PredicateCheckResult, WitnessError> {
    // Try numeric PID first.
    if let Ok(pid) = name_or_pid.parse::<i32>() {
        // kill -0 <pid> — returns 0 if process exists and we can signal it.
        let output = TokioCommand::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        match output {
            Ok(s) if s.success() => {
                return Ok(PredicateCheckResult::pass(
                    format!("process PID {} is alive", pid),
                    0,
                ));
            }
            _ => {
                return Ok(PredicateCheckResult::fail(
                    format!("process PID {} is not alive", pid),
                    0,
                ));
            }
        }
    }
    // Name-based: use pgrep if available.
    let out = TokioCommand::new("pgrep")
        .args(["-f", name_or_pid])
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => Ok(PredicateCheckResult::pass(
            format!("process `{}` found", name_or_pid),
            0,
        )),
        Ok(_) => Ok(PredicateCheckResult::fail(
            format!("process `{}` not found", name_or_pid),
            0,
        )),
        Err(_) => Ok(PredicateCheckResult::inconclusive(
            "pgrep unavailable on this system".to_string(),
            0,
        )),
    }
}

#[cfg(windows)]
async fn check_process_alive(name_or_pid: &str) -> Result<PredicateCheckResult, WitnessError> {
    // tasklist /FI "IMAGENAME eq <name>" or "PID eq <pid>"
    let filter = if name_or_pid.parse::<u32>().is_ok() {
        format!("PID eq {}", name_or_pid)
    } else {
        format!("IMAGENAME eq {}", name_or_pid)
    };
    let out = TokioCommand::new("tasklist")
        .args(["/FI", &filter, "/NH", "/FO", "CSV"])
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout);
            if s.contains(name_or_pid) {
                Ok(PredicateCheckResult::pass(
                    format!("process `{}` found", name_or_pid),
                    0,
                ))
            } else {
                Ok(PredicateCheckResult::fail(
                    format!("process `{}` not found", name_or_pid),
                    0,
                ))
            }
        }
        _ => Ok(PredicateCheckResult::inconclusive(
            "tasklist unavailable".to_string(),
            0,
        )),
    }
}

async fn check_port_listening(
    port: u16,
    _interface: Option<&str>,
) -> Result<PredicateCheckResult, WitnessError> {
    // Attempt to bind. If AddrInUse, port is already held (pass).
    let addr = format!("127.0.0.1:{}", port);
    match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => {
            drop(listener);
            Ok(PredicateCheckResult::fail(
                format!("port {} is NOT listening (bind succeeded)", port),
                0,
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => Ok(PredicateCheckResult::pass(
            format!("port {} is already bound", port),
            0,
        )),
        Err(e) => Ok(PredicateCheckResult::inconclusive(
            format!("cannot probe port {}: {}", port, e),
            0,
        )),
    }
}

// ===========================================================================
// Network (2 checkers)
// ===========================================================================

async fn check_http_status(
    url: &str,
    method: &str,
    expected_status: u16,
) -> Result<PredicateCheckResult, WitnessError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| WitnessError::PredicateCheck(format!("reqwest client: {}", e)))?;
    let req = match method.to_uppercase().as_str() {
        "GET" => client.get(url),
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "DELETE" => client.delete(url),
        "HEAD" => client.head(url),
        other => {
            return Ok(PredicateCheckResult::fail(
                format!("unsupported HTTP method: {}", other),
                0,
            ));
        }
    };
    match req.send().await {
        Ok(resp) => {
            let got = resp.status().as_u16();
            if got == expected_status {
                Ok(PredicateCheckResult::pass(
                    format!("{} {} → {}", method, url, got),
                    0,
                ))
            } else {
                Ok(PredicateCheckResult::fail(
                    format!(
                        "{} {} returned {}, expected {}",
                        method, url, got, expected_status
                    ),
                    0,
                ))
            }
        }
        Err(e) => Ok(PredicateCheckResult::fail(
            format!("{} {} failed: {}", method, url, e),
            0,
        )),
    }
}

const MAX_HTTP_BODY_BYTES: usize = 1024 * 1024; // 1 MB

async fn check_http_body_contains(
    url: &str,
    method: &str,
    regex_str: &str,
) -> Result<PredicateCheckResult, WitnessError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| WitnessError::PredicateCheck(format!("reqwest client: {}", e)))?;
    let req = match method.to_uppercase().as_str() {
        "GET" => client.get(url),
        "POST" => client.post(url),
        _ => client.get(url),
    };
    match req.send().await {
        Ok(resp) => {
            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    return Ok(PredicateCheckResult::fail(
                        format!("body read failed: {}", e),
                        0,
                    ));
                }
            };
            let body_str = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_HTTP_BODY_BYTES)]);
            let re = regex::Regex::new(regex_str)?;
            if re.is_match(&body_str) {
                Ok(PredicateCheckResult::pass(
                    format!(
                        "pattern `{}` found in response body from {}",
                        regex_str, url
                    ),
                    0,
                ))
            } else {
                Ok(PredicateCheckResult::fail(
                    format!(
                        "pattern `{}` NOT found in response body from {}",
                        regex_str, url
                    ),
                    0,
                ))
            }
        }
        Err(e) => Ok(PredicateCheckResult::fail(
            format!("request failed: {}", e),
            0,
        )),
    }
}

// ===========================================================================
// Version control (4 checkers)
// ===========================================================================

async fn git_command(args: &[&str], cwd: &Path) -> Result<(String, i32), WitnessError> {
    let out = TokioCommand::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| WitnessError::PredicateCheck(format!("git spawn: {}", e)))?;
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    Ok((stdout, code))
}

async fn check_git_file_in_diff(
    path: &Path,
    include_staged: bool,
    include_unstaged: bool,
    workspace: &Path,
) -> Result<PredicateCheckResult, WitnessError> {
    let target = path.to_string_lossy().to_string();
    let mut found = false;

    if include_unstaged {
        let (out, code) = git_command(&["diff", "--name-only"], workspace).await?;
        if code == 0 && out.lines().any(|l| l.contains(&target)) {
            found = true;
        }
    }
    if include_staged && !found {
        let (out, code) = git_command(&["diff", "--cached", "--name-only"], workspace).await?;
        if code == 0 && out.lines().any(|l| l.contains(&target)) {
            found = true;
        }
    }
    if found {
        Ok(PredicateCheckResult::pass(
            format!("{} is in diff", target),
            0,
        ))
    } else {
        Ok(PredicateCheckResult::fail(
            format!("{} is NOT in diff", target),
            0,
        ))
    }
}

async fn check_git_diff_line_count_at_most(
    max: u64,
    scope: GitScope,
    workspace: &Path,
) -> Result<PredicateCheckResult, WitnessError> {
    let args: &[&str] = match scope {
        GitScope::Staged => &["diff", "--cached", "--numstat"],
        GitScope::Unstaged => &["diff", "--numstat"],
        GitScope::Both => &["diff", "HEAD", "--numstat"],
        GitScope::LastCommit => &["diff", "HEAD~1..HEAD", "--numstat"],
    };
    let (out, code) = git_command(args, workspace).await?;
    if code != 0 {
        return Ok(PredicateCheckResult::inconclusive(
            format!("git diff failed (scope {:?})", scope),
            0,
        ));
    }
    let mut total: u64 = 0;
    for line in out.lines() {
        // Format: "<added>\t<deleted>\t<path>"
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let a: u64 = parts[0].parse().unwrap_or(0);
            let d: u64 = parts[1].parse().unwrap_or(0);
            total += a + d;
        }
    }
    if total <= max {
        Ok(PredicateCheckResult::pass(
            format!("diff line count {} <= {}", total, max),
            0,
        ))
    } else {
        Ok(PredicateCheckResult::fail(
            format!("diff line count {} > {}", total, max),
            0,
        ))
    }
}

async fn check_git_new_files_match(
    glob_pat: &str,
    workspace: &Path,
) -> Result<PredicateCheckResult, WitnessError> {
    let (out, code) = git_command(&["status", "--porcelain"], workspace).await?;
    if code != 0 {
        return Ok(PredicateCheckResult::inconclusive(
            "git status failed".to_string(),
            0,
        ));
    }
    let pattern = glob::Pattern::new(glob_pat)
        .map_err(|e| WitnessError::InvalidPredicate(format!("bad glob: {}", e)))?;
    let mut new_files: Vec<String> = Vec::new();
    for line in out.lines() {
        if let Some(path) = line.strip_prefix("?? ") {
            if pattern.matches(path) {
                new_files.push(path.to_string());
            }
        }
    }
    if !new_files.is_empty() {
        Ok(PredicateCheckResult::pass(
            format!("{} new files match `{}`", new_files.len(), glob_pat),
            0,
        ))
    } else {
        Ok(PredicateCheckResult::fail(
            format!("no new files match `{}`", glob_pat),
            0,
        ))
    }
}

async fn check_git_commit_message_matches(
    regex_str: &str,
    commits_back: u32,
    workspace: &Path,
) -> Result<PredicateCheckResult, WitnessError> {
    let n = format!("-{}", commits_back.max(1));
    let (out, code) = git_command(&["log", &n, "--format=%B"], workspace).await?;
    if code != 0 {
        return Ok(PredicateCheckResult::inconclusive(
            "git log failed".to_string(),
            0,
        ));
    }
    let re = regex::Regex::new(regex_str)?;
    if re.is_match(&out) {
        Ok(PredicateCheckResult::pass(
            format!(
                "pattern `{}` found in last {} commit message(s)",
                regex_str, commits_back
            ),
            0,
        ))
    } else {
        Ok(PredicateCheckResult::fail(
            format!(
                "pattern `{}` NOT found in last {} commit message(s)",
                regex_str, commits_back
            ),
            0,
        ))
    }
}

// ===========================================================================
// Text search (3 checkers)
// ===========================================================================

const MAX_FILES_PER_GREP: usize = 5000;

async fn collect_glob_files(
    path_glob: &str,
    workspace: &Path,
) -> Result<Vec<PathBuf>, WitnessError> {
    let full_glob = if Path::new(path_glob).is_absolute() {
        path_glob.to_string()
    } else {
        workspace.join(path_glob).to_string_lossy().into_owned()
    };
    let mut files = Vec::new();
    for entry in glob::glob(&full_glob)
        .map_err(|e| WitnessError::InvalidPredicate(format!("bad glob: {}", e)))?
    {
        match entry {
            Ok(p) if p.is_file() => {
                files.push(p);
                if files.len() >= MAX_FILES_PER_GREP {
                    break;
                }
            }
            _ => continue,
        }
    }
    Ok(files)
}

async fn check_grep_present(
    pattern: &str,
    path_glob: &str,
    workspace: &Path,
) -> Result<PredicateCheckResult, WitnessError> {
    let re = regex::Regex::new(pattern)?;
    let files = collect_glob_files(path_glob, workspace).await?;
    for f in &files {
        let content = match tokio::fs::read_to_string(f).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        if re.is_match(&content) {
            return Ok(PredicateCheckResult::pass(
                format!("pattern `{}` found in {}", pattern, f.display()),
                0,
            ));
        }
    }
    Ok(PredicateCheckResult::fail(
        format!(
            "pattern `{}` NOT found in any of {} files matching `{}`",
            pattern,
            files.len(),
            path_glob
        ),
        0,
    ))
}

async fn check_grep_absent(
    pattern: &str,
    path_glob: &str,
    workspace: &Path,
) -> Result<PredicateCheckResult, WitnessError> {
    let re = regex::Regex::new(pattern)?;
    let files = collect_glob_files(path_glob, workspace).await?;
    for f in &files {
        let content = match tokio::fs::read_to_string(f).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        if re.is_match(&content) {
            return Ok(PredicateCheckResult::fail(
                format!(
                    "anti-pattern `{}` FOUND in {} (predicate fails)",
                    pattern,
                    f.display()
                ),
                0,
            ));
        }
    }
    Ok(PredicateCheckResult::pass(
        format!(
            "anti-pattern `{}` not found in any of {} files (predicate passes)",
            pattern,
            files.len()
        ),
        0,
    ))
}

async fn check_grep_count_at_least(
    pattern: &str,
    path_glob: &str,
    n: u32,
    workspace: &Path,
) -> Result<PredicateCheckResult, WitnessError> {
    let re = regex::Regex::new(pattern)?;
    let files = collect_glob_files(path_glob, workspace).await?;
    let mut total: u32 = 0;
    for f in &files {
        let content = match tokio::fs::read_to_string(f).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        total += re.find_iter(&content).count() as u32;
        if total >= n {
            return Ok(PredicateCheckResult::pass(
                format!("found {} matches for `{}` (needed {})", total, pattern, n),
                0,
            ));
        }
    }
    Ok(PredicateCheckResult::fail(
        format!(
            "found only {} matches for `{}` across {} files (needed {})",
            total,
            pattern,
            files.len(),
            n
        ),
        0,
    ))
}

// ===========================================================================
// Time (1 checker)
// ===========================================================================

async fn check_elapsed_under(
    start_marker: &str,
    max_secs: u64,
    ctx: &CheckContext,
) -> Result<PredicateCheckResult, WitnessError> {
    let start = match ctx.markers.get(start_marker) {
        Some(t) => *t,
        None => {
            return Ok(PredicateCheckResult::inconclusive(
                format!("marker `{}` not set", start_marker),
                0,
            ));
        }
    };
    let now = Utc::now();
    let elapsed = now.signed_duration_since(start).num_seconds().max(0) as u64;
    if elapsed <= max_secs {
        Ok(PredicateCheckResult::pass(
            format!("elapsed {}s (under {})", elapsed, max_secs),
            0,
        ))
    } else {
        Ok(PredicateCheckResult::fail(
            format!("elapsed {}s (exceeded {})", elapsed, max_secs),
            0,
        ))
    }
}

// ===========================================================================
// Composites (3 checkers)
// ===========================================================================

async fn check_all_of(
    predicates: &[Predicate],
    ctx: &CheckContext,
) -> Result<PredicateCheckResult, WitnessError> {
    for p in predicates {
        let r = Box::pin(check_tier0(p, ctx)).await?;
        if r.outcome != VerdictOutcome::Pass {
            return Ok(PredicateCheckResult::fail(
                format!("AllOf failed at sub-predicate: {}", r.detail),
                0,
            ));
        }
    }
    Ok(PredicateCheckResult::pass(
        format!("all {} sub-predicates passed", predicates.len()),
        0,
    ))
}

async fn check_any_of(
    predicates: &[Predicate],
    ctx: &CheckContext,
) -> Result<PredicateCheckResult, WitnessError> {
    let mut last_detail = String::new();
    for p in predicates {
        let r = Box::pin(check_tier0(p, ctx)).await?;
        if r.outcome == VerdictOutcome::Pass {
            return Ok(PredicateCheckResult::pass(
                format!("AnyOf satisfied by: {}", r.detail),
                0,
            ));
        }
        last_detail = r.detail;
    }
    Ok(PredicateCheckResult::fail(
        format!(
            "none of {} sub-predicates passed (last: {})",
            predicates.len(),
            last_detail
        ),
        0,
    ))
}

async fn check_not_of(
    predicate: &Predicate,
    ctx: &CheckContext,
) -> Result<PredicateCheckResult, WitnessError> {
    let r = Box::pin(check_tier0(predicate, ctx)).await?;
    Ok(PredicateCheckResult {
        outcome: match r.outcome {
            VerdictOutcome::Pass => VerdictOutcome::Fail,
            VerdictOutcome::Fail => VerdictOutcome::Pass,
            VerdictOutcome::Inconclusive => VerdictOutcome::Inconclusive,
        },
        detail: format!("NotOf({})", r.detail),
        latency_ms: 0,
    })
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn ctx(root: &Path) -> CheckContext {
        CheckContext::new(root)
    }

    #[tokio::test]
    async fn file_exists_pass() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("hello.txt");
        tokio::fs::write(&p, "hi").await.unwrap();
        let c = ctx(dir.path());
        let r = check_tier0(&Predicate::FileExists { path: p }, &c)
            .await
            .unwrap();
        assert_eq!(r.outcome, VerdictOutcome::Pass);
    }

    #[tokio::test]
    async fn file_exists_fail() {
        let dir = tempdir().unwrap();
        let c = ctx(dir.path());
        let r = check_tier0(
            &Predicate::FileExists {
                path: dir.path().join("missing.txt"),
            },
            &c,
        )
        .await
        .unwrap();
        assert_eq!(r.outcome, VerdictOutcome::Fail);
    }

    #[tokio::test]
    async fn file_contains_pass() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("log.txt");
        tokio::fs::write(&p, "hello world").await.unwrap();
        let c = ctx(dir.path());
        let r = check_tier0(
            &Predicate::FileContains {
                path: p,
                regex: "hello".into(),
            },
            &c,
        )
        .await
        .unwrap();
        assert_eq!(r.outcome, VerdictOutcome::Pass);
    }

    #[tokio::test]
    async fn file_contains_fail() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("log.txt");
        tokio::fs::write(&p, "hello world").await.unwrap();
        let c = ctx(dir.path());
        let r = check_tier0(
            &Predicate::FileContains {
                path: p,
                regex: "goodbye".into(),
            },
            &c,
        )
        .await
        .unwrap();
        assert_eq!(r.outcome, VerdictOutcome::Fail);
    }

    #[tokio::test]
    async fn file_does_not_contain_inverts() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("code.rs");
        tokio::fs::write(&p, "fn main() { todo!() }").await.unwrap();
        let c = ctx(dir.path());
        let r = check_tier0(
            &Predicate::FileDoesNotContain {
                path: p.clone(),
                regex: "todo!".into(),
            },
            &c,
        )
        .await
        .unwrap();
        assert_eq!(r.outcome, VerdictOutcome::Fail);

        let r2 = check_tier0(
            &Predicate::FileDoesNotContain {
                path: p,
                regex: "never_here".into(),
            },
            &c,
        )
        .await
        .unwrap();
        assert_eq!(r2.outcome, VerdictOutcome::Pass);
    }

    #[tokio::test]
    async fn command_exits_pass() {
        let dir = tempdir().unwrap();
        let c = ctx(dir.path());
        let r = check_tier0(
            &Predicate::CommandExits {
                cmd: "true".into(),
                args: vec![],
                expected_code: 0,
                cwd: None,
                timeout_ms: 5000,
            },
            &c,
        )
        .await
        .unwrap();
        assert_eq!(r.outcome, VerdictOutcome::Pass);
    }

    #[tokio::test]
    async fn command_exits_fail() {
        let dir = tempdir().unwrap();
        let c = ctx(dir.path());
        let r = check_tier0(
            &Predicate::CommandExits {
                cmd: "false".into(),
                args: vec![],
                expected_code: 0,
                cwd: None,
                timeout_ms: 5000,
            },
            &c,
        )
        .await
        .unwrap();
        assert_eq!(r.outcome, VerdictOutcome::Fail);
    }

    #[tokio::test]
    async fn grep_present_pass() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("foo.rs");
        tokio::fs::write(&f, "fn my_function() {}").await.unwrap();
        let c = ctx(dir.path());
        let r = check_tier0(
            &Predicate::GrepPresent {
                pattern: "my_function".into(),
                path_glob: "*.rs".into(),
            },
            &c,
        )
        .await
        .unwrap();
        assert_eq!(r.outcome, VerdictOutcome::Pass);
    }

    #[tokio::test]
    async fn grep_absent_catches_stubs() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("stub.rs");
        tokio::fs::write(&f, "fn work() { todo!(\"later\") }")
            .await
            .unwrap();
        let c = ctx(dir.path());
        let r = check_tier0(
            &Predicate::GrepAbsent {
                pattern: r"todo!\(".into(),
                path_glob: "*.rs".into(),
            },
            &c,
        )
        .await
        .unwrap();
        assert_eq!(r.outcome, VerdictOutcome::Fail);
    }

    #[tokio::test]
    async fn grep_count_at_least_wiring_check() {
        let dir = tempdir().unwrap();
        let f1 = dir.path().join("a.rs");
        let f2 = dir.path().join("b.rs");
        tokio::fs::write(&f1, "fn my_fn() {}").await.unwrap();
        tokio::fs::write(&f2, "fn main() { my_fn(); }")
            .await
            .unwrap();
        let c = ctx(dir.path());
        let r = check_tier0(
            &Predicate::GrepCountAtLeast {
                pattern: "my_fn".into(),
                path_glob: "*.rs".into(),
                n: 2,
            },
            &c,
        )
        .await
        .unwrap();
        assert_eq!(r.outcome, VerdictOutcome::Pass);
    }

    #[tokio::test]
    async fn grep_count_at_least_unwired_symbol_fails() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.rs");
        // Definition only, no call site.
        tokio::fs::write(&f, "fn unwired() {}").await.unwrap();
        let c = ctx(dir.path());
        let r = check_tier0(
            &Predicate::GrepCountAtLeast {
                pattern: "unwired".into(),
                path_glob: "*.rs".into(),
                n: 2,
            },
            &c,
        )
        .await
        .unwrap();
        assert_eq!(r.outcome, VerdictOutcome::Fail);
    }

    #[tokio::test]
    async fn all_of_composite() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("good.rs");
        tokio::fs::write(&f, "fn ok() {}").await.unwrap();
        let c = ctx(dir.path());
        let r = check_tier0(
            &Predicate::AllOf {
                predicates: vec![
                    Predicate::FileExists { path: f.clone() },
                    Predicate::FileContains {
                        path: f.clone(),
                        regex: "fn ok".into(),
                    },
                ],
            },
            &c,
        )
        .await
        .unwrap();
        assert_eq!(r.outcome, VerdictOutcome::Pass);
    }

    #[tokio::test]
    async fn any_of_composite() {
        let dir = tempdir().unwrap();
        let c = ctx(dir.path());
        let r = check_tier0(
            &Predicate::AnyOf {
                predicates: vec![
                    Predicate::FileExists {
                        path: dir.path().join("missing1.txt"),
                    },
                    Predicate::CommandExits {
                        cmd: "true".into(),
                        args: vec![],
                        expected_code: 0,
                        cwd: None,
                        timeout_ms: 2000,
                    },
                ],
            },
            &c,
        )
        .await
        .unwrap();
        assert_eq!(r.outcome, VerdictOutcome::Pass);
    }

    #[tokio::test]
    async fn tier1_rejected_by_tier0_dispatch() {
        let dir = tempdir().unwrap();
        let c = ctx(dir.path());
        let r = check_tier0(
            &Predicate::AspectVerifier {
                rubric: "is this good?".into(),
                evidence_refs: vec![],
                advisory: false,
            },
            &c,
        )
        .await;
        assert!(r.is_err());
        assert!(matches!(r, Err(WitnessError::InvalidPredicate(_))));
    }
}
