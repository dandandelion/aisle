//! Agent orchestrator: owns config, store, providers, capture backends, and
//! the D-Bus connection for signal emission. Frontends are pure views.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::StreamExt;
use zbus::Connection;

use crate::capture::{self, CapturedImage, ScreenCapture};
use crate::config::Config;
use crate::context;
use crate::error::{Error, Result};
use crate::image;
use crate::provider::{self, Provider, ProviderEvent, Usage};
use crate::store::{Msg, SessionInfo, Store};

pub const BUS_NAME: &str = "org.sideagent.Agent1";
pub const OBJECT_PATH: &str = "/org/sideagent/Agent1";
pub const INTERFACE: &str = "org.sideagent.Agent1";

const HISTORY_TAIL: usize = 200;
const DELTA_BATCH_INTERVAL: Duration = Duration::from_millis(50);
const DELTA_BATCH_BYTES: usize = 2048;

pub struct Agent {
    cfg: Config,
    config_error: Option<String>,
    store: Arc<Mutex<Store>>,
    provider: Option<Arc<dyn Provider>>,
    named: Mutex<HashMap<String, Arc<dyn Provider>>>,
    captures: Vec<Arc<dyn ScreenCapture>>,
    conn: OnceLock<Connection>,
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    frontend: Mutex<Option<String>>,
    data_dir: PathBuf,
    arc_slot: OnceLock<Arc<Agent>>,
}

impl Agent {
    fn make(
        cfg: Config,
        config_error: Option<String>,
        store: Store,
        provider: Option<Arc<dyn Provider>>,
        captures: Vec<Arc<dyn ScreenCapture>>,
        data_dir: PathBuf,
    ) -> Arc<Self> {
        let a = Arc::new(Self {
            store: Arc::new(Mutex::new(store)),
            config_error,
            provider,
            named: Mutex::new(HashMap::new()),
            captures,
            conn: OnceLock::new(),
            cancels: Mutex::new(HashMap::new()),
            frontend: Mutex::new(None),
            data_dir,
            arc_slot: OnceLock::new(),
            cfg,
        });
        a.arc_slot.set(a.clone()).ok();
        a
    }

    pub async fn new(cfg: Config, config_error: Option<String>) -> Result<Arc<Self>> {
        let data_dir = Config::data_dir()?;
        let store = Store::open(&data_dir.join("sessions.db"))?;
        // degraded mode: a broken/missing config must not stop the daemon;
        // Ask then fails in-band with an actionable message (spec §3.3).
        let provider = if config_error.is_some() || cfg.provider.api_key.is_empty() {
            None
        } else {
            Some(provider::build(&cfg.provider)?)
        };
        let captures = capture::resolve(&cfg.capture).await;
        Ok(Self::make(cfg, config_error, store, provider, captures, data_dir))
    }

    /// Test wiring: inject store + provider, no bus, no capture.
    pub fn for_test(cfg: Config, store: Store, provider: Arc<dyn Provider>) -> Arc<Self> {
        Self::make(cfg, None, store, Some(provider), Vec::new(), std::env::temp_dir().join("sideagent-test-data"))
    }

    pub fn set_conn(&self, conn: Connection) {
        let _ = self.conn.set(conn);
    }

    fn me(&self) -> Arc<Agent> {
        self.arc_slot.get().expect("arc set").clone()
    }

    async fn emit(&self, member: &str, body: &(impl zbus::zvariant::Type + serde::Serialize)) {
        let Ok(conn) = self.conn.get().ok_or_else(|| {
            tracing::debug!("emit {member} skipped: no bus");
            Error::Config("no bus".into())
        }) else {
            return;
        };
        if let Err(e) = conn.emit_signal(None::<&str>, OBJECT_PATH, INTERFACE, member, body).await {
            tracing::warn!("signal {member}: {e}");
        }
    }

    fn provider_for(&self, name: Option<&str>) -> Result<Arc<dyn Provider>> {
        let default = || -> Result<Arc<dyn Provider>> {
            self.provider.clone().ok_or_else(|| Error::Provider(format!(
                "no provider configured ({}); edit ~/.config/sideagent/config.toml",
                self.config_error.as_deref().unwrap_or("api_key empty")
            )))
        };
        let Some(name) = name else { return default() };
        let mut guard = self.named.lock().unwrap();
        if let Some(p) = guard.get(name) {
            return Ok(p.clone());
        }
        let section = self.cfg.provider.named.get(name).ok_or_else(|| {
            Error::Config(format!("unknown provider {name:?} (see config [provider.named.*])"))
        })?;
        let p = provider::build(&crate::config::ProviderSection {
            kind: section.kind.clone(),
            base_url: section.base_url.clone(),
            model: section.model.clone(),
            api_key: section.api_key.clone(),
            named: Default::default(),
        })?;
        guard.insert(name.into(), p.clone());
        Ok(p)
    }

    // ---- API surface (mirrors D-Bus methods; daemon-side service is a thin shim) ----

    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        self.store.lock().unwrap().list_sessions()
    }

    pub fn create_session(&self, title: &str, model: Option<&str>) -> Result<SessionInfo> {
        self.store.lock().unwrap().create_session(title, model)
    }

    pub fn get_messages(&self, session_id: &str, tail: usize) -> Result<Vec<Msg>> {
        self.store.lock().unwrap().get_messages(session_id, tail)
    }

    pub fn set_model(&self, session_id: &str, model: Option<&str>) -> Result<()> {
        self.store.lock().unwrap().set_model(session_id, model)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        self.store.lock().unwrap().delete_session(session_id)
    }

    pub fn register_frontend(&self, kind: &str) -> bool {
        let mut guard = self.frontend.lock().unwrap();
        let preferred = match self.cfg.ui.preferred_frontend.as_str() {
            "auto" | "" => None,
            other => Some(other.to_string()),
        };
        // config-preferred wins; otherwise first registration wins
        *guard = match preferred.as_deref() {
            Some(pref) => Some(pref.to_string()),
            None => guard.clone().or_else(|| Some(kind.to_string())),
        };
        let is_active = guard.as_deref() == Some(kind);
        tracing::info!(kind, active = ?*guard, "frontend registered");
        is_active
    }

    pub async fn request_toggle(&self, source: &str) {
        self.emit("ToggleRequested", &(source.to_string(),)).await;
    }

    pub fn cancel(&self, request_id: &str) -> bool {
        if let Some(flag) = self.cancels.lock().unwrap().get(request_id) {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub fn status(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("version".into(), env!("CARGO_PKG_VERSION").into());
        m.insert("provider".into(), match &self.provider {
            Some(p) => format!("{}:{}", p.name(), self.cfg.provider.model),
            None => format!("UNCONFIGURED ({})", self.config_error.as_deref().unwrap_or("api_key empty")),
        });
        m.insert(
            "capture_backends".into(),
            if self.captures.is_empty() {
                "none".into()
            } else {
                self.captures.iter().map(|b| b.name()).collect::<Vec<_>>().join(",")
            },
        );
        m.insert(
            "frontend".into(),
            self.frontend.lock().unwrap().clone().unwrap_or_else(|| "none".into()),
        );
        m
    }

    // ---- the ask flow ----

    /// Capture (if requested), persist the user turn, then stream the answer
    /// in a background task. Returns the request_id; progress via signals.
    pub async fn ask(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
        opts: &HashMap<String, String>,
    ) -> Result<String> {
        let session = self.store.lock().unwrap().get_session(session_id)?;
        let attach = opts.get("attach_screen").map(|v| v == "true").unwrap_or(false);

        let prepared = if let Some(path) = opts.get("screenshot_path") {
            // Tier A extension captures in-shell (GNOME ≥47 lockdown) and
            // hands us the PNG path.
            Some(self.ingest_screenshot(path).await?)
        } else if attach {
            Some(self.capture_and_store().await?)
        } else {
            None
        };

        let user_msg = {
            let mut store = self.store.lock().unwrap();
            let m = store.add_message(session_id, "user", text)?;
            if let Some(p) = &prepared {
                store.add_attachment(m.id, &p.original_path, p.width as i64, p.height as i64, p.bytes as i64)?;
            }
            m
        };

        // clients MAY pre-declare request_id in opts so signal correlation is
        // race-free (fast providers can emit Done before the Ask reply lands).
        let request_id = opts.get("request_id").cloned()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let flag = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(request_id.clone(), flag.clone());

        if let Some(p) = &prepared {
            self.emit("CaptureAttached", &(request_id.clone(), p.width, p.height, p.bytes as i64)).await;
        }

        tokio::spawn(run_answer(
            self.me(),
            session_id.to_string(),
            session.model,
            opts.get("model").cloned(),
            opts.get("provider").cloned(),
            opts.get("style").cloned(),
            request_id.clone(),
            flag,
        ));
        Ok(request_id)
    }

    async fn capture_and_store(&self) -> Result<PreparedAttachment> {
        if self.captures.is_empty() {
            return Err(capture::no_backend_error());
        }
        // try each probed backend in order; a probe can pass but the call
        // still fail (e.g. policy changes) — degrade to the next.
        let mut last_err = None;
        for backend in &self.captures {
            match backend.capture().await {
                Ok(cap) => return self.store_capture(cap).await,
                Err(e) => {
                    tracing::warn!("capture via {}: {e}", backend.name());
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(capture::no_backend_error))
    }

    async fn store_capture(&self, cap: crate::capture::CapturedImage) -> Result<PreparedAttachment> {
        let path = image::store_original(&self.data_dir, &cap.png)?;
        Ok(PreparedAttachment {
            original_path: path.to_string_lossy().into_owned(),
            width: cap.width,
            height: cap.height,
            bytes: cap.png.len(),
        })
    }

    async fn ingest_screenshot(&self, path: &str) -> Result<PreparedAttachment> {
        let fc = capture::file::FileCapture::new(path);
        let cap = ScreenCapture::capture(&fc).await?;
        let stored = image::store_original(&self.data_dir, &cap.png)?;
        Ok(PreparedAttachment {
            original_path: stored.to_string_lossy().into_owned(),
            width: cap.width,
            height: cap.height,
            bytes: cap.png.len(),
        })
    }
}

struct PreparedAttachment {
    original_path: String,
    width: u32,
    height: u32,
    bytes: usize,
}

async fn failed(agent: &Arc<Agent>, request_id: &str, kind: &str, msg: impl Into<String>) {
    agent.emit("Failed", &(request_id.to_string(), kind.to_string(), msg.into())).await;
}

/// Stream provider events, batch deltas, persist the assistant turn.
async fn run_answer(
    agent: Arc<Agent>,
    session_id: String,
    session_model: String,
    model_override: Option<String>,
    provider_name: Option<String>,
    style: Option<String>,
    request_id: String,
    cancel: Arc<AtomicBool>,
) {
    // History + attachment images (the last-2 policy lives in context.rs).
    // Note: finish locking *before* any await — guards must not cross points
    // where the task might yield.
    let (history_r, atts) = {
        let store = agent.store.lock().unwrap();
        (store.get_messages(&session_id, HISTORY_TAIL), store.attachments_for(&session_id).unwrap_or_default())
    };
    let (history, images) = {
        let history = match history_r {
            Ok(h) => h,
            Err(e) => return failed(&agent, &request_id, "internal", e.to_string()).await,
        };
        let idx: HashMap<i64, usize> = history.iter().enumerate().map(|(i, m)| (m.id, i)).collect();
        let mut imgs = Vec::new();
        for (msg_id, path, w, h, _b) in atts {
            let Some(&i) = idx.get(&msg_id) else { continue };
            let Ok(png) = std::fs::read(&path) else { continue };
            let cap = CapturedImage { png, width: w as u32, height: h as u32 };
            match image::prepare(&cap, &agent.cfg.image) {
                Ok(prepared) => imgs.push((i, prepared)),
                Err(e) => tracing::warn!("image prep for msg {msg_id}: {e}"),
            }
        }
        (history, imgs)
    };

    let model = model_override
        .or_else(|| (!session_model.is_empty()).then_some(session_model))
        .unwrap_or_else(|| agent.cfg.provider.model.clone());
    let provider = match agent.provider_for(provider_name.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            let kind = e.kind();
            return failed(&agent, &request_id, kind, e.to_string()).await;
        }
    };

    let req = context::build_request(&model, &history, &images, &agent.cfg.image, style.as_deref());
    let started = Instant::now();
    let mut stream = provider.stream(req);

    let mut full = String::new();
    let mut buf = String::new();
    let mut usage = Usage::default();
    let mut err: Option<(String, String)> = None;
    let mut last_flush = Instant::now();

    while let Some(ev) = stream.next().await {
        match ev {
            ProviderEvent::TextDelta(t) => {
                full.push_str(&t);
                buf.push_str(&t);
                if buf.len() >= DELTA_BATCH_BYTES || last_flush.elapsed() >= DELTA_BATCH_INTERVAL {
                    agent.emit("Delta", &(request_id.clone(), std::mem::take(&mut buf))).await;
                    last_flush = Instant::now();
                }
            }
            ProviderEvent::Done(u) => {
                usage = u;
                break;
            }
            ProviderEvent::Error { kind, message } => {
                err = Some((kind, message));
                break;
            }
            ProviderEvent::ToolCall { .. } => {} // reserved (ADR-0008)
        }
        if cancel.load(Ordering::Relaxed) {
            break; // early-stop: keep what streamed, like a natural end
        }
    }
    if !buf.is_empty() {
        agent.emit("Delta", &(request_id.clone(), buf)).await;
    }
    agent.cancels.lock().unwrap().remove(&request_id);

    if let Some((kind, msg)) = err {
        return failed(&agent, &request_id, &kind, msg).await;
    }

    let persist = {
        let mut store = agent.store.lock().unwrap();
        store.add_message(&session_id, "assistant", &full)
    };
    if let Err(e) = persist {
        return failed(&agent, &request_id, "internal", e.to_string()).await;
    }
    let mut u: HashMap<String, String> = HashMap::new();
    u.insert("in".into(), usage.input_tokens.to_string());
    u.insert("out".into(), usage.output_tokens.to_string());
    u.insert("ms".into(), started.elapsed().as_millis().to_string());
    u.insert("model".into(), model);
    agent.emit("Done", &(request_id, u)).await;
    agent.emit("SessionChanged", &(&session_id,)).await;
}
