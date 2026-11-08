# sideagent — Architecture Decision Records

Short records of the choices made during the 2026-11-07 brainstorm and
the "make it DE-agnostic" follow-up. Accepted means decided; revisit
only when a listed consequence actually bites.

---

## ADR-0001 — Resident daemon + thin frontends (Accepted)

**Context.** Hotkey-to-visible under 100 ms requires a pre-warmed
process; "Linux now, other OSes later" requires the OS-specific code to
be a small, isolated surface; and the project is meant to be *minimal*
yet cover several desktops.

**Decision.** All logic lives in `sideagentd`. Frontends (GNOME
extension, GTK app, CLI) are dumb views over a D-Bus protocol. Exactly
one frontend is registered active at a time.

**Consequences.** + Hotkey latency, session state independent of UI
lifetime, cheap new frontends (each future OS is a view, not a fork).
− Two processes, an IPC protocol to keep stable (versioned from day 1),
a registration/precedence concept to explain.

## ADR-0002 — Rust core, GTK4 for portable frontends (Accepted)

**Context.** Language options were Rust/TS/Go; UI options GTK/Tauri/
native-each.

**Decision.** Rust workspace; `gtk4-rs` for Tier B/C frontends.
`agent-core` is a library crate with no Linux-only dependencies
(capture backends gated).

**Consequences.** + Single static-ish binaries, small idle RSS, zbus/
gtk4-layer-shell/xcb all first-class in Rust; typed protocol structs.
− Steeper iteration loop than TS (acceptable: the protocol layer is
small, the logic is the product). GTK chosen over Tauri because a
browser engine in a "minimal" overlay contradicts the project's own
thesis; Tauri's cross-OS UI reuse is deferred to when a real macOS
frontend is started, where Swift/`NSPanel` is the better overlay anyway.

## ADR-0003 — Cloud + local providers behind one adapter (Accepted)

**Decision.** `openai-compat` adapter (covers OpenAI/OpenRouter/Ollama
`/v1`/LM Studio) plus a dedicated `anthropic` adapter.

**Consequences.** + Most of the market for free, local multimodal via
Ollama with no extra code. − "OpenAI-compatible" is a spectrum, not a
contract → contract tests per server in CI (plan.md).

## ADR-0004 — Frontend tiers: extension (GNOME) / layer-shell (KDE+wlroots) / plain window (X11 & fallbacks) (Accepted)

**Context.** The original design leaned GNOME-extension-first. Follow-up
requirement: the design must stand on *any* desktop, GNOME is one row
in a matrix, not the center.

**Decision.** Three overlay tiers (spec §5) with one protocol beneath
them. Tier choice is per-environment; the daemon is identical
everywhere. Build order puts the portable tier (GTK, M2) *before* the
GNOME extension (M4).

**Consequences.** + Every matrix row has an honest mechanism; no DE
becomes a special case in the core; GNOME's lack of layer-shell and
client positioning is contained in Tier A/C, not leaked into the daemon.
− Three overlay implementations to test; the extension's value is
experience on GNOME specifically, which the GTK Tier C path approximates
but does not match.

## ADR-0005 — Hotkey ownership is environment-specific, `ToggleRequested` is universal (Accepted)

**Decision.** Extension self-binds on GNOME; daemon `XGrabKey` on X11;
portal `GlobalShortcuts` on Wayland where available; `sideagentctl
toggle` documented as the guaranteed floor everywhere. All paths
converge on one signal.

**Consequences.** + No evdev/libinput grabs (they steal input globally —
exactly the "interference" we're avoiding). − Hotkey UX quality varies
by DE; documented per-DE setup is part of v1's definition of done.

## ADR-0006 — Compositor-native capture first; portal last (Accepted)

**Decision.** `org.gnome.Shell.Screenshot`, `org.kde.kwin.Screenshot`,
`grim`, xcb — all prompt-free — before xdg-desktop-portal Screenshot,
which prompts (often repeatedly) and breaks the one-keyboard-press UX.

**Consequences.** + Capture-to-attach <250 ms, no permission dialogs in
the hot flow. − Per-DE backend maintenance; portal path kept as the
universal-but-worse fallback.

## ADR-0007 — Screenshots: original stored locally, model gets downscaled JPEG; only last 2 images in context (Accepted)

**Decision.** Pipeline per spec §7; retention policy per §9.3 is a spec
invariant with tests, not a heuristic.

**Consequences.** + Bounded cost/latency in long sessions; originals
stay user-owned and deletable. − A model can't remember a screenshot
from turn 5 — by design; if that ever hurts, it's a retrieval feature,
not a policy relaxation.

## ADR-0008 — `ToolCall` in the event enum now, zero tool execution in v1 (Accepted)

**Decision.** The provider event enum reserves `ToolCall`; nothing emits
or handles it yet.

**Consequences.** + "Agent" later is additive; protocol doesn't churn.
− Temptation to build it early; plan.md keeps it in backlog on purpose.

## ADR-0009 — D-Bus is the v1 transport, protocol types are transport-agnostic (Accepted)

**Decision.** Linux v1 speaks D-Bus (session bus). `agent-core` types
are plain serde-able structs; cross-OS frontends will use JSON-lines
over Unix socket / named pipe.

**Consequences.** + GJS and Rust both have excellent D-Bus support —
the two v1 frontend languages need no custom glue. − One more
serialization boundary later; accepted cost of portability.

## ADR-0010 — Rename project: sideagent → aisle, `psst` as CLI alias (Accepted)

**Decision.** The project, binaries, and identifiers are renamed
`aisle` (daemon `aisled`, CLI `aisle`, control `aislectl`, D-Bus
`org.aisle.Agent1`, extension `org.aisle`). The binary is also
installed as `psst` — same executable, invoked name picked from
argv[0] — because "psst, what's the command for X" is exactly the
speech act of the tool.

**Why not `psst` as the real name.** "psst" is crowded in its own
niche: a 9.5k-star Rust Spotify client, a 242-star "AI-native secrets
manager", crates.io + PyPI squatters. "aisle" (free on crates.io,
nothing on GitHub/Linux searches) is the unique, searchable brand;
`psst` stays as the personality.

**Migration.** config/data dirs move to `~/.config/aisle` /
`~/.local/share/aisle`; loader honors old `sideagent` dirs and
`SIDEAGENT_*` env as a one-release fallback; install.sh moves state,
retires `sideagentd.service`, and swaps the extension UUID.
