//! `handoff-verify`: typed credential-class proof with structural evidence (R-N3).
//!
//! # What this is
//!
//! A handoff between operators (or between a human and an agent seat) is only
//! real if the receiving seat can *prove* it holds each credential class, not
//! merely assert it. This module runs one small probe set per class and emits a
//! receipt: a table for the operator and a stable JSON document
//! (`tcfs.bulkload.handoff.v1`) for the evidence corpus.
//!
//! # Evidence contract (structural, not best-effort)
//!
//! Evidence strings are assembled *only* from:
//!
//! - process exit statuses,
//! - counts (byte lengths, line counts, context counts),
//! - identifiers the operator already knows: an SSH host alias, a kube context
//!   name, a GPG key id, an age *public* key.
//!
//! Child stdout is consumed to a byte length and dropped. Where a probe needs a
//! name out of stdout, it reads it through [`identifier_lines`], which keeps
//! only lines drawn from a closed identifier charset and drops any line that
//! carries a secret-shaped token. Child stderr is captured, scanned by
//! [`sanitize_note`], and replaced wholesale with `<redacted>` when any
//! secret-shaped token appears. No probe ever writes a credential anywhere.
//!
//! # Process discipline (R-N11)
//!
//! This module never signals a child process. Timeouts prefer each tool's own
//! flag (`--request-timeout=5s`, `-o ConnectTimeout=5`); where a tool has none,
//! [`run`] polls `try_wait` against a wall clock and, on expiry, *detaches* the
//! child onto a reaper thread that only waits on it. There is no `kill`, no
//! `libc::kill`, and no signal of any kind in this file.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tcfs_bulkload_proto::{BulkloadRefusal, Result};

/// The receipt schema identity. Stable once a milestone ships.
pub const SCHEMA: &str = "tcfs.bulkload.handoff.v1";

/// Token shapes that must never reach a receipt.
///
/// A match anywhere in a captured stream replaces the *whole* sanitized note
/// with `<redacted>` and drops the offending identifier line. Over-redaction is
/// the safe direction: a receipt that says less is still a valid receipt.
const SECRET_MARKERS: &[&str] = &[
    "ghp_",
    "gho_",
    "github_pat_",
    "AGE-SECRET-KEY-",
    "-----BEGIN",
    "sk-",
    "xoxb-",
    "Bearer ",
];

/// Bytes of a child stream retained for parsing. Beyond this the stream is
/// still drained (so the child never stalls on a full pipe) but discarded.
const CAPTURE_CAP: usize = 256 * 1024;
/// Longest sanitized stderr note admitted into evidence.
const NOTE_CAP: usize = 160;
/// Longest identifier line admitted, and the most such lines kept.
const LINE_CAP: usize = 200;
const LINES_CAP: usize = 256;
/// `try_wait` poll interval for tools without their own timeout flag.
const POLL: Duration = Duration::from_millis(25);

/// The upstreams the `git` class proves reachability against.
const GIT_SSH_REMOTE: &str = "git@github.com:Jesssullivan/tummycrypt.git";
const GIT_HTTPS_REMOTE: &str = "https://github.com/Jesssullivan/tummycrypt.git";

/// A credential class a handoff must prove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// `sops` decryption and age recipient coverage.
    Sops,
    /// Kubernetes contexts and per-context control-plane reachability.
    Kubeconfig,
    /// Non-interactive SSH reachability for each configured host alias.
    Ssh,
    /// Detached-signature round trip with the configured signing key.
    Gpg,
    /// GitHub CLI authentication state.
    Gh,
    /// Git upstream reachability over both transports.
    Git,
    /// Claude CLI reachability.
    Claude,
    /// Codex credential file custody and CLI presence.
    Codex,
}

impl Class {
    /// Every class a receipt must account for.
    pub const ALL: [Self; 8] = [
        Self::Sops,
        Self::Kubeconfig,
        Self::Ssh,
        Self::Gpg,
        Self::Gh,
        Self::Git,
        Self::Claude,
        Self::Codex,
    ];

    /// The stable machine-readable name of this class.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Sops => "sops",
            Self::Kubeconfig => "kubeconfig",
            Self::Ssh => "ssh",
            Self::Gpg => "gpg",
            Self::Gh => "gh",
            Self::Git => "git",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

impl fmt::Display for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// The result of one probe.
///
/// `Skip` records that a probe could not be attempted. It never *satisfies* a
/// class: see [`class_outcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The probe ran and proved what it set out to prove.
    Pass,
    /// The probe ran and did not prove it, or could not be attempted at all.
    Fail,
    /// The probe was not attempted; carries no proof either way.
    Skip,
}

impl Outcome {
    /// The lowercase wire name used in the JSON receipt.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skip => "skip",
        }
    }

    /// The uppercase column value used in the operator table.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One probe and the evidence it produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The credential class this probe contributes to.
    pub class: Class,
    /// The probe's stable name within its class.
    pub probe: String,
    /// What the probe concluded.
    pub outcome: Outcome,
    /// Exit statuses, counts and operator-known identifiers only.
    pub evidence: String,
}

/// Probe-level totals plus the receipt-level verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Probes that passed.
    pub pass: usize,
    /// Probes that failed.
    pub fail: usize,
    /// Probes that were skipped.
    pub skip: usize,
    /// [`Outcome::Pass`] only when every class in [`Class::ALL`] is satisfied.
    pub verdict: Outcome,
}

/// A complete receipt: metadata plus the probe set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// Wall-clock second the receipt was generated.
    pub generated_at_unix: u64,
    /// The host the probes ran on.
    pub host: String,
    /// The agent build that produced the receipt.
    pub agent_revision: String,
    /// Every probe, in class order.
    pub probes: Vec<Probe>,
}

/// Knobs the operator can turn without changing the probe set.
#[derive(Debug, Clone)]
pub struct Options {
    /// Home directory the per-user probes read from.
    pub home: PathBuf,
    /// A committed sops fixture to decrypt. Absent means the sops probes skip.
    pub sops_fixture: Option<PathBuf>,
    /// Override for `SOPS_AGE_KEY_FILE`.
    pub sops_key_file: Option<PathBuf>,
    /// Wall-clock cap for tools without their own timeout flag.
    pub probe_timeout: Duration,
    /// Most SSH aliases probed in one run.
    pub ssh_alias_cap: usize,
    /// Aliases this run must not connect to at all.
    ///
    /// An excluded alias still appears in the receipt, as a `Skip` carrying
    /// `reason=excluded`: a host held out of a run is evidence too, and the
    /// class roll-up must not be able to launder it into a pass.
    pub ssh_exclude: BTreeSet<String>,
}

impl Options {
    /// Build options from the ambient environment.
    #[must_use]
    pub fn from_environment() -> Self {
        Self {
            home: std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from),
            sops_fixture: None,
            sops_key_file: std::env::var_os("SOPS_AGE_KEY_FILE").map(PathBuf::from),
            probe_timeout: Duration::from_secs(20),
            ssh_alias_cap: 32,
            ssh_exclude: BTreeSet::new(),
        }
    }

    fn key_file(&self) -> PathBuf {
        self.sops_key_file.clone().unwrap_or_else(|| {
            self.home
                .join(".config")
                .join("sops")
                .join("age")
                .join("keys.txt")
        })
    }
}

// ---------------------------------------------------------------------------
// roll-up
// ---------------------------------------------------------------------------

/// Roll a class up from its probes.
///
/// A class is satisfied only by a `Pass`. A class with no probes, or whose
/// probes are all `Skip`, has produced no proof and therefore fails; a class
/// with any `Fail` fails outright.
#[must_use]
pub fn class_outcome(probes: &[Probe], class: Class) -> Outcome {
    let mut seen = false;
    let mut passed = false;
    for probe in probes.iter().filter(|probe| probe.class == class) {
        seen = true;
        match probe.outcome {
            Outcome::Fail => return Outcome::Fail,
            Outcome::Pass => passed = true,
            Outcome::Skip => {}
        }
    }
    if seen && passed {
        Outcome::Pass
    } else {
        Outcome::Fail
    }
}

/// Count probe outcomes and decide the receipt verdict.
#[must_use]
pub fn summarize(probes: &[Probe]) -> Summary {
    let count = |want: Outcome| probes.iter().filter(|p| p.outcome == want).count();
    let satisfied = Class::ALL
        .iter()
        .all(|class| class_outcome(probes, *class) == Outcome::Pass);
    Summary {
        pass: count(Outcome::Pass),
        fail: count(Outcome::Fail),
        skip: count(Outcome::Skip),
        verdict: if satisfied {
            Outcome::Pass
        } else {
            Outcome::Fail
        },
    }
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

/// Render the operator table: one row per probe, then the roll-ups.
#[must_use]
pub fn render_table(probes: &[Probe]) -> String {
    use fmt::Write as _;

    let class_w = probes.iter().fold("CLASS".len(), |acc, probe| {
        acc.max(probe.class.code().len())
    });
    let probe_w = probes
        .iter()
        .fold("PROBE".len(), |acc, probe| acc.max(probe.probe.len()));

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<class_w$}  {:<probe_w$}  {:<7}  EVIDENCE",
        "CLASS", "PROBE", "OUTCOME",
    );
    for probe in probes {
        let _ = writeln!(
            out,
            "{:<class_w$}  {:<probe_w$}  {:<7}  {}",
            probe.class.code(),
            probe.probe,
            probe.outcome.label(),
            probe.evidence
        );
    }
    let classes = Class::ALL
        .iter()
        .map(|class| format!("{}={}", class.code(), class_outcome(probes, *class).label()))
        .collect::<Vec<_>>()
        .join(" ");
    let summary = summarize(probes);
    let _ = writeln!(out, "\nclasses  {classes}");
    let _ = writeln!(
        out,
        "summary  pass={} fail={} skip={} verdict={}",
        summary.pass, summary.fail, summary.skip, summary.verdict
    );
    out
}

/// Escape one string for a JSON document.
///
/// Hand-written on purpose: the agent's R34 dependency wall is closed, and a
/// receipt emitter is not a reason to open it.
#[must_use]
pub fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                use fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Render the `tcfs.bulkload.handoff.v1` receipt.
#[must_use]
pub fn render_json(receipt: &Receipt) -> String {
    use fmt::Write as _;

    let quoted = |value: &str| format!("\"{}\"", json_escape(value));
    let summary = summarize(&receipt.probes);
    let mut out = String::new();
    let _ = writeln!(out, "{{");
    let _ = writeln!(out, "  \"schema\": {},", quoted(SCHEMA));
    let _ = writeln!(
        out,
        "  \"generated_at_unix\": {},",
        receipt.generated_at_unix
    );
    let _ = writeln!(out, "  \"host\": {},", quoted(&receipt.host));
    let _ = writeln!(
        out,
        "  \"agent_revision\": {},",
        quoted(&receipt.agent_revision)
    );
    let _ = writeln!(
        out,
        "  \"summary\": {{\"pass\": {}, \"fail\": {}, \"skip\": {}, \"verdict\": {}}},",
        summary.pass,
        summary.fail,
        summary.skip,
        quoted(summary.verdict.code())
    );
    let _ = writeln!(out, "  \"probes\": [");
    let last = receipt.probes.len().saturating_sub(1);
    for (index, probe) in receipt.probes.iter().enumerate() {
        let comma = if index == last { "" } else { "," };
        let _ = writeln!(
            out,
            "    {{\"class\": {}, \"probe\": {}, \"outcome\": {}, \"evidence\": {}}}{comma}",
            quoted(probe.class.code()),
            quoted(&probe.probe),
            quoted(probe.outcome.code()),
            quoted(&probe.evidence)
        );
    }
    let _ = writeln!(out, "  ]");
    let _ = writeln!(out, "}}");
    out
}

// ---------------------------------------------------------------------------
// redaction and capture
// ---------------------------------------------------------------------------

/// True when `text` carries any token shape from [`SECRET_MARKERS`].
#[must_use]
pub fn looks_secret(text: &str) -> bool {
    SECRET_MARKERS.iter().any(|marker| text.contains(marker))
}

/// Turn a captured stderr stream into an evidence-safe one-line note.
///
/// Any secret-shaped token replaces the entire note with `<redacted>`; the
/// remainder is reduced to printable ASCII, whitespace-collapsed, and capped.
#[must_use]
pub fn sanitize_note(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    if looks_secret(&text) {
        return "<redacted>".to_owned();
    }
    let flattened: String = text
        .chars()
        .map(|c| if c.is_ascii_graphic() { c } else { ' ' })
        .collect();
    let joined = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > NOTE_CAP {
        let head: String = joined.chars().take(NOTE_CAP).collect();
        format!("{head}...")
    } else {
        joined
    }
}

/// Punctuation admitted alongside ASCII alphanumerics in an identifier.
const IDENTIFIER_PUNCT: &str = "._:/@=+-,'()[]#*";

/// True when `c` may appear in evidence.
fn identifier_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || IDENTIFIER_PUNCT.contains(c)
}

/// True when every character of `text` is drawn from the identifier charset.
fn identifier_shaped(text: &str) -> bool {
    text.chars().all(|c| identifier_char(c) || c == ' ')
}

/// Reduce one line to the identifier charset.
///
/// Anything outside the charset becomes a space and the result is
/// whitespace-collapsed, so a tab-separated `ls-remote` row and a check-marked
/// `gh auth status` line survive as their identifiers rather than being thrown
/// away whole. Nothing outside the charset can reach the output, and because
/// the charset keeps `_` and `-` intact, a secret-shaped token still arrives at
/// [`looks_secret`] unbroken.
fn identifier_normalize(line: &str) -> String {
    let mapped: String = line
        .chars()
        .map(|c| if identifier_char(c) { c } else { ' ' })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Keep only identifier-shaped, secret-free lines of a captured stream.
///
/// This is the single doorway through which a *name* (a kube context, an age
/// public key, a gh account) may leave a child process and enter evidence.
#[must_use]
pub fn identifier_lines(raw: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(raw)
        .lines()
        .map(identifier_normalize)
        .filter(|line| !line.is_empty() && line.chars().count() <= LINE_CAP && !looks_secret(line))
        .take(LINES_CAP)
        .collect()
}

/// What a probe is allowed to take out of a child's stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdoutPolicy {
    /// Byte length only; the bytes themselves are dropped.
    CountOnly,
    /// Byte length plus identifier-shaped lines.
    Identifiers,
}

/// One child process observation, already reduced to evidence-safe parts.
#[derive(Debug, Default, Clone)]
struct Capture {
    spawned: bool,
    timed_out: bool,
    code: Option<i32>,
    stdout_bytes: usize,
    stdout_lines: Vec<String>,
    stderr_lines: Vec<String>,
    note: String,
}

impl Capture {
    fn ok(&self) -> bool {
        self.spawned && !self.timed_out && self.code == Some(0)
    }

    /// The `exit=` evidence field: a status, or why there wasn't one.
    fn exit_field(&self) -> String {
        if !self.spawned {
            return "exit=tool-absent".to_owned();
        }
        if self.timed_out {
            return "exit=timeout".to_owned();
        }
        self.code
            .map_or_else(|| "exit=unknown".to_owned(), |code| format!("exit={code}"))
    }

    /// `exit=...` plus a sanitized stderr note when the probe did not pass.
    fn failure_evidence(&self) -> String {
        if self.note.is_empty() {
            self.exit_field()
        } else {
            format!("{} stderr={}", self.exit_field(), self.note)
        }
    }
}

/// Drain a child stream to end, retaining at most [`CAPTURE_CAP`] bytes.
fn drain<R: std::io::Read>(pipe: Option<R>) -> (Vec<u8>, usize) {
    let Some(mut pipe) = pipe else {
        return (Vec::new(), 0);
    };
    let mut kept: Vec<u8> = Vec::new();
    let mut total = 0_usize;
    let mut buffer = [0_u8; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                total = total.saturating_add(count);
                let room = CAPTURE_CAP.saturating_sub(kept.len());
                if room > 0 {
                    if let Some(slice) = buffer.get(..count.min(room)) {
                        kept.extend_from_slice(slice);
                    }
                }
            }
        }
    }
    (kept, total)
}

/// Run one child to completion under a wall-clock cap.
///
/// # Process discipline (R-N11)
///
/// On expiry the child is **detached**, never signalled: it is moved onto a
/// reaper thread whose only job is `wait`, so the agent leaves no zombie and
/// still sends nothing. A tool that supports its own timeout flag should use
/// that instead of relying on this cap.
fn run(command: &mut Command, policy: StdoutPolicy, timeout: Duration) -> Capture {
    let mut capture = Capture::default();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            capture.note = if err.kind() == std::io::ErrorKind::NotFound {
                "tool-absent".to_owned()
            } else {
                "spawn-refused".to_owned()
            };
            return capture;
        }
    };
    capture.spawned = true;

    let out_pipe = child.stdout.take();
    let err_pipe = child.stderr.take();
    let out_thread = std::thread::spawn(move || drain(out_pipe));
    let err_thread = std::thread::spawn(move || drain(err_pipe));

    let deadline = Instant::now().checked_add(timeout);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if deadline.is_some_and(|limit| Instant::now() >= limit) {
            capture.timed_out = true;
            break None;
        }
        std::thread::sleep(POLL);
    };

    let Some(status) = status else {
        // Detach: wait only, never signal.
        drop(std::thread::spawn(move || drop(child.wait())));
        return capture;
    };

    capture.code = status.code();
    let (stdout, stdout_bytes) = out_thread.join().unwrap_or_default();
    let (stderr, _) = err_thread.join().unwrap_or_default();
    capture.stdout_bytes = stdout_bytes;
    if policy == StdoutPolicy::Identifiers {
        capture.stdout_lines = identifier_lines(&stdout);
    }
    capture.stderr_lines = identifier_lines(&stderr);
    capture.note = sanitize_note(&stderr);
    capture
}

// ---------------------------------------------------------------------------
// scratch custody
// ---------------------------------------------------------------------------

/// A 0700 scratch directory in the state root, removed on drop.
///
/// Mirrors the `Exclusive` guard in `estate.rs`: custody of transient material
/// belongs to a value whose `Drop` ends it, not to a cleanup call site that a
/// `?` can skip.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.0));
    }
}

impl Scratch {
    fn create(state_root: &Path) -> Result<Self> {
        let dir = state_root.join(format!("tcfs-handoff-{}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        Ok(Self(dir))
    }

    fn write(&self, name: &str, payload: &[u8]) -> Result<PathBuf> {
        use std::io::Write as _;
        let path = self.0.join(name);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        file.write_all(payload)?;
        file.sync_all()?;
        Ok(path)
    }
}

// ---------------------------------------------------------------------------
// pure parsers
// ---------------------------------------------------------------------------

/// Extract concrete `Host` aliases from one `ssh_config` body.
///
/// Pattern aliases (`*`, `?`, `!`) are excluded: they name a rule, not a host,
/// and connecting to one is not a proof of anything. Order is preserved and
/// duplicates are dropped.
#[must_use]
pub fn parse_ssh_hosts(config: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut hosts = Vec::new();
    for line in config.lines() {
        let line = line.trim();
        let Some(rest) = line
            .strip_prefix("Host ")
            .or_else(|| line.strip_prefix("host "))
        else {
            continue;
        };
        for alias in rest.split_whitespace() {
            if alias.contains('*') || alias.contains('?') || alias.starts_with('!') {
                continue;
            }
            if !identifier_shaped(alias) || alias.chars().count() > 64 {
                continue;
            }
            if seen.insert(alias.to_owned()) {
                hosts.push(alias.to_owned());
            }
        }
    }
    hosts
}

/// Pull the first `age1...` token out of a line, if any.
fn age_token(line: &str) -> Option<String> {
    line.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|token| token.starts_with("age1") && token.chars().count() >= 16)
        .map(ToOwned::to_owned)
}

/// Public keys advertised by the `# public key:` comments in an age key file.
///
/// Only comment lines are read. Secret key material in the same file is never
/// parsed, stored, or reported.
#[must_use]
pub fn parse_public_keys(keys_file: &str) -> Vec<String> {
    keys_file
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('#') && line.contains("public key:"))
        .filter_map(age_token)
        .collect()
}

/// Age recipients declared in a sops file's `sops.age` block.
///
/// Matches both the YAML (`- recipient: age1...`) and JSON (`"recipient":
/// "age1..."`) spellings.
#[must_use]
pub fn parse_recipients(fixture: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    fixture
        .lines()
        .filter(|line| line.contains("recipient"))
        .filter_map(age_token)
        .filter(|key| seen.insert(key.clone()))
        .collect()
}

/// Reduce a URL to `host[:port]`, rejecting anything not host-shaped.
fn server_host(url: &str) -> Option<String> {
    let after_scheme = url.split("//").nth(1)?;
    let authority = after_scheme.split('/').next()?;
    let host = authority.rsplit('@').next()?;
    let ok = !host.is_empty()
        && host.chars().count() <= 64
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
    ok.then(|| host.to_owned())
}

/// The first `https://` control-plane host `kubectl cluster-info` reported.
fn cluster_server(lines: &[String]) -> String {
    lines
        .iter()
        .filter_map(|line| line.split_whitespace().find(|word| word.contains("//")))
        .find_map(server_host)
        .unwrap_or_else(|| "unknown".to_owned())
}

// ---------------------------------------------------------------------------
// probes
// ---------------------------------------------------------------------------

fn probe(class: Class, name: &str, outcome: Outcome, evidence: String) -> Probe {
    Probe {
        class,
        probe: name.to_owned(),
        outcome,
        evidence,
    }
}

const fn pass_or_fail(ok: bool) -> Outcome {
    if ok {
        Outcome::Pass
    } else {
        Outcome::Fail
    }
}

fn sops_probes(options: &Options) -> Vec<Probe> {
    let key_file = options.key_file();
    let Some(fixture) = options.sops_fixture.as_ref().filter(|path| path.is_file()) else {
        return vec![probe(
            Class::Sops,
            "sops-decrypt",
            Outcome::Skip,
            "reason=fixture-absent".to_owned(),
        )];
    };

    let mut command = Command::new("sops");
    command.arg("-d").arg(fixture);
    command.env("SOPS_AGE_KEY_FILE", &key_file);
    let capture = run(&mut command, StdoutPolicy::CountOnly, options.probe_timeout);
    let decrypt = if capture.ok() {
        probe(
            Class::Sops,
            "sops-decrypt",
            Outcome::Pass,
            format!("exit=0 plaintext_bytes={}", capture.stdout_bytes),
        )
    } else {
        probe(
            Class::Sops,
            "sops-decrypt",
            Outcome::Fail,
            capture.failure_evidence(),
        )
    };

    vec![decrypt, recipient_coverage(&key_file, fixture)]
}

fn recipient_coverage(key_file: &Path, fixture: &Path) -> Probe {
    let (Ok(keys), Ok(body)) = (fs::read_to_string(key_file), fs::read_to_string(fixture)) else {
        return probe(
            Class::Sops,
            "age-recipient-coverage",
            Outcome::Skip,
            "reason=inputs-unreadable".to_owned(),
        );
    };
    let identities = parse_public_keys(&keys);
    let recipients = parse_recipients(&body);
    let matched: Vec<String> = identities
        .iter()
        .filter(|key| recipients.contains(key))
        .cloned()
        .collect();
    probe(
        Class::Sops,
        "age-recipient-coverage",
        pass_or_fail(!matched.is_empty()),
        format!(
            "identities={} recipients={} matched=[{}]",
            identities.len(),
            recipients.len(),
            matched.join(",")
        ),
    )
}

fn kubeconfig_probes(options: &Options) -> Vec<Probe> {
    let mut command = Command::new("kubectl");
    command.args(["config", "get-contexts", "-o", "name"]);
    let capture = run(
        &mut command,
        StdoutPolicy::Identifiers,
        options.probe_timeout,
    );
    if !capture.ok() {
        return vec![probe(
            Class::Kubeconfig,
            "kubectl-contexts",
            Outcome::Fail,
            capture.failure_evidence(),
        )];
    }
    let contexts = capture.stdout_lines;
    let mut probes = vec![probe(
        Class::Kubeconfig,
        "kubectl-contexts",
        pass_or_fail(!contexts.is_empty()),
        format!("exit=0 contexts={}", contexts.len()),
    )];
    for context in contexts.iter().take(16) {
        probes.push(cluster_info(context, options.probe_timeout));
    }
    probes
}

fn cluster_info(context: &str, timeout: Duration) -> Probe {
    let mut command = Command::new("kubectl");
    command
        .arg("--context")
        .arg(context)
        .args(["cluster-info", "--request-timeout=5s"]);
    let capture = run(&mut command, StdoutPolicy::Identifiers, timeout);
    let name = format!("cluster-info:{context}");
    if capture.ok() {
        probe(
            Class::Kubeconfig,
            &name,
            Outcome::Pass,
            format!(
                "context={context} exit=0 server={}",
                cluster_server(&capture.stdout_lines)
            ),
        )
    } else {
        probe(
            Class::Kubeconfig,
            &name,
            Outcome::Fail,
            format!("context={context} {}", capture.failure_evidence()),
        )
    }
}

/// Read `~/.ssh/config` and every `~/.ssh/config.d/*` entry.
fn ssh_config_bodies(home: &Path) -> (usize, String) {
    let mut files = 0_usize;
    let mut body = String::new();
    let root = home.join(".ssh");
    if let Ok(text) = fs::read_to_string(root.join("config")) {
        files += 1;
        body.push_str(&text);
        body.push('\n');
    }
    let mut extra: Vec<PathBuf> = fs::read_dir(root.join("config.d"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    extra.sort();
    for path in extra {
        if let Ok(text) = fs::read_to_string(&path) {
            files += 1;
            body.push_str(&text);
            body.push('\n');
        }
    }
    (files, body)
}

fn ssh_probes(options: &Options) -> Vec<Probe> {
    let (files, body) = ssh_config_bodies(&options.home);
    let aliases = parse_ssh_hosts(&body);
    let mut probes = vec![probe(
        Class::Ssh,
        "ssh-config-hosts",
        pass_or_fail(!aliases.is_empty()),
        format!("files={files} aliases={}", aliases.len()),
    )];
    if aliases.len() > options.ssh_alias_cap {
        probes.push(probe(
            Class::Ssh,
            "ssh-alias-budget",
            Outcome::Skip,
            format!(
                "reason={} aliases={} cap={}",
                BulkloadRefusal::BudgetExceeded.code(),
                aliases.len(),
                options.ssh_alias_cap
            ),
        ));
    }
    for alias in aliases.iter().take(options.ssh_alias_cap) {
        if options.ssh_exclude.contains(alias) {
            probes.push(probe(
                Class::Ssh,
                &format!("ssh:{alias}"),
                Outcome::Skip,
                format!("alias={alias} reason=excluded"),
            ));
            continue;
        }
        let mut command = Command::new("ssh");
        command
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-T",
            ])
            .arg("--")
            .arg(alias)
            .arg("true");
        let capture = run(&mut command, StdoutPolicy::CountOnly, options.probe_timeout);
        let evidence = if capture.ok() {
            format!("alias={alias} exit=0")
        } else {
            format!("alias={alias} {}", capture.failure_evidence())
        };
        probes.push(probe(
            Class::Ssh,
            &format!("ssh:{alias}"),
            pass_or_fail(capture.ok()),
            evidence,
        ));
    }
    probes
}

/// The configured signing key id, when it is a portable identifier.
fn signing_key(timeout: Duration) -> Option<String> {
    let mut command = Command::new("git");
    command.args(["config", "--get", "user.signingkey"]);
    let capture = run(&mut command, StdoutPolicy::Identifiers, timeout);
    capture
        .ok()
        .then(|| capture.stdout_lines.first().cloned())
        .flatten()
        .filter(|key| {
            key.chars().count() <= 64
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '-' | '+'))
        })
}

/// Sign and verify a 32-byte scratch blob with the configured GPG key.
///
/// The blob is non-secret fixed text; the point of the probe is the key, not
/// the payload. Both the blob and its signature live 0600 inside the 0700
/// [`Scratch`] directory and are removed when that guard drops.
fn gpg_probes(scratch: &Scratch, options: &Options) -> Vec<Probe> {
    let Some(key) = signing_key(options.probe_timeout) else {
        return vec![probe(
            Class::Gpg,
            "gpg-detach-sign",
            Outcome::Skip,
            "reason=no-signingkey".to_owned(),
        )];
    };
    let payload = b"tcfs handoff verify scratch blob";
    let (Ok(blob), Ok(signature)) = (
        scratch.write("handoff.blob", payload),
        Ok::<PathBuf, BulkloadRefusal>(scratch.0.join("handoff.sig")),
    ) else {
        return vec![probe(
            Class::Gpg,
            "gpg-detach-sign",
            Outcome::Skip,
            "reason=scratch-unavailable".to_owned(),
        )];
    };

    let mut sign = Command::new("gpg");
    sign.args(["--batch", "--yes", "--local-user"])
        .arg(&key)
        .arg("--detach-sign")
        .arg("--output")
        .arg(&signature)
        .arg(&blob);
    let signed = run(&mut sign, StdoutPolicy::CountOnly, options.probe_timeout);
    let sign_probe = probe(
        Class::Gpg,
        "gpg-detach-sign",
        pass_or_fail(signed.ok()),
        if signed.ok() {
            format!("key={key} exit=0")
        } else {
            format!("key={key} {}", signed.failure_evidence())
        },
    );
    if !signed.ok() {
        return vec![sign_probe];
    }

    let mut verify = Command::new("gpg");
    verify
        .args(["--batch", "--verify"])
        .arg(&signature)
        .arg(&blob);
    let verified = run(&mut verify, StdoutPolicy::CountOnly, options.probe_timeout);
    vec![
        sign_probe,
        probe(
            Class::Gpg,
            "gpg-verify",
            pass_or_fail(verified.ok()),
            if verified.ok() {
                format!("key={key} exit=0")
            } else {
                format!("key={key} {}", verified.failure_evidence())
            },
        ),
    ]
}

/// The gh account login, from whichever stream `gh auth status` used.
fn gh_account(capture: &Capture) -> String {
    capture
        .stdout_lines
        .iter()
        .chain(capture.stderr_lines.iter())
        .find_map(|line| {
            let mut words = line.split_whitespace().skip_while(|w| *w != "account");
            words.next()?;
            words.next().map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Scope names from the `Token scopes:` line, which are not secrets.
fn gh_scopes(capture: &Capture) -> Vec<String> {
    capture
        .stdout_lines
        .iter()
        .chain(capture.stderr_lines.iter())
        .find(|line| line.contains("Token scopes:"))
        .map(|line| {
            line.split(':')
                .next_back()
                .unwrap_or_default()
                .split(',')
                .map(|scope| scope.trim().trim_matches('\'').to_owned())
                .filter(|scope| !scope.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn gh_probes(options: &Options) -> Vec<Probe> {
    let mut command = Command::new("gh");
    command.args(["auth", "status"]);
    let capture = run(
        &mut command,
        StdoutPolicy::Identifiers,
        options.probe_timeout,
    );
    if !capture.ok() {
        return vec![probe(
            Class::Gh,
            "gh-auth-status",
            Outcome::Fail,
            capture.failure_evidence(),
        )];
    }
    let scopes = gh_scopes(&capture);
    vec![probe(
        Class::Gh,
        "gh-auth-status",
        Outcome::Pass,
        format!(
            "exit=0 account={} scopes=[{}]",
            gh_account(&capture),
            scopes.join(",")
        ),
    )]
}

fn ls_remote(name: &str, remote: &str, timeout: Duration) -> Probe {
    let mut command = Command::new("git");
    command
        .args(["ls-remote", "--exit-code"])
        .arg(remote)
        .arg("HEAD")
        // Non-interactive by construction: a credential prompt is a refusal,
        // not a pause.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new",
        );
    let capture = run(&mut command, StdoutPolicy::Identifiers, timeout);
    probe(
        Class::Git,
        name,
        pass_or_fail(capture.ok()),
        if capture.ok() {
            format!("exit=0 refs={}", capture.stdout_lines.len())
        } else {
            capture.failure_evidence()
        },
    )
}

fn git_probes(options: &Options) -> Vec<Probe> {
    vec![
        ls_remote("git-ls-remote-ssh", GIT_SSH_REMOTE, options.probe_timeout),
        ls_remote(
            "git-ls-remote-https",
            GIT_HTTPS_REMOTE,
            options.probe_timeout,
        ),
    ]
}

fn claude_probes(options: &Options) -> Vec<Probe> {
    let mut command = Command::new("claude");
    command.args(["-p", "ok"]);
    let capture = run(&mut command, StdoutPolicy::CountOnly, options.probe_timeout);
    vec![probe(
        Class::Claude,
        "claude-prompt",
        pass_or_fail(capture.ok() && capture.stdout_bytes > 0),
        if capture.ok() {
            format!("exit=0 output_bytes={}", capture.stdout_bytes)
        } else {
            capture.failure_evidence()
        },
    )]
}

/// Custody of `~/.codex/auth.json`: mode and size only, never contents.
fn codex_auth(home: &Path) -> Probe {
    let path = home.join(".codex").join("auth.json");
    let Ok(meta) = fs::metadata(&path) else {
        return probe(
            Class::Codex,
            "codex-auth-file",
            Outcome::Fail,
            "reason=absent".to_owned(),
        );
    };
    let mode = meta.permissions().mode() & 0o777;
    let private = mode.trailing_zeros() >= 6;
    probe(
        Class::Codex,
        "codex-auth-file",
        pass_or_fail(private && meta.len() > 0),
        format!("mode={mode:04o} size={}", meta.len()),
    )
}

fn codex_probes(options: &Options) -> Vec<Probe> {
    let mut command = Command::new("codex");
    command.arg("--version");
    let capture = run(&mut command, StdoutPolicy::CountOnly, options.probe_timeout);
    vec![
        codex_auth(&options.home),
        probe(
            Class::Codex,
            "codex-version",
            pass_or_fail(capture.ok() && capture.stdout_bytes > 0),
            if capture.ok() {
                format!("exit=0 output_bytes={}", capture.stdout_bytes)
            } else {
                capture.failure_evidence()
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// entry points
// ---------------------------------------------------------------------------

/// Run every credential-class probe and return the evidence.
///
/// Probes are run in class order and never in parallel: a handoff receipt is
/// read by a human, and a stable order is worth more than a few seconds.
///
/// # Errors
///
/// Refuses with [`BulkloadRefusal::Io`] when the 0700 scratch directory under
/// `state_root` cannot be created. Probe *failures* are not refusals: they are
/// returned as [`Outcome::Fail`] rows so the receipt records them.
pub fn verify(state_root: &Path, options: &Options) -> Result<Vec<Probe>> {
    let scratch = Scratch::create(state_root)?;
    let mut probes = Vec::new();
    probes.extend(sops_probes(options));
    probes.extend(kubeconfig_probes(options));
    probes.extend(ssh_probes(options));
    probes.extend(gpg_probes(&scratch, options));
    probes.extend(gh_probes(options));
    probes.extend(git_probes(options));
    probes.extend(claude_probes(options));
    probes.extend(codex_probes(options));
    drop(scratch);
    Ok(probes)
}

/// This host's name, for the receipt header.
#[must_use]
pub fn hostname() -> String {
    let mut buffer = vec![0_u8; 256];
    // SAFETY: `buffer` is a live, writable allocation of exactly `len` bytes.
    let rc = unsafe { libc::gethostname(buffer.as_mut_ptr().cast::<libc::c_char>(), buffer.len()) };
    if rc != 0 {
        return "unknown".to_owned();
    }
    let end = buffer.iter().position(|byte| *byte == 0).unwrap_or(0);
    buffer.truncate(end);
    String::from_utf8(buffer).unwrap_or_else(|_| "unknown".to_owned())
}

/// Wrap a probe set in a receipt with this host's metadata.
#[must_use]
pub fn receipt(probes: Vec<Probe>) -> Receipt {
    Receipt {
        generated_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
        host: hostname(),
        agent_revision: env!("CARGO_PKG_VERSION").to_owned(),
        probes,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{
        class_outcome, identifier_lines, json_escape, parse_public_keys, parse_recipients,
        parse_ssh_hosts, render_json, render_table, sanitize_note, summarize, Class, Options,
        Outcome, Probe, Receipt,
    };

    fn probe(class: Class, name: &str, outcome: Outcome, evidence: &str) -> Probe {
        Probe {
            class,
            probe: name.to_owned(),
            outcome,
            evidence: evidence.to_owned(),
        }
    }

    /// A receipt is read by other programs: the escaping must be JSON, not
    /// "mostly JSON".
    #[test]
    fn json_escaping_covers_quotes_backslashes_and_controls() {
        assert_eq!(json_escape(r#"a"b"#), "a\\\"b");
        assert_eq!(json_escape(r"a\b"), "a\\\\b");
        assert_eq!(json_escape("a\nb\tc\rd"), "a\\nb\\tc\\rd");
        assert_eq!(json_escape("a\u{1}b"), "a\\u0001b");
        assert_eq!(json_escape("a\u{7f}b"), "a\\u007fb");
        assert_eq!(json_escape("a\u{8}\u{c}b"), "a\\b\\fb");
        assert_eq!(json_escape("plain"), "plain");
    }

    /// Every token shape in the wall must collapse the whole note, not merely
    /// the matched substring: a partially redacted secret is a leaked secret.
    #[test]
    fn every_secret_shape_redacts_the_whole_note() {
        for marker in [
            "ghp_",
            "gho_",
            "github_pat_",
            "AGE-SECRET-KEY-",
            "-----BEGIN",
            "sk-",
            "xoxb-",
            "Bearer ",
        ] {
            let raw = format!("error: token {marker}AAAABBBBCCCC rejected");
            assert_eq!(
                sanitize_note(raw.as_bytes()),
                "<redacted>",
                "marker {marker} did not redact"
            );
        }
        assert_eq!(sanitize_note(b"  plain   failure \n"), "plain failure");
        assert!(sanitize_note(&vec![b'x'; 4096]).ends_with("..."));
    }

    /// The identifier doorway is the only path from stdout to evidence; it must
    /// drop secret-bearing and non-identifier lines outright.
    #[test]
    fn identifier_lines_drop_secrets_and_normalize_noise() {
        let raw = b"docker-desktop\n- Token: gho_AAAABBBB\nrancher-desktop\nweird \xc2\xa7 line\n";
        assert_eq!(
            identifier_lines(raw),
            vec![
                "docker-desktop".to_owned(),
                "rancher-desktop".to_owned(),
                "weird line".to_owned()
            ]
        );
    }

    /// Regression from the first live receipt on `neo`: a tab-separated
    /// `ls-remote` row and a check-marked `gh auth status` line were dropped
    /// whole, so a passing probe reported `refs=0` and `account=unknown`.
    #[test]
    fn identifier_lines_keep_tab_and_glyph_bearing_rows() {
        let ls_remote = "0123456789abcdef0123456789abcdef01234567\tHEAD\n";
        assert_eq!(
            identifier_lines(ls_remote.as_bytes()),
            vec!["0123456789abcdef0123456789abcdef01234567 HEAD".to_owned()]
        );

        let status = "\u{2713} Logged in to github.com account someone (keyring)\n";
        assert_eq!(
            identifier_lines(status.as_bytes()),
            vec!["Logged in to github.com account someone (keyring)".to_owned()]
        );
    }

    /// The gh login must come out of whichever stream the CLI used, and only
    /// out of a line that cleared the doorway.
    #[test]
    fn gh_account_and_scopes_read_the_status_line() {
        let lines = identifier_lines(
            "\u{2713} Logged in to github.com account someone (keyring)\n\
             - Token: gho_AAAABBBBCCCC\n\
             - Token scopes: 'org', 'repo', 'workflow'\n"
                .as_bytes(),
        );
        let capture = super::Capture {
            stderr_lines: lines,
            ..super::Capture::default()
        };
        assert_eq!(super::gh_account(&capture), "someone");
        assert_eq!(
            super::gh_scopes(&capture),
            vec!["org".to_owned(), "repo".to_owned(), "workflow".to_owned()]
        );
        assert!(
            capture
                .stderr_lines
                .iter()
                .all(|line| !line.contains("gho_")),
            "the masked token line must never reach evidence"
        );
    }

    /// `Skip` records the absence of an attempt, never the presence of a
    /// credential.
    #[test]
    fn skip_never_satisfies_a_class() {
        let probes = vec![
            probe(Class::Sops, "sops-decrypt", Outcome::Skip, "reason=x"),
            probe(Class::Sops, "age-recipient-coverage", Outcome::Skip, ""),
        ];
        assert_eq!(class_outcome(&probes, Class::Sops), Outcome::Fail);
    }

    #[test]
    fn class_with_no_pass_fails_and_any_pass_without_fail_passes() {
        let missing: Vec<Probe> = Vec::new();
        assert_eq!(class_outcome(&missing, Class::Gh), Outcome::Fail);

        let mixed = vec![
            probe(Class::Ssh, "ssh:a", Outcome::Pass, ""),
            probe(Class::Ssh, "ssh:b", Outcome::Fail, ""),
        ];
        assert_eq!(class_outcome(&mixed, Class::Ssh), Outcome::Fail);

        let ok = vec![
            probe(Class::Ssh, "ssh:a", Outcome::Pass, ""),
            probe(Class::Ssh, "ssh-alias-budget", Outcome::Skip, ""),
        ];
        assert_eq!(class_outcome(&ok, Class::Ssh), Outcome::Pass);
    }

    /// The verdict is a class roll-up, not a probe tally: eight passing probes
    /// in one class still leave seven classes unproven.
    #[test]
    fn verdict_requires_every_class() {
        let probes = vec![probe(Class::Git, "git-ls-remote-ssh", Outcome::Pass, "")];
        let summary = summarize(&probes);
        assert_eq!(summary.pass, 1);
        assert_eq!(summary.verdict, Outcome::Fail);

        let full: Vec<Probe> = Class::ALL
            .iter()
            .map(|class| probe(*class, "p", Outcome::Pass, ""))
            .collect();
        assert_eq!(summarize(&full).verdict, Outcome::Pass);
    }

    #[test]
    fn ssh_host_parsing_excludes_patterns_and_keeps_order() {
        let config = "\
Host *
  ServerAliveInterval 30
Host neo honey
  User jess
Host !excluded
Host sting?
  Port 22
Host neo
Host build-01
";
        assert_eq!(
            parse_ssh_hosts(config),
            vec!["neo".to_owned(), "honey".to_owned(), "build-01".to_owned()]
        );
    }

    /// Beyond the cap the run emits a `BudgetExceeded` skip rather than
    /// quietly probing a subset.
    #[test]
    fn ssh_alias_budget_caps_at_thirty_two() {
        use std::fmt::Write as _;
        let config = (0..40).fold(String::new(), |mut acc, index| {
            let _ = writeln!(acc, "Host host-{index:02}");
            acc
        });
        let aliases = parse_ssh_hosts(&config);
        assert_eq!(aliases.len(), 40);
        let options = Options::from_environment();
        assert_eq!(options.ssh_alias_cap, 32);
        assert!(
            options.ssh_exclude.is_empty(),
            "exclusion is opt-in; a default hold-out would silently narrow the proof"
        );
        assert!(aliases.len() > options.ssh_alias_cap);
        assert_eq!(aliases.iter().take(options.ssh_alias_cap).count(), 32);
    }

    /// Only the `# public key:` comments are read; the secret lines beside them
    /// must not surface as identities.
    #[test]
    fn recipient_coverage_parses_public_keys_only() {
        let keys = "\
# created: 2026-09-20
# public key: age1exampleaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaq0
AGE-SECRET-KEY-1EXAMPLENOTAREALKEYAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
# public key: age1examplebbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbq0
AGE-SECRET-KEY-1EXAMPLENOTAREALKEYBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB
";
        let identities = parse_public_keys(keys);
        assert_eq!(
            identities,
            vec![
                "age1exampleaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaq0".to_owned(),
                "age1examplebbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbq0".to_owned(),
            ]
        );
        assert!(identities.iter().all(|key| key.starts_with("age1")));

        let fixture = "\
secret: ENC[AES256_GCM,data:xx,type:str]
sops:
    age:
        - recipient: age1exampleaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaq0
          enc: |
            -----BEGIN AGE ENCRYPTED FILE-----
        - recipient: age1examplecccccccccccccccccccccccccccccccccccccccccccccccq0
          enc: |
            -----BEGIN AGE ENCRYPTED FILE-----
";
        let recipients = parse_recipients(fixture);
        assert_eq!(recipients.len(), 2);
        let matched = identities
            .iter()
            .filter(|key| recipients.contains(key))
            .count();
        assert_eq!(matched, 1);
    }

    #[test]
    fn table_render_is_stable() {
        let probes = vec![
            probe(
                Class::Sops,
                "sops-decrypt",
                Outcome::Pass,
                "exit=0 plaintext_bytes=412",
            ),
            probe(
                Class::Ssh,
                "ssh:neo",
                Outcome::Fail,
                "alias=neo exit=255 stderr=<redacted>",
            ),
            probe(
                Class::Codex,
                "codex-version",
                Outcome::Skip,
                "reason=tool-absent",
            ),
        ];
        let expected = "\
CLASS  PROBE          OUTCOME  EVIDENCE
sops   sops-decrypt   PASS     exit=0 plaintext_bytes=412
ssh    ssh:neo        FAIL     alias=neo exit=255 stderr=<redacted>
codex  codex-version  SKIP     reason=tool-absent

classes  sops=PASS kubeconfig=FAIL ssh=FAIL gpg=FAIL gh=FAIL git=FAIL claude=FAIL codex=FAIL
summary  pass=1 fail=1 skip=1 verdict=FAIL
";
        assert_eq!(render_table(&probes), expected);
    }

    #[test]
    fn json_receipt_carries_the_schema_and_every_probe() {
        let receipt = Receipt {
            generated_at_unix: 1_758_326_400,
            host: "neo".to_owned(),
            agent_revision: "0.0.0".to_owned(),
            probes: vec![probe(
                Class::Gh,
                "gh-auth-status",
                Outcome::Pass,
                "exit=0 account=\"x\"",
            )],
        };
        let json = render_json(&receipt);
        assert!(json.contains("\"schema\": \"tcfs.bulkload.handoff.v1\""));
        assert!(json.contains("\"generated_at_unix\": 1758326400"));
        assert!(json.contains("\"host\": \"neo\""));
        assert!(json.contains(
            "\"summary\": {\"pass\": 1, \"fail\": 0, \"skip\": 0, \"verdict\": \"fail\"}"
        ));
        assert!(json.contains("\"evidence\": \"exit=0 account=\\\"x\\\"\""));
        assert!(json.trim_end().ends_with('}'));
    }

    /// Real-tool coverage. Ignored by default: the unit suite must not require
    /// sops, kubectl, gh, gpg or claude to be installed.
    #[test]
    #[ignore = "requires sops/kubectl/gh/gpg/claude on the host"]
    fn live_probe_set_covers_every_class() {
        let root = std::env::temp_dir();
        let probes = super::verify(&root, &Options::from_environment()).unwrap();
        for class in Class::ALL {
            assert!(
                probes.iter().any(|probe| probe.class == class),
                "class {class} produced no probe"
            );
        }
        print!("{}", render_table(&probes));
    }
}
