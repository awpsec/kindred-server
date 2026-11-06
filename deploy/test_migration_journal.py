"""Synthetic source/destination profiles; never touch a running Kindred server."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import uuid

spec = importlib.util.spec_from_file_location('migration', Path(__file__).with_name('migration-journal.py'))
migration = importlib.util.module_from_spec(spec)
spec.loader.exec_module(migration)


class MigrationJournalTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source_root = self.root / 'source'
        self.target_root = self.root / 'target'
        self.source_root.mkdir()
        self.target_root.mkdir()
        self.id = str(uuid.uuid4())
        self.source = migration.Journal(self.source_root, self.id)
        self.target = migration.Journal(self.target_root, self.id)
        self.peer = {'server': 'https://destination.invalid', 'profile': str(uuid.uuid4())}
        self.origin = {'server': 'https://source.invalid', 'profile': str(uuid.uuid4())}
        self.data = b'Synthetic profile state with yesterday\xe2\x80\x99s completion.'
        self.source.begin_source(self.peer)
        self.manifest = self.source.source_ready({'format': 'kindred-profile-migration', 'version': 1,
            'id': self.id, 'files': [{'name': 'kindred.db', 'size': len(self.data),
                                    'sha256': migration.digest(self.data)}]})

    def stage(self):
        receipt = self.target.begin_destination(self.manifest, self.origin)
        self.source.bind_destination(receipt, self.peer)
        return receipt

    def verified(self):
        self.stage()
        self.target.chunk(0, 0, self.data, migration.digest(self.data))
        self.target.verify_files()
        return self.target.ready({'database': True, 'computer': True, 'credentials': True})

    def test_lost_responses_and_restart_keep_exactly_one_cutover_identity(self):
        receipt = self.verified()
        proof = self.source.commit_source(receipt)
        source = migration.Journal(self.source_root, self.id)
        target = migration.Journal(self.target_root, self.id)
        self.assertEqual(source.commit_source(receipt), proof)
        self.assertEqual(source.status()['state'], 'committed')
        self.assertEqual(target.status()['state'], 'verified')
        target.begin_activation(proof)
        # Crash after beginning activation: replay resumes the same destination.
        target = migration.Journal(self.target_root, self.id)
        target.begin_activation(proof)
        target.activated()
        target.begin_activation(proof)
        target.activated()
        self.assertEqual(target.status()['state'], 'active')
        with self.assertRaises(ValueError):
            source.cancel_source()
        with self.assertRaises(ValueError):
            target.abort_destination()
        self.assertEqual(source.status()['state'], 'committed')

    def test_cancel_after_lost_commit_response_requires_destination_abort(self):
        receipt = self.verified()
        delayed_proof = self.source.commit_source(receipt)
        with self.assertRaises(ValueError):
            self.source.cancel_source()
        abort = self.target.abort_destination()
        self.assertEqual(self.target.abort_destination(), abort)
        self.source.cancel_source(abort)
        self.source.cancel_source(abort)
        with self.assertRaises(ValueError):
            self.target.begin_activation(delayed_proof)
        with self.assertRaises(ValueError):
            self.source.commit_source(receipt)
        self.assertEqual(self.source.status()['state'], 'cancelled')
        self.assertEqual(self.target.status()['state'], 'aborted')

    def test_incomplete_copy_and_forged_proofs_cannot_activate(self):
        receiving = self.stage()
        with self.assertRaises(ValueError):
            self.target.verify_files()
        with self.assertRaises(ValueError):
            self.source.commit_source(receiving)
        with self.assertRaises(ValueError):
            self.target.begin_activation({'id': self.id, 'digest': receiving['digest'], 'activation': 'fake'})
        with self.assertRaises(ValueError):
            self.source.cancel_source({'id': self.id, 'digest': receiving['digest'], 'abort': 'fake'})
        self.assertEqual(self.source.status()['state'], 'prepared')

    def test_partial_write_before_journal_update_resumes_without_duplication(self):
        self.stage()
        half = len(self.data) // 2
        self.target.chunk(0, 0, self.data[:half], migration.digest(self.data[:half]))
        path = self.target.root / 'files' / '0'
        with path.open('ab') as stream:
            stream.write(b'bytes from interrupted write')
        target = migration.Journal(self.target_root, self.id)
        self.assertEqual(target.chunk(0, 0, self.data[:half], migration.digest(self.data[:half])), half)
        target.chunk(0, half, self.data[half:], migration.digest(self.data[half:]))
        target.verify_files()
        self.assertEqual(path.read_bytes(), self.data)

    def test_bad_checksum_offset_and_overlap_preserve_saved_bytes(self):
        self.stage()
        self.target.chunk(0, 0, self.data[:10], migration.digest(self.data[:10]))
        for offset, data, checksum in [(10, b'x', 'f' * 64), (11, b'x', migration.digest(b'x')),
                                       (5, b'changed', migration.digest(b'changed')),
                                       (0, b'different!', migration.digest(b'different!'))]:
            with self.assertRaises(ValueError):
                self.target.chunk(0, offset, data, checksum)
        self.assertEqual((self.target.root / 'files' / '0').read_bytes(), self.data[:10])
        self.assertEqual(self.target.status()['files']['0'], 10)

    def test_failed_journal_write_never_releases_activation_secret(self):
        receipt = self.verified()
        with patch.object(migration, 'atomic', side_effect=OSError('Disk full')):
            with self.assertRaises(OSError):
                self.source.commit_source(receipt)
        self.assertEqual(self.source.status()['state'], 'prepared')
        proof = self.source.commit_source(receipt)
        self.assertEqual(self.source.status()['state'], 'committed')
        self.target.begin_activation(proof)

    def test_inventory_and_destination_identity_are_immutable(self):
        receipt = self.stage()
        with self.assertRaises(ValueError):
            self.source.bind_destination(dict(receipt, abort_hash='f' * 64), self.peer)
        with self.assertRaises(ValueError):
            self.source.bind_destination(receipt, dict(self.peer, server='https://other.invalid'))
        changed = json.loads(json.dumps(self.manifest))
        changed['files'][0]['size'] += 1
        with self.assertRaises(ValueError):
            self.target.begin_destination(changed, self.origin)
        with self.assertRaises(ValueError):
            self.source.source_ready(changed)

    def test_paths_cannot_escape_or_alias_staging(self):
        for path in ('../outside', '/absolute', 'a/../outside', 'a//b', 'a\\b', '.', 'a/./b'):
            with self.assertRaises(ValueError, msg=path):
                migration.relative_name(path)
        manifest = dict(self.manifest, files=[dict(self.manifest['files'][0], name='a'),
                                               dict(self.manifest['files'][0], name='a/b')])
        with self.assertRaises(ValueError):
            migration.manifest_files(manifest)
        with self.assertRaises(ValueError):
            migration.Journal(self.source_root, '../outside')

    def test_capacity_failure_and_cancel_before_copy_preserve_source(self):
        free = type('Usage', (), {'free': 0})()
        with patch.object(migration.shutil, 'disk_usage', return_value=free):
            with self.assertRaises(ValueError):
                self.target.begin_destination(self.manifest, self.origin)
        self.assertFalse(self.target.path.exists())
        self.source.cancel_source()
        self.assertEqual(self.source.status()['state'], 'cancelled')
        self.assertTrue((self.source.root / 'manifest.json').exists())

    def test_public_status_never_contains_cutover_secrets(self):
        self.stage()
        for journal, key in [(self.source, 'activation_secret'), (self.target, 'abort_secret')]:
            raw = journal.read()
            public = json.dumps(journal.status())
            self.assertNotIn(key, public)
            self.assertNotIn(raw[key], public)

    def test_cancelled_destination_cannot_accept_late_chunks_or_reservation(self):
        self.stage()
        self.target.abort_destination()
        with self.assertRaises(ValueError):
            self.target.chunk(0, 0, self.data, migration.digest(self.data))
        with self.assertRaises(ValueError):
            self.target.begin_destination(self.manifest, self.origin)
        self.assertFalse((self.target.root / 'files' / '0').exists())


if __name__ == '__main__':
    unittest.main()
