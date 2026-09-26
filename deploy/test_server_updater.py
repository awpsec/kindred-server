import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('updater',Path(__file__).with_name('server-updater.py'))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)

class SupervisorTests(unittest.TestCase):
    def test_only_valid_stable_versions(self):
        for value in ['',None,'1.2','../1.2.3','1.2.3;reboot','1.2.3-rc1','https://example.org']:
            with self.assertRaises(ValueError):m.version(value)
        self.assertEqual(m.version('0.83.0'),(0,83,0))

    def test_stale_or_downgrade_requests_never_start(self):
        u=m.Updater({'mode':'native','binary':'/unused'})
        with patch.object(m,'release',return_value={'version':'0.83.0'}),patch.object(u,'current_version',return_value='0.84.0'),patch.object(m.threading,'Thread') as thread:
            for requested in ['0.82.0','0.83.0']:
                with self.assertRaises(ValueError):u.start(requested)
            thread.assert_not_called()

    def test_duplicate_start_keeps_one_operation(self):
        u=m.Updater({'mode':'native','binary':'/unused'})
        with patch.object(m,'release',return_value={'version':'0.83.0'}),patch.object(u,'current_version',return_value='0.82.0'),patch.object(m.threading,'Thread') as thread:
            first=u.start('0.83.0');second=u.start('0.83.0')
            self.assertEqual(first['operation'],second['operation']);thread.assert_called_once()

    def test_compose_updates_only_server_and_keeps_volumes(self):
        u=m.Updater({'mode':'compose','compose_directory':'/fixture'})
        u.target={'version':'0.83.0'};u.operation='test';calls=[]
        def run(args,**kwargs):
            calls.append(args)
            return json.dumps({'services':{'server':{'image':'ghcr.io/awpsec/kindred-server:latest'}}}) if args[-2:]==['--format','json'] else ''
        with patch.object(m,'run',side_effect=run),patch.object(u,'ready'):
            u.update()
        self.assertEqual(u.phase,'complete')
        self.assertIn(['docker','pull','ghcr.io/awpsec/kindred-server:0.83.0'],calls)
        self.assertTrue(any(args[-4:]==['up','-d','--no-deps','server'] for args in calls))
        self.assertFalse(any('down' in args for args in calls))
        self.assertTrue(any('python3' in args and 'backup' in ''.join(args) for args in calls))

    def test_compose_refuses_owner_version_pin(self):
        u=m.Updater({'mode':'compose','compose_directory':'/fixture'})
        u.target={'version':'0.83.0'};u.operation='test'
        with patch.object(m,'run',return_value=json.dumps({'services':{'server':{'image':'ghcr.io/awpsec/kindred-server:0.81.0'}}})) as run:
            u.update()
        self.assertEqual(u.phase,'failed');run.assert_called_once()

    def test_bad_digest_never_stops_server(self):
        import io
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);binary=root/'kindred';binary.write_text('old')
            u=m.Updater({'mode':'native','binary':str(binary),'backup_directory':str(root/'backups')});u.operation='test';u.target={'version':'0.83.0','url':'https://github.com/test','digest':'0'*64}
            response=io.BytesIO(b'bad bundle');response.headers={}
            with patch.object(m.urllib.request,'urlopen',return_value=response),patch.object(m,'run') as run:
                with self.assertRaisesRegex(ValueError,'checksum'):u.native_update()
                run.assert_not_called()
            self.assertEqual(binary.read_text(),'old')

    def test_failed_health_restores_binary_and_database(self):
        import io,zipfile,hashlib
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);binary=root/'kindred';binary.write_text('old');data=root/'data';data.mkdir();db=data/'workspace.db';db.write_text('old database')
            archive=io.BytesIO()
            with zipfile.ZipFile(archive,'w') as z:z.writestr('kindred','new')
            payload=archive.getvalue();response=io.BytesIO(payload);response.headers={}
            u=m.Updater({'mode':'native','binary':str(binary),'data_directory':str(data),'backup_directory':str(root/'backups')});u.operation='test';u.target={'version':'0.83.0','url':'https://github.com/test','digest':hashlib.sha256(payload).hexdigest()}
            calls=[]
            def run(args,**kwargs):
                calls.append(args)
                if args[-1]=='--version':return 'kindred 0.83.0'
                if args[:2]==['systemctl','start'] and binary.read_text()=='new':db.write_text('migrated')
                return ''
            with patch.object(m.urllib.request,'urlopen',return_value=response),patch.object(m,'run',side_effect=run),patch.object(u,'ready',side_effect=RuntimeError('unhealthy')):
                with self.assertRaisesRegex(RuntimeError,'unhealthy'):u.native_update()
            self.assertEqual(binary.read_text(),'old');self.assertEqual(db.read_text(),'old database')
            self.assertEqual(calls[-1],['systemctl','start','kindred.service'])

if __name__=='__main__':unittest.main()
