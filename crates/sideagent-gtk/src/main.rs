// sideagent-gtk: Tier B frontend (spec §5) — a plain GTK4 window that talks
// to sideagentd over D-Bus. Works on any desktop without a shell extension;
// capture rides the daemon's backends (portal included). Multi-line composer,
// which the GNOME extension's St-18 cannot offer.
//
// Threading: GLib main loop owns the UI thread; a tokio runtime lives on a
// background thread for zbus. Events cross via glib::MainContext::channel;
// method calls are spawned onto the runtime handle.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use futures::StreamExt;
use gtk::glib;
use gtk::prelude::*;
use gtk::{self, pango};
use gtk::gdk;

// ---------------------------------------------------------------- D-Bus ---

#[zbus::proxy(
    interface = "org.sideagent.Agent1",
    default_service = "org.sideagent.Agent1",
    default_path = "/org/sideagent/Agent1"
)]
trait Agent1 {
    #[zbus(name = "Ask")]
    async fn ask(&self, session_id: &str, text: &str, opts: HashMap<String, String>) -> zbus::Result<String>;
    #[zbus(name = "NewSession")]
    async fn new_session(&self, title: &str, model: &str) -> zbus::Result<String>;
    #[zbus(name = "ListSessions")]
    async fn list_sessions(&self) -> zbus::Result<Vec<agent_core::store::SessionInfo>>;
    #[zbus(name = "GetMessages")]
    async fn get_messages(&self, session_id: &str, tail: i32) -> zbus::Result<Vec<agent_core::store::Msg>>;
    #[zbus(name = "RegisterFrontend")]
    async fn register_frontend(&self, kind: &str) -> zbus::Result<bool>;
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
}

enum UiEvent {
    Ready(Box<Result<Agent1Proxy<'static>, String>>),
    SessionInit { session: String, msgs: Vec<(String, String)>, model: String },
    AskErr(String),
    Delta { rid: String, text: String },
    Done { rid: String, usage: HashMap<String, String> },
    Failed { rid: String, kind: String, message: String },
    Attached { w: u32, h: u32 },
}

// ------------------------------------------------------------- markdown ---

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Minimal markdown -> Pango markup: `code`, **bold**, *italic*.
fn md_to_pango(src: &str) -> String {
    let mut out = String::new();
    for (i, seg) in src.split('`').enumerate() {
        if i % 2 == 1 {
            out.push_str(&format!("<tt background=\"#1f2430\" foreground=\"#7ee787\">{}</tt>", esc(seg)));
        } else {
            let mut s = esc(seg);
            // bold
            let mut parts: Vec<String> = Vec::new();
            let mut rest = s.as_str();
            while let Some(p) = rest.find("**") {
                let (head, tail) = rest.split_at(p);
                parts.push(head.to_string());
                rest = &tail[2..];
                if let Some(p2) = rest.find("**") {
                    let (b, tail2) = rest.split_at(p2);
                    parts.push(format!("<b>{b}</b>"));
                    rest = tail2;
                }
            }
            parts.push(rest.to_string());
            s = parts.concat();
            // italic (single *) — crude but v1
            let mut parts2: Vec<String> = Vec::new();
            let mut rest = s.as_str();
            while let Some(p) = rest.find('*') {
                let (head, tail) = rest.split_at(p);
                parts2.push(head.to_string());
                rest = &tail[1..];
                if let Some(p2) = rest.find('*') {
                    let (b, tail2) = rest.split_at(p2);
                    parts2.push(format!("<i>{b}</i>"));
                    rest = tail2;
                }
            }
            parts2.push(rest.to_string());
            out.push_str(&parts2.concat());
        }
    }
    out
}

// ------------------------------------------------------------------ css ---

const CSS: &str = "
window { background-color: #0d1117; }
headerbar {
  background-color: #0d1117;
  background-image: none;
  color: #e6edf3;
  box-shadow: none;
  border-bottom: 1px solid #21262d;
  padding: 4px 10px;
  min-height: 40px;
}
headerbar button, headerbar button.image-button {
  background: none;
  background-color: transparent;
  border-color: transparent;
}
headerbar button:hover { background-color: #21262d; }
.model { color: #8b949e; font-size: 11px; }
.title-dot { font-size: 15px; }
.transcript { margin: 6px 10px; }
.msg {
  color: #c9d1d9;
  font-family: 'Source Code Pro', 'Adwaita Mono', monospace;
  font-size: 13px;
}
.msg-user { color: #7ee787; }
.msg-system { color: #8b949e; font-style: italic; }
.status { color: #8b949e; font-size: 11px; padding: 2px 12px 4px 12px; }
.inputrow {
  background-color: #161b22;
  border: 1px solid #30363d;
  border-radius: 10px;
  margin: 8px 12px 12px 12px;
  padding: 6px 8px;
}
.inputrow:focus-within { border-color: #58a6ff; }
textview { background-color: transparent; font-family: 'Source Code Pro', 'Adwaita Mono', monospace; font-size: 13px; }
.inputrow textview text { background-color: #161b22; color: #e6edf3; caret-color: #58a6ff; }
button {
  background: none;
  background-color: #21262d;
  color: #c9d1d9;
  border: 1px solid #30363d;
  border-radius: 8px;
  margin: 0 3px;
  padding: 4px 12px;
  min-height: 22px;
  font-size: 13px;
}
button:hover { background-color: #30363d; border-color: #8b949e; }
button:checked { background-color: #1f6feb; color: #ffffff; border-color: #58a6ff; }
";

// ----------------------------------------------------------------- state --

struct State {
    proxy: Option<Agent1Proxy<'static>>,
    session: String,
    current_req: String,
    stream_buf: String,
    stream_label: Option<gtk::Label>,
    tx: tokio::sync::mpsc::UnboundedSender<UiEvent>,
    rt: tokio::runtime::Handle,
    rid_seq: u64,
    // widgets
    transcript: gtk::Box,
    scrolled: gtk::ScrolledWindow,
    status: gtk::Label,
    model_chip: gtk::Label,
    input: gtk::TextView,
    attach: gtk::ToggleButton,
}

impl State {
    fn set_status(&self, s: &str) {
        self.status.set_text(s);
    }

    fn scroll_bottom(&self) {
        let adj = self.scrolled.vadjustment();
        adj.set_value((adj.upper() - adj.page_size()).max(0.0));
    }

    fn add_msg(&self, role: &str, text: &str) -> gtk::Label {
        let label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .build();
        label.add_css_class("msg");
        if role == "user" { label.add_css_class("msg-user"); }
        if role == "system" { label.add_css_class("msg-system"); }
        let who = match role {
            "user" => "<span foreground=\"#7ee787\">❯ you</span>",
            "assistant" => "<span foreground=\"#58a6ff\">❯ sideagent</span>",
            _ => "<span foreground=\"#8b949e\">·</span>",
        };
        label.set_markup(&format!("{who}\n{}", md_to_pango(text)));
        self.transcript.append(&label);
        self.scroll_bottom();
        label
    }

    fn send(&mut self) {
        let Some(proxy) = self.proxy.clone() else {
            self.set_status("daemon not connected yet");
            return;
        };
        let buf = self.input.buffer();
        let text = buf.text(&buf.start_iter(), &buf.end_iter(), false)
            .trim().to_string();
        if text.is_empty() || !self.current_req.is_empty() {
            return;
        }
        buf.set_text("");
        self.input.grab_focus();

        self.rid_seq += 1;
        let rid = format!("gtk-{}-{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(), self.rid_seq);

        let mut opts: HashMap<String, String> = HashMap::new();
        opts.insert("request_id".into(), rid.clone());
        if self.attach.is_active() {
            opts.insert("screen".into(), "1".into());
            self.set_status("capturing screen… (portal may prompt)");
        } else {
            self.set_status("thinking…");
        }

        self.add_msg("user", &text);
        self.stream_buf.clear();
        self.stream_label = Some(self.add_msg("assistant", "…"));
        self.current_req = rid.clone();

        let tx = self.tx.clone();
        let session = self.session.clone();
        self.rt.spawn(async move {
            let r = proxy.ask(&session, &text, opts).await;
            match r {
                Ok(_) => {}
                Err(e) => { let _ = tx.send(UiEvent::AskErr(e.to_string())); }
            }
        });
    }

    fn boot_session(&self) {
        let Some(proxy) = self.proxy.clone() else { return };
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let sessions = proxy.list_sessions().await.unwrap_or_default();
            let (session, msgs) = if let Some(first) = sessions.first() {
                let msgs = proxy.get_messages(&first.id, 30).await.unwrap_or_default();
                (first.id.clone(), msgs.into_iter().map(|m| (m.role, m.text)).collect::<Vec<_>>())
            } else {
                let id = proxy.new_session("", "").await.unwrap_or_default();
                (id, Vec::new())
            };
            let st = proxy.status().await.unwrap_or_default();
            let model = st.get("provider").cloned().unwrap_or_default();
            let _ = tx.send(UiEvent::SessionInit { session, msgs, model });
        });
    }
}

// ------------------------------------------------------------------- ui ---

fn build_ui(app: &gtk::Application, rt: tokio::runtime::Handle) {
    let win = gtk::ApplicationWindow::new(app);
    win.set_title(Some("sideagent"));
    win.set_default_size(440, 720);

    let panel = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    panel.add_css_class("panel");

    // headerbar: CSD title bar, dark-styled, with the live controls
    let header = gtk::HeaderBar::new();
    let title = gtk::Label::new(None);
    title.add_css_class("title-dot");
    title.set_markup("<span foreground=\"#58a6ff\">●</span> <span foreground=\"#e6edf3\">sideagent</span>");
    let model_chip = gtk::Label::builder().build();
    model_chip.add_css_class("model");
    let hbox = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
    hbox.append(&title);
    hbox.append(&model_chip);
    header.set_title_widget(Some(&hbox));
    let attach = gtk::ToggleButton::builder().label("📎").build();
    attach.set_tooltip_text(Some("attach screenshot of the screen"));
    attach.set_valign(gtk::Align::Center);
    let new_btn = gtk::Button::builder().label("＋").build();
    new_btn.set_tooltip_text(Some("new session"));
    new_btn.set_valign(gtk::Align::Center);
    header.pack_end(&new_btn);
    header.pack_end(&attach);
    win.set_titlebar(Some(&header));

    // transcript
    let transcript = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical).spacing(10).build();
    transcript.add_css_class("transcript");
    let scrolled = gtk::ScrolledWindow::builder()
        .vexpand(true).child(&transcript).build();

    // status + input
    let status = gtk::Label::builder().xalign(0.0).build();
    status.add_css_class("status");
    let input = gtk::TextView::new();
    input.set_wrap_mode(gtk::WrapMode::WordChar);
    input.set_monospace(true);
    input.set_pixels_above_lines(4);
    input.set_pixels_below_lines(4);
    input.set_tooltip_text(Some("Enter send · Shift+Enter newline"));
    let inputrow = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).build();
    inputrow.add_css_class("inputrow");
    inputrow.append(&input);

    panel.append(&scrolled);
    panel.append(&status);
    panel.append(&inputrow);
    win.set_child(Some(&panel));

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<UiEvent>();
    let state = Rc::new(RefCell::new(State {
        proxy: None,
        session: String::new(),
        current_req: String::new(),
        stream_buf: String::new(),
        stream_label: None,
        tx,
        rt: rt.clone(),
        rid_seq: 0,
        transcript,
        scrolled,
        status: status.clone(),
        model_chip: model_chip.clone(),
        input: input.clone(),
        attach: attach.clone(),
    }));

    // event pump: drain the mpsc on a 40ms glib timer
    let rx = Rc::new(RefCell::new(rx));
    glib::timeout_add_local(std::time::Duration::from_millis(40), {
        let state = state.clone();
        let rx = rx.clone();
        move || {
            while let Ok(ev) = rx.borrow_mut().try_recv() {
            handle_event(&state, ev);
            }
            glib::ControlFlow::Continue
        }
    });
    fn handle_event(state: &Rc<RefCell<State>>, ev: UiEvent) {
        {
            let mut st = state.borrow_mut();
            match ev {
                UiEvent::Ready(res) => match *res {
                    Ok(p) => {
                        st.proxy = Some(p);
                        st.set_status("connected");
                        st.boot_session();
                    }
                    Err(e) => st.set_status(&format!("daemon unreachable: {e}")),
                },
                UiEvent::SessionInit { session, msgs, model } => {
                    st.session = session;
                    st.model_chip.set_text(&format!(" {}", model.split(':').nth(1).unwrap_or("?")));
                    for (role, text) in msgs {
                        st.add_msg(&role, &text);
                    }
                    st.set_status("ready · Enter send · Shift+Enter newline");
                }
                UiEvent::AskErr(e) => {
                    st.current_req.clear();
                    st.set_status(&format!("ask failed: {e}"));
                }
                UiEvent::Attached { w, h } => {
                    if st.current_req.is_empty() { return; }
                    st.set_status(&format!("screen attached ({w}×{h}) · thinking…"));
                }
                UiEvent::Delta { rid, text } => {
                    if rid != st.current_req { return; }
                    st.stream_buf.push_str(&text);
                    if let Some(lbl) = &st.stream_label {
                        lbl.set_markup(&format!(
                            "<span foreground=\"#58a6ff\">❯ sideagent</span>\n{}",
                            md_to_pango(&st.stream_buf)));
                        st.scroll_bottom();
                    }
                }
                UiEvent::Done { rid, usage } => {
                    if rid != st.current_req { return; }
                    st.current_req.clear();
                    st.stream_label = None;
                    st.stream_buf.clear();
                    let trunc = if usage.get("truncated").map(|v| v == "true").unwrap_or(false) {
                        " · ⚠ cut at token cap — raise provider.max_tokens"
                    } else { "" };
                    st.set_status(&format!("✓ {} ms · {}→{} tok · {}{trunc}",
                        usage.get("ms").cloned().unwrap_or_default(),
                        usage.get("in").cloned().unwrap_or_default(),
                        usage.get("out").cloned().unwrap_or_default(),
                        usage.get("model").cloned().unwrap_or_default()));
                }
                UiEvent::Failed { rid, kind, message } => {
                    if rid != st.current_req { return; }
                    st.current_req.clear();
                    st.stream_label = None;
                    st.set_status(&format!("{kind}: {message}"));
                }
            }
            let _ = ();
        }
    }

    new_btn.connect_clicked({
        let st = state.clone();
        move |_| {
            let b = st.borrow();
            let Some(proxy) = b.proxy.clone() else { return };
            let tx = b.tx.clone();
            b.rt.spawn(async move {
                if let Ok(id) = proxy.new_session("", "").await {
                    let _ = tx.send(UiEvent::SessionInit { session: id, msgs: vec![], model: String::new() });
                }
            });
        }
    });

    // Enter = send, Shift+Enter = newline
    input.add_controller({
        let st = state.clone();
        let win2 = win.clone();
        let kc = gtk::EventControllerKey::new();
        kc.connect_key_pressed(move |_, key, _code, mods| {
            if key == gdk::Key::Return || key == gdk::Key::KP_Enter {
                if mods.contains(gdk::ModifierType::SHIFT_MASK) {
                    glib::Propagation::Proceed // let the buffer insert the newline
                } else {
                    st.borrow_mut().send();
                    glib::Propagation::Stop
                }
            } else if key == gdk::Key::Escape {
                win2.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        kc
    });

    win.present();

    // bus bootstrap on the runtime thread
    let tx = state.borrow().tx.clone();
    rt.spawn(async move {
        let res = async {
            let conn = zbus::Connection::session().await
                .map_err(|e| e.to_string())?;
            let p = Agent1Proxy::new(&conn).await.map_err(|e| e.to_string())?;
            let _ = p.register_frontend("gtk").await;

            // signal pumps -> channel
            {
                let p = p.clone(); let tx = tx.clone();
                tokio::spawn(async move {
                    if let Ok(mut s) = p.receive_delta().await {
                        while let Some(d) = s.next().await {
                            if let Ok(b) = d.args() {
                                let (rid, text) = (b.request_id().to_string(), b.text().to_string());
                                let _ = tx.send(UiEvent::Delta { rid, text });
                            }
                        }
                    }
                });
            }
            {
                let p = p.clone(); let tx = tx.clone();
                tokio::spawn(async move {
                    if let Ok(mut s) = p.receive_done().await {
                        while let Some(d) = s.next().await {
                            if let Ok(b) = d.args() {
                                let _ = tx.send(UiEvent::Done {
                                    rid: b.request_id().to_string(),
                                    usage: b.usage().clone(),
                                });
                            }
                        }
                    }
                });
            }
            {
                let p = p.clone(); let tx = tx.clone();
                tokio::spawn(async move {
                    if let Ok(mut s) = p.receive_failed().await {
                        while let Some(d) = s.next().await {
                            if let Ok(b) = d.args() {
                                let _ = tx.send(UiEvent::Failed {
                                    rid: b.request_id().to_string(),
                                    kind: b.kind().to_string(),
                                    message: b.message().to_string(),
                                });
                            }
                        }
                    }
                });
            }
            {
                let p = p.clone(); let tx = tx.clone();
                tokio::spawn(async move {
                    if let Ok(mut s) = p.receive_capture_attached().await {
                        while let Some(d) = s.next().await {
                            if let Ok(b) = d.args() {
                                let _ = tx.send(UiEvent::Attached {
                                    w: *b.width(), h: *b.height(),
                                });
                            }
                        }
                    }
                });
            }
            Ok::<_, String>(p)
        }.await;
        let _ = tx.send(UiEvent::Ready(Box::new(res)));
    });
}

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;

    // CSS once the display exists (do it in startup)
    let app = gtk::Application::builder()
        .application_id("org.sideagent.Gtk")
        .build();
    app.connect_startup(|_| {
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_application_prefer_dark_theme(true);
        }
        let provider = gtk::CssProvider::new();
        provider.load_from_data(CSS);
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().expect("no display"),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
    app.connect_activate(move |app| build_ui(app, rt.handle().clone()));
    let _ = app.run();
    Ok(())
}
