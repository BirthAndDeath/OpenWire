use sqlx::{Pool, Sqlite, Row};

// ========== 统计查询==========

/// 获取消息数量统计 - 暂未使用
#[allow(dead_code)]
pub async fn get_message_count(pool: &Pool<Sqlite>, peer_id: &str) -> anyhow::Result<i64> {
    let row = sqlx::query("SELECT COUNT(*) FROM messages WHERE peer_id = ?")
        .bind(peer_id)
        .fetch_one(pool)
        .await?;
    Ok(row.get(0))
}

/// 获取未读消息估算 - 暂未使用
#[allow(dead_code)]
pub async fn get_unread_estimate(
    pool: &Pool<Sqlite>,
    peer_id: &str,
    last_read_ts: i64,
) -> anyhow::Result<i64> {
    let row = sqlx::query(
        r#"SELECT COUNT(*) FROM messages 
          WHERE peer_id = ? AND ts > ? AND is_outgoing = 0"#,
    )
    .bind(peer_id)
    .bind(last_read_ts)
    .fetch_one(pool)
    .await?;
    Ok(row.get(0))
}