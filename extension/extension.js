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
    return new Promise((resolve, reject) => {
        Gio.DBus.session.call(
            BUS, OBJ, IFACE, member, sig,
            sig ? new GLib.Variant(sig, params) : null,
            Gio.DBus.CallFlags.NONE, 30000,
            (conn, res) => {
                try {
                    resolve(conn.call_finish(res).deep_unpack());
                } catch (e) {
                    reject(e);
                }
            });
    });
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
    constructor(width) {
        this._width = width;
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
        header.add_child(new St.Label({ text: 'sideagent', y_align: Clutter.ActorAlign.CENTER }));
        const spacer = new St.Label({ text: '' });
        spacer.expand = true;
        header.add_child(spacer);

        this._attachBtn = new St.Button({ style_class: 'sideagent-button', label: '📎', toggleMode: true });
        this._attachBtn.set_tooltip_text('attach screen to next message');
        header.add_child(this._attachBtn);

        const newBtn = new St.Button({ style_class: 'sideagent-button', label: '＋' });
        newBtn.set_tooltip_text('new session');
        newBtn.connect('clicked', () => this._newSession());
        header.add_child(newBtn);

        const closeBtn = new St.Button({ style_class: 'sideagent-button', label: '✕' });
        closeBtn.connect('clicked', () => this.close());
        header.add_child(closeBtn);
        this._root.add_child(header);

        // transcript
        const scroll = new St.ScrollView({ x_fill: true, y_fill: true });
        scroll.get_vscrollbar().connect('scroll', () => {});
        this._transcript = new St.BoxLayout({
            vertical: true,
            style_class: 'sideagent-transcript',
            x_align: Clutter.ActorAlign.FILL,
        });
        scroll.add_actor(this._transcript);
        this._scroll = scroll;
        this._root.add_child(scroll);

        // status line
        this._status = new St.Label({ style_class: 'sideagent-status', text: '' });
        this._root.add_child(this._status);

        // input
        this._entry = new St.TextArea({ style_class: 'sideagent-input', height: 72 });
        this._entry.clutter_text.connect('key-press-event', (_a, event) => this._onKeyPress(event));
        this._root.add_child(this._entry);

        const hint = new St.Label({
            style_class: 'sideagent-status',
            text: 'Enter send · Shift+Enter newline · Esc close',
        });
        this._root.add_child(hint);

        Main.layoutManager.addChrome(this._root, { trackAlpha: false });
        this._root.hide();

        this._subscribe();
    }

    destroy() {
        for (const id of this._sigIds)
            Gio.DBus.session.unsubscribe_signal(id);
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
            const mods = event.get_state();
            if (!(mods & Clutter.ModifierType.SHIFT_MASK)) {
                this._send();
                return Clutter.EVENT_STOP;
            }
        }
        return Clutter.EVENT_PROPAGATE;
    }

    toggle() {
        this._visible ? this.close() : this.open();
    }

    open() {
        const mon = Main.layoutManager.primaryMonitor;
        this._root.set_position(mon.x + mon.width - this._width, mon.y);
        this._root.set_size(this._width, mon.height);
        this._root.show();
        this._visible = true;
        this._prevFocus = global.stage.get_key_focus();
        this._entry.grab_key_focus();
        this._setStatus('connecting…');
        this._ensureSession().catch(e => this._setStatus(`daemon unreachable: ${e.message || e}`));
    }

    close() {
        this._root.hide();
        this._visible = false;
        if (this._prevFocus) {
            try {
                global.stage.set_key_focus(this._prevFocus);
            } catch (_) { /* actor may be gone */ }
            this._prevFocus = null;
        }
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
                BUS, IFACE, sig, OBJ, null, Gio.DBus.SignalMatchFlags.NONE,
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
        case 'ToggleRequested':
            this.toggle();
            break;
        case 'Delta': {
            const [rid, text] = body;
            if (rid === this._currentReq) {
                this._streamBuf += text;
                this._streamLabel.clutter_text.set_markup(mdToPango(this._streamBuf));
                this._scroll.get_vscrollbar().get_adjustment().set_value(1e9);
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
                this._setStatus(`✓ ${usage.get('ms')} ms · ${usage.get('in')}→${usage.get('out')} tok · ${usage.get('model')}`);
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
        if (this._session)
            return;
        const [sessions] = await dbusCall('ListSessions', null, null);
        if (sessions.length) {
            this._session = sessions[0][0];
            this._setStatus(`session: ${sessions[0][1] || '(untitled)'}`);
        } else {
            await this._newSession();
        }
    }

    async _newSession() {
        const [id] = await dbusCall('NewSession', 'ss', ['', '']);
        this._session = id;
        this._transcript.destroy_all_children();
        this._setStatus('new session');
    }

    async _send() {
        const text = this._entry.text.trim();
        if (!text || this._currentReq)
            return;
        this._entry.text = '';

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

        this._addMsg('user', text);
        this._streamBuf = '';
        this._streamLabel = this._addMsg('assistant', '…');
        this._setStatus('thinking…');

        try {
            const [rid] = await dbusCall('Ask', 'ssa{ss}', [this._session ?? '', text, opts]);
            this._currentReq = rid;
        } catch (e) {
            this._setStatus(`ask failed: ${e.message || e}`);
            this._streamLabel = null;
        }
    }

    // ---------- capture (in-shell, prompt-free) ----------

    async _capturePath() {
        promisifyScreenshot();
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
        return path;
    }

    // ---------- transcript ----------

    _addMsg(role, text) {
        const label = new St.Label({
            style_class: `sideagent-msg sideagent-msg-${role}`,
            x_align: Clutter.ActorAlign.FILL,
            x_expand: true,
        });
        label.clutter_text.set_line_wrap(true);
        label.clutter_text.set_line_wrap_mode(Pango.WrapMode.WORD_CHAR);
        label.clutter_text.set_ellipsize(Pango.EllipsizeMode.NONE);
        label.clutter_text.set_markup(mdToPango(text));
        label.width = this._width - 40;
        this._transcript.add_child(label);
        this._scroll.get_vscrollbar().get_adjustment().set_value(1e9);
        return label;
    }

    _setStatus(s) {
        this._status.set_text(s);
    }
}

// ---------- extension ----------

export default class SideagentExtension extends Extension {
    enable() {
        const width = this.getSettings().get_int('panel-width');
        this._panel = new Panel(width);

        // load stylesheet
        const file = this.dir.get_child('stylesheet.css');
        const theme = St.ThemeContext.get_for_stage(global.stage).get_theme();
        theme.load_stylesheet(file);
        this._stylesheetFile = file;

        // keybindings from our schema — GNOME 50 moved these to Meta.Display;
        // fall back to the removed Meta namespace for older shells.
        this._settings = this.getSettings();
        this._addBinding = (name, handler) => {
            if (global.display.add_keybinding)
                global.display.add_keybinding(name, this._settings, Shell.ActionMode.ALL, handler);
            else
                Meta.add_keybinding(name, this._settings, Shell.ActionMode.ALL, handler);
        };
        this._removeBinding = name =>
            (global.display.remove_keybinding ?? Meta.remove_keybinding).call(
                global.display.remove_keybinding ? global.display : Meta, name);

        this._addBinding('toggle-shortcut', () => this._panel.toggle());
        this._addBinding('attach-shortcut', () => this._panel.armAttach());

        // announce ourselves; daemon suppresses other frontends
        dbusCall('RegisterFrontend', 's', ['extension']).catch(
            () => log('sideagent: daemon not running yet'));
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
