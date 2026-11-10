"""Run the production capped count SQL against isolated SQLite fixtures."""
from pathlib import Path
import re,sqlite3,unittest
ROOT=Path(__file__).resolve().parents[2]
SQL=re.search(r'c.query_row\("(SELECT count\(\*\) FROM \(SELECT p.message_seq[^"\n]+)"', (ROOT/'src/progress_updates.rs').read_text()).group(1)
class GatherCount(unittest.TestCase):
 def setUp(self):
  self.c=sqlite3.connect(':memory:');self.addCleanup(self.c.close)
  self.c.executescript('''CREATE TABLE runs(id TEXT PRIMARY KEY,bot_id TEXT,chat_id TEXT);CREATE TABLE chat_messages(seq INTEGER PRIMARY KEY,run_id TEXT,sender TEXT,chat_id TEXT,kind TEXT,suppressed INTEGER,body TEXT,source_event_seq INTEGER);CREATE TABLE message_progress(message_seq INTEGER PRIMARY KEY,run_id TEXT,mode TEXT,phase TEXT,work_qualified INTEGER,group_id TEXT);CREATE INDEX message_progress_group ON message_progress(group_id,message_seq);CREATE TABLE events(seq INTEGER PRIMARY KEY,run_id TEXT,body TEXT);INSERT INTO runs VALUES('run','bot','chat'),('other','other-bot','other-chat');''')
 def add(self,seq,group='group',run='run',sender='bot',chat='chat',kind='assistant',hidden=0,phase='commentary',qualified=1,body='Work update',mode='summaries'):
  self.c.execute('INSERT INTO chat_messages VALUES(?,?,?,?,?,?,?,?)',(seq,run,sender,chat,kind,hidden,body,seq))
  self.c.execute('INSERT INTO message_progress VALUES(?,?,?,?,?,?)',(seq,run,mode,phase,qualified,group))
 def count(self):return self.c.execute(SQL,('group','bot','chat')).fetchone()[0]
 def test_threshold_and_cap_ignore_page_boundaries(self):
  for i in range(1,9):self.add(i);self.assertEqual(self.count(),min(i,4))
 def test_foreign_and_noncommentary_rows_never_count(self):
  for i,kw in enumerate([dict(group='other'),dict(run='other'),dict(sender='other-bot'),dict(chat='other-chat'),dict(kind='result'),dict(kind='task_progress'),dict(hidden=1),dict(phase='final_answer'),dict(qualified=0),dict(body=' '),dict(mode='balanced')],1):self.add(i,**kw)
  self.assertEqual(self.count(),0)
 def test_suppression_and_final_correction_reduce_count(self):
  for i in range(1,5):self.add(i)
  self.assertEqual(self.count(),4);self.c.execute('UPDATE chat_messages SET suppressed=1 WHERE seq=4');self.assertEqual(self.count(),3)
  self.c.execute("UPDATE message_progress SET phase='final_answer' WHERE message_seq=3");self.assertEqual(self.count(),2)
 def test_diagnostics_do_not_promote_and_legacy_qualification_survives(self):
  self.add(1);self.c.execute('INSERT INTO events VALUES(1,\'run\',\'{"status_notice":"provider_error"}\')');self.add(2,qualified=None);self.assertEqual(self.count(),1)
 def test_group_lookup_uses_index(self):
  plan=' '.join(str(r) for r in self.c.execute('EXPLAIN QUERY PLAN '+SQL,('group','bot','chat')));self.assertIn('message_progress_group',plan)
if __name__=='__main__':unittest.main(verbosity=2)
