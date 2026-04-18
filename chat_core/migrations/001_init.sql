-- ML-KEM 身份表（持久化身份）
CREATE TABLE IF NOT EXISTS mlkem_identity (
    id INTEGER PRIMARY KEY,
    identity_id TEXT UNIQUE NOT NULL,
    -- ML-KEM 公钥的 hex 编码
    public_key BLOB NOT NULL,
    -- ML-KEM 公钥原始字节
    is_current INTEGER NOT NULL DEFAULT 0 CHECK (is_current IN (0, 1))
);
-- 联系人表
CREATE TABLE IF NOT EXISTS contacts (
    peer_id TEXT PRIMARY KEY,
    name TEXT,
    public_key BLOB,
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
CREATE INDEX IF NOT EXISTS idx_pending ON messages(pending)
WHERE pending != 0;