#![allow(missing_docs)]

use sqlx::{self, AssertSqlSafe, FromRow, Pool, Row, Sqlite};

use crate::error::StorageResult;

/// 单次加载消息的最大分页数。所有层（storage、Tauri command、JS isolation hook）共享此值。
pub const MAX_MESSAGE_PAGE_SIZE: i64 = 200;

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
    /// 消息哈希（ChatMessage.hash hex，用于去重和送达回执匹配）
    pub message_hash: Option<String>,
    /// 消息类型（ChatMessageType 的 u8 值，消除 detect_msgtype 内容推断）
    pub msgtype: i32,
}

// 参数逐一对应 messages 表列与调用方字段；聚合结构体需新增无复用价值的中间类型
#[allow(clippy::too_many_arguments)]
pub async fn add_message_with_hash(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    peer_pubkey_hex: &str,
    content: &str,
    is_outgoing: bool,
    pending: bool,
    dedup_hash: &str,
    msgtype: i32,
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
        r#"INSERT INTO messages (owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash, msgtype)
          VALUES (?1, ?2, ?3, ?4, ?5, unixepoch(), ?6, ?7)
          RETURNING id"#,
    )
    .bind(owner_identity_id)
    .bind(peer_pubkey_hex)
    .bind(content)
    .bind(is_outgoing as i32)
    .bind(pending_val)
    .bind(hash)
    .bind(msgtype)
    .fetch_one(pool)
    .await?;
    Ok(Some(row.get(0)))
}

pub async fn get_messages(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    peer_pubkey_hex: &str,
    limit: Option<i64>,
    offset: i64,
) -> StorageResult<Vec<Message>> {
    let messages = if let Some(limit) = limit {
        sqlx::query_as::<_, Message>(
            "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash, msgtype \
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
            "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash, msgtype \
             FROM messages WHERE owner_identity_id = ?1 AND peer_pubkey_hex = ?2 ORDER BY ts DESC",
        )
        .bind(owner_identity_id)
        .bind(peer_pubkey_hex)
        .fetch_all(pool)
        .await?
    };
    Ok(messages)
}

// 分页游标参数（before/after + id 键控游标）必须同时存在才能定位翻页位置
#[allow(clippy::too_many_arguments)]
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
    let limit = limit.clamp(0, MAX_MESSAGE_PAGE_SIZE);
    let before = before.unwrap_or(i64::MAX);
    let before_id = before_id.unwrap_or(i64::MAX);
    let messages = if let (Some(after_id), Some(after)) = (after_id, after) {
        sqlx::query_as::<_, Message>(
            "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash, msgtype \
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
            "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash, msgtype \
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

pub async fn get_message(pool: &Pool<Sqlite>, id: i64) -> StorageResult<Option<Message>> {
    let msg = sqlx::query_as::<_, Message>(
        "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash, msgtype \
         FROM messages WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(msg)
}

pub async fn get_message_by_hash(
    pool: &Pool<Sqlite>,
    hash: &str,
) -> StorageResult<Option<Message>> {
    let msg = sqlx::query_as::<_, Message>(
        "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash, msgtype \
         FROM messages WHERE message_hash = ?1",
    )
    .bind(hash)
    .fetch_optional(pool)
    .await?;
    Ok(msg)
}

pub async fn get_last_message(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    peer_pubkey_hex: &str,
) -> StorageResult<Option<Message>> {
    let msg = sqlx::query_as::<_, Message>(
        "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash, msgtype \
         FROM messages WHERE owner_identity_id = ?1 AND peer_pubkey_hex = ?2 \
         ORDER BY ts DESC LIMIT 1",
    )
    .bind(owner_identity_id)
    .bind(peer_pubkey_hex)
    .fetch_optional(pool)
    .await?;
    Ok(msg)
}

pub async fn delete_message(pool: &Pool<Sqlite>, id: i64) -> StorageResult<i64> {
    let result = sqlx::query("DELETE FROM messages WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() as i64)
}

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

pub async fn list_pending(pool: &Pool<Sqlite>) -> StorageResult<Vec<Message>> {
    let msgs = sqlx::query_as::<_, Message>(
        "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash, msgtype \
         FROM messages WHERE pending = 1 ORDER BY ts ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(msgs)
}

pub async fn list_pending_by_peer(
    pool: &Pool<Sqlite>,
    peer_pubkey_hex: &str,
) -> StorageResult<Vec<Message>> {
    let msgs = sqlx::query_as::<_, Message>(
        "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash, msgtype \
         FROM messages WHERE pending = 1 AND peer_pubkey_hex = ?1 ORDER BY ts ASC",
    )
    .bind(peer_pubkey_hex)
    .fetch_all(pool)
    .await?;
    Ok(msgs)
}

/// 批量查询多个联系人的待发送消息
pub async fn list_failed(pool: &Pool<Sqlite>) -> StorageResult<Vec<Message>> {
    let msgs = sqlx::query_as::<_, Message>(
        "SELECT id, owner_identity_id, peer_pubkey_hex, content, is_outgoing, pending, ts, message_hash, msgtype \
         FROM messages WHERE pending = 2 ORDER BY ts ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(msgs)
}

pub async fn mark_sent(pool: &Pool<Sqlite>, id: i64) -> StorageResult<()> {
    sqlx::query("UPDATE messages SET pending = 0 WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

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

pub async fn mark_sent_by_hash(pool: &Pool<Sqlite>, hash: &str) -> StorageResult<()> {
    sqlx::query("UPDATE messages SET pending = 0 WHERE message_hash = ?1")
        .bind(hash)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn mark_pending(pool: &Pool<Sqlite>, id: i64) -> StorageResult<()> {
    sqlx::query("UPDATE messages SET pending = 1 WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn mark_failed(pool: &Pool<Sqlite>, id: i64) -> StorageResult<()> {
    sqlx::query("UPDATE messages SET pending = 2 WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn update_message_hash(
    pool: &Pool<Sqlite>,
    id: i64,
    hash: &str,
) -> StorageResult<()> {
    sqlx::query("UPDATE messages SET message_hash = ?1 WHERE id = ?2")
        .bind(hash)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn init_test_db() -> &'static Pool<Sqlite> {
        let dir = std::env::temp_dir().join("openwire_msg_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.sqlite");
        let _ = std::fs::remove_file(&path);
        crate::storage::init_path(&path).await.expect("init db");
        crate::storage::pool().expect("pool")
    }

    #[tokio::test]
    async fn test_load_messages_query_path() {
        let pool = init_test_db().await;
        let owner = "owner";
        let peer = "peer";
        crate::storage::add_identity(pool, owner).await.expect("add identity");
        crate::storage::set_current_identity(pool, owner).await.expect("set current");
        crate::storage::upsert_contact(pool, owner, peer, Some("p"), None)
            .await
            .expect("add contact");
        for i in 0..5 {
            add_message_with_hash(
                pool,
                owner,
                peer,
                &format!("msg-{i}"),
                i % 2 == 0,
                false,
                &format!("hash-{i}"),
                0,
            )
            .await
            .expect("insert");
        }

        // 首次加载（loadLatest 路径：无 before/after）
        let first = get_messages_range(pool, owner, peer, None, None, None, None, 50)
            .await
            .expect("first page");
        assert_eq!(first.len(), 5);

        // 游标翻页（loadOlder 路径）
        let cursor = first.last().unwrap();
        let older = get_messages_range(
            pool,
            owner,
            peer,
            Some(cursor.ts),
            Some(cursor.id),
            None,
            None,
            50,
        )
        .await
        .expect("cursor page");
        assert!(older.is_empty());
    }
}


