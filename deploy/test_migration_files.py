import importlib.util
import json
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
import uuid

spec = importlib.util.spec_from_file_location('files', Path(__file__).with_name('migration-files.py'))
files = importlib.util.module_from_spec(spec)
spec.loader.exec_module(files)
journal = files.journal


class MigrationFilesTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source_root = self.root / 'source'
        self.target_root = self.root / 'target'
        self.source_root.mkdir()
        self.target_root.mkdir()
        self.id = str(uuid.uuid4())
        self.db = sqlite3.connect(self.source_root / 'kindred.db')
        self.addCleanup(self.db.close)
        self.db.executescript('''PRAGMA journal_mode=WAL;
            CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT);
            CREATE TABLE bots(id TEXT PRIMARY KEY,profile TEXT,memory TEXT);
            CREATE TABLE runs(id TEXT PRIMARY KEY,status TEXT,output TEXT);
            CREATE TABLE chats(id TEXT PRIMARY KEY,body TEXT);
            CREATE TABLE local_devices(id TEXT PRIMARY KEY,secret TEXT,mode TEXT);
            CREATE TABLE local_requests(id TEXT PRIMARY KEY,status TEXT,result TEXT);
            CREATE TABLE mail_receipts(id TEXT PRIMARY KEY,message TEXT);
            CREATE TABLE slack_receipts(id TEXT PRIMARY KEY,message TEXT);
            CREATE TABLE memory_overflow(id TEXT PRIMARY KEY,body TEXT);
            CREATE TABLE memory_snapshots(id TEXT PRIMARY KEY,body TEXT);
            CREATE TABLE mobile_devices(id TEXT PRIMARY KEY,token TEXT);
            CREATE TABLE mobile_push_outbox(id TEXT PRIMARY KEY);
            CREATE TABLE mobile_push_cursor(id INTEGER PRIMARY KEY);
            CREATE TABLE screens(id TEXT PRIMARY KEY,takeover INTEGER,quiet_until INTEGER);
            CREATE TABLE workspace_transfers(id TEXT PRIMARY KEY);
        ''')
        self.db.execute("INSERT INTO bots VALUES('bot',?,?)", (json.dumps({'local_access': True}), 'Yesterday was completed'))
        self.db.execute("INSERT INTO runs VALUES('run','completed','Verified completion')")
        self.db.execute("INSERT INTO chats VALUES('chat','Recent message in WAL')")
        self.db.execute("INSERT INTO settings VALUES('general',?)", (json.dumps({'local_access': True, 'theme': 'dark'}),))
        self.db.execute("INSERT INTO local_devices VALUES('device','synthetic-device-secret','full')")
        self.db.execute("INSERT INTO local_requests VALUES('request','completed','Preserved receipt')")
        self.db.execute("INSERT INTO mobile_devices VALUES('phone','synthetic-push-binding')")
        self.db.execute("INSERT INTO mail_receipts VALUES('mail','Already handled')")
        self.db.execute("INSERT INTO slack_receipts VALUES('slack','Already handled')")
        self.db.execute("INSERT INTO memory_overflow VALUES('new','Newly saved facts')")
        self.db.execute("INSERT INTO memory_snapshots VALUES('old','Original memory')")
        files.set_gate(self.db, self.id, 'source')
        self.db.commit()
        self.source = journal.Journal(self.source_root, self.id)
        self.target = journal.Journal(self.target_root, self.id)
        self.source.begin_source({'server': 'https://target.invalid', 'profile': str(uuid.uuid4())})

    def copy(self, manifest):
        self.target.begin_destination(manifest, {'server': 'https://source.invalid', 'profile': str(uuid.uuid4())})
        for index, item in enumerate(manifest['files']):
            offset = 0
            while offset < item['size']:
                data = files.read_chunk(self.source, index, offset)
                offset = self.target.chunk(index, offset, data, journal.digest(data))
        self.target.verify_files()

    def test_full_snapshot_keeps_wal_memory_monitors_and_execution_receipts(self):
        (self.source_root / 'kindred.db.credentials').write_text('{"example":"synthetic-provider-key"}')
        manifest = files.snapshot(self.source, 'test')
        self.copy(manifest)
        restored = files.verified_database(self.target)
        with files.database(restored) as connection:
            for table in ('chats', 'mail_receipts', 'slack_receipts', 'memory_overflow', 'memory_snapshots', 'runs'):
                self.assertEqual(connection.execute(f'SELECT * FROM {table}').fetchall(), self.db.execute(f'SELECT * FROM {table}').fetchall())
            self.assertEqual(connection.execute('SELECT secret,mode FROM local_devices').fetchone(), ('', 'off'))
            self.assertEqual(connection.execute('SELECT result FROM local_requests').fetchone(), ('Preserved receipt',))
            self.assertEqual(connection.execute('SELECT count(*) FROM mobile_devices').fetchone(), (0,))
            self.assertEqual(json.loads(connection.execute('SELECT profile FROM bots').fetchone()[0])['local_access'], False)
            self.assertTrue(files.gate_matches(connection, self.id))
        self.assertEqual(self.db.execute('SELECT secret FROM local_devices').fetchone(), ('synthetic-device-secret',))
        self.assertEqual(self.db.execute('SELECT count(*) FROM mobile_devices').fetchone(), (1,))
        self.assertTrue((self.source.root / 'source-backup.db').is_file())
        self.assertIn('kindred.db.credentials', [item['name'] for item in manifest['files']])
        self.assertNotIn('kindred.db-wal', [item['name'] for item in manifest['files']])

    def test_source_rejects_snapshot_without_fence_or_with_active_work(self):
        self.db.execute("DELETE FROM settings WHERE key='_migration_gate'")
        self.db.commit()
        with self.assertRaisesRegex(ValueError, 'Pause source'):
            files.snapshot(self.source, 'test')
        files.set_gate(self.db, self.id, 'source')
        self.db.execute("UPDATE runs SET status='running'")
        self.db.commit()
        with self.assertRaisesRegex(ValueError, 'Finish active work'):
            files.snapshot(self.source, 'test')
        self.assertFalse((self.source.root / 'manifest.json').exists())

    def test_account_credentials_conflict_never_overwrites_destination(self):
        source_keys = self.root / 'source-account.json'
        source_keys.write_text('{"openai":"source-synthetic"}')
        manifest = files.snapshot(self.source, 'test', source_keys)
        self.copy(manifest)
        destination = self.root / 'destination-account.json'
        destination.write_text('{"openai":"different-synthetic","other":"keep"}')
        before = destination.read_bytes()
        with self.assertRaisesRegex(ValueError, 'different API credentials'):
            files.account_key_merge(self.target, destination)
        self.assertEqual(destination.read_bytes(), before)
        destination.write_text('{"other":"keep"}')
        self.assertEqual(files.account_key_merge(self.target, destination), {'other': 'keep', 'openai': 'source-synthetic'})
        self.assertEqual(destination.read_text(), '{"other":"keep"}')

    def test_tampered_database_and_counts_do_not_pass_readiness(self):
        manifest = files.snapshot(self.source, 'test')
        self.copy(manifest)
        path = files.verified_database(self.target)
        with files.database(path) as connection:
            connection.execute("UPDATE chats SET body='Changed'")
        with self.assertRaisesRegex(ValueError, 'checksum'):
            files.verified_database(self.target)
        with self.assertRaisesRegex(ValueError, 'verification'):
            self.target.verify_files()

    @unittest.skipUnless(shutil.which('qemu-img'), 'qemu-img required for real disk-chain test')
    def test_real_qcow2_chain_is_flattened_without_changing_original_disks(self):
        computer = self.source_root / 'computer'
        computer.mkdir()
        base = self.root / 'base.qcow2'
        disk = computer / 'disk.qcow2'
        subprocess.run(['qemu-img', 'create', '-f', 'qcow2', str(base), '16M'], check=True, stdout=subprocess.DEVNULL)
        subprocess.run(['qemu-img', 'create', '-f', 'qcow2', '-F', 'qcow2', '-b', str(base), str(disk)], check=True, stdout=subprocess.DEVNULL)
        hashes = [journal.file_digest(path) for path in (base, disk)]
        for name in files.COMPUTER_FILES:
            (computer / name).write_text('synthetic identity')
        (self.source_root / '.migration-vm.json').write_text(json.dumps({'id': self.id}))
        manifest = files.snapshot(self.source, 'test')
        flat = self.source.root / 'snapshot/computer/disk.qcow2'
        info = json.loads(subprocess.check_output(['qemu-img', 'info', '--output=json', str(flat)]))
        self.assertNotIn('backing-filename', info)
        subprocess.run(['qemu-img', 'compare', str(disk), str(flat)], check=True, stdout=subprocess.DEVNULL)
        self.assertEqual([journal.file_digest(path) for path in (base, disk)], hashes)
        self.assertTrue(manifest['computer'])
        self.assertIn('computer/ssh_client', [item['name'] for item in manifest['files']])


if __name__ == '__main__':
    unittest.main()
