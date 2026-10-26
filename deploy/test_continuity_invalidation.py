"""Run the shipped continuity migration against SQLite, without models or VMs."""
from pathlib import Path
import sqlite3
import unittest

MIGRATION=Path(__file__).resolve().parents[1]/'src/migrations/continuity-2.sql'


def fixture():
    c=sqlite3.connect(':memory:')
    c.executescript('''
      CREATE TABLE bots(id TEXT PRIMARY KEY,memory TEXT);
      CREATE TABLE chats(id TEXT PRIMARY KEY,members TEXT);
      CREATE TABLE runs(id TEXT PRIMARY KEY,bot_id TEXT,chat_id TEXT,status TEXT DEFAULT 'completed');
      CREATE TABLE chat_messages(seq INTEGER PRIMARY KEY,chat_id TEXT,sender TEXT,body TEXT,kind TEXT,run_id TEXT,suppressed INTEGER DEFAULT 0);
      CREATE TABLE events(seq INTEGER PRIMARY KEY,run_id TEXT,kind TEXT,body TEXT);
      CREATE TABLE run_message_sources(run_id TEXT PRIMARY KEY,message_seq INTEGER);
      CREATE TABLE followup_offers(id TEXT PRIMARY KEY,run_id TEXT,state TEXT);
      CREATE TABLE followup_offer_sources(source_run_id TEXT PRIMARY KEY,offer_id TEXT);
      INSERT INTO bots VALUES('bot','Keep existing rules');
      INSERT INTO chats VALUES('room','["bot"]');
      INSERT INTO runs VALUES('run','bot','room','completed');
    ''')
    c.executescript(MIGRATION.read_text())
    return c


class ContinuityInvalidation(unittest.TestCase):
    def test_hiding_activity_marker_does_not_invalidate_memory_or_history(self):
        self.check_visibility('task_progress')

    def test_hiding_checklist_does_not_invalidate_memory_or_history(self):
        self.check_visibility('checklist')

    def test_hiding_duplicate_artifact_card_does_not_invalidate_memory_or_history(self):
        self.check_visibility('workspace_artifact')

    def check_visibility(self,kind):
        c=fixture()
        c.execute("INSERT INTO chat_messages VALUES(1,'room','bot','persisted-record-id',?,'run',0)",[kind])
        c.execute('UPDATE chat_messages SET suppressed=1 WHERE seq=1')
        self.assertEqual(c.execute('SELECT count(*) FROM continuity_memory_guard').fetchone()[0],0)
        self.assertEqual(c.execute('SELECT count(*) FROM continuity_epochs').fetchone()[0],0)
        c.execute('UPDATE chat_messages SET suppressed=0 WHERE seq=1')
        self.assertEqual(c.execute('SELECT count(*) FROM continuity_memory_guard').fetchone()[0],0)
        self.assertEqual(c.execute('SELECT memory FROM bots').fetchone()[0],'Keep existing rules')

    def test_user_edit_hide_and_delete_still_invalidate(self):
        for operation in ["UPDATE chat_messages SET body='Corrected rule' WHERE seq=1",'UPDATE chat_messages SET suppressed=1 WHERE seq=1','DELETE FROM chat_messages WHERE seq=1']:
            with self.subTest(operation=operation):
                c=fixture();c.execute("INSERT INTO chat_messages VALUES(1,'room','user','Original rule','message','run',0)")
                c.execute(operation)
                self.assertEqual(c.execute('SELECT invalidated FROM continuity_memory_guard').fetchone()[0],1)
                self.assertEqual(c.execute('SELECT epoch FROM continuity_epochs').fetchone()[0],1)

    def test_presentation_body_changes_are_not_silently_exempted(self):
        c=fixture();c.execute("INSERT INTO chat_messages VALUES(1,'room','bot','original','task_progress','run',0)")
        c.execute("UPDATE chat_messages SET body='changed' WHERE seq=1")
        self.assertEqual(c.execute('SELECT invalidated FROM continuity_memory_guard').fetchone()[0],1)

    def test_execution_receipt_edit_and_delete_remain_protected(self):
        for operation in ["UPDATE events SET body='{}' WHERE seq=1",'DELETE FROM events WHERE seq=1']:
            with self.subTest(operation=operation):
                c=fixture();c.execute("INSERT INTO events VALUES(1,'run','tool_result','{\"text\":\"original result\"}')")
                c.execute(operation)
                self.assertEqual(c.execute('SELECT invalidated FROM continuity_memory_guard').fetchone()[0],1)

    def test_upgrade_replaces_old_trigger_and_remains_idempotent(self):
        c=fixture()
        c.executescript('''DROP TRIGGER continuity_source_changed;
          CREATE TRIGGER continuity_source_changed AFTER UPDATE OF suppressed ON chat_messages BEGIN
            INSERT OR REPLACE INTO continuity_memory_guard VALUES('bot',1);
          END;''')
        c.executescript(MIGRATION.read_text());c.executescript(MIGRATION.read_text())
        c.execute("INSERT INTO chat_messages VALUES(1,'room','bot','record','task_progress','run',0)")
        c.execute('UPDATE chat_messages SET suppressed=1 WHERE seq=1')
        self.assertEqual(c.execute('SELECT count(*) FROM continuity_memory_guard').fetchone()[0],0)

    def test_edit_and_withdraw_undelivered_message_preserve_existing_memory(self):
        c=fixture();c.execute("UPDATE runs SET status='queued'")
        c.execute("INSERT INTO chat_messages VALUES(1,'room','user','Unsent draft','message','',0)")
        c.execute("INSERT INTO run_message_sources VALUES('run',1)")
        c.execute("UPDATE chat_messages SET body='Edited unsent draft' WHERE seq=1")
        c.execute("UPDATE runs SET status='withdrawn'")
        c.execute("UPDATE chat_messages SET suppressed=1,kind='withdrawn' WHERE seq=1")
        self.assertEqual(c.execute('SELECT count(*) FROM continuity_memory_guard').fetchone()[0],0)
        self.assertEqual(c.execute('SELECT count(*) FROM continuity_epochs').fetchone()[0],0)

    def test_started_handed_off_unknown_and_partially_delivered_messages_remain_protected(self):
        for scenario in ['running','old-start','transported','missing-run','other-recipient']:
            with self.subTest(scenario=scenario):
                c=fixture();c.execute("UPDATE runs SET status='queued'")
                c.execute("INSERT INTO chat_messages VALUES(1,'room','user','Original rule','message','',0)")
                c.execute("INSERT INTO run_message_sources VALUES('run',1)")
                if scenario=='running':c.execute("UPDATE runs SET status='running'")
                if scenario=='old-start':c.execute("INSERT INTO events VALUES(1,'run','run_started','{}')")
                if scenario=='transported':
                    c.execute("INSERT INTO followup_offers VALUES('offer','parent','transported')")
                    c.execute("INSERT INTO followup_offer_sources VALUES('run','offer')")
                if scenario=='missing-run':c.execute('DELETE FROM runs')
                if scenario=='other-recipient':
                    c.execute("INSERT INTO runs VALUES('other','another-bot','room','completed')")
                    c.execute("INSERT INTO run_message_sources VALUES('other',1)")
                c.execute("UPDATE chat_messages SET body='Corrected rule' WHERE seq=1")
                self.assertEqual(c.execute('SELECT invalidated FROM continuity_memory_guard').fetchone()[0],1)


if __name__=='__main__':unittest.main()
