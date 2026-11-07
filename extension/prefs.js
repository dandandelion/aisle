// sideagent extension preferences (runs in the separate prefs process,
// GTK4 + Adwaita). Shortcut changes apply live: the shell's keybinding
// engine watches the GSettings keys we bind to.

import Gtk from 'gi://Gtk';
import Adw from 'gi://Adw';
import { ExtensionPreferences } from 'resource:///org/gnome/shell/extensions/extension.js';

export default class SideagentPrefs extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();

        const page = new Adw.PreferencePage({
            title: 'sideagent',
            icon_name: 'dialog-information-symbolic',
        });

        const shortcuts = new Adw.PreferencesGroup({
            title: 'Shortcuts',
            description: 'GTK accelerator strings; applied live',
        });

        const accelRow = (title, key) => {
            const row = new Adw.ActionRow({ title });
            const current = settings.get_strv(key)[0] ?? '';
            const entry = new Gtk.Entry({
                text: current,
                valign: Gtk.Align.CENTER,
                width_request: 200,
                placeholder_text: '<Super><Shift>a',
            });
            entry.connect('changed', () => {
                if (entry.text.length > 0)
                    settings.set_strv(key, [entry.text]);
            });
            row.add_suffix(entry);
            return row;
        };

        shortcuts.add(accelRow('Toggle panel', 'toggle-shortcut'));
        shortcuts.add(accelRow('Toggle attach-screen', 'attach-shortcut'));
        page.add(shortcuts);

        const panel = new Adw.PreferencesGroup({title: 'Panel'});
        const widthRow = new Adw.ActionRow({title: 'Width (px)'});
        const spin = new Gtk.SpinButton({
            adjustment: new Gtk.Adjustment({
                lower: 280, upper: 800, step_increment: 20,
                value: settings.get_int('panel-width'),
            }),
            valign: Gtk.Align.CENTER,
        });
        spin.connect('value-changed', () => settings.set_int('panel-width', spin.value));
        widthRow.add_suffix(spin);
        panel.add(widthRow);

        const sizeRow = new Adw.ActionRow({title: 'Prompt icon size (px)'});
        const sizeSpin = new Gtk.SpinButton({
            adjustment: new Gtk.Adjustment({lower: 8, upper: 32, step_increment: 1,
                value: settings.get_int('prompt-size')}),
            valign: Gtk.Align.CENTER,
        });
        sizeSpin.connect('value-changed', () => settings.set_int('prompt-size', sizeSpin.value));
        sizeRow.add_suffix(sizeSpin);
        panel.add(sizeRow);

        const yRow = new Adw.ActionRow({title: 'Prompt vertical nudge (px, +down)'});
        const ySpin = new Gtk.SpinButton({
            adjustment: new Gtk.Adjustment({lower: -10, upper: 10, step_increment: 1,
                value: settings.get_int('prompt-y')}),
            valign: Gtk.Align.CENTER,
        });
        ySpin.connect('value-changed', () => settings.set_int('prompt-y', ySpin.value));
        yRow.add_suffix(ySpin);
        panel.add(yRow);
        page.add(panel);

        window.add(page);
    }
}
