use sqlx::{FromRow, Pool, Sqlite, Row};

#[derive(Debug, Clone, FromRow)]
pub struct Message {
    pub id: i64,
    pub peer_id: String,
    pub content: String,
    pub is_outgoing: i32,
    pub ts: i64,
    pub pending: i32,
}

// ========== 消息管理 ==========

pub async fn add_message(
    pool: &Pool<Sqlite>,
    peer_id: &str,
    content: &str,
    is_outgoing: bool,
    pending: bool,
) -> anyhow::Result<i64> {
    let pending_val = if pending { 1 } else { 0 };
    let row = sqlx::query(
        r#"INSERT INTO messages (peer_id, content, is_outgoing, pending, ts)
          VALUES (?1, ?2, ?3, ?4, unixepoch()) 
          RETURNING id"#,
    )
    .bind(peer_id)
    .bind(content)
    .bind(is_outgoing as i32)
    .bind(pending_val)
    .fetch_one(pool)
    .await?;
    Ok(row.get(0))
}

pub async fn get_message(pool: &Pool<Sqlite>, msg_id: i64) -> anyhow::Result<Option<Message>> {
    sqlx::query_as::<_, Message>(
        r#"SELECT id, peer_id, content, is_outgoing, ts, pending 
          FROM messages WHERE id = ?"#,
    )
    .bind(msg_id)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

pub async fn get_messages(
    pool: &Pool<Sqlite>,
    peer_id: &str,
    before: Option<i64>,
    limit: i64,
) -> anyhow::Result<Vec<Message>> {
    // 限制最大查询数量防止DoS
    let limit = limit.min(1000);

    sqlx::query_as::<_, Message>(
        r#"SELECT id, peer_id, content, is_outgoing, ts, pending FROM messages 
          WHERE peer_id = ?1 AND (?2 IS NULL OR ts < ?2)
          ORDER BY ts DESC LIMIT ?3"#,
    )
    .bind(peer_id)
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// 获取最后一条消息
pub async fn get_last_message(
    pool: &Pool<Sqlite>,
    peer_id: &str,
) -> anyhow::Result<Option<Message>> {
    sqlx::query_as::<_, Message>(
        r#"SELECT id, peer_id, content, is_outgoing, ts, pending 
          FROM messages WHERE peer_id = ? ORDER BY ts DESC LIMIT 1"#,
    )
    .bind(peer_id)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

pub async fn delete_message(pool: &Pool<Sqlite>, msg_id: i64) -> anyhow::Result<u64> {
    Ok(sqlx::query("DELETE FROM messages WHERE id = ?")
        .bind(msg_id)
        .execute(pool)
        .await?
        .rows_affected())
}

// ========== 批量操作（新增） ==========

pub async fn add_messages_batch(
    pool: &Pool<Sqlite>,
    messages: &[(String, String, bool, bool)], // (peer_id, content, is_outgoing, pending)
) -> anyhow::Result<Vec<i64>> {
    let mut tx = pool.begin().await?;
    let mut ids = Vec::with_capacity(messages.len());

    for (peer_id, content, is_outgoing, pending) in messages {
        let pending_val = if *pending { 1 } else { 0 };
        let row = sqlx::query(
            r#"INSERT INTO messages (peer_id, content, is_outgoing, pending, ts)
              VALUES (?1, ?2, ?3, ?4, unixepoch()) RETURNING id"#,
        )
        .bind(peer_id)
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

pub async fn mark_sent_batch(pool: &Pool<Sqlite>, ids: &[i64]) -> anyhow::Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    if ids.len() > 1000 {
        anyhow::bail!("Batch size too large");
    }

    // 使用参数化查询，无注入风险
    let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "UPDATE messages SET pending = 0 WHERE id IN ({})",
        placeholders.join(",")
    );

    let mut query = sqlx::query(&sql);
    for id in ids {
        query = query.bind(id);
    }

    Ok(query.execute(pool).await?.rows_affected())
}

pub async fn delete_messages_batch(pool: &Pool<Sqlite>, ids: &[i64]) -> anyhow::Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    if ids.len() > 1000 {
        anyhow::bail!("Batch size too large");
    }

    let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "DELETE FROM messages WHERE id IN ({})",
        placeholders.join(",")
    );

    let mut query = sqlx::query(&sql);
    for id in ids {
        query = query.bind(id);
    }

    Ok(query.execute(pool).await?.rows_affected())
}

// ========== 待发送消息管理 ==========

pub async fn list_pending(pool: &Pool<Sqlite>) -> anyhow::Result<Vec<Message>> {
    sqlx::query_as::<_, Message>(
        r#"SELECT id, peer_id, content, is_outgoing, ts, pending 
          FROM messages WHERE pending = 1 ORDER BY ts"#,
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

pub async fn list_failed(pool: &Pool<Sqlite>) -> anyhow::Result<Vec<Message>> {
    sqlx::query_as::<_, Message>(
        r#"SELECT id, peer_id, content, is_outgoing, ts, pending 
          FROM messages WHERE pending = 2 ORDER BY ts"#,
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

pub async fn mark_sent(pool: &Pool<Sqlite>, msg_id: i64) -> anyhow::Result<bool> {
    let rows = sqlx::query("UPDATE messages SET pending = 0 WHERE id = ?")
        .bind(msg_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(rows > 0)
}

pub async fn mark_pending(pool: &Pool<Sqlite>, msg_id: i64) -> anyhow::Result<bool> {
    let rows = sqlx::query("UPDATE messages SET pending = 1 WHERE id = ?")
        .bind(msg_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(rows > 0)
}

pub async fn mark_failed(pool: &Pool<Sqlite>, msg_id: i64) -> anyhow::Result<bool> {
    let rows = sqlx::query("UPDATE messages SET pending = 2 WHERE id = ?")
        .bind(msg_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(rows > 0)
}