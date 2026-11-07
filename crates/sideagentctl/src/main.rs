//! sideagentctl — thin CLI over the daemon's D-Bus API. Doubles as the test
//! harness and the universal hotkey fallback (spec §6.4).

use std::collections::HashMap;

use agent_core::agent::{BUS_NAME, INTERFACE, OBJECT_PATH};
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
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
    #[zbus(name = "GetMessages")]
    async fn get_messages(&self, session_id: &str, tail: i32) -> zbus::Result<Vec<agent_core::store::Msg>>;
    #[zbus(name = "DeleteSession")]
    async fn delete_session(&self, session_id: &str) -> zbus::Result<()>;
    #[zbus(name = "RequestToggle")]
    async fn request_toggle(&self, source: &str) -> zbus::Result<()>;
    #[zbus(name = "Status")]
    async fn status(&self) -> zbus::Result<HashMap<String, String>>;

    #[zbus(signal, name = "CaptureAttached")]
    async fn capture_attached(&self, request_id: &str, width: u32, height: u32, bytes: i64) -> zbus::Result<()>;
    #[zbus(signal, name = "Delta")]
    async fn delta(&self, request_id: &str, text: &str) -> zbus::Result<()>;
    #[zbus(signal, name = "Done")]
    async fn done(&self, request_id: &str, usage: HashMap<String, String>) -> zbus::Result<()>;
    #[zbus(signal, name = "Failed")]
    async fn failed(&self, request_id: &str, kind: &str, message: &str) -> zbus::Result<()>;
    #[zbus(signal, name = "ToggleRequested")]
    async fn toggle_requested(&self, source: &str) -> zbus::Result<()>;
    #[zbus(signal, name = "SessionChanged")]
    async fn session_changed(&self, session_id: &str) -> zbus::Result<()>;
}

#[derive(Parser)]
#[command(name = "sideagentctl", about = "talk to sideagentd")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Ask the agent; streams the answer to stdout.
    Ask {
        text: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        provider: Option<String>,
        /// Attach a screenshot of the current screen (spec §7).
        #[arg(long = "screen")]
        attach_screen: bool,
        /// Attach an existing PNG instead of capturing (e.g. extension-captured).
        #[arg(long = "screenshot-file")]
        screenshot_file: Option<String>,
    },
    /// Create a new session and print its id.
    New {
        #[arg(default_value = "")]
        title: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// List sessions.
    Sessions,
    /// Show a session's messages.
    Show { session: String },
    /// Delete a session.
    Rm { session: String },
    /// Ask the daemon to emit ToggleRequested (hotkey floor).
    Toggle,
    /// Ask the daemon to emit ToggleRequested("selftest") — drives the
    /// extension's headless end-to-end hook.
    Selftest,
    /// Daemon status.
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let conn = zbus::Connection::session()
        .await
        .context("no session bus: is sideagentd running (systemctl --user status sideagentd)?")?;
    let proxy = Agent1Proxy::new(&conn).await.context(
        "cannot reach org.sideagent.Agent1: start the daemon (systemctl --user start sideagentd)",
    )?;

    match cli.cmd {
        Cmd::Ask { text, session, model, provider, attach_screen, screenshot_file } => {
            let session_id = match session {
                Some(s) => s,
                None => {
                    // fall back to most recent, else create one
                    let list = proxy.list_sessions().await?;
                    match list.first() {
                        Some(s) => s.id.clone(),
                        None => proxy.new_session("", "").await?,
                    }
                }
            };
            let mut opts = HashMap::new();
            if attach_screen {
                opts.insert("attach_screen".into(), "true".into());
            }
            if let Some(m) = model {
                opts.insert("model".into(), m);
            }
            if let Some(p) = provider {
                opts.insert("provider".into(), p);
            }
            if let Some(f) = screenshot_file {
                opts.insert("screenshot_path".into(), f);
            }

            // subscribe BEFORE Ask so no signal can slip past us
            let mut delta = proxy.receive_delta().await?;
            let mut done = proxy.receive_done().await?;
            let mut failed = proxy.receive_failed().await?;
            let mut capture = proxy.receive_capture_attached().await?;

            let request_id = proxy.ask(&session_id, &text, opts).await?;
            loop {
                tokio::select! {
                    ev = delta.next() => {
                        if let Some(d) = ev {
                            let body = d.args().unwrap();
                            let (ref rid, text) = (body.request_id().clone(), body.text().clone());
                            if **rid == *request_id {
                                print!("{text}");
                                use std::io::Write;
                                std::io::stdout().flush().ok();
                            }
                        }
                    }
                    ev = capture.next() => {
                        if let Some(d) = ev {
                            let body = d.args().unwrap();
                            let (ref rid, w, h, bytes) = (body.request_id().clone(), *body.width(), *body.height(), *body.bytes());
                            if **rid == *request_id {
                                eprintln!("[captured screen {w}x{h}, {} KB]", bytes / 1024);
                            }
                        }
                    }
                    ev = done.next() => {
                        if let Some(d) = ev {
                            let body = d.args().unwrap();
                            let (ref rid, u) = (body.request_id().clone(), body.usage().clone());
                            if **rid == *request_id {
                                println!();
                                eprintln!("[done in {} ms · in {} / out {} tok · {}]",
                                    u.get("ms").map(String::as_str).unwrap_or("?"),
                                    u.get("in").map(String::as_str).unwrap_or("?"),
                                    u.get("out").map(String::as_str).unwrap_or("?"),
                                    u.get("model").map(String::as_str).unwrap_or("?"));
                                break;
                            }
                        }
                    }
                    ev = failed.next() => {
                        if let Some(d) = ev {
                            let body = d.args().unwrap();
                            let (ref rid, kind, message) = (body.request_id().clone(), body.kind().clone(), body.message().clone());
                            if **rid == *request_id {
                                eprintln!("\n[{kind}: {message}]");
                                std::process::exit(1);
                            }
                        }
                    }
                }
            }
        }
        Cmd::New { title, model } => {
            let id = proxy.new_session(&title, model.as_deref().unwrap_or("")).await?;
            println!("{id}");
        }
        Cmd::Sessions => {
            for s in proxy.list_sessions().await? {
                println!("{}  {:>13}  {}", s.id, s.updated_at, if s.title.is_empty() { "(untitled)" } else { &s.title });
            }
        }
        Cmd::Show { session } => {
            for m in proxy.get_messages(&session, 500).await? {
                println!("{}: {}", m.role, m.text);
            }
        }
        Cmd::Rm { session } => proxy.delete_session(&session).await?,
        Cmd::Toggle => proxy.request_toggle("ctl").await?,
        Cmd::Selftest => proxy.request_toggle("selftest").await?,
        Cmd::Status => {
            let mut st = proxy.status().await?;
            for (k, v) in st.drain() {
                println!("{k} = {v}");
            }
        }
    }
    Ok(())
}
