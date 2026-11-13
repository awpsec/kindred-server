import importlib.util
import json
from pathlib import Path
import sqlite3
import unittest
from unittest.mock import patch
import uuid
import test_migration_files as fixtures

spec = importlib.util.spec_from_file_location('worker', Path(__file__).with_name('migration-worker.py'))
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class MigrationSchemaTests(unittest.TestCase):
    def fingerprint(self, sql):
        connection = sqlite3.connect(':memory:')
        self.addCleanup(connection.close)
        connection.executescript(sql)
        return worker.schema(connection)

    def test_fresh_and_incrementally_upgraded_column_order_match(self):
        upgraded = self.fingerprint('CREATE TABLE chats(id TEXT PRIMARY KEY, pinned INTEGER NOT NULL DEFAULT 0); '
                                    'ALTER TABLE chats ADD COLUMN bot_only INTEGER NOT NULL DEFAULT 0;')
        fresh = self.fingerprint('CREATE TABLE chats(id TEXT PRIMARY KEY, bot_only INTEGER NOT NULL DEFAULT 0, '
                                 'pinned INTEGER NOT NULL DEFAULT 0);')
        self.assertEqual(upgraded, fresh)

    def test_semantic_column_and_primary_key_differences_still_reject(self):
        original = self.fingerprint('CREATE TABLE chats(a TEXT, b INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(a,b));')
        for sql in [
            'CREATE TABLE chats(a TEXT, b TEXT NOT NULL DEFAULT 0, PRIMARY KEY(a,b));',
            'CREATE TABLE chats(a TEXT, b INTEGER DEFAULT 0, PRIMARY KEY(a,b));',
            'CREATE TABLE chats(a TEXT, b INTEGER NOT NULL DEFAULT 1, PRIMARY KEY(a,b));',
            'CREATE TABLE chats(a TEXT, b INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(b,a));',
            'CREATE TABLE chats(a TEXT, b INTEGER NOT NULL DEFAULT 0, extra TEXT, PRIMARY KEY(a,b));',
        ]:
            with self.subTest(sql=sql):
                self.assertNotEqual(original, self.fingerprint(sql))


class MigrationWorkerTests(unittest.TestCase):
    def setUp(self):
        self.fixture = fixtures.MigrationFilesTests()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        f = self.fixture
        self.source = f.source_root
        self.target = f.target_root
        self.id = f.id
        self.profile = str(uuid.uuid4())
        self.peer = f.source.read()['peer']
        self.keys = f.root / 'account.credentials'
        with worker.files.database(self.target / 'kindred.db') as target:
            f.db.backup(target)
            for table in ('bots', 'runs', 'chats', 'settings'):
                target.execute(f'DELETE FROM {table}')

    def call(self, root, action, **kw):
        return worker.dispatch(root, self.profile, {'id': self.id, 'action': action, **kw}, 'test', self.keys)

    def prepared(self):
        package = self.call(self.source, 'prepare', peer=self.peer)
        reserve = self.call(self.target, 'reserve', manifest=package['manifest'], peer={'server': 'https://source.invalid', 'profile': self.profile})
        self.call(self.source, 'bind', peer=self.peer, receipt=reserve['receipt'])
        self.package = package
        self.reserve = reserve
        return package

    def verified(self):
        package = self.prepared()
        for index, item in enumerate(package['manifest']['files']):
            offset = 0
            while offset < item['size']:
                chunk = self.call(self.source, 'read', index=index, offset=offset)
                offset = self.call(self.target, 'write', index=index, offset=offset, **chunk)['offset']
        self.ready = self.call(self.target, 'verify')
        return self.ready

    def restore(self, prepared):
        with worker.files.database(prepared['database']) as source:
            with worker.files.database(self.target / 'kindred.db') as target:
                source.backup(target)

    def test_full_cutover_and_lost_activation_reply_preserve_new_work(self):
        ready = self.verified()
        proof = self.call(self.source, 'commit', receipt=ready['receipt'])
        activation = self.call(self.target, 'activate_prepare', proof=proof)
        self.restore(activation)
        # Lost reply immediately after DB restore: replay skips restoration.
        self.assertTrue(self.call(self.target, 'activate_prepare', proof=proof)['already_restored'])
        self.assertEqual(self.call(self.target, 'activated')['state'], 'active')
        with worker.files.database(self.target / 'kindred.db') as db:
            db.execute("INSERT INTO chats VALUES('after','Work after cutover')")
        self.assertTrue(self.call(self.target, 'activate_prepare', proof=proof)['already_restored'])
        self.call(self.target, 'activated')
        with worker.files.database(self.target / 'kindred.db') as db:
            self.assertEqual(db.execute("SELECT body FROM chats WHERE id='after'").fetchone()[0], 'Work after cutover')
            self.assertFalse(worker.files.gate_matches(db, self.id))
        self.assertTrue(worker.files.gate_matches(self.fixture.db, self.id))
        with self.assertRaisesRegex(ValueError, 'active'):
            self.call(self.target, 'abort')
        with self.assertRaisesRegex(ValueError, 'inactive'):
            self.call(self.source, 'cancel')

    def test_cancel_after_lost_commit_reply_requires_destination_abort(self):
        ready = self.verified()
        proof = self.call(self.source, 'commit', receipt=ready['receipt'])
        with self.assertRaisesRegex(ValueError, 'inactive'):
            self.call(self.source, 'cancel')
        aborted = self.call(self.target, 'abort')
        self.call(self.source, 'cancel', proof=aborted)
        self.assertFalse(worker.files.gate_matches(self.fixture.db, self.id))
        self.assertFalse((self.source / '.migration-vm.json').exists())
        with self.assertRaisesRegex(ValueError, 'activation'):
            self.call(self.target, 'activate_prepare', proof=proof)
        self.assertEqual(self.call(self.source, 'cancel', proof=aborted)['state'], 'cancelled')

    def test_cancel_signal_interrupts_preparation_without_losing_original(self):
        self.call(self.source, 'cancel_signal')
        with self.assertRaisesRegex(ValueError, 'cancellation'):
            self.call(self.source, 'prepare', peer=self.peer)
        self.call(self.source, 'cancel')
        self.assertEqual(self.fixture.db.execute('SELECT body FROM chats').fetchone()[0], 'Recent message in WAL')
        self.assertFalse(worker.files.gate_matches(self.fixture.db, self.id))

    def test_destination_existing_work_and_version_mismatch_rejected(self):
        package = self.call(self.source, 'prepare', peer=self.peer)
        package['manifest']['server_version'] = 'different'
        with self.assertRaisesRegex(ValueError, 'same version'):
            self.call(self.target, 'reserve', manifest=package['manifest'], peer=self.peer)
        package['manifest']['server_version'] = 'test'
        with worker.files.database(self.target / 'kindred.db') as db:
            db.execute("INSERT INTO bots VALUES('existing','{}','keep')")
        with self.assertRaisesRegex(ValueError, 'empty destination'):
            self.call(self.target, 'reserve', manifest=package['manifest'], peer=self.peer)

    def test_account_keys_changed_during_copy_stop_cutover(self):
        ready = self.verified()
        self.keys.write_text('{"openai":"new-synthetic-key"}')
        with self.assertRaisesRegex(ValueError, 'keys changed'):
            self.call(self.source, 'commit', receipt=ready['receipt'])
        self.assertEqual(self.call(self.source, 'status')['state'], 'prepared')

    def test_restart_status_is_public_and_absent_copy_can_cancel(self):
        self.assertEqual(self.call(self.target, 'status')['state'], 'absent')
        self.prepared()
        status = self.call(self.source, 'status')
        text = json.dumps(status)
        self.assertNotIn('activation_secret', text)
        self.assertNotIn('abort_secret', text)
        self.assertEqual(status['state'], 'prepared')
        self.assertGreater(len(status['steps']), 0)

    def test_changed_validated_computer_is_not_replaced_on_activation_retry(self):
        self.verified()
        # install_staged_files is complete before the isolated validation boot.
        path = self.target / 'computer/disk.qcow2'
        path.parent.mkdir(exist_ok=True)
        path.write_text('Disk changes from validation boot')
        worker.install_staged_files(self.fixture.target, self.profile)
        self.assertEqual(path.read_text(), 'Disk changes from validation boot')



class ComputerMigrationFenceTests(unittest.TestCase):
    def test_database_gate_blocks_computer_when_file_fence_was_not_written(self):
        import tempfile
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);computer=root/'computer';computer.mkdir()
            with worker.files.database(root/'kindred.db') as connection:
                connection.execute('CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT)')
                worker.files.set_gate(connection,str(uuid.uuid4()),'source')
            manager=worker.vm_manager()
            self.assertTrue(manager.migration_paused(computer))
            with self.assertRaisesRegex(ValueError,'paused for workspace migration'):
                manager.start(computer,str(uuid.uuid4()))
            with worker.files.database(root/'kindred.db') as connection:
                connection.execute('DELETE FROM settings')
            self.assertFalse(manager.migration_paused(computer))

if __name__=='__main__':unittest.main()
