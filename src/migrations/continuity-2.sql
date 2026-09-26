-- Additive, constant-work migration. No history scan or model calls here.
CREATE TABLE IF NOT EXISTS continuity_schema(version INTEGER PRIMARY KEY, applied INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS continuity_epochs(chat_id TEXT PRIMARY KEY REFERENCES chats(id), epoch INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS continuity_note_meta(bot_id TEXT NOT NULL REFERENCES bots(id),chat_id TEXT NOT NULL REFERENCES chats(id),topic TEXT NOT NULL,revision INTEGER NOT NULL,epoch INTEGER NOT NULL,kind TEXT NOT NULL,statement_kind TEXT NOT NULL,sources TEXT NOT NULL,PRIMARY KEY(bot_id,chat_id,topic));
CREATE TABLE IF NOT EXISTS continuity_source_state(message_seq INTEGER PRIMARY KEY,chat_id TEXT NOT NULL REFERENCES chats(id),revision INTEGER NOT NULL,status TEXT NOT NULL,successor INTEGER,updated INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS continuity_memory_guard(bot_id TEXT PRIMARY KEY REFERENCES bots(id),invalidated INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS continuity_obligations(id TEXT PRIMARY KEY,bot_id TEXT NOT NULL REFERENCES bots(id),chat_id TEXT NOT NULL REFERENCES chats(id),request_key TEXT NOT NULL,revision INTEGER NOT NULL,description TEXT NOT NULL,status TEXT NOT NULL,source_run_id TEXT NOT NULL REFERENCES runs(id),sources TEXT NOT NULL,link_kind TEXT NOT NULL,link_id TEXT NOT NULL,updated INTEGER NOT NULL,UNIQUE(bot_id,chat_id,request_key));
CREATE INDEX IF NOT EXISTS continuity_obligations_open ON continuity_obligations(bot_id,chat_id,status,id);
CREATE TABLE IF NOT EXISTS continuity_sessions(run_id TEXT PRIMARY KEY REFERENCES runs(id),bot_id TEXT NOT NULL REFERENCES bots(id),chat_id TEXT NOT NULL REFERENCES chats(id),epoch INTEGER NOT NULL,source_highwater INTEGER NOT NULL,provider TEXT NOT NULL,model TEXT NOT NULL,created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS continuity_dependencies(run_id TEXT NOT NULL REFERENCES runs(id),source_chat_id TEXT NOT NULL REFERENCES chats(id),epoch INTEGER NOT NULL,PRIMARY KEY(run_id,source_chat_id));
CREATE INDEX IF NOT EXISTS continuity_dependency_source ON continuity_dependencies(source_chat_id,epoch);
-- Search is disposable. Its progress is not part of workspace transfer.
CREATE VIRTUAL TABLE IF NOT EXISTS continuity_search USING fts5(body, tokenize='porter unicode61');
CREATE TABLE IF NOT EXISTS continuity_index_progress(id INTEGER PRIMARY KEY CHECK(id=1),cursor INTEGER NOT NULL,complete INTEGER NOT NULL);
INSERT OR IGNORE INTO continuity_index_progress VALUES(1,0,0);
CREATE TRIGGER IF NOT EXISTS continuity_search_insert AFTER INSERT ON chat_messages BEGIN
  INSERT INTO continuity_search(rowid,body) VALUES(new.seq,new.body);
END;
CREATE TRIGGER IF NOT EXISTS continuity_search_delete AFTER DELETE ON chat_messages BEGIN
  DELETE FROM continuity_search WHERE rowid=old.seq;
END;
CREATE TRIGGER IF NOT EXISTS continuity_search_update AFTER UPDATE OF body ON chat_messages WHEN old.body<>new.body BEGIN
  DELETE FROM continuity_search WHERE rowid=old.seq;
  INSERT INTO continuity_search(rowid,body) VALUES(new.seq,new.body);
END;
-- Coarse invalidation deliberately covers legacy notes without source edges too.
-- Raw source records remain available; stale derived prose is excluded on read.
CREATE TRIGGER IF NOT EXISTS continuity_source_changed AFTER UPDATE OF body,suppressed ON chat_messages
WHEN old.body<>new.body OR (old.suppressed<>new.suppressed AND old.kind NOT IN ('assistant','result')) BEGIN
  INSERT INTO continuity_epochs VALUES(old.chat_id,1) ON CONFLICT(chat_id) DO UPDATE SET epoch=epoch+1;
  INSERT INTO continuity_memory_guard SELECT value,1 FROM json_each((SELECT members FROM chats WHERE id=old.chat_id)) WHERE value IN(SELECT id FROM bots) ON CONFLICT(bot_id) DO UPDATE SET invalidated=1;
END;
CREATE TRIGGER IF NOT EXISTS continuity_source_deleted AFTER DELETE ON chat_messages WHEN EXISTS(SELECT 1 FROM chats WHERE id=old.chat_id) BEGIN
  INSERT INTO continuity_epochs VALUES(old.chat_id,1) ON CONFLICT(chat_id) DO UPDATE SET epoch=epoch+1;
  INSERT INTO continuity_memory_guard SELECT value,1 FROM json_each((SELECT members FROM chats WHERE id=old.chat_id)) WHERE value IN(SELECT id FROM bots) ON CONFLICT(bot_id) DO UPDATE SET invalidated=1;
END;
CREATE TRIGGER IF NOT EXISTS continuity_memory_replaced AFTER UPDATE OF memory ON bots WHEN old.memory<>new.memory BEGIN
  DELETE FROM continuity_memory_guard WHERE bot_id=new.id;
END;
CREATE TRIGGER IF NOT EXISTS continuity_receipt_changed AFTER UPDATE OF body ON events
WHEN old.kind='tool_result' AND old.body<>new.body BEGIN
  INSERT INTO continuity_epochs SELECT chat_id,1 FROM runs WHERE id=old.run_id AND chat_id IN(SELECT id FROM chats) ON CONFLICT(chat_id) DO UPDATE SET epoch=epoch+1;
  INSERT INTO continuity_memory_guard SELECT bot_id,1 FROM runs WHERE id=old.run_id AND bot_id IN(SELECT id FROM bots) ON CONFLICT(bot_id) DO UPDATE SET invalidated=1;
END;
CREATE TRIGGER IF NOT EXISTS continuity_receipt_deleted AFTER DELETE ON events
WHEN old.kind='tool_result' BEGIN
  INSERT INTO continuity_epochs SELECT chat_id,1 FROM runs WHERE id=old.run_id AND chat_id IN(SELECT id FROM chats) ON CONFLICT(chat_id) DO UPDATE SET epoch=epoch+1;
  INSERT INTO continuity_memory_guard SELECT bot_id,1 FROM runs WHERE id=old.run_id AND bot_id IN(SELECT id FROM bots) ON CONFLICT(bot_id) DO UPDATE SET invalidated=1;
END;
INSERT OR IGNORE INTO continuity_schema VALUES(2,unixepoch());

-- Model-derived chat prose inherits the source epochs of its authoring run.
CREATE VIEW IF NOT EXISTS continuity_current_messages AS
SELECT m.* FROM chat_messages m WHERE m.suppressed=0
AND NOT EXISTS(SELECT 1 FROM continuity_source_state s WHERE s.message_seq=m.seq AND s.status<>'active')
AND (m.sender='user' OR NOT EXISTS(SELECT 1 FROM continuity_dependencies d LEFT JOIN continuity_epochs e ON e.chat_id=d.source_chat_id WHERE d.run_id=m.run_id AND d.epoch<>COALESCE(e.epoch,0)));
