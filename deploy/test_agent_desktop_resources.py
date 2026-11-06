import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec=importlib.util.spec_from_file_location('installer',Path(__file__).with_name('install-agent-desktop.py'))
installer=importlib.util.module_from_spec(spec);spec.loader.exec_module(installer)

class PackagedResources(unittest.TestCase):
    def check_layout(self,packaged):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);base=root if packaged else root/'deploy';base.mkdir(exist_ok=True)
            (base/'agent-desktop-manifest.json').write_text('{"test":"synthetic"}')
            (base/'agent-desktop-launch.py').write_text('launcher fixture')
            (base/'agent-desktop-skill.md').write_text('skill fixture')
            license_path=root/'third-party/agent-desktop/LICENSE.txt';license_path.parent.mkdir(parents=True);license_path.write_text('required notice')
            with patch.object(installer,'__file__',str(base/'install-agent-desktop.py')):
                self.assertEqual(installer.resources(),{'manifest':{'test':'synthetic'},'launcher':'launcher fixture','skill':'skill fixture','license':'required notice'})
                license_path.unlink()
                with self.assertRaises(FileNotFoundError):installer.resources()
    def test_packaged_guest_media_includes_required_notice(self):self.check_layout(True)
    def test_source_checkout_resources_are_unchanged(self):self.check_layout(False)

if __name__=='__main__':unittest.main()
