"""Exercise the runtime helper installation from both container recipes."""
import contextlib
import pathlib
import shlex
import sqlite3
import subprocess
import sys
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parent.parent


class MigrationPackagingTests(unittest.TestCase):
    def check_recipe(self, recipe):
        with tempfile.TemporaryDirectory() as temporary:
            stage = pathlib.Path(temporary)
            runtime = stage / 'runtime'
            runtime.mkdir()
            installed = False
            for line in recipe.read_text().splitlines():
                if not line.startswith('RUN '):
                    continue
                for command in line[4:].split('&&'):
                    args = shlex.split(command)
                    if not args or args[0] != 'install' or not any('vm-manager.py' in arg for arg in args):
                        continue
                    args = [arg.replace('/opt/kindred/source/deploy/', str(ROOT / 'deploy') + '/')
                            .replace('/usr/local/lib/kindred', str(runtime)) for arg in args]
                    subprocess.run(args, check=True, capture_output=True)
                    installed = True
            self.assertTrue(installed, 'Recipe must install the runtime helpers')
            profile = stage / 'profile'
            profile.mkdir()
            with contextlib.closing(sqlite3.connect(profile / 'kindred.db')) as db:
                db.executescript('CREATE TABLE bots(id TEXT); CREATE TABLE runs(status TEXT); '
                                 'CREATE TABLE settings(key TEXT,value TEXT);')
            # Import only from the installed directory: a copy under guest/source
            # cannot satisfy the server's sibling-of-vm-manager worker lookup.
            subprocess.run([sys.executable, '-c', '''
import importlib.util,pathlib,sys
runtime=pathlib.Path(sys.argv[1])
spec=importlib.util.spec_from_file_location('worker',runtime/'migration-worker.py')
worker=importlib.util.module_from_spec(spec);spec.loader.exec_module(worker)
result=worker.preflight(pathlib.Path(sys.argv[2]),'packaging-test')
assert result['empty'] and result['active_work']==0
assert worker.vm_manager() is not None
''', str(runtime), str(profile)], check=True, capture_output=True)

    def test_server_container_migration_preflight(self):
        self.check_recipe(ROOT / 'Dockerfile')

    def test_standalone_container_migration_preflight(self):
        self.check_recipe(ROOT / 'deploy/Dockerfile.standalone')


if __name__ == '__main__':
    unittest.main()
