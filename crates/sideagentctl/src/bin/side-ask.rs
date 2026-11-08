//! side-ask — quick questions straight from the terminal.
//!
//!   side-ask what was the command to list listening sockets again
//!   side-ask -e  explain ffmpeg concat while I'm at it
//!   side-ask -r  how do I clean up old docker images   # offers to RUN it (Y/n)
//!   git log --oneline -50 | side-ask "any of these look like the release commit?"
//!
//! Default style is *brief* (just the answer); `-e/--explain` asks for a
//! compact explanation + a memorable tip. Each run opens its own titled
//! session; `-c/--continue` reuses the previous side-ask session for
//! follow-ups. In a terminal, every suggested command comes with an
//! offer-to-execute prompt (Y/n) — the model never runs anything you don't
//! approve; `-R/--no-run` disables offers, `-r` stays as explicit opt-in.

use std::collections::HashMap;
use std::io::{IsTerminal, Read, Write};

use anyhow::{bail, Context, Result};
use clap::Parser;
use futures::StreamExt;
use zbus::proxy;

#[proxy(
    interface = "org.sideagent.Agent1",
    default_service = "org.sideagent.Agent1",
    default_path = "/org/sideagent/Agent1"
)]
trait Agent1 {
    #[zbus(name = "Ask")]
    async fn ask(&self, session_id: &str, text: &str, opts: HashMap<String, String>) -> zbus::Result<String>;
    #[zbus(name = "Cancel")]
    async fn cancel(&self, request_id: &str) -> zbus::Result<bool>;
    #[zbus(name = "NewSession")]
    async fn new_session(&self, title: &str, model: &str) -> zbus::Result<String>;
    #[zbus(name = "ListSessions")]
    async fn list_sessions(&self) -> zbus::Result<Vec<agent_core::store::SessionInfo>>;

    #[zbus(signal, name = "CaptureAttached")]
    async fn capture_attached(&self, request_id: &str, width: u32, height: u32, bytes: i64) -> zbus::Result<()>;
    #[zbus(signal, name = "Delta")]
    async fn delta(&self, request_id: &str, text: &str) -> zbus::Result<()>;
    #[zbus(signal, name = "Done")]
    async fn done(&self, request_id: &str, usage: HashMap<String, String>) -> zbus::Result<()>;
    #[zbus(signal, name = "Failed")]
    async fn failed(&self, request_id: &str, kind: &str, message: &str) -> zbus::Result<()>;
}

#[derive(Parser)]
#[command(name = "side-ask", about = "Ask sideagent from the terminal.", version)]
struct Cli {
    /// The question. Quote it, or just type the words — they get joined.
    /// Use "-" (or no args with piped stdin) to read the question from stdin.
    #[arg(num_args = 0.., value_name = "QUESTION")]
    text: Vec<String>,

    /// Teach mode: answer + short explanation + a tip to remember it.
    #[arg(short = 'e', long)]
    explain: bool,

    /// Continue the last side-ask conversation (keeps context for follow-ups).
    #[arg(short = 'c', long)]
    cont: bool,

    /// Kept for explicitness: execution offers are already the default when
    /// side-ask runs in a terminal. (See --no-run.)
    #[arg(short = 'r', long)]
    run: bool,

    /// Never offer to execute suggested commands (copy-paste mode).
    #[arg(short = 'R', long, conflicts_with = "run")]
    no_run: bool,

    /// Attach a screenshot of the current screen as context.
    #[arg(long)]
    screen: bool,

    /// Override the model for this question.
    #[arg(long)]
    model: Option<String>,

    /// Hide the stats line on stderr.
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Don't tell the agent where the question was asked (cwd, OS, git branch).
    #[arg(long)]
    no_context: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let question = compose_question(&cli)?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("runtime")?;
    rt.block_on(run(cli, question))
}

fn compose_question(cli: &Cli) -> Result<String> {
    let mut q = cli.text.join(" ").trim().to_string();
    let stdin_piped = !std::io::stdin().is_terminal();
    let mut stdin_text = String::new();
    if stdin_piped {
        std::io::stdin().read_to_string(&mut stdin_text).ok();
        stdin_text = stdin_text.trim().to_string();
    }
    if q == "-" || (q.is_empty() && !stdin_text.is_empty()) {
        q = stdin_text;
    } else if stdin_piped && !stdin_text.is_empty() {
        q.push_str("\n\nPasted for context:\n");
        q.push_str(&stdin_text);
    }
    if q.trim().is_empty() {
        bail!("nothing to ask. e.g.: side-ask how do I undo the last git commit?\n(pipes work too: `some-cmd | side-ask ...`)");
    }
    Ok(q)
}

/// Per-request runtime context: makes "here" mean the terminal's cwd.
/// Sent as a system-note, never persisted into conversation history.
fn env_context() -> String {
    let mut lines = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        lines.push(format!("The user asked from an interactive shell whose working directory is: {}", cwd.display()));
    }
    // OS: distro pretty-name on Linux, std fallback elsewhere (Windows/mac).
    match std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|os| {
            os.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME=\"").and_then(|s| s.strip_suffix('"')))
                .map(str::to_owned)
        }) {
        Some(pretty) => lines.push(format!("OS: {pretty}")),
        None => lines.push(format!("OS: {} ({})", std::env::consts::OS, std::env::consts::ARCH)),
    }
    if let Some(shell) = detect_shell() {
        lines.push(format!("The user's live shell is: {shell} — reply with commands valid in THAT shell (PowerShell/cmd syntax on Windows, POSIX syntax on Unix)."));
    }
    if let Ok(cwd) = std::env::current_dir() {
        let out = std::process::Command::new("git")
            .args(["-C", &cwd.to_string_lossy(), "rev-parse", "--abbrev-ref", "HEAD"])
            .output();
        if let Ok(out) = out {
            if out.status.success() {
                let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
                lines.push(format!("Inside a git repository, branch: {branch}"));
            }
        }
    }
    lines.push("IMPORTANT: this is side-ask — any command you output in a code \
        fence is OFFERED to the user for immediate execution (they confirm each \
        one with Y/n, and failed runs can be sent back to you to diagnose). So if \
        the user asks you to do something, DO it by outputting the command — \
        never say you cannot run things.".into());
    lines.join("\n")
}

/// The shell actually running side-ask (not $SHELL, which is the login
/// shell). Unix: parent process name via /proc. Windows: env heuristics.
fn detect_shell() -> Option<String> {
    #[cfg(unix)]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let ppid = status
            .lines()
            .find_map(|l| l.strip_prefix("PPid:"))?
            .trim()
            .to_string();
        let comm = std::fs::read_to_string(format!("/proc/{ppid}/comm")).ok()?;
        let comm = comm.trim().to_string();
        // side-ask launched via a wrapper (sh -c, cargo run…) — fall back
        // to $SHELL unless the parent looks like a real shell.
        const SHELLS: [&str; 8] = ["bash", "zsh", "fish", "sh", "dash", "nu", "tcsh", "csh"];
        if SHELLS.contains(&comm.as_str()) {
            return Some(comm);
        }
        let login = std::env::var("SHELL").ok()?;
        return login.rsplit('/').next().map(str::to_owned);
    }
    #[cfg(windows)]
    {
        if std::env::var_os("PSModulePath").is_some() {
            return Some("powershell".into());
        }
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
        return comspec.rsplit(['\\', '/']).next().map(str::to_owned);
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

async fn run(cli: Cli, question: String) -> Result<()> {
    let conn = zbus::Connection::session().await
        .context("no session bus: is sideagentd running (systemctl --user status sideagentd)?")?;
    let proxy = Agent1Proxy::new(&conn).await.context(
        "cannot reach org.sideagent.Agent1: start the daemon (systemctl --user start sideagentd)",
    )?;

    // Session: fresh titled session per run; -c reuses the last side-ask one.
    let data_dir = agent_core::config::Config::data_dir()?;
    std::fs::create_dir_all(&data_dir).ok();
    let marker = data_dir.join("side-ask.session");

    let mut session_note = None;
    let session_id = if cli.cont {
        let saved = std::fs::read_to_string(&marker).unwrap_or_default().trim().to_string();
        let live = proxy.list_sessions().await?.into_iter().any(|s| s.id == saved);
        if live {
            saved
        } else {
            session_note = Some("no previous side-ask session — starting a new one");
            proxy.new_session(&session_title(&question), "").await?
        }
    } else {
        proxy.new_session(&session_title(&question), "").await?
    };
    std::fs::write(&marker, &session_id).ok();

    // Execution offers are ON by default in a terminal (always Y/n-gated);
    // --no-run opts out, and piped mode can't prompt so it's off there.
    let run_mode = std::io::stdin().is_terminal() && !cli.no_run;
    if cli.run && !run_mode {
        note("--run needs an interactive terminal; answering without it");
    }

    let style: String = if cli.explain { "explain".into() } else { "brief".into() };
    let mut question = question;
    let mut first_round = true;
    let mut diagnose_rounds = 0u32;

    // Subscribe BEFORE any Ask so no signal can slip past us. Streams live
    // across rounds; each round filters on its own request id.
    let mut delta = proxy.receive_delta().await?;
    let mut done = proxy.receive_done().await?;
    let mut failed = proxy.receive_failed().await?;
    let mut capture = proxy.receive_capture_attached().await?;

    let exit_code = loop {
        // opts per round: attach/screen/context only on the first round;
        // style every round (diagnosis should keep the same voice).
        let mut opts = HashMap::new();
        opts.insert("style".into(), style.clone());
        // Pre-declare the request id: fast providers can emit Done before the
        // Ask reply lands, so correlation must not depend on reply order.
        let rid = uuid::Uuid::new_v4().to_string();
        opts.insert("request_id".into(), rid.clone());
        if first_round {
            if cli.screen {
                opts.insert("attach_screen".into(), "true".into());
            }
        }
        if !cli.no_context {
            opts.insert("context".into(), env_context());
        }
        if let Some(m) = &cli.model {
            opts.insert("model".into(), m.clone());
        }
        first_round = false;

        proxy.ask(&session_id, &question, opts).await?;

        let tty = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        let mut renderer = Renderer::new(tty);
        let mut full = String::new();
        let code = loop {
            tokio::select! {
                ev = delta.next() => {
                    if let Some(d) = ev {
                        let body = d.args().unwrap();
                        if *body.request_id() == rid {
                            let t = body.text().as_ref();
                            full.push_str(t);
                            renderer.push(t);
                            std::io::stdout().flush().ok();
                        }
                    }
                }
                ev = capture.next() => {
                    if let Some(d) = ev {
                        let body = d.args().unwrap();
                        if *body.request_id() == rid {
                            note(&format!("attached screenshot {}x{} ({} KB)", body.width(), body.height(), body.bytes() / 1024));
                        }
                    }
                }
                ev = done.next() => {
                    if let Some(d) = ev {
                        let body = d.args().unwrap();
                        if *body.request_id() == rid {
                            renderer.finish();
                            let u = body.usage();
                            if !cli.quiet {
                                note(&format!(
                                    "{} · {} ms · {}/{} tok{} · session {} (side-ask -c to continue)",
                                    u.get("model").map(String::as_str).unwrap_or("?"),
                                    u.get("ms").map(String::as_str).unwrap_or("?"),
                                    u.get("in").map(String::as_str).unwrap_or("?"),
                                    u.get("out").map(String::as_str).unwrap_or("?"),
                                    if u.get("truncated").map(|v| v == "true").unwrap_or(false) {
                                        " · ⚠ TRUNCATED at cap — raise provider.max_tokens"
                                    } else { "" },
                                    short(&session_id),
                                ));
                            }
                            break 0;
                        }
                    }
                }
                ev = failed.next() => {
                    if let Some(d) = ev {
                        let body = d.args().unwrap();
                        if *body.request_id() == rid {
                            renderer.finish_nl();
                            eprintln!("error [{}]: {}", body.kind(), body.message());
                            break 1;
                        }
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    proxy.cancel(&rid).await.ok();
                    renderer.finish_nl();
                    note("cancelled (partial answer kept in the session)");
                    break 130;
                }
            }
        };
        if code != 0 {
            break code;
        }
        if !run_mode {
            break 0;
        }

        // Offer each fenced command block the agent suggested.
        let mut last_fail: Option<(String, i32, String)> = None;
        for block in extract_fenced(&full) {
            if !confirm_run(&block) {
                continue;
            }
            let (c, out) = run_cmd(&block);
            println!("{}", out.trim_end());
            note(&format!("exit {c}"));
            if c != 0 {
                last_fail = Some((block, c, out));
            }
        }

        match last_fail {
            Some((block, c, out)) if diagnose_rounds < 2 => {
                if !confirm(&format!(
                    "that command failed (exit {c}). want me to diagnose and fix it?"
                )) {
                    break 0;
                }
                diagnose_rounds += 1;
                question = format!(
                    "I ran the command you suggested:\n```\n{block}\n```\n\
                     It exited with code {c}. Its output (possibly clipped):\n```\n{}\n```\n\
                     Diagnose briefly and give the corrected command.",
                    clip(&out, 3000)
                );
            }
            _ => break 0,
        }
    };

    if let Some(n) = session_note {
        eprintln!("note: {n}");
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}


/// Fenced ``` blocks from the raw markdown answer, as executable units.
fn extract_fenced(md: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut cur: Option<String> = None;
    for line in md.lines() {
        if line.trim_start().starts_with("```") {
            match cur.take() {
                Some(b) => {
                    let b = b.trim_end();
                    if !b.trim().is_empty() {
                        blocks.push(b.to_string());
                    }
                }
                None => cur = Some(String::new()),
            }
        } else if let Some(b) = cur.as_mut() {
            b.push_str(line);
            b.push('\n');
        }
    }
    blocks
}

fn confirm(prompt: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    eprint!("{prompt} [Y/n] ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
        return false; // EOF: treat as no
    }
    let a = line.trim().to_lowercase();
    a.is_empty() || a == "y" || a == "yes"
}

fn confirm_run(block: &str) -> bool {
    println!();
    println!("⚡ agent suggests running:");
    for line in block.lines() {
        println!("   {line}");
    }
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    confirm(&format!("   in {cwd}?"))
}

/// Run a command block exactly where the user is typing, through the
/// platform's shell. Interactive commands (sudo password, editors…) are
/// not supported in v1.
fn run_cmd(block: &str) -> (i32, String) {
    #[cfg(windows)]
    let mut cmd = {
        // match the shell the answer was written for
        let mut c = if std::env::var_os("PSModulePath").is_some() {
            let mut c = std::process::Command::new("powershell");
            c.args(["-NoProfile", "-NonInteractive", "-Command", block]);
            c
        } else {
            let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
            let mut c = std::process::Command::new(comspec);
            c.arg("/C").arg(block);
            c
        };
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(block);
        c
    };
    let out = cmd.output();
    match out {
        Ok(o) => {
            let mut combined = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr).into_owned();
            if !err.trim().is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&err);
            }
            (o.status.code().unwrap_or(1), combined)
        }
        Err(e) => (127, format!("could not spawn command: {e}")),
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mid = max / 2;
    format!("{}\n… {} chars clipped …\n{}", &s[..mid], s.len() - 2 * mid, &s[s.len() - mid..])
}

fn session_title(question: &str) -> String {
    let t: String = question.chars().take(60).collect();
    let t: String = t.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    format!("side-ask: {t}")
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// One-line note on stderr, dim on terminals.
fn note(msg: &str) {
    if std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        eprintln!("\x1b[2m· {msg}\x1b[0m");
    } else {
        eprintln!("· {msg}");
    }
}

/// Line-buffered markdown-lite renderer: fences -> cyan blocks (fences
/// stripped), `inline code` -> cyan, **bold** -> bold, # headings -> bold.
/// Plain passthrough when stdout is not a TTY (pipes/scripts get raw text).
struct Renderer {
    tty: bool,
    in_code: bool,
    line: String,
}

impl Renderer {
    fn new(tty: bool) -> Self {
        Self { tty, in_code: false, line: String::new() }
    }

    fn push(&mut self, delta: &str) {
        for ch in delta.chars() {
            self.line.push(ch);
            if ch == '\n' {
                self.flush_line();
            }
        }
    }

    fn finish(&mut self) {
        if !self.line.is_empty() {
            self.flush_line();
        }
    }

    fn finish_nl(&mut self) {
        self.finish();
        println!();
    }

    fn flush_line(&mut self) {
        let raw = std::mem::take(&mut self.line);
        let trimmed = raw.trim_end();
        if trimmed.trim_start().starts_with("```") {
            self.in_code = !self.in_code;
            return;
        }
        if trimmed.is_empty() {
            println!();
            return;
        }
        if !self.tty {
            println!("{trimmed}");
            return;
        }
        // markdown heading -> bold
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if hashes > 0 && hashes < trimmed.len() && trimmed[hashes..].starts_with(' ') {
            println!("\x1b[1m{}\x1b[0m", trimmed[hashes..].trim_start());
            return;
        }
        let body = render_inline(trimmed);
        if self.in_code {
            println!("\x1b[36m{body}\x1b[0m");
        } else {
            println!("{body}");
        }
    }
}

/// Inline spans: `code` and **bold**. Byte-indexed but every split point is
/// a marker boundary (ASCII), so char boundaries hold.
fn render_inline(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    loop {
        let bt = rest.find('`');
        let bd = rest.find("**");
        let (at, marker) = match (bt, bd) {
            (None, None) => {
                out.push_str(rest);
                break;
            }
            (Some(a), None) => (a, "`"),
            (None, Some(b)) => (b, "**"),
            (Some(a), Some(b)) => if a < b { (a, "`") } else { (b, "**") },
        };
        out.push_str(&rest[..at]);
        let inner_start = at + marker.len();
        match rest[inner_start..].find(marker) {
            Some(end) => {
                let inner = &rest[inner_start..inner_start + end];
                let color = if marker == "`" { "36" } else { "1" };
                out.push_str(&format!("\x1b[{color}m{inner}\x1b[0m"));
                rest = &rest[inner_start + end + marker.len()..];
            }
            None => {
                // unmatched marker: literal, move past it
                out.push_str(&rest[at..inner_start]);
                rest = &rest[inner_start..];
            }
        }
    }
    out
}
