# sideagent — Build Plan

Companion to [spec.md](spec.md). Milestones are independently demoable and
ordered so that the two riskiest abstractions — the provider core and the
capture pipeline — are proven from a terminal *before* any GUI exists,
and so the **portable** frontend (GTK) lands before the **platform-
specific** one (GNOME extension). The extension can never block the
critical path, and building the GTK app first forces the daemon protocol
to be real rather than extension-shaped.

Estimates are rough (±50%); the order is the contract.

---

## M0 — Talking to the world from a terminal (~1 weekend)

**Deliverable:** `sideagentd` (bare) + `sideagentctl ask "hello"` with
streaming output; sessions persist.

- Cargo workspace: `agent-core`, `sideagentd`, `sideagentctl`.
- `config.toml` loading (all §9.4 fields, validation, 0600 check).
- `Provider` trait + `ProviderEvent` enum (incl. `ToolCall` variant, unused).
- `openai-compat` adapter (reqwest + SSE) — covers OpenAI, OpenRouter,
  Ollama's `/v1`, LM Studio. Then `anthropic` adapter.
- SQLite session store (schema verbatim from §9.2).
- `sideagentctl ask` renders deltas to stdout; `sessions`/`new` subcommands.
- systemd user unit installed by a plain `install.sh`.

**Accept:** two consecutive `ask` calls in one session, restart the
daemon, session still there; provider switch mid-session via `--model`.

**Why first:** every later milestone assumes these types are right.
Cheap to fix now, expensive after two frontends exist.

## M1 — Your screen, as context (~1 weekend)

**Deliverable:** `sideagentctl ask --screen "what's this button?"`.

- `ScreenCapture` trait + backend autodetect/probe (spec §7).
- Backends, in build order: **gnome-shell** (dev box), **grim**, **x11
  (xcb)**. kwin and portal can slide to M5 if time-boxed.
- Image pipeline exactly per spec §7 (cap 1568, JPEG q85, originals on
  disk, content-addressed store).
- Context assembly implements the **last-2-images rule** with unit tests
  against a fake provider that snapshots the outbound request shape.

**Accept:** on GNOME Wayland, `--screen` round-trips in <250 ms capture-
to-sent and the model answers about visible on-screen content; outbound
request payload snapshot test is green.

## M2 — The GTK4 overlay: one protocol, first view (~1–2 weeks)

**Deliverable:** `ui-gtk` — the Tier B/C frontend, usable daily on KDE,
wlroots, and X11; degraded-but-working on GNOME-without-extension.

- GTK4 (rust `gtk4-rs`), zbus client; `RegisterFrontend` handshake.
- **layer-shell path:** `gtk4-layer-shell`, anchor right, exclusive_zone
  0, margin; runtime-switched to the plain keep-above/sticky path on X11
  and GNOME.
- Transcript rendering: md-subset → Pango markup (spec §3.2), streaming
  delta batches, stop button.
- Draft persistence across close/open; Esc + hotkey close; per-message
  attach toggle wired to `Ask{attach_screen:true}`.
- Session list (basic), model override combo.

**Accept:** daily-drivable on at least one wlroots compositor **and** one
X11 WM; focus-return behavior measured and written back into spec §13.2
(close the open question).

## M3 — Hotkeys everywhere (~1 week)

**Deliverable:** G1 satisfied on every matrix row except GNOME-with-
extension (M4).

- X11: daemon `XGrabKey` (+ chord-conflict detection → log + degrade to
  ctl mode, spec §6.2).
- Wayland: portal `GlobalShortcuts` request flow + activation →
  `ToggleRequested`.
- Per-DE docs: exact "bind `sideagentctl toggle`" steps (the guaranteed
  floor, spec §6.4).

**Accept:** one hotkey path works on Sway, Hyprland, KDE, and i3; on each,
the *documented* path is the one that works — no environment requires an
undocumented trick.

## M4 — GNOME Shell extension: Tier A (~1–2 weeks)

**Deliverable:** the native GNOME experience on the dev box.

- Extension JS: stage-actor panel (right edge, slide-in), `St.TextArea`,
  Pango-markup transcript (port of M2's converter), GSettings schema with
  keybindings + prefs page.
- D-Bus client in GJS (`Gio.make_proxy`), `RegisterFrontend kind=extension`
  — the daemon must suppress the GTK app when the extension is active
  (spec §8 `preferred_frontend` + last-wins).
- Pack/install: `gnome-extensions pack`, `--user-install`, upgrade story
  (disable/enable, no shell restart).

**Accept:** toggle <100 ms; attach-screenshot hotkey works; extension
disabled cleanly leaves zero residue; pinned `shell-version` list tested
on the versions actually available on the distros we target.
**Status:** extension JS live-verified headless on GNOME 50.6
(end-to-end: RegisterFrontend → in-shell capture → Ask with image →
streamed reply; `.dev/nested-test.sh`). Remaining: key press test in a
real session (needs one logout for GNOME to pick up the new extension).

## M5 — Fill the matrix, then polish (~open-ended)

- Remaining capture backends: **kwin**, **portal** (with the prompt-UX
  caveat surfaced in `Status`).
- History/UX: session titles, delete + `vacuum`, usage/token readout.
- `Status` diagnostics command: chosen capture backend, hotkey mode,
  provider health — half of all support requests, pre-built.
- Packaging pass: `justfile`, README install matrix per DE, nix flake or
  AUR if/when the name is settled (§13.1).

## Backlog (no dates, in rough order)

Region/window capture (slurp on wlroots, portal region on GNOME 46+);
Secret Service for keys; model-generated session titles; clipboard-image
attach (`wl-paste`/xclip); multi-monitor picker; tool registry +
confirm-UI (the `ToolCall` event's whole reason for existing); Ollama
image quirks (`/api/chat` native shape); macOS/Windows frontends.

---

## Ordering amendment (M1 completion, 2026-11-07 04:22)

M1 revealed that modern GNOME (verified on 50.6) denies `org.gnome.Shell.Screenshot`
to third-party apps — prompt-free capture on GNOME *requires* the Tier A
extension. M4 (extension) is therefore pulled **before** M2 (GTK app): the
protocol rationale that put M2 first ("prove the protocol with a portable
client") is already satisfied by `sideagentctl`, which exercises the full
D-Bus API including signals. M2 follows as planned afterwards.

## Test strategy

- **Unit:** context assembly (image policy!), pipeline (real PNG
  fixtures), config parsing, DB migrations.
- **Contract:** a fake provider (wiremock) asserting exact outbound
  payloads per adapter — the single highest-value test class here, since
  three "OpenAI-compatible" servers disagree in practice.
- **Protocol:** daemon speaks its D-Bus API against a headless client
  (rust) in CI — no GUI needed; this also guards M4's assumptions.
- **Manual matrix:** nested/compositor-per-VM checks for M2/M3
  (`cage` for wlroots, Xvfb for X11, `dbus-run-session gnome-shell`
  where feasible). Documented run-throughs, not automated — honest
  scope for a personal project.
- CI: `cargo fmt/clippy/test` + `gjs-check`/eslint for the extension.

## Risk register

| Risk | Impact | Mitigation |
|---|---|---|
| GNOME extension API churn (46→47→48) | M4 rework | Pin `shell-version`; extension is last GUI milestone; GTK Tier C is the guaranteed floor |
| Portal `GlobalShortcuts` backend gaps | M3 | ctl-toggle floor is first-class + documented; X11 grab unaffected |
| "OpenAI-compatible" servers aren't | provider bugs | contract tests vs OpenAI, Ollama, OpenRouter shapes |
| kwin D-Bus API drift (Plasma 5→6) | capture on KDE | version probe + spectacle CLI fallback (spec §7) |
| Focus-return quirks Tier C | feels broken | measured in M2, spec updated explicitly (§13.2) |
| Wayland positioning on GNOME w/o extension | Tier C looks bad | accepted limitation + documented helper extension |
| Image token cost in long sessions | bills/slow | last-2-images rule is a spec invariant with tests |
| Scope creep toward "agent" | never ships | `ToolCall` reserved in types; execution is backlog, not v1 |

## Definition of v1 "done"

All matrix rows in spec §5 have a working documented path; M0–M4
acceptance criteria green; `install.sh` + per-DE README get a stranger
from hotkey to answer-about-screenshot in under 10 minutes on GNOME,
KDE, and one wlroots compositor.
