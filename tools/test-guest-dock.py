#!/usr/bin/python3
"""Native X11 acceptance in an isolated Xvfb display; never touches a guest.

Real GTK windows under Openbox; input goes through xdotool (XTEST). The dock
must follow the window manager from libwnck signals alone: the test never calls
its refresh directly. Screenshots go to KINDRED_TEST_DOCK_SHOTS (default /tmp).
"""
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
WIDTH = int(os.environ.get('KINDRED_TEST_DOCK_WIDTH', '1024'))
HEIGHT = 768
SHOTS = Path(os.environ.get('KINDRED_TEST_DOCK_SHOTS', '/tmp'))


def test():
    with tempfile.TemporaryDirectory(prefix='kindred-dock-') as tmp:
        tmp = Path(tmp)
        launches = tmp / 'launches'
        launcher = tmp / 'desktop-launch'
        launcher.write_text(f'#!/bin/sh\necho "$1" >> {launches}\n')
        launcher.chmod(0o755)
        read, write = os.pipe()
        x = subprocess.Popen(['Xvfb', '-displayfd', str(write), '-screen', '0', f'{WIDTH}x{HEIGHT}x24'],
                             pass_fds=[write], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        os.close(write)
        display = os.read(read, 32).decode().strip()
        os.close(read)
        os.environ['DISPLAY'] = ':' + display
        os.environ['KINDRED_DOCK_ASSETS'] = str(ROOT / 'deploy/desktop')
        os.environ['KINDRED_DOCK_LAUNCHER'] = str(launcher)
        wm = subprocess.Popen(['openbox'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(.5)
            if shutil.which('rsvg-convert') and shutil.which('feh'):
                # Guest wallpaper, for representative screenshots only.
                subprocess.run(['rsvg-convert', str(ROOT / 'deploy/desktop/wallpaper.svg'), '-o', str(tmp / 'wall.png')], check=True)
                subprocess.run(['feh', '--no-fehbg', '--bg-fill', str(tmp / 'wall.png')], check=True)
            loader = importlib.machinery.SourceFileLoader('dock', str(ROOT / 'deploy/kindred-dock'))
            spec = importlib.util.spec_from_loader(loader.name, loader)
            dock = importlib.util.module_from_spec(spec)
            loader.exec_module(dock)
            Gdk, Gtk, Wnck = dock.Gdk, dock.Gtk, dock.Wnck
            Wnck.set_client_type(Wnck.ClientType.PAGER)
            panel = dock.Dock()
            top = HEIGHT - dock.HEIGHT

            def settle(seconds=.6):
                end = time.monotonic() + seconds
                while time.monotonic() < end:
                    while Gtk.events_pending():
                        Gtk.main_iteration_do(False)
                    time.sleep(.01)

            def click(widget, button=1):
                ox, oy = panel.get_window().get_origin()[1:]
                alloc = widget.get_allocation()
                wx, wy = widget.translate_coordinates(panel, alloc.width // 2, alloc.height // 2)
                subprocess.run(['xdotool', 'mousemove', str(ox + wx), str(oy + wy), 'click', str(button)], check=True)
                settle()

            def key(name):
                subprocess.run(['xdotool', 'key', name], check=True)
                settle()

            def shot(name):
                path = SHOTS / name
                Gdk.pixbuf_get_from_window(Gdk.get_default_root_window(), 0, 0, WIDTH, HEIGHT).savev(str(path), 'png', [], [])
                strip = SHOTS / name.replace('.png', '-strip.png')
                Gdk.pixbuf_get_from_window(Gdk.get_default_root_window(), 0, top - 150, WIDTH, 150 + dock.HEIGHT).savev(str(strip), 'png', [], [])
                return path

            def launched():
                return launches.read_text().split() if launches.exists() else []

            def classes(item):
                return {name for name in ['running', 'active', 'minimized'] if item.get_style_context().has_class(name)}

            def labels(menu):
                return [item.get_child().get_text() for item in menu.get_children() if item.get_child()]

            def entry(menu, text):
                return next(item for item in menu.get_children() if item.get_child() and text in item.get_child().get_text())

            def close_menu():
                panel.menu.popdown()
                settle()

            windows = {}
            for title, wmclass in [('Chromium hidden', 'Chromium'), ('Editor', 'Editor'),
                                   ('Konsole', 'konsole'), ('Chromium one', 'Chromium')]:
                window = Gtk.Window(title=title)
                window.set_wmclass(wmclass.lower(), wmclass)
                window.set_default_size(420, 260)
                window.move(60 + 70 * len(windows), 60 + 50 * len(windows))
                window.show_all()
                windows[title] = window
                settle(.3)
            settle()
            items = panel.items
            browser, files, terminal, editor = items['browser'], items['files'], items['terminal'], items['editor']
            assert len(panel.groups['browser']) == 2, panel.groups
            assert [c.key for c in panel.tasks.get_children() if isinstance(c, dock.DockItem)] == ['browser', 'files', 'terminal', 'editor']
            assert panel.divider.get_visible(), 'divider separates pinned from other running apps'
            assert len(browser.dots.get_children()) == 2 and len(terminal.dots.get_children()) == 1
            assert not files.dots.get_children() and classes(files) == set()
            assert classes(browser) == {'running', 'active'}, classes(browser)
            assert classes(terminal) == {'running'} and classes(editor) == {'running'}
            generic = dock.icon_for('editor', 'Editor', panel.groups['editor'][0], generic=None)
            assert editor.image.get_pixbuf().get_pixels() != generic.get_pixels(), 'icon-less app gets a letter tile, not the WM stock icon'
            assert editor.image.get_pixbuf().get_width() == dock.ICON
            assert browser.get_tooltip_text() == 'Browser · 2 windows', browser.get_tooltip_text()

            # Minimized discovery and state, all through WM signals.
            windows['Chromium hidden'].iconify()
            settle()
            hidden = next(w for w in panel.groups['browser'] if w.get_name() == 'Chromium hidden')
            assert hidden.is_minimized() and 'minimized' not in classes(browser)
            assert browser.get_tooltip_text() == 'Browser · 2 windows · 1 minimized'

            # Left-click with several windows: chooser above the icon, then restore.
            click(browser)
            assert panel.menu and panel.menu.get_visible()
            menu_window = panel.menu.get_toplevel().get_window()
            my = menu_window.get_origin()[2]
            assert my + menu_window.get_height() <= top + 2, 'chooser opens above the dock'
            assert any('Chromium hidden · minimized' in text for text in labels(panel.menu)), labels(panel.menu)
            assert 'New window' in labels(panel.menu)
            shot('kindred-dock-chooser.png')
            entry(panel.menu, 'Chromium hidden').activate()
            close_menu()
            assert not hidden.is_minimized() and panel.screen.get_active_window() == hidden

            # Right-click manage menu: per-window Show/Minimize/Close.
            click(browser, 3)
            assert panel.menu and panel.menu.get_visible()
            assert labels(panel.menu)[0] == 'Browser' and 'Close all windows' in labels(panel.menu), labels(panel.menu)
            actions = entry(panel.menu, 'Chromium hidden').get_submenu()
            assert [i.get_child().get_text() for i in actions.get_children()] == ['Show window', 'Minimize', 'Close window']
            actions.get_children()[2].activate()
            close_menu()
            assert len(panel.groups['browser']) == 1 and len(browser.dots.get_children()) == 1

            # Single window: a real left click restores it from minimized.
            windows['Chromium one'].iconify()
            settle()
            assert 'minimized' in classes(browser)
            click(browser)
            only = panel.groups['browser'][0]
            assert not only.is_minimized() and panel.screen.get_active_window() == only
            assert classes(browser) == {'running', 'active'}
            click(browser)
            assert not only.is_minimized(), 'clicking the focused app never hides it'

            # Launching: Applications search and a closed pinned app use ordinary actions (the
            # browser restores its session); intentional new windows use explicit ones.
            click(panel.applications)
            click(files)
            click(browser, 2)
            assert launched() == ['applications', 'files', 'browser-new'], launched()
            click(browser, 3)
            entry(panel.menu, 'New window').activate()
            close_menu()
            click(terminal, 3)
            entry(panel.menu, 'New window').activate()
            close_menu()
            click(files, 3)
            entry(panel.menu, 'Open Files').activate()
            close_menu()
            settle()
            assert launched()[3:] == ['browser-new', 'terminal', 'files'], launched()

            # Keyboard: Menu key on a focused item opens its menu with the first entry selected.
            subprocess.run(['xdotool', 'windowfocus', '--sync', str(panel.get_window().get_xid())], check=True)
            settle()
            terminal.grab_focus()
            key('Menu')
            assert panel.menu and panel.menu.get_visible(), 'keyboard opens the menu'
            assert panel.menu.get_selected_item() is not None
            key('Escape')
            assert not (panel.menu and panel.menu.get_visible())
            key('Return')
            assert panel.screen.get_active_window().get_name() == 'Konsole'

            # Clock, geometry and reserved work area.
            assert len(panel.time.get_text()) == 5 and ' · ' in panel.date.get_text(), panel.date.get_text()
            assert dock.timezone_name() in panel.clock.get_tooltip_text() and 'UTC' in panel.clock.get_tooltip_text()
            xid = str(panel.get_window().get_xid())
            strut = subprocess.check_output(['xprop', '-id', xid, '_NET_WM_STRUT_PARTIAL'], text=True)
            assert f'0, 0, 0, {dock.HEIGHT}' in strut, strut
            assert panel.get_size().width == WIDTH, panel.get_size()
            assert panel.get_position().root_y == top, panel.get_position()
            assert panel.apps_label.get_visible() == (WIDTH >= dock.COMPACT)
            windows['Editor'].iconify()
            windows['Chromium one'].maximize()
            windows['Chromium one'].present()
            settle()
            assert 'minimized' in classes(editor) and 'active' in classes(browser)
            window = windows['Chromium one']
            assert window.get_position().root_y + window.get_size().height <= top
            windows['Chromium one'].unmaximize()
            windows['Editor'].deiconify()
            windows['Chromium one'].present()
            # Park the pointer in the empty top-right corner so no tooltip covers the dock.
            subprocess.run(['xdotool', 'mousemove', str(WIDTH - 8), '8'], check=True)
            settle(1)
            assert not any(isinstance(w, Gtk.Window) and w.get_type_hint() == Gdk.WindowTypeHint.TOOLTIP
                           and w.get_visible() for w in Gtk.Window.list_toplevels()), 'tooltip visible in screenshot'
            final = shot('kindred-dock-final.png')

            # Overflow: many apps scroll; the clock and Applications stay put.
            extra = []
            for n in range(14):
                window = Gtk.Window(title=f'Tool {n}')
                window.set_wmclass(f'tool{n}', f'Tool{n}')
                window.set_default_size(200, 120)
                window.show_all()
                extra.append(window)
            settle(1.2)
            adjustment = panel.scroll.get_hadjustment()
            clock = panel.clock.get_allocation()
            cx = panel.clock.translate_coordinates(panel, 0, 0)[0]
            assert cx + clock.width <= WIDTH and panel.clock.get_mapped()
            if WIDTH < 1400:
                assert adjustment.get_upper() > adjustment.get_page_size(), 'icons overflow into a scroller'
                last = panel.items['tool13']
                subprocess.run(['xdotool', 'windowfocus', '--sync', xid], check=True)
                last.grab_focus()
                settle()
                assert adjustment.get_value() > 0, 'keyboard focus scrolls the item into view'
                shot('kindred-dock-overflow.png')
            for window in extra:
                window.destroy()

            for window in windows.values():
                window.destroy()
            settle(1)
            assert not panel.groups.get('browser') and 'editor' not in panel.items
            assert not panel.divider.get_visible() and not browser.dots.get_children()
            print(f'PASS at {WIDTH}px: grouping, dots, active/minimized states, chooser, manage menu, '
                  'restore, launches, keyboard, clock, geometry, work area, overflow, cleanup. '
                  f'Screenshot: {final}')
            panel.destroy()
        finally:
            wm.terminate()
            wm.wait(timeout=5)
            x.terminate()
            x.wait(timeout=5)


if __name__ == '__main__':
    test()
