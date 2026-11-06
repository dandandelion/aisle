//! sideagentd — resident daemon: owns the Agent, serves the D-Bus API
//! (spec §8), and exits cleanly on SIGTERM/SIGINT.

use std::collections::HashMap;

use agent_core::agent::{Agent, BUS_NAME, OBJECT_PATH};
use agent_core::config::Config;
use anyhow::Context;
use zbus::conn::Builder;

/// Thin D-Bus shim over Agent's inherent methods (all orchestration lives
/// in agent-core so frontends/tests can drive it directly).
pub struct Agent1Service {
    agent: std::sync::Arc<Agent>,
}

#[zbus::interface(name = "org.sideagent.Agent1")]
impl Agent1Service {
    #[zbus(name = "Ask")]
    async fn ask(
        &self,
        session_id: &str,
        text: &str,
        opts: HashMap<String, String>,
    ) -> zbus::fdo::Result<String> {
        self.agent
            .ask(session_id, text, &opts)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    #[zbus(name = "Cancel")]
    async fn cancel(&self, request_id: &str) -> bool {
        self.agent.cancel(request_id)
    }

    #[zbus(name = "RegisterFrontend")]
    async fn register_frontend(&self, kind: &str) -> bool {
        self.agent.register_frontend(kind)
    }

    #[zbus(name = "RequestToggle")]
    async fn request_toggle(&self) {
        self.agent.request_toggle("ctl").await;
    }

    #[zbus(name = "NewSession")]
    async fn new_session(&self, title: &str, model: &str) -> zbus::fdo::Result<String> {
        let s = self
            .agent
            .create_session(title, (!model.is_empty()).then_some(model))
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(s.id)
    }

    #[zbus(name = "ListSessions")]
    async fn list_sessions(&self) -> zbus::fdo::Result<Vec<agent_core::store::SessionInfo>> {
        self.agent.list_sessions().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    #[zbus(name = "GetMessages")]
    async fn get_messages(
        &self,
        session_id: &str,
        tail: i32,
    ) -> zbus::fdo::Result<Vec<agent_core::store::Msg>> {
        self.agent
            .get_messages(session_id, tail.max(0) as usize)
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    #[zbus(name = "SetModel")]
    async fn set_model(&self, session_id: &str, model: &str) -> zbus::fdo::Result<()> {
        self.agent
            .set_model(session_id, (!model.is_empty()).then_some(model))
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    #[zbus(name = "DeleteSession")]
    async fn delete_session(&self, session_id: &str) -> zbus::fdo::Result<()> {
        self.agent
            .delete_session(session_id)
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    #[zbus(name = "Status")]
    async fn status(&self) -> HashMap<String, String> {
        self.agent.status()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cfg = Config::load().context("loading config.toml")?;
    let agent = Agent::new(cfg).await.context("starting agent core")?;

    let conn = Builder::session()?
        .name(BUS_NAME)
        .context("acquiring bus name (is another sideagentd running?)")?
        .serve_at(OBJECT_PATH, Agent1Service { agent: agent.clone() })?
        .build()
        .await?;
    agent.set_conn(conn.clone());

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        status = ?agent.status(),
        "sideagentd ready"
    );

    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT"),
        _ = wait_sigterm() => tracing::info!("SIGTERM"),
    }
    // conn drops here; name released
    Ok(())
}

async fn wait_sigterm() {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(_) => std::future::pending::<()>().await,
    }
}
