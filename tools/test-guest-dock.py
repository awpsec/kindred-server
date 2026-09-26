#!/usr/bin/python3
"""Native X11 acceptance in an isolated Xvfb display; never touches a guest."""
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
WIDTH = int(os.environ.get('KINDRED_TEST_DOCK_WIDTH', '1024'))


def test():
    with tempfile.TemporaryDirectory(prefix='kindred-dock-') as tmp:
        read, write = os.pipe()
        x = subprocess.Popen(['Xvfb', '-displayfd', str(write), '-screen', '0', f'{WIDTH}x768x24'], pass_fds=[write], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        os.close(write)
        display = os.read(read, 32).decode().strip()
        os.close(read)
        os.environ['DISPLAY'] = ':' + display
        os.environ['KINDRED_DOCK_ASSETS'] = str(ROOT / 'deploy/desktop')
        wm = subprocess.Popen(['openbox'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(.5)
            loader = importlib.machinery.SourceFileLoader('dock', str(ROOT / 'deploy/kindred-dock'))
            spec = importlib.util.spec_from_loader(loader.name, loader)
            dock = importlib.util.module_from_spec(spec)
            loader.exec_module(dock)
            Gtk, Wnck = dock.Gtk, dock.Wnck
            Wnck.set_client_type(Wnck.ClientType.PAGER)
            panel = dock.Dock()
            def settle():
                end = time.monotonic() + .6
                while time.monotonic() < end:
                    while Gtk.events_pending():
                        Gtk.main_iteration_do(False)
                    time.sleep(.01)
                panel.screen.force_update()
                panel.update()
            windows = []
            for title in ['Chromium one', 'Chromium hidden', 'Editor']:
                window = Gtk.Window(title=title)
                window.set_wmclass('chromium' if 'Chromium' in title else 'editor', 'Chromium' if 'Chromium' in title else 'Editor')
                window.set_default_size(300, 180)
                window.show_all()
                windows.append(window)
            settle()
            assert len(panel.groups['browser']) == 2, panel.groups
            windows[1].iconify()
            settle()
            hidden = next(w for w in panel.groups['browser'] if w.get_name() == 'Chromium hidden')
            assert hidden.is_minimized()
            button = panel.tasks.get_children()[0]
            bx, by = button.translate_coordinates(panel, 12, 12)
            subprocess.run(['xdotool', 'mousemove', str(bx), str(704 + by), 'click', '3'], check=True)
            settle()
            assert panel.menu and panel.menu.get_visible()
            labels = [item.get_label() for item in panel.menu.get_children()]
            assert 'Minimized · Chromium hidden' in labels, labels
            item = next(item for item in panel.menu.get_children() if 'hidden' in item.get_label())
            item.get_submenu().get_children()[0].activate()
            panel.menu.popdown()
            settle()
            assert not hidden.is_minimized()
            button = panel.tasks.get_children()[0]
            bx, by = button.translate_coordinates(panel, 12, 12)
            subprocess.run(['xdotool', 'mousemove', str(bx), str(704 + by), 'click', '3'], check=True)
            settle()
            assert panel.menu and panel.menu.get_visible()
            item = next(item for item in panel.menu.get_children() if 'hidden' in item.get_label())
            item.get_submenu().get_children()[1].activate()
            panel.menu.popdown()
            settle()
            assert len(panel.groups['browser']) == 1
            assert panel.time.get_text() and panel.date.get_text()
            xid = str(panel.get_window().get_xid())
            strut = subprocess.check_output(['xprop', '-id', xid, '_NET_WM_STRUT_PARTIAL'], text=True)
            assert '0, 0, 0, 64' in strut, strut
            assert panel.get_size().width == WIDTH, panel.get_size()
            assert panel.get_position().root_y == 704, panel.get_position()
            windows[0].maximize()
            settle()
            assert windows[0].get_position().root_y + windows[0].get_size().height <= 704
            dock.Gdk.pixbuf_get_from_window(dock.Gdk.get_default_root_window(), 0, 0, WIDTH, 768).savev('/tmp/kindred-dock-acceptance.png', 'png', [], [])
            for window in windows:
                window.destroy()
            settle()
            assert not panel.groups.get('browser')
            print('PASS: grouped windows, minimized discovery, restore/close menu actions, clock, full width, reserved work area, closed-app cleanup')
            panel.destroy()
        finally:
            wm.terminate()
            wm.wait(timeout=5)
            x.terminate()
            x.wait(timeout=5)


if __name__ == '__main__':
    test()
