#![allow(missing_docs)]

use sqlx::{self, AssertSqlSafe, FromRow, Pool, Row, Sqlite};

use crate::error::StorageResult;

#[derive(Debug, Clone, FromRow)]
/// 消息结构
pub struct Message {
    /// 消息自增 ID
    pub id: i64,
    /// 己方身份
    pub owner_identity_id: String,
    /// 对方 ML-DSA 公钥 hex
    pub peer_pubkey_hex: String,
    /// 消息内容
    pub content: String,
    /// 是否已发送（1=是，0=否）
    pub is_outgoing: i32,
    /// 待发送状态（0=已发送，1=待发送，2=失败）
    pub pending: i32,
    /// 时间戳
    pub ts: i64,
    /// 消息哈希（用于去重和送达回执匹配）
    pub message_hash: Option<String>,
}

#[allow(dead_code)]
fn feature_err<T>() -> StorageResult<T> {
    Err(crate::error::StorageError::FeatureNotEnabled(
        "sqlite_history_storage",
    ))
}

#[cfg(feature = "sqlite_history_storage")]
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
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn add_message(
    _pool: &Pool<Sqlite>,
    _owner_identity_id: &str,
    _peer_pubkey_hex: &str,
    _content: &str,
    _is_outgoing: bool,
    _pending: bool,
) -> StorageResult<i64> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn add_message_with_hash(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    peer_pubkey_hex: &str,
    content: &str,
    is_outgoing: bool,
    pending: bool,
    dedup_hash: &str,
) -> StorageResult<Option<i64>> {
    if !dedup_hash.is_empty() {
        let existing: Option<String> =
            sqlx::query_scalar("SELECT message_hash FROM messages WHERE message_hash = ?1 LIMIT 1")
                .bind(dedup_hash)
                .fetch_optional(pool)
                .await?;
        if existing.is_some() {
            return Ok(None);
        }
    }

    let pending_val = if pending { 1 } else { 0 };
    let hash = if dedup_hash.is_empty() {
        None
    } else {
        Some(dedup_hash)
    };
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
    .bind(hash)
    .fetch_one(pool)
    .await?;
    Ok(Some(row.get(0)))
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn add_message_with_hash(
    _pool: &Pool<Sqlite>,
    _owner_identity_id: &str,
    _peer_pubkey_hex: &str,
    _content: &str,
    _is_outgoing: bool,
    _pending: bool,
    _dedup_hash: &str,
) -> StorageResult<Option<i64>> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn add_messages_batch(
    pool: &Pool<Sqlite>,
    messages: &[(&str, &str, &str, bool, bool)],
) -> StorageResult<()> {
    let mut tx = pool.begin().await?;
    for (owner_id, peer, content, is_outgoing, pending) in messages {
        let pending_val = if *pending { 1 } else { 0 };
        sqlx::query(
            r#"INSERT INTO messages (owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts)
              VALUES (?1, ?2, ?3, ?4, ?5, unixepoch())"#,
        )
        .bind(owner_id)
        .bind(peer)
        .bind(content)
        .bind(*is_outgoing as i32)
        .bind(pending_val)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn add_messages_batch(
    _pool: &Pool<Sqlite>,
    _messages: &[(&str, &str, &str, bool, bool)],
) -> StorageResult<()> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn get_messages(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    peer_pubkey_hex: &str,
    limit: Option<i64>,
    offset: i64,
) -> StorageResult<Vec<Message>> {
    let messages = if let Some(limit) = limit {
        sqlx::query_as::<_, Message>(
            "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash \
             FROM messages WHERE owner_identity_id = ?1 AND peer_pubkey_hex = ?2 ORDER BY ts DESC LIMIT ?3 OFFSET ?4",
        )
        .bind(owner_identity_id)
        .bind(peer_pubkey_hex)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, Message>(
            "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash \
             FROM messages WHERE owner_identity_id = ?1 AND peer_pubkey_hex = ?2 ORDER BY ts DESC",
        )
        .bind(owner_identity_id)
        .bind(peer_pubkey_hex)
        .fetch_all(pool)
        .await?
    };
    Ok(messages)
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn get_messages(
    _pool: &Pool<Sqlite>,
    _owner_identity_id: &str,
    _peer_pubkey_hex: &str,
    _limit: Option<i64>,
    _offset: i64,
) -> StorageResult<Vec<Message>> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn get_messages_range(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    peer_pubkey_hex: &str,
    before: Option<i64>,
    before_id: Option<i64>,
    after: Option<i64>,
    after_id: Option<i64>,
    limit: i64,
) -> StorageResult<Vec<Message>> {
    let limit = limit.min(200);
    let before = before.unwrap_or(i64::MAX);
    let before_id = before_id.unwrap_or(i64::MAX);
    let messages = if let (Some(after_id), Some(after)) = (after_id, after) {
        sqlx::query_as::<_, Message>(
            "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash \
             FROM messages WHERE owner_identity_id = ?1 AND peer_pubkey_hex = ?2 \
             AND (ts, id) < (?3, ?4) AND (ts, id) > (?5, ?6) \
             ORDER BY ts DESC LIMIT ?7",
        )
        .bind(owner_identity_id)
        .bind(peer_pubkey_hex)
        .bind(before)
        .bind(before_id)
        .bind(after)
        .bind(after_id)
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, Message>(
            "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash \
             FROM messages WHERE owner_identity_id = ?1 AND peer_pubkey_hex = ?2 \
             AND (ts, id) < (?3, ?4) \
             ORDER BY ts DESC LIMIT ?5",
        )
        .bind(owner_identity_id)
        .bind(peer_pubkey_hex)
        .bind(before)
        .bind(before_id)
        .bind(limit)
        .fetch_all(pool)
        .await?
    };
    Ok(messages)
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn get_messages_range(
    _pool: &Pool<Sqlite>,
    _owner_identity_id: &str,
    _peer_pubkey_hex: &str,
    _before: Option<i64>,
    _before_id: Option<i64>,
    _after: Option<i64>,
    _after_id: Option<i64>,
    _limit: i64,
) -> StorageResult<Vec<Message>> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn get_message(pool: &Pool<Sqlite>, id: i64) -> StorageResult<Option<Message>> {
    let msg = sqlx::query_as::<_, Message>(
        "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash \
         FROM messages WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(msg)
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn get_message(_pool: &Pool<Sqlite>, _id: i64) -> StorageResult<Option<Message>> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn get_message_by_hash(
    pool: &Pool<Sqlite>,
    hash: &str,
) -> StorageResult<Option<Message>> {
    let msg = sqlx::query_as::<_, Message>(
        "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash \
         FROM messages WHERE message_hash = ?1",
    )
    .bind(hash)
    .fetch_optional(pool)
    .await?;
    Ok(msg)
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn get_message_by_hash(
    _pool: &Pool<Sqlite>,
    _hash: &str,
) -> StorageResult<Option<Message>> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn get_last_message(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    peer_pubkey_hex: &str,
) -> StorageResult<Option<Message>> {
    let msg = sqlx::query_as::<_, Message>(
        "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash \
         FROM messages WHERE owner_identity_id = ?1 AND peer_pubkey_hex = ?2 \
         ORDER BY ts DESC LIMIT 1",
    )
    .bind(owner_identity_id)
    .bind(peer_pubkey_hex)
    .fetch_optional(pool)
    .await?;
    Ok(msg)
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn get_last_message(
    _pool: &Pool<Sqlite>,
    _owner_identity_id: &str,
    _peer_pubkey_hex: &str,
) -> StorageResult<Option<Message>> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn delete_message(pool: &Pool<Sqlite>, id: i64) -> StorageResult<i64> {
    let result = sqlx::query("DELETE FROM messages WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() as i64)
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn delete_message(_pool: &Pool<Sqlite>, _id: i64) -> StorageResult<i64> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn delete_messages_batch(pool: &Pool<Sqlite>, ids: &[i64]) -> StorageResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "DELETE FROM messages WHERE id IN ({})",
        placeholders.join(",")
    );
    let mut query = sqlx::query(AssertSqlSafe(&*sql));
    for id in ids {
        query = query.bind(id);
    }
    query.execute(pool).await?;
    Ok(())
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn delete_messages_batch(_pool: &Pool<Sqlite>, _ids: &[i64]) -> StorageResult<()> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn delete_messages_by_peer(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    peer_pubkey_hex: &str,
) -> StorageResult<i64> {
    let result =
        sqlx::query("DELETE FROM messages WHERE owner_identity_id = ?1 AND peer_pubkey_hex = ?2")
            .bind(owner_identity_id)
            .bind(peer_pubkey_hex)
            .execute(pool)
            .await?;
    Ok(result.rows_affected() as i64)
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn delete_messages_by_peer(
    _pool: &Pool<Sqlite>,
    _owner_identity_id: &str,
    _peer_pubkey_hex: &str,
) -> StorageResult<i64> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn list_pending(pool: &Pool<Sqlite>) -> StorageResult<Vec<Message>> {
    let msgs = sqlx::query_as::<_, Message>(
        "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash \
         FROM messages WHERE pending = 1 ORDER BY ts ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(msgs)
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn list_pending(_pool: &Pool<Sqlite>) -> StorageResult<Vec<Message>> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn list_pending_by_peer(
    pool: &Pool<Sqlite>,
    peer_pubkey_hex: &str,
) -> StorageResult<Vec<Message>> {
    let msgs = sqlx::query_as::<_, Message>(
        "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash \
         FROM messages WHERE pending = 1 AND peer_pubkey_hex = ?1 ORDER BY ts ASC",
    )
    .bind(peer_pubkey_hex)
    .fetch_all(pool)
    .await?;
    Ok(msgs)
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn list_pending_by_peer(
    _pool: &Pool<Sqlite>,
    _peer_pubkey_hex: &str,
) -> StorageResult<Vec<Message>> {
    feature_err()
}

/// 批量查询多个联系人的待发送消息
/// 用于 retry_pending_for_online_peers 避免全表扫描

#[cfg(feature = "sqlite_history_storage")]
pub async fn list_failed(pool: &Pool<Sqlite>) -> StorageResult<Vec<Message>> {
    let msgs = sqlx::query_as::<_, Message>(
        "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash \
         FROM messages WHERE pending = 2 ORDER BY ts ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(msgs)
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn list_failed(_pool: &Pool<Sqlite>) -> StorageResult<Vec<Message>> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn mark_sent(pool: &Pool<Sqlite>, id: i64) -> StorageResult<()> {
    sqlx::query("UPDATE messages SET pending = 0 WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn mark_sent(_pool: &Pool<Sqlite>, _id: i64) -> StorageResult<()> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn mark_sent_batch(pool: &Pool<Sqlite>, ids: &[i64]) -> StorageResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "UPDATE messages SET pending = 0 WHERE id IN ({})",
        placeholders.join(",")
    );
    let mut query = sqlx::query(AssertSqlSafe(&*sql));
    for id in ids {
        query = query.bind(id);
    }
    query.execute(pool).await?;
    Ok(())
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn mark_sent_batch(_pool: &Pool<Sqlite>, _ids: &[i64]) -> StorageResult<()> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn mark_sent_by_hash(pool: &Pool<Sqlite>, hash: &str) -> StorageResult<()> {
    sqlx::query("UPDATE messages SET pending = 0 WHERE message_hash = ?1")
        .bind(hash)
        .execute(pool)
        .await?;
    Ok(())
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn mark_sent_by_hash(_pool: &Pool<Sqlite>, _hash: &str) -> StorageResult<()> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn mark_pending(pool: &Pool<Sqlite>, id: i64) -> StorageResult<()> {
    sqlx::query("UPDATE messages SET pending = 1 WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn mark_pending(_pool: &Pool<Sqlite>, _id: i64) -> StorageResult<()> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn mark_failed(pool: &Pool<Sqlite>, id: i64) -> StorageResult<()> {
    sqlx::query("UPDATE messages SET pending = 2 WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn mark_failed(_pool: &Pool<Sqlite>, _id: i64) -> StorageResult<()> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn update_message_hash(pool: &Pool<Sqlite>, id: i64, hash: &str) -> StorageResult<()> {
    sqlx::query("UPDATE messages SET message_hash = ?1 WHERE id = ?2")
        .bind(hash)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn update_message_hash(_pool: &Pool<Sqlite>, _id: i64, _hash: &str) -> StorageResult<()> {
    feature_err()
}


