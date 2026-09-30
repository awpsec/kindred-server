#!/usr/bin/python3
"""Icon failures must not crash the desktop; no display or guest required."""
import importlib.machinery
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

loader = importlib.machinery.SourceFileLoader(
    'dock', str(Path(__file__).resolve().parents[1] / 'deploy/kindred-dock'))
spec = importlib.util.spec_from_loader(loader.name, loader)
dock = importlib.util.module_from_spec(spec)
loader.exec_module(dock)


class Icons(unittest.TestCase):
    def test_missing_svg_loader(self):
        with tempfile.TemporaryDirectory() as directory:
            assets = Path(directory)
            (assets / 'applications.svg').write_text('<svg/>')
            error = dock.GLib.Error('SVG loader unavailable')
            with patch.object(dock, 'ASSETS', assets), patch.object(
                dock.GdkPixbuf.Pixbuf, 'new_from_file_at_scale', side_effect=error
            ), patch.object(dock.GdkPixbuf.PixbufLoader, 'new_with_type', side_effect=error):
                icon = dock.icon_for('applications', 'Applications', size=28)
                self.assertEqual((icon.get_width(), icon.get_height()), (28, 28))
                self.assertTrue(icon.get_has_alpha())

    def test_corrupt_asset(self):
        with tempfile.TemporaryDirectory() as directory:
            assets = Path(directory)
            (assets / 'browser.svg').write_text('not an image')
            with patch.object(dock, 'ASSETS', assets):
                icon = dock.icon_for('browser', 'Browser', size=36)
                self.assertEqual(icon.get_width(), 36)


if __name__ == '__main__':
    unittest.main()
