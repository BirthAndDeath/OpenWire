-- 身份表
CREATE TABLE IF NOT EXISTS identity (
    peer_id TEXT PRIMARY KEY,
    key_enc BLOB
);

-- 联系人表
CREATE TABLE IF NOT EXISTS contacts (
    peer_id TEXT PRIMARY KEY,
    name TEXT,
    added_at INTEGER DEFAULT (unixepoch())
);

-- 消息表
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY,
    peer_id TEXT NOT NULL REFERENCES contacts(peer_id) ON DELETE CASCADE,
    content TEXT NOT NULL,
    is_outgoing INTEGER NOT NULL CHECK (is_outgoing IN (0, 1)),
    pending INTEGER NOT NULL DEFAULT 0 CHECK (pending IN (0, 1, 2)),
    ts INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_messages_peer_ts ON messages(peer_id, ts);
CREATE INDEX IF NOT EXISTS idx_pending ON messages(pending) WHERE pending != 0;