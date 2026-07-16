// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! LSP diagnostics backend: a *persistent*, incremental Language Server client.
//!
//! A [`LspServer`] is initialized once and kept warm for the session: a
//! background reader task tracks the latest `publishDiagnostics` per file, and a
//! snapshot only sends `didOpen`/`didChange` for files that actually changed,
//! then reads the current state. This avoids the per-collection re-index that
//! makes a one-shot client slow for heavy servers (rust-analyzer).
//!
//! A process-global [`LspManager`] keeps one server per (workspace, command) so
//! both the stateless `diagnostics` query tool and the app's post-edit loop
//! reuse the same warm server. Servers are torn down by [`shutdown_all`] when
//! the app exits.
//!
//! The wire codec, URI handling, and diagnostic normalization are pure and
//! unit-tested; the process/session orchestration is covered by a real-server
//! integration test (clangd) when one is installed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{Mutex, Notify};

use super::{Checker, Diagnostic, Severity};

/// After sending changes, wait for the server to be quiet this long before
/// trusting the diagnostics have settled.
const QUIESCENCE: Duration = Duration::from_millis(1200);
/// Ceiling on one snapshot's wait, in case the server never goes quiet.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling on the initialize handshake.
const INIT_TIMEOUT: Duration = Duration::from_secs(30);
/// Cap on how many workspace files we track per checker.
const MAX_OPEN_FILES: usize = 200;

// ---------------------------------------------------------------------------
// Wire codec (pure)
// ---------------------------------------------------------------------------

/// Frame a JSON-RPC value as an LSP message: `Content-Length` header + body.
pub fn encode_message(value: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(value).unwrap_or_default();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Parse a `Content-Length: N` header line, ignoring case and whitespace.
/// Returns `None` for any other header (e.g. `Content-Type`).
pub fn parse_header_content_length(line: &str) -> Option<usize> {
    let (name, value) = line.split_once(':')?;
    if !name.trim().eq_ignore_ascii_case("content-length") {
        return None;
    }
    value.trim().parse::<usize>().ok()
}

/// Read one framed LSP message. `Ok(None)` means clean EOF.
async fn read_message(reader: &mut BufReader<ChildStdout>) -> Result<Option<Value>, String> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some(len) = parse_header_content_length(trimmed) {
            content_length = Some(len);
        }
    }
    let len = content_length.ok_or("LSP message missing Content-Length")?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await.map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map(Some).map_err(|e| e.to_string())
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

// ---------------------------------------------------------------------------
// URI / normalization (pure)
// ---------------------------------------------------------------------------

/// Turn an absolute filesystem path into a `file://` URI.
pub fn path_to_uri(path: &Path) -> String {
    // Normalize Windows backslashes to forward slashes — file URIs always use `/`.
    let s = path.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{}", s.trim_start_matches('/'))
    }
}

/// Turn a `file://` URI back into a path string, percent-decoded and made
/// relative to `cwd` when it lives inside the workspace. Both sides are
/// normalised to forward slashes before comparison so Windows paths work.
pub fn uri_to_path(uri: &str, cwd: &Path) -> String {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let decoded = percent_decode(raw);
    let cwd_norm = cwd.to_string_lossy().replace('\\', "/");
    match decoded.strip_prefix(&cwd_norm) {
        Some(rest) => rest.trim_start_matches('/').to_string(),
        None => decoded,
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Map an LSP `DiagnosticSeverity` (1=Error … 4=Hint) to our [`Severity`].
pub fn severity_from_lsp(code: Option<u64>) -> Severity {
    match code {
        Some(1) => Severity::Error,
        Some(2) => Severity::Warning,
        _ => Severity::Info,
    }
}

/// Convert one LSP diagnostic object into a normalized [`Diagnostic`].
pub fn diagnostic_from_lsp(value: &Value, file: &str) -> Diagnostic {
    let start = value.get("range").and_then(|r| r.get("start"));
    let line = start
        .and_then(|s| s.get("line"))
        .and_then(Value::as_u64)
        .map(|l| l as u32 + 1)
        .unwrap_or(0);
    let col = start
        .and_then(|s| s.get("character"))
        .and_then(Value::as_u64)
        .map(|c| c as u32 + 1);
    let severity = severity_from_lsp(value.get("severity").and_then(Value::as_u64));
    let code = value.get("code").and_then(|c| match c {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    });
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    Diagnostic { file: file.to_string(), line, col, severity, code, message }
}

// ---------------------------------------------------------------------------
// Persistent server
// ---------------------------------------------------------------------------

/// Diagnostics the reader task keeps up to date, keyed by file URI.
#[derive(Default)]
struct ServerState {
    by_uri: HashMap<String, Vec<Diagnostic>>,
}

/// One warm language-server process.
pub struct LspServer {
    child: Mutex<tokio::process::Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    state: Arc<Mutex<ServerState>>,
    /// Pulsed by the reader task whenever diagnostics change.
    updated: Arc<Notify>,
    /// Files we've opened, with the file mtime we last sent — so a snapshot only
    /// re-sends `didChange` for files that actually changed.
    opened: Mutex<HashMap<PathBuf, SystemTime>>,
    /// Serializes concurrent snapshots against the same server.
    op_lock: Mutex<()>,
    cwd: PathBuf,
    next_id: std::sync::atomic::AtomicU64,
}

impl LspServer {
    /// Spawn the server and complete the `initialize` handshake.
    async fn start(checker: &Checker, cwd: &Path) -> Result<LspServer, String> {
        let (program, flag) = crate::tools::shell();
        let mut cmd = tokio::process::Command::new(program);
        cmd.arg(flag)
            .arg(&checker.command)
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("checker '{}': failed to start LSP server: {e}", checker.name))?;

        let stdin = Arc::new(Mutex::new(
            child
                .stdin
                .take()
                .ok_or_else(|| format!("checker '{}': no stdin", checker.name))?,
        ));
        let mut reader = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| format!("checker '{}': no stdout", checker.name))?,
        );

        // initialize → wait for the response, answering server requests.
        let root_uri = path_to_uri(cwd);
        send(&stdin, &request(1, "initialize", json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": { "textDocument": { "publishDiagnostics": { "relatedInformation": false } } },
            "workspaceFolders": [{ "uri": root_uri, "name": "workspace" }],
        })))
        .await
        .map_err(|e| format!("checker '{}': write initialize: {e}", checker.name))?;

        let init = tokio::time::timeout(INIT_TIMEOUT, async {
            loop {
                match read_message(&mut reader).await {
                    Ok(Some(msg)) => {
                        if msg.get("id").and_then(Value::as_u64) == Some(1)
                            && msg.get("result").is_some()
                        {
                            return Ok(());
                        }
                        answer_server_request(&stdin, &msg).await;
                    }
                    Ok(None) => return Err("server closed during initialize".to_string()),
                    Err(e) => return Err(e),
                }
            }
        })
        .await;
        match init {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("checker '{}': {e}", checker.name)),
            Err(_) => return Err(format!("checker '{}': initialize timed out", checker.name)),
        }

        send(&stdin, &notification("initialized", json!({}))).await.ok();

        let state = Arc::new(Mutex::new(ServerState::default()));
        let updated = Arc::new(Notify::new());
        spawn_reader(reader, stdin.clone(), state.clone(), updated.clone(), cwd.to_path_buf());

        Ok(LspServer {
            child: Mutex::new(child),
            stdin,
            state,
            updated,
            opened: Mutex::new(HashMap::new()),
            op_lock: Mutex::new(()),
            cwd: cwd.to_path_buf(),
            next_id: std::sync::atomic::AtomicU64::new(2),
        })
    }

    async fn is_alive(&self) -> bool {
        matches!(self.child.lock().await.try_wait(), Ok(None))
    }

    /// Sync changed files, wait for the server to settle, and return the current
    /// diagnostics across all tracked files.
    async fn snapshot(&self, checker: &Checker) -> Result<Vec<Diagnostic>, String> {
        let _guard = self.op_lock.lock().await;
        let _checking = CheckGuard::new();

        // Send didOpen for newly-seen files and didChange for changed ones.
        let mut sent_change = false;
        {
            let mut opened = self.opened.lock().await;
            for path in workspace_files(checker, &self.cwd) {
                let mtime = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                match opened.get(&path) {
                    None => {
                        self.did_open(&path, &text).await;
                        opened.insert(path, mtime);
                        sent_change = true;
                    }
                    Some(&prev) if prev != mtime => {
                        self.did_change(&path, &text).await;
                        opened.insert(path, mtime);
                        sent_change = true;
                    }
                    Some(_) => {} // unchanged — the server already has it
                }
            }
        }

        // Wait for diagnostics to settle. If nothing changed we still give the
        // server a brief window (it may still be finishing initial analysis).
        let start = Instant::now();
        let quiet = if sent_change { QUIESCENCE } else { Duration::from_millis(300) };
        loop {
            if start.elapsed() > SNAPSHOT_TIMEOUT {
                break;
            }
            match tokio::time::timeout(quiet, self.updated.notified()).await {
                Ok(_) => {}        // an update arrived; keep waiting for it to settle
                Err(_) => break,   // quiet ⇒ settled
            }
        }

        let state = self.state.lock().await;
        let mut diagnostics: Vec<Diagnostic> = state.by_uri.values().flatten().cloned().collect();
        diagnostics.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then_with(|| a.line.cmp(&b.line))
                .then_with(|| a.severity.cmp(&b.severity))
        });
        Ok(diagnostics)
    }

    async fn did_open(&self, path: &Path, text: &str) {
        let msg = notification(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": path_to_uri(path),
                "languageId": language_id(path),
                "version": 1,
                "text": text,
            }}),
        );
        send(&self.stdin, &msg).await.ok();
    }

    async fn did_change(&self, path: &Path, text: &str) {
        let version = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let msg = notification(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": path_to_uri(path), "version": version },
                // Full-document sync: one change with the whole new text.
                "contentChanges": [{ "text": text }],
            }),
        );
        send(&self.stdin, &msg).await.ok();
    }

    async fn shutdown(&self) {
        send(&self.stdin, &request(9999, "shutdown", json!(null))).await.ok();
        send(&self.stdin, &notification("exit", json!(null))).await.ok();
        let _ = self.child.lock().await.start_kill();
    }
}

/// The continuous reader: track publishDiagnostics and answer server requests.
fn spawn_reader(
    mut reader: BufReader<ChildStdout>,
    stdin: Arc<Mutex<ChildStdin>>,
    state: Arc<Mutex<ServerState>>,
    updated: Arc<Notify>,
    cwd: PathBuf,
) {
    tokio::spawn(async move {
        loop {
            match read_message(&mut reader).await {
                Ok(Some(msg)) => {
                    if msg.get("method").and_then(Value::as_str)
                        == Some("textDocument/publishDiagnostics")
                    {
                        if let Some(params) = msg.get("params") {
                            let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
                            let file = uri_to_path(uri, &cwd);
                            let diags = params
                                .get("diagnostics")
                                .and_then(Value::as_array)
                                .map(|arr| arr.iter().map(|d| diagnostic_from_lsp(d, &file)).collect())
                                .unwrap_or_default();
                            state.lock().await.by_uri.insert(uri.to_string(), diags);
                            updated.notify_waiters();
                        }
                    } else {
                        answer_server_request(&stdin, &msg).await;
                    }
                }
                _ => break, // EOF or parse error: the server is gone
            }
        }
    });
}

/// If `msg` is a server→client request, reply with a null result so the server
/// isn't left waiting.
async fn answer_server_request(stdin: &Mutex<ChildStdin>, msg: &Value) {
    let (Some(id), true) = (msg.get("id"), msg.get("method").is_some()) else {
        return;
    };
    let reply = json!({ "jsonrpc": "2.0", "id": id, "result": null });
    send(stdin, &reply).await.ok();
}

async fn send(stdin: &Mutex<ChildStdin>, value: &Value) -> Result<(), String> {
    let mut s = stdin.lock().await;
    s.write_all(&encode_message(value)).await.map_err(|e| e.to_string())?;
    s.flush().await.map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Global manager
// ---------------------------------------------------------------------------

static MANAGER: OnceLock<LspManager> = OnceLock::new();

/// Number of warm servers in the manager, mirrored as an atomic so the footer
/// can read LSP status synchronously without touching the async lock.
static SERVER_COUNT: AtomicUsize = AtomicUsize::new(0);
/// Number of snapshots currently in flight (a checker is running).
static CHECKING: AtomicUsize = AtomicUsize::new(0);
/// Whether the last LSP attempt failed (server wouldn't start / snapshot errored).
static FAILED: AtomicBool = AtomicBool::new(false);

/// A sync-readable summary of LSP activity, for the status footer.
pub struct LspStatus {
    /// Warm language servers currently held.
    pub servers: usize,
    /// Whether a diagnostics snapshot is running right now.
    pub checking: bool,
    /// Whether the most recent LSP attempt failed.
    pub failed: bool,
}

/// Current LSP status. Cheap; safe to call every frame.
pub fn status() -> LspStatus {
    LspStatus {
        servers: SERVER_COUNT.load(Ordering::Relaxed),
        checking: CHECKING.load(Ordering::Relaxed) > 0,
        failed: FAILED.load(Ordering::Relaxed),
    }
}

/// Increments the in-flight counter for its lifetime.
struct CheckGuard;
impl CheckGuard {
    fn new() -> Self {
        CHECKING.fetch_add(1, Ordering::Relaxed);
        CheckGuard
    }
}
impl Drop for CheckGuard {
    fn drop(&mut self) {
        CHECKING.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Keeps one warm [`LspServer`] per (workspace, command).
struct LspManager {
    servers: Mutex<HashMap<String, Arc<LspServer>>>,
}

impl LspManager {
    fn global() -> &'static LspManager {
        MANAGER.get_or_init(|| LspManager { servers: Mutex::new(HashMap::new()) })
    }

    async fn snapshot(&self, checker: &Checker, cwd: &Path) -> Result<Vec<Diagnostic>, String> {
        let key = format!("{}\u{0}{}", cwd.display(), checker.command);
        let server = {
            let mut servers = self.servers.lock().await;
            match servers.get(&key) {
                Some(s) if s.is_alive().await => s.clone(),
                _ => {
                    let s = Arc::new(LspServer::start(checker, cwd).await?);
                    servers.insert(key, s.clone());
                    s
                }
            }
        };
        SERVER_COUNT.store(self.servers.lock().await.len(), Ordering::Relaxed);
        server.snapshot(checker).await
    }
}

/// Entry point used by the runner: return the LSP checker's current diagnostics,
/// starting or reusing a warm server as needed.
pub async fn collect_lsp(checker: &Checker, cwd: &Path) -> Result<Vec<Diagnostic>, String> {
    let result = LspManager::global().snapshot(checker, cwd).await;
    FAILED.store(result.is_err(), Ordering::Relaxed);
    result
}

/// Tear down every running server. Called when the app exits.
pub async fn shutdown_all() {
    if let Some(mgr) = MANAGER.get() {
        let servers: Vec<Arc<LspServer>> = mgr.servers.lock().await.drain().map(|(_, s)| s).collect();
        SERVER_COUNT.store(0, Ordering::Relaxed);
        for server in servers {
            server.shutdown().await;
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Guess an LSP `languageId` from a file extension.
fn language_id(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "java" => "java",
        other => Box::leak(other.to_string().into_boxed_str()),
    }
}

/// Enumerate workspace files matching the checker's `run_on` globs, skipping
/// build/vcs directories and capping the count.
fn workspace_files(checker: &Checker, cwd: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, cwd: &Path, checker: &Checker, out: &mut Vec<PathBuf>) {
        if out.len() >= MAX_OPEN_FILES {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if out.len() >= MAX_OPEN_FILES {
                return;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if matches!(name.as_ref(), "target" | "node_modules" | ".git" | ".programmer")
                    || name.starts_with('.')
                {
                    continue;
                }
                walk(&path, cwd, checker, out);
            } else {
                let rel = path.strip_prefix(cwd).unwrap_or(&path);
                if checker.applies_to(&rel.to_string_lossy()) {
                    out.push(path);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(cwd, cwd, checker, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_frames_with_content_length() {
        let text = String::from_utf8(encode_message(&json!({"a": 1}))).unwrap();
        assert!(text.starts_with("Content-Length: 7\r\n\r\n"));
        assert!(text.ends_with("{\"a\":1}"));
    }

    #[test]
    fn header_parse_is_case_insensitive_and_selective() {
        assert_eq!(parse_header_content_length("Content-Length: 42"), Some(42));
        assert_eq!(parse_header_content_length("content-length:  7 "), Some(7));
        assert_eq!(parse_header_content_length("Content-Type: application/json"), None);
    }

    #[test]
    fn uri_roundtrip_and_relativization() {
        let cwd = Path::new("/home/u/proj");
        let uri = path_to_uri(&cwd.join("src/main.rs"));
        assert_eq!(uri, "file:///home/u/proj/src/main.rs");
        assert_eq!(uri_to_path(&uri, cwd), "src/main.rs");
        assert_eq!(uri_to_path("file:///etc/hosts", cwd), "/etc/hosts");
        assert_eq!(uri_to_path("file:///home/u/proj/a%20b.rs", cwd), "a b.rs");
    }

    #[test]
    fn severity_mapping() {
        assert_eq!(severity_from_lsp(Some(1)), Severity::Error);
        assert_eq!(severity_from_lsp(Some(2)), Severity::Warning);
        assert_eq!(severity_from_lsp(Some(4)), Severity::Info);
        assert_eq!(severity_from_lsp(None), Severity::Info);
    }

    #[test]
    fn normalizes_lsp_diagnostic_1based() {
        let v = json!({
            "range": { "start": { "line": 41, "character": 4 } },
            "severity": 1, "code": "E0308", "message": "  mismatched types  "
        });
        let d = diagnostic_from_lsp(&v, "src/foo.rs");
        assert_eq!((d.line, d.col), (42, Some(5)));
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.code.as_deref(), Some("E0308"));
        assert_eq!(d.message, "mismatched types");
    }

    #[test]
    fn numeric_code_is_stringified() {
        let v = json!({ "range": { "start": { "line": 0, "character": 0 } }, "severity": 2, "code": 2322, "message": "x" });
        assert_eq!(diagnostic_from_lsp(&v, "a.ts").code.as_deref(), Some("2322"));
    }

    fn which(bin: &str) -> Option<String> {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {bin}"))
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn c_checker() -> Checker {
        Checker {
            name: "clangd".into(),
            kind: super::super::CheckerKind::Lsp,
            command: "clangd".into(),
            parser: "gnu".into(),
            pattern: None,
            run_on: vec!["*.c".into()],
        }
    }

    // Real-server integration test: a single warm server serves two snapshots,
    // observing an edit take effect via didChange — no re-initialization.
    #[cfg(unix)]
    #[tokio::test]
    async fn persistent_server_reflects_edits_incrementally() {
        if which("clangd").is_none() {
            eprintln!("skipping: clangd not installed");
            return;
        }
        let dir = std::env::temp_dir().join(format!("programmer-lspb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bad.c");
        std::fs::write(&file, "int main() { return undefined_symbol; }\n").unwrap();

        let checker = c_checker();
        let server = LspServer::start(&checker, &dir).await.unwrap();

        // First snapshot: the error is present.
        let first = server.snapshot(&checker).await.unwrap();
        assert!(
            first.iter().any(|d| d.severity == Severity::Error),
            "expected an error first, got: {first:?}"
        );

        // Fix the file on disk; the same warm server should report it clean
        // after a didChange — without restarting.
        std::fs::write(&file, "int main() { return 0; }\n").unwrap();
        // mtime granularity: ensure the change is observable.
        let _ = std::fs::File::open(&file);
        let second = server.snapshot(&checker).await.unwrap();
        assert!(
            !second.iter().any(|d| d.severity == Severity::Error),
            "expected clean after fix, got: {second:?}"
        );

        assert!(server.is_alive().await, "server should still be running");
        server.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
