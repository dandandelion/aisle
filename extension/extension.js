// sideagent GNOME Shell extension — Tier A frontend (spec §5).
// An edge-anchored overlay on the shell stage: hotkey, capture, and
// always-on-top come for free from living inside the compositor.
// Talks to sideagentd over the session bus (spec §8); holds no state.

import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';
import Meta from 'gi://Meta';
import Clutter from 'gi://Clutter';
import Shell from 'gi://Shell';
import Pango from 'gi://Pango';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';

const BUS = 'org.sideagent.Agent1';
const OBJ = '/org/sideagent/Agent1';
const IFACE = 'org.sideagent.Agent1';

// ---------- tiny helpers ----------

function mdToPango(text) {
    let s = GLib.markup_escape_text(text, -1);
    s = s.replace(/```([\s\S]*?)```/g, (_m, code) => `<tt>${code.trim()}</tt>`);
    s = s.replace(/`([^`\n]+)`/g, '<tt>$1</tt>');
    s = s.replace(/\*\*([^*\n]+)\*\*/g, '<b>$1</b>');
    return s;
}

function dbusCall(member, sig, params) {
    // call_sync: deterministic across gjs versions (auto-promisification of
    // async call() differs between releases). Ask/Cancel return immediately;
    // the reply stream is signal-based, so sync here never blocks the loop.
    const reply = Gio.DBus.session.call_sync(
        BUS, OBJ, IFACE, member,
        sig ? new GLib.Variant(sig, params) : null,
        null, // return type: infer
        Gio.DBusCallFlags.NONE, 5000, null);
    return reply ? reply.deep_unpack() : null;
}

let _promisified = false;
function promisifyScreenshot() {
    if (_promisified)
        return;
    Gio._promisify(Shell.Screenshot.prototype, 'screenshot_stage_to_content');
    Gio._promisify(Shell.Screenshot, 'composite_to_stream');
    _promisified = true;
}

// ---------- panel ----------

class Panel {
    constructor(width, settings) {
        this._width = width;
        this._settings = settings;
        this._visible = false;
        this._session = null;
        this._currentReq = null;
        this._streamLabel = null;
        this._streamBuf = '';
        this._sigIds = [];
        this._prevFocus = null;

        this._root = new St.BoxLayout({
            vertical: true,
            style_class: 'sideagent-panel',
            reactive: true,
            can_focus: true,
        });

        // header
        const header = new St.BoxLayout({ style_class: 'sideagent-header' });
        const title = new St.Label({ y_align: Clutter.ActorAlign.CENTER });
        title.clutter_text.set_markup('<span foreground="#58a6ff">●</span> <span foreground="#e6edf3">sideagent</span>');
        header.add_child(title);
        this._modelChip = new St.Label({ text: '', style_class: 'sideagent-model', y_align: Clutter.ActorAlign.CENTER });
        header.add_child(this._modelChip);
        const spacer = new St.Label({ text: '', x_expand: true });
        header.add_child(spacer);

        this._attachBtn = new St.Button({ style_class: 'sideagent-button', label: '📎', toggleMode: true });
        this._attachBtn.add_tooltip?.('attach screen to next message');
        header.add_child(this._attachBtn);

        const newBtn = new St.Button({ style_class: 'sideagent-button', label: '＋' });
        newBtn.add_tooltip?.('new session');
        newBtn.connect('clicked', () => this._newSession());
        header.add_child(newBtn);

        const closeBtn = new St.Button({ style_class: 'sideagent-button', label: '✕' });
        closeBtn.connect('clicked', () => this.close());
        header.add_child(closeBtn);
        this._root.add_child(header);

        // transcript
        const scroll = new St.ScrollView({
            hscrollbar_policy: St.PolicyType.NEVER,
            vscrollbar_policy: St.PolicyType.AUTOMATIC,
            x_expand: true, y_expand: true,
        });
        this._transcript = new St.BoxLayout({
            vertical: true,
            style_class: 'sideagent-transcript',
            x_align: Clutter.ActorAlign.FILL,
        });
        scroll.add_child(this._transcript);
        this._scroll = scroll;
        this._root.add_child(scroll);

        // status line
        this._status = new St.Label({ style_class: 'sideagent-status', text: '' });
        this._root.add_child(this._status);

        // input row: terminal prompt glyph + entry
        // (GNOME 50's St dropped multi-line text widgets; v1 input is
        // single-line. Multi-line is backlog.)
        const inputRow = new St.BoxLayout({ style_class: 'sideagent-input-row' });
        this._entry = new St.Entry({ style_class: 'sideagent-input', x_expand: true });
        // prompt chevron as the entry's own primary icon: centered by
        // St.Entry internals, correct at any HiDPI scale — no layout games
        this._prompt = new St.Icon({
            icon_name: 'go-next-symbolic',
            icon_size: 14,
            style_class: 'symbolic sideagent-prompt',
        });
        this._entry.set_primary_icon(this._prompt);
        this._entry.clutter_text.connect('activate', () => this._send());
        this._entry.connect('key-press-event', (_a, event) => this._onKeyPress(event));
        inputRow.add_child(this._entry);
        this._root.add_child(inputRow);

        const hint = new St.Label({
            style_class: 'sideagent-status',
            text: 'Enter send · Esc close',
            style_class: 'sideagent-hint',
        });
        this._root.add_child(hint);

        Main.layoutManager.addTopChrome(this._root);
        this._root.hide();

        this._subscribe();
    }

    destroy() {
        this._stopPulse();
        for (const id of this._sigIds)
            Gio.DBus.session.signal_unsubscribe(id);
        this._sigIds = [];
        Main.layoutManager.removeChrome(this._root);
        this._root.destroy();
    }

    _onKeyPress(event) {
        const sym = event.get_key_symbol();
        if (sym === Clutter.KEY_Escape) {
            this.close();
            return Clutter.EVENT_STOP;
        }
        if (sym === Clutter.KEY_Return || sym === Clutter.KEY_KP_Enter) {
            this._send();
            return Clutter.EVENT_STOP;
        }
        return Clutter.EVENT_PROPAGATE;
    }

    toggle() {
        this._visible ? this.close() : this.open();
    }

    open() {
        // prompt knobs re-read every open: live-tune via gsettings, no logout
        this._prompt.icon_size = this._settings.get_int('prompt-size');
        this._prompt.set_translation(0, this._settings.get_int('prompt-y'), 0);
        const mon = Main.layoutManager.primaryMonitor;
        this._root.set_size(this._width, mon.height);
        this._root.set_position(mon.x + mon.width, mon.y); // start off-screen
        this._root.show();
        this._root.save_easing_state();
        this._root.set_easing_duration(180);
        this._root.set_easing_mode(Clutter.AnimationMode.EASE_OUT_QUAD);
        this._root.set_position(mon.x + mon.width - this._width, mon.y);
        this._root.restore_easing_state();
        this._visible = true;
        this._prevFocus = global.stage.get_key_focus();
        this._entry.grab_key_focus();
        this._startPulse();
        this._setStatus('connecting…');
        // re-announce on every open: survives daemon restarts (registration
        // lives only in the daemon's memory)
        try {
            dbusCall('RegisterFrontend', '(s)', ['extension']);
        } catch (_) { /* daemon down; ensureSession below reports it */ }
        this._ensureSession().catch(e => this._setStatus(`daemon unreachable: ${e.message || e}`));
        this._refreshModelChip();
    }

    close() {
        const mon = Main.layoutManager.primaryMonitor;
        this._visible = false;
        this._stopPulse();
        this._root.save_easing_state();
        this._root.set_easing_duration(140);
        this._root.set_easing_mode(Clutter.AnimationMode.EASE_IN_QUAD);
        this._root.set_position(mon.x + mon.width, mon.y);
        this._root.restore_easing_state();
        GLib.timeout_add(GLib.PRIORITY_DEFAULT, 150, () => {
            if (!this._visible)
                this._root.hide();
            return GLib.SOURCE_REMOVE;
        });
        if (this._prevFocus) {
            try {
                global.stage.set_key_focus(this._prevFocus);
            } catch (_) { /* actor may be gone */ }
            this._prevFocus = null;
        }
    }

    // terminal-style pulsing prompt while the panel is open
    _startPulse() {
        this._stopPulse();
        this._pulseUp = true;
        this._pulseId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 530, () => {
            if (!this._prompt || !this._visible)
                return GLib.SOURCE_REMOVE;
            this._prompt.save_easing_state();
            this._prompt.set_easing_duration(260);
            this._prompt.set_easing_mode(Clutter.AnimationMode.LINEAR);
            this._prompt.opacity = this._pulseUp ? 60 : 255;
            this._prompt.restore_easing_state();
            this._pulseUp = !this._pulseUp;
            return GLib.SOURCE_CONTINUE;
        });
    }

    _stopPulse() {
        if (this._pulseId) {
            GLib.source_remove(this._pulseId);
            this._pulseId = 0;
        }
        if (this._prompt)
            this._prompt.opacity = 255;
    }

    _refreshModelChip() {
        try {
            const [st] = dbusCall('Status', null, null);
            const prov = st.provider ?? ''; // "openai:model" or "UNCONFIGURED (...)"
            this._modelChip.set_text(prov.startsWith('UNCONFIGURED') ? '⚠ no provider' : ` ${prov.split(':')[1] ?? ''}`);
        } catch (_) { /* daemon down; status line already says it */ }
    }

    get visible() {
        return this._visible;
    }

    armAttach() {
        if (!this._visible)
            this.open();
        this._attachBtn.checked = !this._attachBtn.checked;
        this._entry.grab_key_focus();
    }

    // ---------- dbus ----------

    _subscribe() {
        for (const sig of ['Delta', 'CaptureAttached', 'Done', 'Failed', 'ToggleRequested']) {
            const id = Gio.DBus.session.signal_subscribe(
                BUS, IFACE, sig, OBJ, null, Gio.DBusSignalFlags.NONE,
                (_c, _s, _p, _i, _m, data) => {
                    try {
                        this._onSignal(sig, data.deep_unpack());
                    } catch (e) {
                        logError(e, 'sideagent signal');
                    }
                });
            this._sigIds.push(id);
        }
    }

    _onSignal(sig, body) {
        switch (sig) {
        case 'ToggleRequested': {
            const unpacked = Array.isArray(body) ? body : [body];
            const source = typeof unpacked[0] === 'string' ? unpacked[0] : '';
            if (source === 'selftest') {
                // driven headless e2e: open, capture via Shell.Screenshot,
                // ask with the captured file, expect a streamed reply.
                if (!this._visible)
                    this.open();
                this._attachBtn.checked = true;
                this._selftestShot = true;
                this._entry.set_text('selftest: describe the attached screen');
                // small delay so the slide-in animation settles before capture
                GLib.timeout_add(GLib.PRIORITY_DEFAULT, 400, () => {
                    this._ensureSession()
                        .then(() => this._send())
                        .catch(e => this._setStatus(`selftest session error: ${e.message || e}`));
                    return GLib.SOURCE_REMOVE;
                });
                break;
            }
            this.toggle();
            break;
        }
        case 'Delta': {
            const [rid, text] = body;
            if (rid === this._currentReq && this._streamLabel) {
                this._streamBuf += text;
                try {
                    this._streamLabel.clutter_text.set_markup(
                        `${this._streamLabel._roleMarkup ?? ''}\n${mdToPango(this._streamBuf)}`);
                    const adj = this._scroll.get_vadjustment();
                    adj.set_value(adj.get_upper());
                } catch (_) {
                    this._streamLabel = null; // actor disposed (panel closed/session reset)
                }
            }
            break;
        }
        case 'CaptureAttached': {
            const [rid, w, h, bytes] = body;
            if (rid === this._currentReq)
                this._setStatus(`screen attached: ${w}×${h}, ${Math.round(bytes / 1024)} KB`);
            break;
        }
        case 'Done': {
            const [rid, usage] = body;
            if (rid === this._currentReq) {
                this._currentReq = null;
                this._streamLabel = null;
                const u = usage instanceof Map ? Object.fromEntries(usage) : usage;
                this._setStatus(`✓ ${u.ms} ms · ${u.in}→${u.out} tok · ${u.model}${u.truncated === 'true' ? ' · ⚠ cut at token cap — raise provider.max_tokens' : ''}`);
                if (this._selftestShot) {
                    this._selftestShot = false;
                    this._entry.set_text('Hjkl typ| test'); // exercise composer layout in shot
                    this._capturePath({full: true}).catch(() => {}); // visual regression artifact
                }
            }
            break;
        }
        case 'Failed': {
            const [rid, kind, message] = body;
            if (rid === this._currentReq) {
                this._currentReq = null;
                this._streamLabel = null;
                this._addMsg('system', `error [${kind}]: ${message}`);
                this._setStatus('');
            }
            break;
        }
        }
    }

    async _ensureSession() {
        if (this._session) {
            await this._loadHistory();
            return;
        }
        const [sessions] = await dbusCall('ListSessions', null, null);
        if (sessions.length) {
            this._session = sessions[0][0];
            this._setStatus(`session: ${sessions[0][1] || '(untitled)'}`);
            await this._loadHistory();
        } else {
            await this._newSession();
        }
    }

    async _loadHistory() {
        if (this._transcript.get_n_children() > 0)
            return; // already rendered
        const [msgs] = await dbusCall('GetMessages', '(si)', [this._session, 30]);
        if (msgs.length && this._transcript.get_n_children() === 0) {
            for (const m of msgs) {
                // Msg struct unpacks to [id, session_id, role, text, created_at]
                this._addMsg(m[2] === 'system' ? 'system' : m[2], m[3]);
            }
            this._setStatus(`session restored · ${msgs.length} message(s)`);
        }
    }

    async _newSession() {
        const [id] = await dbusCall('NewSession', '(ss)', ['', '']);
        this._session = id;
        this._transcript.destroy_all_children();
        this._setStatus('new session');
    }

    async _send() {
        const text = this._entry.get_text().trim();
        if (!text || this._currentReq)
            return;
        this._entry.set_text('');

        const opts = {};
        if (this._attachBtn.checked) {
            this._setStatus('capturing screen…');
            try {
                opts.screenshot_path = await this._capturePath();
            } catch (e) {
                this._setStatus(`capture failed: ${e.message || e}`);
                return;
            }
            this._attachBtn.checked = false;
        }

        // pre-declare the id so early signals correlate (fast providers can
        // emit Done before the Ask reply arrives)
        const rid = `${Date.now()}-${Math.floor(Math.random() * 1e9)}`;
        opts.request_id = rid;
        this._currentReq = rid;
        this._addMsg('user', text);
        this._streamBuf = '';
        this._streamLabel = this._addMsg('assistant', '…');
        this._setStatus('thinking…');

        try {
            await dbusCall('Ask', '(ssa{ss})', [this._session ?? '', text, opts]);
        } catch (e) {
            this._currentReq = null;
            this._setStatus(`ask failed: ${e.message || e}`);
            this._streamLabel = null;
        }
    }

    // ---------- capture (in-shell, prompt-free) ----------

    // Normal captures fade the panel out first, so the model receives the
    // full desktop *including content underneath us* (context preservation).
    // full=true (selftest artifacts) skips the fade for UI regression shots.
    async _capturePath({full = false} = {}) {
        promisifyScreenshot();
        const hidden = !full && this._visible;
        if (hidden)
            await this._fadeSelf(false);
        try {
            return await this._captureInner();
        } finally {
            if (hidden)
                await this._fadeSelf(true);
        }
    }

    _fadeSelf(in_) {
        return new Promise(resolve => {
            this._root.save_easing_state();
            this._root.set_easing_duration(in_ ? 160 : 60);
            this._root.set_easing_mode(in_ ? Clutter.AnimationMode.EASE_OUT_QUAD
                                           : Clutter.AnimationMode.LINEAR);
            this._root.opacity = in_ ? 255 : 0;
            this._root.restore_easing_state();
            // let the compositor paint at least one frame in the new state
            GLib.timeout_add(GLib.PRIORITY_DEFAULT, in_ ? 170 : 90, () => {
                resolve();
                return GLib.SOURCE_REMOVE;
            });
        });
    }

    async _captureInner() {
        const shooter = new Shell.Screenshot();
        const [content, scale] = await shooter.screenshot_stage_to_content();
        const texture = content.get_texture();

        const stream = Gio.MemoryOutputStream.new_resizable();
        await Shell.Screenshot.composite_to_stream(
            texture, 0, 0, -1, -1, scale, null, 0, 0, 1, stream);
        stream.close(null);
        const bytes = stream.steal_as_bytes();

        const dir = GLib.build_filenamev([GLib.get_user_runtime_dir(), 'sideagent']);
        GLib.mkdir_with_parents(dir, 0o700);
        const path = GLib.build_filenamev([dir, `ext-${Date.now()}.png`]);
        GLib.file_set_contents(path, bytes.get_data());
        log(`sideagent: captured screen -> ${path}`);
        return path;
    }

    // ---------- transcript ----------

    _addMsg(role, text) {
        const who = role === 'user'
            ? '<span foreground="#7ee787">❯ you</span>'
            : role === 'assistant'
                ? '<span foreground="#58a6ff">❯ sideagent</span>'
                : '<span foreground="#8b949e">·</span>';
        const label = new St.Label({
            style_class: `sideagent-msg sideagent-msg-${role}`,
            x_align: Clutter.ActorAlign.FILL,
            x_expand: true,
        });
        label.clutter_text.set_line_wrap(true);
        label.clutter_text.set_line_wrap_mode(Pango.WrapMode.WORD_CHAR);
        label.clutter_text.set_ellipsize(Pango.EllipsizeMode.NONE);
        label._roleMarkup = who;
        label.clutter_text.set_markup(`${who}\n${mdToPango(text)}`);
        label.set_width(this._width - 40);
        this._transcript.add_child(label);
        const adj = this._scroll.get_vadjustment();
                adj.set_value(adj.get_upper());
        return label;
    }

    _setStatus(s) {
        this._status.set_text(s);
    }
}

// ---------- extension ----------

export default class SideagentExtension extends Extension {
    enable() {
        log('sideagent: extension build 2026-11-07.1845');
        const width = this.getSettings().get_int('panel-width');
        this._panel = new Panel(width, this.getSettings());

        // load stylesheet
        const file = this.dir.get_child('stylesheet.css');
        const theme = St.ThemeContext.get_for_stage(global.stage).get_theme();
        theme.load_stylesheet(file);
        this._stylesheetFile = file;

        // keybindings from our schema — GNOME 50 moved these to Meta.Display;
        // fall back to the removed Meta namespace for older shells.
        this._settings = this.getSettings();
        // GNOME 50 validates the flags mask; build from defined bits
        // instead of ALL (0xffffffff rejected).
        const AM = Shell.ActionMode;
        const mode = AM.GLOBAL | AM.MENU | AM.OVERVIEW | AM.POPUP | AM.MODAL | AM.LOCKSCREEN;
        this._addBinding = (name, handler) => {
            if (global.display.add_keybinding)
                global.display.add_keybinding(name, this._settings, mode, handler);
            else
                Meta.add_keybinding(name, this._settings, mode, handler);
        };
        this._removeBinding = name =>
            (global.display.remove_keybinding ?? Meta.remove_keybinding).call(
                global.display.remove_keybinding ? global.display : Meta, name);

        this._addBinding('toggle-shortcut', () => this._panel.toggle());
        this._addBinding('attach-shortcut', () => this._panel.armAttach());

        // announce ourselves; daemon suppresses other frontends
        try {
            const ok = dbusCall('RegisterFrontend', '(s)', ['extension']);
            log(`sideagent: frontend registered (active=${ok})`);
        } catch (e) {
            log(`sideagent: daemon not reachable: ${e.message || e}`);
        }
    }

    disable() {
        this._removeBinding?.('toggle-shortcut');
        this._removeBinding?.('attach-shortcut');
        this._settings?.run_dispose();
        this._settings = null;

        if (this._stylesheetFile) {
            const theme = St.ThemeContext.get_for_stage(global.stage).get_theme();
            theme.unload_stylesheet(this._stylesheetFile);
            this._stylesheetFile = null;
        }
        this._panel?.destroy();
        this._panel = null;
    }
}
