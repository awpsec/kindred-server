"""Exercise the exact production SQLite trigger without model calls or live data."""
from pathlib import Path
import json
import sqlite3
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / 'src/migrations/continuity-2.sql'

def trigger(sql):
    start = sql.index('DROP TRIGGER IF EXISTS continuity_source_changed;')
    return sql[start:sql.index('END;', start) + 4]

SCHEMA = '''
CREATE TABLE bots(id TEXT PRIMARY KEY, memory TEXT);
CREATE TABLE chats(id TEXT PRIMARY KEY, members TEXT);
CREATE TABLE chat_messages(seq INTEGER PRIMARY KEY, chat_id TEXT, sender TEXT, body TEXT, kind TEXT, run_id TEXT, suppressed INTEGER DEFAULT 0);
CREATE TABLE continuity_epochs(chat_id TEXT PRIMARY KEY,epoch INTEGER);
CREATE TABLE continuity_memory_guard(bot_id TEXT PRIMARY KEY,invalidated INTEGER);
CREATE TABLE runs(id TEXT PRIMARY KEY,status TEXT,error TEXT,output TEXT);
CREATE TABLE events(run_id TEXT,kind TEXT,body TEXT);
CREATE TABLE voice_calls(id TEXT PRIMARY KEY,chat_id TEXT,message_seq INTEGER,seconds INTEGER);
INSERT INTO bots VALUES('bot','Use the bot VM'),('teammate','Keep owner context');
INSERT INTO chats VALUES('chat','["bot","teammate"]');
INSERT INTO chat_messages VALUES(1,'chat','user','Call in progress','voice_call','',0);
INSERT INTO voice_calls VALUES('call','chat',1,83);
'''

class CallMemoryTests(unittest.TestCase):
    def setup_db(self, sql=None):
        c = sqlite3.connect(':memory:')
        self.addCleanup(c.close)
        c.executescript(SCHEMA)
        c.executescript(trigger(sql or SOURCE.read_text()))
        return c

    def assert_intact(self, c):
        self.assertEqual(c.execute('SELECT count(*) FROM continuity_memory_guard').fetchone()[0], 0)
        self.assertEqual(c.execute('SELECT count(*) FROM continuity_epochs').fetchone()[0], 0)
        self.assertEqual(c.execute("SELECT memory FROM bots WHERE id='bot'").fetchone()[0], 'Use the bot VM')

    def assert_invalid(self, c):
        self.assertEqual(c.execute('SELECT count(*) FROM continuity_memory_guard WHERE invalidated=1').fetchone()[0], 2)
        self.assertEqual(c.execute('SELECT epoch FROM continuity_epochs').fetchone()[0], 1)

    def test_published_trigger_reproduces_failure(self):
        old = subprocess.check_output(['git','-C',str(ROOT),'show','c4503cbb2a8891d4116b11c03e1f8d76b0feed85:src/migrations/continuity-2.sql'],text=True)
        c = self.setup_db(old)
        c.execute("UPDATE chat_messages SET body='Call ended · 1:23' WHERE seq=1")
        self.assert_invalid(c)

    def test_end_label_preserves_all_members_and_repeated_finish(self):
        for seconds in [0, 1, 59, 60, 83, 3601]:
            with self.subTest(seconds=seconds):
                c = self.setup_db()
                c.execute('UPDATE voice_calls SET seconds=?', [seconds])
                label = f'Call ended · {seconds // 60}:{seconds % 60:02}'
                for _ in range(2): c.execute('UPDATE chat_messages SET body=? WHERE seq=1', [label])
                self.assert_intact(c)

    def test_metadata_exemption_does_not_allow_source_edits(self):
        cases = [
            ("DELETE FROM voice_calls", 'Call ended · 1:23'),
            ("UPDATE voice_calls SET chat_id='other'", 'Call ended · 1:23'),
            ("UPDATE voice_calls SET message_seq=2", 'Call ended · 1:23'),
            ("UPDATE voice_calls SET seconds=-1", 'Call ended · -1:59'),
            ('', 'Call ended · 2:23'),
            ('', 'Changed user instruction'),
        ]
        for setup, label in cases:
            with self.subTest(setup=setup,label=label):
                c = self.setup_db()
                if setup: c.execute(setup)
                c.execute('UPDATE chat_messages SET body=? WHERE seq=1', [label])
                self.assert_invalid(c)

    def test_changed_identity_or_kind_is_not_metadata(self):
        for extra in [",sender='bot'", ",kind='message'", ",run_id='different'", ",chat_id='other'"]:
            with self.subTest(extra=extra):
                c = self.setup_db()
                c.execute("UPDATE chat_messages SET body='Call ended · 1:23'"+extra+' WHERE seq=1')
                self.assert_invalid(c)

    def test_later_edit_and_hiding_call_still_invalidate(self):
        for update in ["body='Withdrawn call'", 'suppressed=1']:
            with self.subTest(update=update):
                c=self.setup_db()
                c.execute("UPDATE chat_messages SET body='Call ended · 1:23' WHERE seq=1")
                self.assert_intact(c)
                c.execute('UPDATE chat_messages SET '+update+' WHERE seq=1')
                self.assert_invalid(c)

if __name__ == '__main__': unittest.main(verbosity=2)
