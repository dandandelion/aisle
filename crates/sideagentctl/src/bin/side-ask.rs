//! side-ask — quick questions straight from the terminal.
//!
//!   side-ask what was the command to list listening sockets again
//!   side-ask -e  explain ffmpeg concat while I'm at it
//!   git log --oneline -50 | side-ask "any of these look like the release commit?"
//!
//! Default style is *brief* (just the answer); `-e/--explain` asks for a
//! compact explanation + a memorable tip. Each run opens its own titled
//! session; `-c/--continue` reuses the previous side-ask session for
//! follow-ups.

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

    /// Attach a screenshot of the current screen as context.
    #[arg(long)]
    screen: bool,

    /// Override the model for this question.
    #[arg(long)]
    model: Option<String>,

    /// Hide the stats line on stderr.
    #[arg(short = 'q', long)]
    quiet: bool,
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

    let mut opts = HashMap::new();
    opts.insert("style".into(), if cli.explain { "explain".into() } else { "brief".into() });
    // Pre-declare the request id: fast providers can emit Done before the
    // Ask reply lands, so correlation must not depend on reply order.
    let rid = uuid::Uuid::new_v4().to_string();
    opts.insert("request_id".into(), rid.clone());
    if cli.screen {
        opts.insert("attach_screen".into(), "true".into());
    }
    if let Some(m) = cli.model {
        opts.insert("model".into(), m);
    }

    // Subscribe BEFORE Ask so no signal can slip past us.
    let mut delta = proxy.receive_delta().await?;
    let mut done = proxy.receive_done().await?;
    let mut failed = proxy.receive_failed().await?;
    let mut capture = proxy.receive_capture_attached().await?;

    proxy.ask(&session_id, &question, opts).await?;

    let tty = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let mut renderer = Renderer::new(tty);
    let code = loop {
        tokio::select! {
            ev = delta.next() => {
                if let Some(d) = ev {
                    let body = d.args().unwrap();
                    if *body.request_id() == rid {
                        renderer.push(body.text().as_ref());
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
                                "{} · {} ms · {}/{} tok · session {} (side-ask -c to continue)",
                                u.get("model").map(String::as_str).unwrap_or("?"),
                                u.get("ms").map(String::as_str).unwrap_or("?"),
                                u.get("in").map(String::as_str).unwrap_or("?"),
                                u.get("out").map(String::as_str).unwrap_or("?"),
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
    if let Some(n) = session_note {
        eprintln!("note: {n}");
    }
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
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
