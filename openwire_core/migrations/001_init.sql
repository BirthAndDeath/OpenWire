
-- 注意：本迁移文件一旦被应用即不可修改（sqlx 按字节校验和校验）；schema 变更一律新增后续迁移。



-- 身份表（以 ML-DSA 公钥为唯一身份标识）
CREATE TABLE identities (
    id INTEGER PRIMARY KEY,
    identity_id TEXT NOT NULL UNIQUE,           -- 身份ID (Hex encoded ML-DSA PubKey)
    is_current INTEGER NOT NULL DEFAULT 0 CHECK (is_current IN (0, 1)),
    created_at INTEGER DEFAULT (unixepoch())
);

-- 联系人表（owner_identity_id 标识该联系人属于哪个本地身份）
CREATE TABLE contacts (
    id INTEGER PRIMARY KEY,
    owner_identity_id TEXT NOT NULL,            -- 所属身份（己方公钥 hex）
    mldsa_pubkey_hex TEXT NOT NULL,             -- 对方 ML-DSA 公钥 hex
    name TEXT,                                  -- 联系人名称
    mlkem_public_key BLOB,                      -- 对方 ML-KEM 公钥（临时密钥交换，每次会话可更新）
    added_at INTEGER DEFAULT (unixepoch()),
    notes TEXT NOT NULL DEFAULT '',             -- 联系人备注
    group_name TEXT NOT NULL DEFAULT '',        -- 联系人分组（空 = 默认分组）
    blocked INTEGER NOT NULL DEFAULT 0 CHECK (blocked IN (0, 1)), -- 屏蔽状态
    UNIQUE (owner_identity_id, mldsa_pubkey_hex),
    FOREIGN KEY (owner_identity_id) REFERENCES identities(identity_id) ON DELETE CASCADE
);

-- 消息表（owner_identity_id 标识所属身份，contact_id 外键引用联系人）
CREATE TABLE messages (
    id INTEGER PRIMARY KEY,
    owner_identity_id TEXT NOT NULL,            -- 己方身份（所属身份）
    contact_id INTEGER NOT NULL,                -- 联系人 id（替代冗余的 peer_pubkey_hex）
    content TEXT NOT NULL,
    is_outgoing INTEGER NOT NULL CHECK (is_outgoing IN (0, 1)),
    pending INTEGER NOT NULL DEFAULT 0 CHECK (pending IN (0, 1, 2)),
    ts INTEGER NOT NULL DEFAULT (unixepoch()),
    message_hash TEXT,                          -- 消息哈希，用于去重
    msgtype INTEGER NOT NULL DEFAULT 0,         -- 消息类型（ChatMessageType 的 u8 值，权威来源）
    FOREIGN KEY (owner_identity_id) REFERENCES identities(identity_id) ON DELETE CASCADE,
    FOREIGN KEY (contact_id) REFERENCES contacts(id) ON DELETE CASCADE
);

CREATE INDEX idx_messages_contact_ts ON messages(contact_id, ts);
CREATE INDEX idx_pending ON messages(pending) WHERE pending != 0;
CREATE UNIQUE INDEX idx_messages_hash ON messages(message_hash) WHERE message_hash IS NOT NULL;

-- 已发送文件历史表（用于验证下载请求的合法性）
CREATE TABLE sent_files (
    file_hash BLOB NOT NULL PRIMARY KEY,        -- SHA256 文件哈希（32字节）
    file_path TEXT NOT NULL,                    -- 发送时的文件路径（本地）
    filename TEXT NOT NULL,                     -- 文件名
    total_size INTEGER NOT NULL,                -- 文件总大小
    sent_at INTEGER DEFAULT (unixepoch()),      -- 发送时间
    recipient_pubkey_hex TEXT NOT NULL DEFAULT '' -- 下载授权绑定接收人（空 = 无授权）
);

CREATE INDEX idx_sent_files_sent_at ON sent_files(sent_at);

-- 扫码添加好友的暂存表：digest 是唯一标识，不存完整公钥（经 FriendOnline 获取）
CREATE TABLE pending_contacts (
    id INTEGER PRIMARY KEY,
    owner_identity_id TEXT NOT NULL,            -- 所属身份（己方公钥 hex）
    digest_hex TEXT NOT NULL,                   -- SHA256(ML-DSA pubkey) hex，64 字符
    name TEXT,                                  -- 联系人名称（可选）
    peer_id TEXT,                               -- 二维码推荐的 PeerID（Base58）
    addrs TEXT NOT NULL DEFAULT '[]',           -- 二维码推荐的 Multiaddr 列表（JSON 数组）
    relay_addrs TEXT NOT NULL DEFAULT '[]',     -- 二维码推荐的公共 relay 节点（JSON 数组）
    version INTEGER NOT NULL DEFAULT 1,         -- 来源二维码的版本字段
    created_at INTEGER DEFAULT (unixepoch()),
    UNIQUE (owner_identity_id, digest_hex)
);

CREATE INDEX idx_pending_contacts_digest ON pending_contacts(digest_hex);
