# aisle

**A quiet AI at the edge of your Linux screen.** One hotkey opens a chat
panel that floats beside what you're doing — ask questions, optionally
attach a screenshot of your screen as context, and let it run commands
for you *only when you say yes*. There's also a terminal voice:
`psst what was that tar flag again`.

*Renamed from `sideagent` (2026-11-08). `psst` is a second name for the
same CLI binary — `psst …` and `aisle …` are identical.*

## What you get

- **Global hotkey → floating side panel.** On GNOME: a shell extension
  draws a real overlay that never interferes with your windows. On any
  other desktop: a GTK4 window (WM decides where).
- **Optional screen context** — per-message opt-in; the panel shows a
  thumbnail of exactly what will be sent before you send it.
- **Terminal-first questions** — `aisle`/`psst` answer from your shell,
  aware of your cwd, OS, shell and git state; suggested commands come
  with a `Y/n` offer to actually run them, and if one fails it offers to
  diagnose and fix it.
- **Local-first** — one resident Rust daemon (`aisled`) holds all state;
  sessions live in a local SQLite DB; API key stays in a `chmod 600`
  config. No telemetry, no accounts.

## Architecture

```
┌──────────────┐  D-Bus (org.aisle.Agent1)  ┌────────────────────────┐
│ frontends    │ ─────────────────────────► │ aisled (resident)      │
│ • gnome ext  │   streaming deltas,        │ • agent-core (Rust)    │
│ • GTK4 app   │   signals, session ops     │   provider adapters:   │
│ • aisle/psst │                            │   OpenAI-compat,       │
│ • aislectl   │                            │   Anthropic            │
└──────────────┘                            │ • sessions (sqlite)    │
                                            │ • capture (portal/grim)│
                                            └────────────────────────┘
```

| Desktop | Panel | Hotkey | Capture |
|---|---|---|---|
| GNOME (50) | extension overlay, side-anchored | extension keybinding (`Super+Shift+A`) | `org.gnome.Shell.Screenshot` |
| Other Wayland | GTK4 window | `aislectl toggle` bound by you | xdg-desktop-portal (or `grim`) |
| X11 | GTK4 window | planned XGrabKey | `maim` |
| Headless / terminal only | `aisle` / `psst` | — | — |

## Setup

Requirements: Linux, a Rust toolchain (if `cargo` isn't on PATH,
`scripts/env.sh` bootstraps one into `./.toolchain`), and an
**image-capable model** if you want screen context (any
OpenAI-compatible endpoint — Ollama, LM Studio, vLLM, OpenRouter,
OpenAI — or Anthropic).

```bash
git clone https://github.com/dandandelion/aisle.git && cd aisle
./install.sh            # builds release, installs binaries, systemd
                        # user service (aisle.service), GNOME extension
mkdir -p ~/.config/aisle
cp config.example.toml ~/.config/aisle/config.toml
$EDITOR ~/.config/aisle/config.toml   # provider kind/model/base_url/api_key
chmod 600 ~/.config/aisle/config.toml
systemctl --user restart aisle
aislectl status         # provider = openai:<model>, capture_backends=…
```

**GNOME:** log out and back in (new extensions load at login), then
`gnome-extensions enable org.aisle`. `Super+Shift+A` toggles the panel,
`Super+Shift+S` attaches the current screen to your next message.
Tune the panel with `gsettings set org.gnome.shell.extensions.aisle
prompt-size 14` (also `prompt-y`, `panel-width`, the shortcut keys).

**Other desktops:** run `aisle-gtk`; bind any key to `aislectl toggle`.

## The terminal voice

```bash
psst how do I undo the last git commit       # brief: just the command
aisle -e how do I undo the last git commit   # teach: answer + why + a tip
aisle -c but what if i already pushed it     # follow-up, same session
psst --screen what's this error in my terminal
git log --oneline -50 | psst "which of these was the release?"
```

In a terminal, every fenced command the agent suggests comes with a
`Y/n` run offer (it never runs anything unprompted; `-R` opts out, `-r`
explicitly opts in). A failed command offers a diagnose-and-fix round.
More: `aisle --help`.

## Configuration

Single file, `~/.config/aisle/config.toml` — see
[config.example.toml](config.example.toml):

- `[provider]` — `kind`, `model`, `base_url`, `api_key`,
  `max_tokens` (output cap; long answers ending with a `⚠ TRUNCATED`
  note mean raise it), plus `[provider.named.*]` extras selectable with
  `--provider`.
- `[capture]` — screenshot backend preference.
- `[image]` — client-side downscale before upload.
- Env overrides: `AISLE_API_KEY`, `AISLE_CONFIG`, `AISLE_DATA`.

## Privacy & safety

- Screenshots are taken **only** when you opted in for that message,
  with a visible preview of what goes out.
- Agent-suggested commands require an explicit `Y/n`; nothing executes
  without you.
- Conversation history is a local SQLite file
  (`~/.local/share/aisle/sessions.db`). No telemetry, no crash reports,
  nothing leaves the machine except the provider request you triggered.

## Development

```bash
cargo test --workspace          # unit + wire-contract tests
.dev/nested-test.sh             # full nested GNOME session selftest
```

Design docs: [docs/spec.md](docs/spec.md), decision records in
[docs/decisions.md](docs/decisions.md). Current milestone: M2 shipped
(daemon, GNOME overlay, GTK4 app, CLI); M3 = portal-level global
shortcuts for the GTK tier. The Windows story is designed for in the
spec but not built.
