# sideagent (working name)

A minimal, hotkey-away AI side panel for Linux. One global hotkey opens a
floating overlay on the edge of your screen — it never takes over your
desktop, never steals your window layout, and disappears as fast as it
appeared. You chat with a multimodal model and, when you want, attach a
screenshot of your actual screen as context.

> Status: **M0 + M1 shipped and live-tested; M4 (GNOME extension)
> verified headless end-to-end, awaiting one logout to activate on the
> real session.** `sideagentd` runs as a systemd user service now
> (degraded until `~/.config/sideagent/config.toml` exists — run
> `sideagentctl status` to see the hint). Build order + acceptance
> criteria in [docs/plan.md](docs/plan.md); rationale in
> [docs/decisions.md](docs/decisions.md).

## The shape of it

```
┌──────────────────────────────┐
│  sideagentd (resident, Rust) │   all real logic, portable
│  chat sessions · providers   │
│  screen capture · image pipe │
│  D-Bus service               │
└──────────────┬───────────────┘
               │ D-Bus (session bus)
   ┌───────────┼───────────────┐
   ▼           ▼               ▼
GNOME Shell  GTK4 +        GTK4 window /
extension    layer-shell    X11 fallback
(GNOME)      (KDE, wlroots) (X11 WMs)
```

The daemon is the product. The overlay frontends are thin,
environment-specific shells — which is what makes "Linux only for now,
other OSes later" an honest statement rather than a hope.

## Environment support (v1 target)

| Environment | Overlay | Global hotkey | Screenshot |
|---|---|---|---|
| GNOME (Wayland/X11) | Shell extension (native stage overlay) | extension keybinding | in-shell capture (≥47 locks the D-Bus screenshot API from apps) |
| KDE Plasma (Wayland/X11) | GTK4 + `gtk4-layer-shell` | portal `GlobalShortcuts` / user shortcut | `org.kde.kwin.Screenshot` |
| wlroots — Sway, Hyprland, Wayfire | GTK4 + `gtk4-layer-shell` | compositor keybind / portal `GlobalShortcuts` | `grim` (wlr-screencopy) |
| X11 window managers — i3, bspwm, openbox, awesome | GTK4 X11 window (`keep-above` + sticky + positioned) | daemon `XGrabKey` (xcb) | xcb root-window capture |
| GNOME without the extension | GTK4 window, WM-decided position (helper extension optional) | portal / `sideagentctl toggle` | `org.gnome.Shell.Screenshot` |
| Anything else Wayland | GTK4 window | `sideagentctl toggle` bound by user | xdg-desktop-portal (prompts — last resort) |

## Try it (current state)

```bash
# daemon (already installed + enabled on this box):
sideagentctl status                       # degraded until configured
mkdir -p ~/.config/sideagent && cp config.example.toml ~/.config/sideagent/config.toml
$EDITOR ~/.config/sideagent/config.toml   # set provider.model + api_key
chmod 600 ~/.config/sideagent/config.toml
systemctl --user restart sideagentd
sideagentctl ask --screen "what is this?" # terminal chat with screen context

# overlay (GNOME extension — needs ONE logout for GNOME 50 to load it):
gnome-extensions enable org.sideagent     # after next login
# Super+Shift+A toggle · Super+Shift+S attach-screen · Esc close

# live-tune the composer prompt (reopen panel to apply):
gsettings set org.gnome.shell.extensions.sideagent prompt-y 6

# side-ask — quick question right from the terminal (no quotes needed):
side-ask how do I undo the last git commit      # brief: just the command
side-ask -e how do I undo the last git commit   # teach: answer + why + a tip
side-ask -c but what if i already pushed it     # follow-up, same conversation
git log --oneline -50 | side-ask which of these is the release commit
side-ask --screen what app is drawing this window
```

## Documentation map

- [docs/spec.md](docs/spec.md) — what we are building: UX contract, architecture,
  D-Bus protocol, capture/hotkey matrices, data model, privacy, non-goals.
- [docs/plan.md](docs/plan.md) — how and when: milestones with acceptance
  criteria, ordering rationale, test strategy, risk register, packaging.
- [docs/decisions.md](docs/decisions.md) — why: ADR-style record of the
  architecture choices and their tradeoffs.

## Design principles

1. **Daemon-first.** A resident daemon holds all state; frontends are dumb.
   Hotkey → visible in well under 100 ms because nothing spawns.
2. **One protocol, many shells.** The D-Bus API is the only coupling. A new
   DE/OS frontend is a few hundred lines, never a fork.
3. **Never interfere.** The overlay floats above everything but reserves no
   space, keeps no persistent focus, and vanishes on Esc. Your desktop is
   yours.
4. **Screenshots only on explicit opt-in**, per message, visibly indicated.
5. **Boring storage.** One SQLite file plus a folder of images. Back it up
   by copying, delete it to forget.
