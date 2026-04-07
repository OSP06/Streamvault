use anyhow::Result;
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool, Row};
use crate::models::Video;

pub struct Database {
    pool: SqlitePool,
}

impl Database {
    pub async fn new(url: &str) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(10)
            .connect(url)
            .await?;

        // Enable WAL mode immediately after connecting.
        //
        // Default SQLite journal mode (DELETE) takes an exclusive write lock
        // on the entire database file for every write — reads block during
        // uploads. WAL (Write-Ahead Logging) allows concurrent reads while a
        // write is in progress, which matters for a streaming service where
        // token-lookup reads vastly outnumber upload writes.
        //
        // synchronous=NORMAL is safe with WAL — it provides crash safety
        // without the performance cost of synchronous=FULL (the default).
        sqlx::query("PRAGMA journal_mode=WAL")
            .execute(&pool)
            .await?;
        sqlx::query("PRAGMA synchronous=NORMAL")
            .execute(&pool)
            .await?;
        // Increase the page cache to 8MB (default is ~2MB).
        // Reduces disk I/O on repeated token lookups.
        sqlx::query("PRAGMA cache_size=-8000")
            .execute(&pool)
            .await?;

        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<()> {
        // Run each statement separately — SQLx SQLite does not support
        // multiple statements in a single execute() call.
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS videos (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                token         TEXT NOT NULL UNIQUE,
                filename      TEXT NOT NULL,
                original_name TEXT NOT NULL,
                content_type  TEXT NOT NULL,
                size_bytes    INTEGER NOT NULL,
                duration_secs REAL,
                width         INTEGER,
                height        INTEGER,
                hls_ready     BOOLEAN NOT NULL DEFAULT FALSE,
                created_at    TEXT NOT NULL DEFAULT (datetime('now'))
            )"#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_videos_token ON videos(token)",
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn insert_video(&self, video: &Video) -> Result<i64> {
        let id = sqlx::query(
            r#"INSERT INTO videos (token, filename, original_name, content_type, size_bytes, created_at)
               VALUES (?, ?, ?, ?, ?, datetime('now'))"#,
        )
        .bind(&video.token)
        .bind(&video.filename)
        .bind(&video.original_name)
        .bind(&video.content_type)
        .bind(video.size_bytes)
        .execute(&self.pool)
        .await?
        .last_insert_rowid();
        Ok(id)
    }

    pub async fn get_video_by_token(&self, token: &str) -> Result<Option<Video>> {
        let row = sqlx::query(
            r#"SELECT id, token, filename, original_name, content_type,
                      size_bytes, duration_secs, width, height, hls_ready, created_at
               FROM videos WHERE token = ?"#,
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Video {
            id: r.get("id"),
            token: r.get("token"),
            filename: r.get("filename"),
            original_name: r.get("original_name"),
            content_type: r.get("content_type"),
            size_bytes: r.get("size_bytes"),
            duration_secs: r.get("duration_secs"),
            width: r.get("width"),
            height: r.get("height"),
            hls_ready: r.get("hls_ready"),
            created_at: r.get("created_at"),
        }))
    }

    pub async fn list_videos(&self) -> Result<Vec<Video>> {
        let rows = sqlx::query(
            r#"SELECT id, token, filename, original_name, content_type,
                      size_bytes, duration_secs, width, height, hls_ready, created_at
               FROM videos ORDER BY created_at DESC LIMIT 100"#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(|r| Video {
            id: r.get("id"),
            token: r.get("token"),
            filename: r.get("filename"),
            original_name: r.get("original_name"),
            content_type: r.get("content_type"),
            size_bytes: r.get("size_bytes"),
            duration_secs: r.get("duration_secs"),
            width: r.get("width"),
            height: r.get("height"),
            hls_ready: r.get("hls_ready"),
            created_at: r.get("created_at"),
        }).collect())
    }

    pub async fn mark_hls_ready(&self, token: &str) -> Result<()> {
        sqlx::query("UPDATE videos SET hls_ready = TRUE WHERE token = ?")
            .bind(token)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}