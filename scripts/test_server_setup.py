import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec=importlib.util.spec_from_file_location('setup',Path(__file__).with_name('server-setup.py'))
setup=importlib.util.module_from_spec(spec);spec.loader.exec_module(setup)


class ServerSetup(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name)
        for name in ('compose.yaml','compose.kvm.yaml','custom.yaml'):(self.root/name).write_text('services: {}\n')
        self.device=self.root/'kvm';self.device.touch()
        self.env=patch.dict(os.environ,{},clear=True);self.env.start();self.addCleanup(self.env.stop)

    def test_actual_group_persisted_and_plain_updates_keep_overlay(self):
        original='KINDRED_PUBLIC_URL=https://existing.example\nSECRET=unchanged\n'
        (self.root/'.env').write_text(original)
        result=setup.configure_kvm(self.root,['compose.yaml','custom.yaml'],self.device)
        saved=(self.root/'.env').read_text()
        self.assertIn('COMPOSE_FILE="compose.yaml:custom.yaml:compose.kvm.yaml"',saved)
        self.assertIn('KINDRED_KVM_GID="'+str(self.device.stat().st_gid)+'"',saved)
        self.assertTrue(saved.startswith(original))
        self.assertEqual((self.root/result['backup']).read_text(),original)
        self.assertEqual((self.root/result['backup']).stat().st_mode&0o777,0o600)
        self.assertEqual((self.root/'.env').stat().st_mode&0o777,0o600)
        self.assertFalse(setup.configure_kvm(self.root,[],self.device)['changed'])

    def test_existing_overlays_preserved_without_network_migration(self):
        (self.root/'.env').write_text('COMPOSE_FILE=custom.yaml:compose.yaml\n')
        result=setup.configure_kvm(self.root,[],self.device)
        self.assertEqual(result['compose_files'],['custom.yaml','compose.yaml','compose.kvm.yaml'])
        with self.assertRaisesRegex(ValueError,'already configured'):setup.configure_kvm(self.root,['compose.yaml'],self.device)

    def test_missing_device_base_overlay_shell_override_and_symlink_fail_without_writes(self):
        with self.assertRaisesRegex(ValueError,'missing'):setup.configure_kvm(self.root,['compose.yaml'],self.root/'absent')
        with self.assertRaisesRegex(ValueError,'Specify'):setup.configure_kvm(self.root,[],self.device)
        with self.assertRaisesRegex(ValueError,'must exist'):setup.configure_kvm(self.root,['unknown.yaml'],self.device)
        with patch.dict(os.environ,{'COMPOSE_FILE':'other.yaml'}):
            with self.assertRaisesRegex(ValueError,'overrides'):setup.configure_kvm(self.root,['compose.yaml'],self.device)
        self.assertFalse((self.root/'.env').exists())
        (self.root/'.env').symlink_to(self.root/'custom.yaml')
        with self.assertRaisesRegex(ValueError,'redirected'):setup.configure_kvm(self.root,['compose.yaml'],self.device)

    def test_doctor_has_no_mutations_or_secret_output_and_detects_old_checkout(self):
        (self.root/'.env').write_text('SECRET=not-for-output\n')
        (self.root/'Cargo.toml').write_text('[package]\nversion = "0.83.0"\n')
        report={'kvm_accessible':False,'running_binary_version':'kindred 0.87.2'}
        with patch.object(setup.subprocess,'run',return_value=subprocess.CompletedProcess([],0,json.dumps(report),'')) as execute:
            result=setup.doctor(self.root)
            args=execute.call_args.args[0]
            self.assertEqual(args[:7],['docker','compose','exec','-T','server','python3','-c'])
        self.assertTrue(any('versions differ' in warning for warning in result['warnings']))
        self.assertNotIn('not-for-output',json.dumps(result))
        self.assertEqual((self.root/'.env').read_text(),'SECRET=not-for-output\n')
        self.assertEqual(list(self.root.glob('.env.kindred*')),[])

    def test_compose_failure_does_not_echo_config_or_error_secrets(self):
        with patch.object(setup.subprocess,'run',return_value=subprocess.CompletedProcess([],1,'secret env dump','secret credentials')):
            result=setup.doctor(self.root)
        self.assertNotIn('secret',json.dumps(result).lower())


if __name__=='__main__':unittest.main()
