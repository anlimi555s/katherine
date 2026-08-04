// memory/schema.rs — libSQL / SQLite schema for Katherine v3.
// 与桌面设计稿 Katherine-v3-设计稿.md §二 保持一致。

/// 创建所有表和索引。幂等（IF NOT EXISTS）。
pub fn create_tables(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS events (
            id TEXT PRIMARY KEY,
            content TEXT NOT NULL,
            source TEXT NOT NULL,
            importance REAL NOT NULL DEFAULT 0.5,
            decay_curve TEXT NOT NULL DEFAULT 'power_law',
            created_at TEXT NOT NULL,
            last_retrieved TEXT,
            retrieval_count INTEGER DEFAULT 0
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS fts_events
            USING fts5(content, tokenize='unicode61');

        CREATE TABLE IF NOT EXISTS edges (
            source_id TEXT NOT NULL REFERENCES events(id),
            target_id TEXT NOT NULL REFERENCES events(id),
            edge_type TEXT NOT NULL,
            weight REAL DEFAULT 1.0,
            created_at TEXT NOT NULL,
            PRIMARY KEY (source_id, target_id, edge_type)
        );

        CREATE TABLE IF NOT EXISTS vectors (
            event_id TEXT PRIMARY KEY REFERENCES events(id),
            embedding BLOB NOT NULL
        );

        CREATE TABLE IF NOT EXISTS neuro_snapshots (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            snapshot_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS runtime_state (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        ",
    )?;
    Ok(())
}

/// 触发器：插入 events 时自动同步 FTS 索引。
/// 存 events.content 的副本到 fts_events。
pub fn create_triggers(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TRIGGER IF NOT EXISTS events_ai AFTER INSERT ON events BEGIN
            INSERT INTO fts_events(rowid, content) VALUES (NEW.rowid, NEW.content);
        END;

        CREATE TRIGGER IF NOT EXISTS events_ad AFTER DELETE ON events BEGIN
            INSERT INTO fts_events(fts_events, rowid, content) VALUES ('delete', OLD.rowid, OLD.content);
        END;

        CREATE TRIGGER IF NOT EXISTS events_au AFTER UPDATE ON events BEGIN
            INSERT INTO fts_events(fts_events, rowid, content) VALUES ('delete', OLD.rowid, OLD.content);
            INSERT INTO fts_events(rowid, content) VALUES (NEW.rowid, NEW.content);
        END;
        ",
    )?;
    Ok(())
}

pub fn current_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    let mut y = 1970i64;
    let mut d = days_since_epoch;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if d < days_in_year { break; }
        d -= days_in_year;
        y += 1;
    }
    let months = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 0i64;
    while m < 12 && d >= months[m as usize] {
        d -= months[m as usize];
        m += 1;
    }

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        y, m + 1, d + 1, hours, minutes, seconds
    )
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn now_epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
