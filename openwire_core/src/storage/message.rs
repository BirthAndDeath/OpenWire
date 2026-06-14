use sqlx::{self, AssertSqlSafe, FromRow, Pool, Row, Sqlite};

use crate::error::{StorageError, StorageResult};

#[derive(Debug, Clone, FromRow)]
pub struct Message {
    pub id: i64,
    /// 己方身份
    pub owner_identity_id: String,
    /// 对方 ML-DSA 公钥 hex
    pub peer_pubkey_hex: String,
    pub content: String,
    pub is_outgoing: i32,
    pub pending: i32,
    pub ts: i64,
    pub message_hash: Option<String>,
}

// ========== 消息管理 ==========

pub async fn add_message(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    peer_pubkey_hex: &str,
    content: &str,
    is_outgoing: bool,
    pending: bool,
) -> StorageResult<i64> {
    let pending_val = if pending { 1 } else { 0 };
    let row = sqlx::query(
        r#"INSERT INTO messages (owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts)
          VALUES (?1, ?2, ?3, ?4, ?5, unixepoch())
          RETURNING id"#,
    )
    .bind(owner_identity_id)
    .bind(peer_pubkey_hex)
    .bind(content)
    .bind(is_outgoing as i32)
    .bind(pending_val)
    .fetch_one(pool)
    .await?;
    Ok(row.get(0))
}

/// 添加消息并附带消息哈希（用于去重）
///
/// 如果相同哈希的消息已存在，则跳过插入并返回 None。
/// 否则插入新消息并返回 Some(id)。
pub async fn add_message_with_hash(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    peer_pubkey_hex: &str,
    content: &str,
    is_outgoing: bool,
    pending: bool,
    message_hash: &str,
) -> StorageResult<Option<i64>> {
    // 先检查哈希是否已存在
    let existing: Option<String> =
        sqlx::query_scalar("SELECT message_hash FROM messages WHERE message_hash = ?1 LIMIT 1")
            .bind(message_hash)
            .fetch_optional(pool)
            .await?;

    if existing.is_some() {
        // 消息已存在，跳过去重
        return Ok(None);
    }

    let pending_val = if pending { 1 } else { 0 };
    let row = sqlx::query(
        r#"INSERT INTO messages (owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash)
          VALUES (?1, ?2, ?3, ?4, ?5, unixepoch(), ?6)
          RETURNING id"#,
    )
    .bind(owner_identity_id)
    .bind(peer_pubkey_hex)
    .bind(content)
    .bind(is_outgoing as i32)
    .bind(pending_val)
    .bind(message_hash)
    .fetch_one(pool)
    .await?;
    Ok(Some(row.get(0)))
}

pub async fn get_message(pool: &Pool<Sqlite>, msg_id: i64) -> StorageResult<Option<Message>> {
    sqlx::query_as::<_, Message>(
        r#"SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, ts, pending, message_hash
          FROM messages WHERE id = ?"#,
    )
    .bind(msg_id)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

pub async fn get_messages(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    peer_pubkey_hex: &str,
    before: Option<i64>,
    limit: i64,
) -> StorageResult<Vec<Message>> {
    get_messages_range(
        pool,
        owner_identity_id,
        peer_pubkey_hex,
        before,
        None,
        None,
        None,
        limit,
    )
    .await
}

/// 双向游标分页：before → 加载更旧消息；after → 加载更新消息。
/// 使用 (ts, id) 复合游标确保同一秒内多条消息也能准确定位。
pub async fn get_messages_range(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    peer_pubkey_hex: &str,
    before_ts: Option<i64>,
    before_id: Option<i64>,
    after_ts: Option<i64>,
    after_id: Option<i64>,
    limit: i64,
) -> StorageResult<Vec<Message>> {
    let limit = limit.min(200);

    sqlx::query_as::<_, Message>(
        r#"SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, ts, pending, message_hash
          FROM messages
          WHERE owner_identity_id = ?1
            AND peer_pubkey_hex = ?2
            AND (?3 IS NULL OR ts < ?3 OR (ts = ?3 AND ?4 IS NOT NULL AND id < ?4))
            AND (?5 IS NULL OR ts > ?5 OR (ts = ?5 AND ?6 IS NOT NULL AND id > ?6))
          ORDER BY ts DESC, id DESC
          LIMIT ?7"#,
    )
    .bind(owner_identity_id)
    .bind(peer_pubkey_hex)
    .bind(before_ts)
    .bind(before_id)
    .bind(after_ts)
    .bind(after_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// 获取最后一条消息
pub async fn get_last_message(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    peer_pubkey_hex: &str,
) -> StorageResult<Option<Message>> {
    sqlx::query_as::<_, Message>(
        r#"SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, ts, pending, message_hash
          FROM messages WHERE owner_identity_id = ?1 AND peer_pubkey_hex = ?2 ORDER BY ts DESC LIMIT 1"#,
    )
    .bind(owner_identity_id)
    .bind(peer_pubkey_hex)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

pub async fn delete_message(pool: &Pool<Sqlite>, msg_id: i64) -> StorageResult<u64> {
    Ok(sqlx::query("DELETE FROM messages WHERE id = ?")
        .bind(msg_id)
        .execute(pool)
        .await?
        .rows_affected())
}

/// 删除与指定联系人的所有聊天记录
pub async fn delete_messages_by_peer(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    peer_pubkey_hex: &str,
) -> StorageResult<u64> {
    Ok(
        sqlx::query("DELETE FROM messages WHERE owner_identity_id = ?1 AND peer_pubkey_hex = ?2")
            .bind(owner_identity_id)
            .bind(peer_pubkey_hex)
            .execute(pool)
            .await?
            .rows_affected(),
    )
}

// ========== 批量操作 ==========

pub async fn add_messages_batch(
    pool: &Pool<Sqlite>,
    messages: &[(String, String, String, bool, bool)], // (owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending)
) -> StorageResult<Vec<i64>> {
    let mut tx = pool.begin().await?;
    let mut ids = Vec::with_capacity(messages.len());

    for (owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending) in messages {
        let pending_val = if *pending { 1 } else { 0 };
        let row = sqlx::query(
            r#"INSERT INTO messages (owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts)
              VALUES (?1, ?2, ?3, ?4, ?5, unixepoch()) RETURNING id"#,
        )
        .bind(owner_identity_id)
        .bind(peer_pubkey_hex)
        .bind(content)
        .bind(*is_outgoing as i32)
        .bind(pending_val)
        .fetch_one(&mut *tx)
        .await?;
        ids.push(row.get::<i64, _>(0));
    }

    tx.commit().await?;
    Ok(ids)
}

pub async fn mark_sent_batch(pool: &Pool<Sqlite>, ids: &[i64]) -> StorageResult<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    if ids.len() > 1000 {
        return Err(StorageError::BatchSizeTooLarge);
    }

    // 使用参数化查询，无注入风险
    let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "UPDATE messages SET pending = 0 WHERE id IN ({})",
        placeholders.join(",")
    );

    let mut query = sqlx::query(AssertSqlSafe(&*sql));
    for id in ids {
        query = query.bind(id);
    }

    Ok(query.execute(pool).await?.rows_affected())
}

pub async fn delete_messages_batch(pool: &Pool<Sqlite>, ids: &[i64]) -> StorageResult<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    if ids.len() > 1000 {
        return Err(StorageError::BatchSizeTooLarge);
    }

    let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "DELETE FROM messages WHERE id IN ({})",
        placeholders.join(",")
    );

    let mut query = sqlx::query(AssertSqlSafe(&*sql));
    for id in ids {
        query = query.bind(id);
    }

    Ok(query.execute(pool).await?.rows_affected())
}

// ========== 待发送消息管理（离线消息队列） ==========

pub async fn list_pending(pool: &Pool<Sqlite>) -> StorageResult<Vec<Message>> {
    sqlx::query_as::<_, Message>(
        r#"SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, ts, pending, message_hash
          FROM messages WHERE pending = 1 ORDER BY ts"#,
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

pub async fn list_failed(pool: &Pool<Sqlite>) -> StorageResult<Vec<Message>> {
    sqlx::query_as::<_, Message>(
        r#"SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, ts, pending, message_hash
          FROM messages WHERE pending = 2 ORDER BY ts"#,
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

pub async fn mark_sent(pool: &Pool<Sqlite>, msg_id: i64) -> StorageResult<bool> {
    let rows = sqlx::query("UPDATE messages SET pending = 0 WHERE id = ?")
        .bind(msg_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(rows > 0)
}

pub async fn mark_pending(pool: &Pool<Sqlite>, msg_id: i64) -> StorageResult<bool> {
    let rows = sqlx::query("UPDATE messages SET pending = 1 WHERE id = ?")
        .bind(msg_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(rows > 0)
}

pub async fn mark_failed(pool: &Pool<Sqlite>, msg_id: i64) -> StorageResult<bool> {
    let rows = sqlx::query("UPDATE messages SET pending = 2 WHERE id = ?")
        .bind(msg_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(rows > 0)
}

/// 通过 message_hash 标记消息为已发送（用于首次发送成功后标记）
pub async fn mark_sent_by_hash(pool: &Pool<Sqlite>, message_hash: &str) -> StorageResult<bool> {
    let rows =
        sqlx::query("UPDATE messages SET pending = 0 WHERE message_hash = ? AND pending = 1")
            .bind(message_hash)
            .execute(pool)
            .await?
            .rows_affected();
    Ok(rows > 0)
}

/// 更新消息的 message_hash 字段（用于重发后修正 hash）
pub async fn update_message_hash(
    pool: &Pool<Sqlite>,
    msg_id: i64,
    new_hash: &str,
) -> StorageResult<bool> {
    let rows = sqlx::query("UPDATE messages SET message_hash = ? WHERE id = ?")
        .bind(new_hash)
        .bind(msg_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(rows > 0)
}