import copy
import json
import os
from pathlib import Path
import tempfile
import unittest
import network_settings as network


class NetworkSettings(unittest.TestCase):
    def setUp(self):
        self.folder=tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.root=Path(self.folder.name)
        self.env={'KINDRED_NETWORK_MODE':'host'}

    def saved_change(self,first,**settings):
        saved={key:copy.deepcopy(first[key]) for key in ('revision','settings','baseline')}
        saved['revision']='web-change'
        saved['settings'].update(settings)
        network.atomic_json(self.root/'network-settings.json',saved)
        return saved

    def test_local_default_and_unchanged_restart_preserves_web_settings(self):
        first=network.load(self.root,self.env)
        self.assertEqual(network.listeners(first['settings']),('127.0.0.1:9444',[]))
        self.saved_change(first,access='all',public_url='https://kindred.example',allowed_origins=['http://127.0.0.1:9444'])
        again=network.load(self.root,self.env)
        self.assertEqual(again['revision'],'web-change')
        self.assertEqual(again['settings']['access'],'all')
        self.assertEqual(again['settings']['public_url'],'https://kindred.example')
        self.assertEqual((self.root/'network-settings.json').stat().st_mode&0o777,0o600)

    def test_changed_compose_field_overrides_only_that_saved_field(self):
        first=network.load(self.root,self.env)
        self.saved_change(first,access='all',public_url='https://web.example',allowed_origins=['https://other.example'])
        result=network.load(self.root,{**self.env,'KINDRED_PUBLIC_URL':'https://compose.example'})
        self.assertEqual(result['settings']['public_url'],'https://compose.example')
        self.assertEqual(result['settings']['access'],'all')
        self.assertEqual(result['settings']['allowed_origins'],['https://other.example'])
        self.assertNotEqual(result['revision'],'web-change')

    def test_topology_changes_do_not_inherit_incompatible_access(self):
        first=network.load(self.root,{})
        self.assertEqual(first['settings']['access'],'bridge')
        host=network.load(self.root,self.env)
        self.assertEqual(host['settings']['access'],'local')
        self.saved_change(host,access='tailnet',tailnet_address='100.64.1.2')
        bridge=network.load(self.root,{})
        self.assertEqual(network.listeners(bridge['settings']),('0.0.0.0:9444',[]))
        self.assertEqual(bridge['settings']['tailnet_address'],'')

    def test_tailnet_keeps_loopback_for_health(self):
        first=network.load(self.root,self.env)
        for ip,address in [('100.64.1.2','100.64.1.2:9444'),('fd7a:115c:a1e0::1','[fd7a:115c:a1e0::1]:9444')]:
            settings={**first['settings'],'access':'tailnet','tailnet_address':ip}
            self.assertEqual(network.listeners(settings),('127.0.0.1:9444',[address]))
        for ip in ['192.168.1.2','100.128.1.2','::1','invalid']:
            with self.assertRaises(ValueError):network.listeners({**first['settings'],'access':'tailnet','tailnet_address':ip})

    def test_corrupt_state_never_silently_resets_to_an_exposed_default(self):
        path=self.root/'network-settings.json';path.write_text('{broken')
        with self.assertRaises(ValueError):network.load(self.root,self.env)
        self.assertEqual(path.read_text(),'{broken')

    def test_unrecognized_network_mode_does_not_write_settings(self):
        with self.assertRaises(ValueError):network.load(self.root,{'KINDRED_NETWORK_MODE':'typo'})
        self.assertFalse((self.root/'network-settings.json').exists())

    def test_invalid_compose_url_never_erases_saved_web_settings(self):
        first=network.load(self.root,self.env)
        self.saved_change(first,public_url='https://saved.example')
        before=(self.root/'network-settings.json').read_bytes()
        for value in ['not-a-url','http://public.example','https://user:secret@example.com','https://example.com/path']:
            with self.assertRaises(ValueError):network.load(self.root,{**self.env,'KINDRED_PUBLIC_URL':value})
            self.assertEqual((self.root/'network-settings.json').read_bytes(),before)

    def test_invalid_origins_are_rejected_before_persistence(self):
        for value in ['"https://example.com"','[1]','["javascript:alert(1)"]','["http://public.example"]']:
            with self.assertRaises(ValueError):network.load(self.root,{**self.env,'KINDRED_ALLOWED_ORIGINS':value})
            self.assertFalse((self.root/'network-settings.json').exists())


if __name__=='__main__':unittest.main()
