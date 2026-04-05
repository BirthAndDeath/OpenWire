use sqlx::{Pool, Sqlite};

struct Migration {
    version: i32,
    sql: &'static str,
    name: &'static str,
}

macro_rules! embed {
    ($v:literal, $f:literal) => {
        Migration {
            version: $v,
            sql: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations/", $f)),
            name: $f,
        }
    };
}

static MIGRATIONS: &[Migration] = &[
    embed!(1, "001_init.sql"),
    // embed!(2, "002_add_settings.sql"),
];

pub async fn run(pool: &Pool<Sqlite>) -> anyhow::Result<()> {
    // 版本表
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS _migrations (
            version INTEGER PRIMARY KEY,
            applied_at INTEGER DEFAULT (unixepoch())
        )"#,
    )
    .execute(pool)
    .await?;

    let current: i32 = sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM _migrations")
        .fetch_one(pool)
        .await?;

    for m in MIGRATIONS {
        if m.version > current {
            tracing::info!("Applying migration {}: {}", m.version, m.name);
            let mut tx = pool.begin().await?;
            sqlx::query(m.sql).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO _migrations (version) VALUES (?)")
                .bind(m.version)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
    }
    Ok(())
}
