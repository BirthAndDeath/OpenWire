-- 已发送文件历史表（用于验证下载请求的合法性）
CREATE TABLE IF NOT EXISTS sent_files (
    file_hash BLOB NOT NULL PRIMARY KEY,           -- SHA256 文件哈希（32字节）
    file_path TEXT NOT NULL,                        -- 发送时的文件路径（本地）
    filename TEXT NOT NULL,                         -- 文件名
    total_size INTEGER NOT NULL,                    -- 文件总大小
    sent_at INTEGER DEFAULT (unixepoch())           -- 发送时间
);