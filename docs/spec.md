# sideagent — Specification

Version 0.1 · status: agreed in brainstorm, open questions listed at §13.
Decisions with rationale live in [decisions.md](decisions.md); this file
describes the *what*, plan.md the *when*.

---

## 1. Goals

- **G1 — Hotkey away.** A single global shortcut opens (and closes) the
  overlay from anywhere, on any Linux desktop, without disturbing the
  user's windows, focus model, or screen layout.
- **G2 — Chat with your screen.** Standard multimodal chat; the user can
  attach a current screenshot of the screen (full-output) as context,
  per-message, on explicit opt-in.
- **G3 — Minimal yet comprehensive.** Small codebase, few moving parts —
  but complete as a daily driver: persistent sessions, streaming, model
  switching, works across mainstream Linux desktops.
- **G4 — Portable core.** All desktop-specific code lives behind narrow
  seams so macOS/Windows frontends (and more capture backends) are
  additive later, not rewrites.

## 2. Non-goals (v1)

- Region/window-selective capture (v1 = whole active output).
- Voice input, OCR, screen *streaming* (continuous observation).
- Autonomous agents: v1 has no tool execution; the provider interface
  reserves a `ToolCall` event so it can arrive without protocol churn.
- Ships-as-packages polish: AUR/Nix/deb, autostart migrations, updater.
- Multi-monitor *selection* UX (v1: active output; `--output` flag later).
- Anything on non-Linux. The spec keeps the seams; it does not build them.

## 3. UX contract

The contract is environment-independent; §5 explains how each environment
delivers it.

### 3.1 Opening

- One configured hotkey toggles the overlay. Opening: fades/slides in at
  the **right edge**, ~420 px wide, full height minus a 12 px margin,
  above every window, on every workspace, with **no exclusive zone** (it
  floats over content; it does not reflow the desktop).
- The overlay takes keyboard focus **only for itself** — no window
  manager focus changes of the user's apps; when closed, focus returns
  to where it was (best effort; see §5.1).
- A second press of the hotkey, Esc, or clicking outside (configurable)
  closes it instantly. Closing is never lossy: the draft text survives.

### 3.2 Chat

- One scrollable transcript, one input area. `Enter` sends,
  `Shift+Enter` newline (rebindable in prefs).
- Answers stream token-by-token (batched, see §8). A stop/cancel button
  appears while generating.
- Markdown subset rendering: paragraphs, bold, inline code, fenced code
  blocks (monospace, selectable). No tables/images in transcript v1.
- Attach control: a per-message toggle (or second hotkey, default
  `Super+Shift+S`) adds the current screen screenshot to the outgoing
  message. The overlay shows a small thumbnail + "captured 1920×1080"
  line so the user always knows what is being sent.
- Sessions: conversations persist across daemon restarts. v1 shows a
  simple session list (title = first question, auto-generated); new
  session button; delete session.

### 3.3 Status & failure

- If `sideagentd` is unreachable, the overlay says so plainly and offers
  a one-line fix (`systemctl --user start sideagentd`). No silent failure.
- Provider errors surface in-transcript as a distinct message style.
- A tray-less indicator: the daemon writes a systemd journal line per
  capture (audit trail, see §10).

## 4. Architecture

```
sideagentd (resident daemon)
├── agent-core (library crate, portable, no Linux-only deps)
│   ├── providers/       OpenAI-compatible · Anthropic · (Ollama via compat)
│   ├── session store    SQLite + image files
│   ├── context assembly system prompt + history + image policy
│   ├── capture/         trait ScreenCapture + per-DE backends
│   └── image pipeline   decode → cap → re-encode → base64
├── hotkeys/             X11 grab · portal GlobalShortcuts · (extension self-binds)
├── transport            D-Bus session-bus service (Linux)
└── sideagentctl         CLI: ask · toggle · sessions · config
```

Frontends (separate artifacts, thin):

```
extension/   GNOME Shell extension (JS)        — Tier A on GNOME
ui-gtk/      GTK4 app                          — Tier B/C everywhere else
             · layer-shell via gtk4-layer-shell on Wayland compositors
               that support it (KDE, wlroots)
             · plain keep-above window on X11 and GNOME-without-extension
```

Rules:

- Frontends hold **no conversation state**. They are views over the
  daemon's protocol; any frontend can die and be restarted mid-session.
- The daemon holds **no view state**. It never knows about pixels,
  fonts, or the panel's geometry.
- Exactly one frontend is *active* at a time (§8.2 registration), so a
  user with both the extension and the GTK app installed doesn't get
  two panels.

## 5. Frontend tiers & environment matrix

Three tiers, picked per environment; the daemon is identical everywhere.

### Tier A — GNOME Shell extension (GNOME only)

> **Verified on GNOME 50.6 (headless nested shell, 2026-11-07).**
> **Reload rules (hard-won, corrected by build-marker forensics):** on
> GNOME 50 *nothing* short of logout changes extension CODE — disable/enable
> re-runs the cached module (verified: a new `enable()` log line never
> appeared after install+disable+enable). New *installs* also only appear
> at login (no directory rescan). Dev loop: iterate headless via
> `.dev/nested-test.sh` (fresh shell each run = fresh code), then one
> logout to apply to the live session. A `disable()` that throws leaves a
> permanent ERROR zombie.
> A `disable()` that throws mid-way leaves the extension in permanent
> ERROR state (unrecoverable without logout) — so `destroy()`/`disable()`
> must be paranoid (all API calls try/caught). Uninstall+reinstall does
> *not* hot-load a zombie back. Hard-won API notes:
> in-shell capture = `Shell.Screenshot` (`screenshot_stage_to_content` →
> `composite_to_stream`) — prompt-free, no D-Bus sender whitelist involved;
> the panel **fades out (60 ms) for the capture and back in (160 ms)** so the
> model receives the full desktop including content under the panel — context
> over blinklessness, per user preference;
> keybindings = `global.display.add_keybinding` with schema keys of **type
> `as`** (strv; plain `s` silently fails validation); `Shell.ActionMode.ALL`
> (0xffffffff) is rejected — compose from defined bits; `St.TextArea` and
> multi-line entries are gone from St-18 (v1 input = single-line `St.Entry`);
> `addChrome` takes no `trackAlpha`; `Gio.DBusConnection.call` async form is
> version-flaky — `call_sync` with explicit return-type slot is stable.
> Live e2e (register → capture → ask → stream → render) is scripted in
> `.dev/nested-test.sh` (runs a disposable headless shell; zero impact on
> the user session).

The overlay is a Clutter actor on the shell's stage, not a window:
guaranteed placement at the right edge, always above, no taskbar entry,
no WM interaction, animated slide-in. Keybindings are registered via the
extension's GSettings schema (`Keybindings.Action`, GNOME 45+ pattern).
This tier exists because GNOME Wayland offers clients neither
layer-shell nor window placement — anything else is a compromise there.

### Tier B — GTK4 + `gtk4-layer-shell` (KDE, wlroots)

The *standard* Wayland answer: wlr-layer-shell protocol (supported by
KDE Plasma 6 and all wlroots compositors). Anchor right, `margin` for
inset, `exclusive_zone = 0` so it floats over content, keyboard
interactivity `on-demand`. Placement, always-on-top, and all-workspaces
are protocol-guaranteed.

### Tier C — plain always-on-top window (X11 WMs; GNOME fallback)

GTK4 window with `_NET_WM_STATE_ABOVE` + `_NET_WM_STATE_STICKY` +
skip-taskbar/pager. On X11 the app can also position/size itself
directly, which gets very close to Tier B. On GNOME-without-extension
the compositor decides position (client positioning is forbidden on
Wayland); we accept that, and an optional one-file "placement helper"
extension is documented for users who care.

### Matrix (v1 support promise)

| Environment | Tier | Hotkey mechanism | Capture backend |
|---|---|---|---|
| GNOME Wayland | A (pref) / C | extension schema keybind / portal / ctl | `org.gnome.Shell.Screenshot` |
| GNOME X11 session | A / C | same | same |
| KDE Plasma 6 (Wayland or X11) | B | portal `GlobalShortcuts`, else user custom shortcut → `sideagentctl toggle` | `org.kde.kwin.Screenshot`, fallback `spectacle -b -n` |
| Sway / Hyprland / Wayfire | B | user keybind in compositor config (`bindsym`), portal also available | `grim` binary or wlr-screencopy client |
| i3 / bspwm / openbox / awesome (X11) | C | daemon `XGrabKey` on the configured chord | xcb root capture (`XGetImage`/composite) |
| Other Wayland (rootless, exotic) | C | `sideagentctl toggle` bound by user | portal (prompts) |

## 6. Hotkey subsystem

Ownership rules, so exactly one mechanism is live at a time:

1. **Extension present (GNOME):** the extension binds keys itself and
   opens directly. The daemon does *not* grab those chords.
2. **X11 sessions:** the daemon performs an `XGrabKey` on the configured
   chord (xcb). On X11 this is reliable and interference-free beyond the
   chord itself. If the grab fails (key owned by WM config), the daemon
   logs and degrades to ctl mode.
3. **Wayland (no extension):** the daemon requests the chord via the
   xdg-desktop-portal `GlobalShortcuts` API where the backend supports
   it (GNOME 42+, wlroots ≥1.7, KDE recent). The portal persists the
   binding in the DE; the daemon receives activation and emits
   `ToggleRequested` to the active frontend.
4. **Universal fallback:** `sideagentctl toggle` is always available for
   users to bind in their DE settings. Documented per-DE with exact
   steps; it is not a second-class citizen — it's the guaranteed floor.

`ToggleRequested` is a signal on the bus; frontends act on it. This is
also what makes "open the panel from a script/keyboard macro/tile rule"
a free feature.

## 7. Capture subsystem

```rust
trait ScreenCapture {
    fn capture(&self, output: OutputSel) -> Result<CapturedImage>; // PNG bytes
}
```

Backend selection order: config override → autodetect
(`XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE`, bus name presence) → first
backend that answers a cheap probe.

| Backend | How | Prompt? | Notes |
|---|---|---|---|
| gnome-shell | `org.gnome.Shell.Screenshot.Screenshot` → tmp PNG in `$XDG_RUNTIME_DIR/sideagent/` | **no — but restricted** | **GNOME ≥ ~47 restricts this API to the shell/portal itself (AccessDenied for third-party callers); verified on GNOME 50.6.** Backend kept for GNOME 40–46 and X11 sessions. On modern GNOME the prompt-free capture path is the Tier A extension (in-shell, writes the PNG and hands it to the daemon via `Ask{screenshot_path}`) or the portal (prompts). |
| kwin | `org.kde.kwin.Screenshot` (Plasma 6 interface; version-probe) | **no** | Fallback: `spectacle -b -n -o` CLI. |
| wlroots | spawn `grim -` (or wlr-screencopy client lib later) | **no** | grim is a hard dep on these DEs; document it. |
| X11 | xcb: root window + composite redirect via `XGetImage`/`xcomposite` | **no** | Unredirected fullscreen games may capture black — acceptable v1. |
| portal | `org.freedesktop.portal.Screenshot` | **yes, often every time** | Last resort. UX caveat documented in the picker. |

**Image pipeline** (all backends converge here):

1. Decode PNG → validate dimensions.
2. If long edge > **1568 px**, scale down preserving aspect
   (provider-side tokenization caps there anyway; Anthropic/OpenAI both
   stop paying attention above it).
3. Re-encode **JPEG q85** (PNG kept only if alpha present).
4. Reject > **4 MB** after encoding (should be unreachable post-scale).
5. Store original capture *on disk* (content-addressed, §9.3); attach
   only the processed bytes to the request.

## 8. Frontend protocol (D-Bus, v1 transport)

Service `org.sideagent.Agent1`, object `/org/sideagent/Agent1`.
Introspectable, standard bus policy (session bus = same user; fine).

### Methods

| Method | Signature (rough) | Notes |
|---|---|---|
| `Ask` | `ss{sb} → s` (session_id, text, opts{attach_screen,model…}) → request_id | returns immediately; progress via signals |
| `Cancel` | `s → b` | aborts the in-flight request (provider stream closed) |
| `RegisterFrontend` | `s → b` (kind: `extension`/`gtk`/`cli`) | claims the single active-frontend slot; last-wins, config `preferred_frontend` overrides |
| `NewSession` | `s → s` (title → session_id) | |
| `ListSessions` | `→ a(sstt)` | id, title, updated_at… |
| `GetMessages` | `s i → a(...)` | session, tail count — for re-rendering after frontend restart |
| `SetModel` | `ss → b` | per-session model override |
| `Status` | `→ sa{sv}` | version, capture backend chosen, provider health |

### Signals

| Signal | Payload | Notes |
|---|---|---|
| `ToggleRequested` | `s` (source hotkey id) | emitted by daemon on hotkey activation (§6) |
| `CaptureAttached` | `s iiis` (request, w, h, bytes) | so the UI can show the thumbnail/indicator |
| `Delta` | `s s` (request, text) | **batched ~50 ms / ~2 KB** — never one signal per token |
| `Done` | `s a{sv}` (request, usage incl. tokens, model, ms) | |
| `Failed` | `s s s` (request, kind, message) | kind: `provider`/`capture`/`internal` |
| `SessionChanged` | `s` | hint for the session list |

### Agent core interfaces (internal, Rust)

```rust
#[async_trait] trait Provider {
    fn stream(&self, req: ChatRequest) -> BoxStream<ProviderEvent>;
}
enum ProviderEvent { TextDelta(String), ToolCall(ToolCall), Done(Usage), Error(ProviderError) }
struct ChatRequest { model, messages: Vec<Message<ContentPart>>, .. }
enum ContentPart { Text(String), Image { mime, bytes, .. } }
```

`ToolCall` is defined but never emitted in v1 — deliberate forward
compatibility (§12).

## 9. Data model & config

### 9.1 Files

```
~/.config/sideagent/config.toml        (0600; holds api_key for v1)
~/.local/share/sideagent/sessions.db   SQLite (WAL)
~/.local/share/sideagent/images/       content-addressed PNGs
$XDG_RUNTIME_DIR/sideagent/            live captures (tmpfs, ephemeral)
```

### 9.2 SQLite schema (v1)

```sql
sessions(id TEXT PK, title TEXT, model TEXT, created_at, updated_at)
messages(id INTEGER PK, session_id → sessions, role CHECK(user|assistant|system),
         text TEXT, created_at)
attachments(id INTEGER PK, message_id → messages, kind='screenshot',
            path TEXT, width, height, bytes INTEGER)
```

No FTS in v1; session list is ordered by recency.

### 9.3 Image retention policy

- Captures are stored as originals under `images/` (sha256 name) and
  referenced from `attachments`; deleting a session deletes its rows
  and unreferenced images (a `sideagentctl vacuum` reclaims).
- **Context assembly keeps only the last 2 images** in the outbound
  request; older messages ship text-only. This is a spec rule, not a
  heuristic — it bounds cost and is what every screen-aware chat has to
  get right eventually.

### 9.4 config.toml (v1 fields)

```toml
[provider]        # kind = "openai"|"anthropic"; base_url, model, api_key
[capture]         # backend = "auto"|"gnome"|"kwin"|"grim"|"x11"|"portal"; output = "active"
[ui]              # preferred_frontend, panel_width = 420
[hotkey]          # toggle = "super+shift+a", attach = "super+shift+s"  (used by X11 grab / portal tiers)
[image]           # max_edge = 1568, jpeg_quality = 85
```

## 10. Privacy & security

- Screenshots are taken **only** when the user opted in for that message.
  The overlay visibly shows what will be sent (thumbnail + dimensions).
- No telemetry, no crash reporting, nothing leaves the machine except
  the configured provider call.
- Journal audit line per capture: `sideagentd: captured 1920x1080 via
  gnome-shell for session 7f3a…`.
- API key: v1 keeps it in `config.toml` (0600, user-owned) — documented
  clearly. Secret Service integration is a backlog item, not v1.
- Images never auto-upload elsewhere; retention is local and user-owned
  (`vacuum`, `rm -rf` both work).

## 11. Non-functional requirements

| | target |
|---|---|
| Daemon idle RSS | < 40 MB |
| Hotkey → overlay visible | < 100 ms local (daemon already resident) |
| Capture → attached | < 250 ms on gnome-shell/kwin/wlroots backends |
| First token | provider-bound, but the UI must render deltas as they arrive |
| Dependencies | no Electron, no browser engine, no Python; GTK stack only on Tier B/C |
| Portability | `agent-core` + `ui-gtk` compile on non-Linux *except* capture backends (gated by cfg/target); D-Bus transport is the only Linux-only seam (§12) |

## 12. Portability notes (explicitly future, not v1 work)

- The **protocol is the contract.** D-Bus is the Linux transport; the
  message shapes (Ask/Delta/Done/…) are transport-agnostic JSON-able
  types in `agent-core`. macOS/Windows frontends speak JSON-lines over a
  Unix socket / named pipe, or wrap the same types with their own IPC.
- `agent-core` has no Linux-only deps; `capture/` backends are
  `cfg(unix)`/platform-gated. Capture on macOS = ScreenCaptureKit,
  Windows = `Windows.Graphics.Capture` — new backend impls of the same
  trait.
- Frontend sketches for later: macOS menu-bar app (Swift, `NSPanel`
  floating right), Windows tray app (topmost tool window). Both are
  views over the identical daemon.

## 13. Open questions (tracked)

1. **Name.** "sideagent" is a working name; check for collisions on
   crates.io/Flathub before publishing.
2. **Focus return semantics** under Tier C — does the WM cooperate?
   Prototype in M2; worst case documented behavior is "focus stays in
   the panel until you Esc".
3. Click-outside-to-close: default on or off (accidental dismissal
   risk)? Spec says configurable, default **off**.
4. Session title generation: v1 = first user message truncated. Model-
   generated titles are backlog.
5. Plasma 6 kwin Screenshot interface stability across 6.x — probe at
   M5, may need the spectacle fallback promoted.
