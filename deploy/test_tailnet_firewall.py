"""The guest upgrade preserves custom rules and survives repeated updates."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('updater',Path(__file__).with_name('update-harnesses.py'))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)

class TailnetFirewall(unittest.TestCase):
    def test_migration_is_durable_idempotent_and_never_flushes_live_rules(self):
        template=Path(__file__).with_name('vm-manager.py').read_text().split("<<'RULES'\n",1)[1].split('\nRULES',1)[0]+'\n'
        rule='oifname "tailscale0" accept comment "kindred-tailnet"'
        self.assertLess(template.index(rule),template.index('100.64.0.0/10'))
        old=template.replace('  '+rule+'\n','')
        with tempfile.TemporaryDirectory() as directory:
            config=Path(directory)/'nftables.conf';config.write_text(old)
            with patch.object(m,'run') as run,patch.object(m.subprocess,'run',return_value=subprocess.CompletedProcess([],0,stdout=old)):
                m.update_tailnet_firewall(config)
                self.assertEqual(config.read_text(),template)
                self.assertEqual(config.with_name(config.name+'.before-kindred-tailnet').read_text(),old)
                self.assertEqual(run.call_args_list[-1].args[0],['nft','insert','rule','inet','kindred','output',rule])
                self.assertEqual(run.call_count,2)
            with patch.object(m,'run') as run,patch.object(m.subprocess,'run',return_value=subprocess.CompletedProcess([],0,stdout=template)):
                m.update_tailnet_firewall(config);run.assert_not_called()
            self.assertEqual(config.read_text(),template)
    def test_invalid_staging_preserves_original(self):
        with tempfile.TemporaryDirectory() as directory:
            config=Path(directory)/'nftables.conf';source='table inet kindred {\n  oifname "lo" accept\n';config.write_text(source)
            with patch.object(m,'run',side_effect=RuntimeError('invalid')):
                with self.assertRaises(RuntimeError):m.update_tailnet_firewall(config)
            self.assertEqual(config.read_text(),source)
            self.assertFalse(config.with_name(config.name+'.kindred-staged').exists())
    def test_external_policy_is_not_modified(self):
        with tempfile.TemporaryDirectory() as directory,patch.object(m,'run') as run:
            config=Path(directory)/'nftables.conf';config.write_text('table inet custom {}')
            m.update_tailnet_firewall(config);run.assert_not_called();self.assertEqual(config.read_text(),'table inet custom {}')

if __name__=='__main__':unittest.main()
