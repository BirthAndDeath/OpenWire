-- 身份表（以 ML-DSA 公钥为唯一身份标识）
CREATE TABLE IF NOT EXISTS identities (
    id INTEGER PRIMARY KEY,
    identity_id TEXT NOT NULL UNIQUE,           -- 身份ID (Hex encoded ML-DSA PubKey)
    is_current INTEGER NOT NULL DEFAULT 0 CHECK (is_current IN (0, 1)),
    created_at INTEGER DEFAULT (unixepoch())
);

-- 联系人表（owner_identity_id 标识该联系人属于哪个本地身份）
CREATE TABLE IF NOT EXISTS contacts (
    mldsa_pubkey_hex TEXT NOT NULL,             -- 对方 ML-DSA 公钥 hex
    owner_identity_id TEXT NOT NULL,            -- 所属身份（己方公钥 hex）
    name TEXT,                                  -- 联系人名称
    mlkem_public_key BLOB,                      -- 对方 ML-KEM 公钥（临时密钥交换，每次会话可更新）
    added_at INTEGER DEFAULT (unixepoch()),
    PRIMARY KEY (owner_identity_id, mldsa_pubkey_hex),
    FOREIGN KEY (owner_identity_id) REFERENCES identities(identity_id) ON DELETE CASCADE
);

-- 消息表（owner_identity_id 标识所属身份，peer_pubkey_hex 为对方 ML-DSA 公钥）
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY,
    owner_identity_id TEXT NOT NULL,            -- 己方身份（所属身份）
    peer_pubkey_hex TEXT NOT NULL,              -- 对方 ML-DSA 公钥 hex
    content TEXT NOT NULL,
    is_outgoing INTEGER NOT NULL CHECK (is_outgoing IN (0, 1)),
    pending INTEGER NOT NULL DEFAULT 0 CHECK (pending IN (0, 1, 2)),
    ts INTEGER NOT NULL DEFAULT (unixepoch()),
    message_hash TEXT,                          -- 消息哈希，用于去重
    FOREIGN KEY (owner_identity_id, peer_pubkey_hex) REFERENCES contacts(owner_identity_id, mldsa_pubkey_hex) ON DELETE CASCADE,
    FOREIGN KEY (owner_identity_id) REFERENCES identities(identity_id) ON DELETE CASCADE
);

-- 索引
CREATE INDEX IF NOT EXISTS idx_messages_owner_peer_ts ON messages(owner_identity_id, peer_pubkey_hex, ts);
CREATE INDEX IF NOT EXISTS idx_pending ON messages(pending) WHERE pending != 0;
CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_hash ON messages(message_hash) WHERE message_hash IS NOT NULL;
