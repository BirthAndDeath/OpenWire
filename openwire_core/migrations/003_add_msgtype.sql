-- 添加消息类型列，消除 detect_msgtype 内容推断
ALTER TABLE messages ADD COLUMN msgtype INTEGER NOT NULL DEFAULT 0;