// memory/retrieval.rs — 检索管道 v3.0：BM25 + 级联升级。
// 与桌面设计稿 Katherine-v3-设计稿.md §三 保持一致。

use std::collections::HashSet;

use crate::memory::decay::DecayCurve;

/// 检索结果（从 events 表查出的一条记录）。
#[derive(Debug, Clone)]
pub struct RetrievedMemory {
    pub id: String,
    pub content: String,
    pub source: String,
    pub importance: f32,
    pub decay_curve: DecayCurve,
    pub retrieval_count: u32,
    /// 最后检索时间（ISO 字符串，可能为空）
    pub last_retrieved: Option<String>,
    /// 综合得分（BM25 + 时效 + 重要性加权后的结果）
    pub score: f64,
}

/// BM25 + 级联检索。
///
/// 1. FTS5 BM25 搜索 top-20
/// 2. 如果 BM25 置信度高（头名和次名差距 > τ），直接返回
/// 3. 否则 Jaccard 兜底重排
///
/// `conn` 是 rusqlite 连接。
/// `now_epoch` 是当前 Unix 时间戳（秒），用于计算 days_since。
pub fn search(
    conn: &rusqlite::Connection,
    query: &str,
    limit: u32,
    now_epoch: f64,
) -> rusqlite::Result<Vec<String>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }

    // ── Step 1: FTS5 BM25 召回 ──────────────────────────
    let bm25_results = bm25_search(conn, query, 20)?;

    if bm25_results.is_empty() {
        // FTS5 没命中 → Jaccard 兜底（搜全文）
        return jaccard_fallback(conn, query, limit, now_epoch);
    }

    // ── Step 2: 级联决策 ──────────────────────────────
    let cascade = bm25_results.len() >= 2
        && (bm25_results[0].score - bm25_results[1].score) < 0.05; // τ = 0.05

    let results = if cascade {
        // BM25 置信度不够 → Jaccard 重排
        jaccard_fallback(conn, query, limit, now_epoch)?
    } else {
        // BM25 够 → 直接返回 top-k，加时效和重要性微调
        let mut sorted = bm25_results;
        for m in &mut sorted {
            let days_since = days_since_last_retrieved(&m.last_retrieved, now_epoch);
            let recency = m.decay_curve.compute(days_since);
            // 时效只破平局：α = 0.01
            m.score += 0.01 * recency + 0.05 * m.importance as f64;
        }
        sorted.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        sorted
            .into_iter()
            .take(limit as usize)
            .map(|m| m.content)
            .collect()
    };

    // ── Step 3: 更新检索统计 ──────────────────────────
    // 异步更新 retrieval_count 和 last_retrieved（不阻塞返回）
    // TODO: 在 libsql_store 层做

    Ok(results)
}

/// FTS5 BM25 搜索。
fn bm25_search(
    conn: &rusqlite::Connection,
    query: &str,
    limit: u32,
) -> rusqlite::Result<Vec<RetrievedMemory>> {
    // FTS5 MATCH 语法需要处理特殊字符
    let escaped = query
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>();

    if escaped.trim().is_empty() {
        return Ok(Vec::new());
    }

    let sql = format!(
        "SELECT e.id, e.content, e.source, e.importance, e.decay_curve, \
                e.retrieval_count, e.last_retrieved, fts.rank \
         FROM fts_events fts \
         JOIN events e ON fts.rowid = e.rowid \
         WHERE fts.content MATCH ? \
         ORDER BY rank \
         LIMIT {}",
        limit
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([&escaped], |row| {
        let rank: f64 = row.get(7)?;
        Ok(RetrievedMemory {
            id: row.get(0)?,
            content: row.get(1)?,
            source: row.get(2)?,
            importance: row.get(3)?,
            decay_curve: DecayCurve::from_str(&row.get::<_, String>(4)?),
            retrieval_count: row.get(5)?,
            last_retrieved: row.get(6)?,
            score: 0.45 * rank, // BM25 归一化得分，后续调整
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Jaccard 兜底——FTS5 没命中或级联升级时使用。
/// 对 events 表中所有未归档记录算 Jaccard(query, content)。
fn jaccard_fallback(
    conn: &rusqlite::Connection,
    query: &str,
    limit: u32,
    now_epoch: f64,
) -> rusqlite::Result<Vec<String>> {
    let query_words: HashSet<&str> = split_words(query);

    let mut stmt = conn.prepare(
        "SELECT id, content, source, importance, decay_curve, retrieval_count, last_retrieved \
         FROM events WHERE content IS NOT NULL",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(RetrievedMemory {
            id: row.get(0)?,
            content: row.get(1)?,
            source: row.get(2)?,
            importance: row.get(3)?,
            decay_curve: DecayCurve::from_str(&row.get::<_, String>(4)?),
            retrieval_count: row.get(5)?,
            last_retrieved: row.get(6)?,
            score: 0.0,
        })
    })?;

    let mut scored: Vec<RetrievedMemory> = Vec::new();
    for row in rows {
        let mut m = row?;
        if query_words.is_empty() {
            m.score = 0.0;
        } else {
            let content_words: HashSet<&str> = split_words(&m.content);
            let intersection = query_words.intersection(&content_words).count() as f64;
            let union = query_words.union(&content_words).count() as f64;
            let jaccard = if union > 0.0 { intersection / union } else { 0.0 };
            let contains_bonus = if m.content.contains(query) { 0.3 } else { 0.0 };
            m.score = jaccard + contains_bonus + (m.importance as f64 * 0.1);
        }

        let days_since = days_since_last_retrieved(&m.last_retrieved, now_epoch);
        m.score += 0.01 * m.decay_curve.compute(days_since);
        scored.push(m);
    }

    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit as usize);

    Ok(scored.into_iter().map(|m| m.content).collect())
}

fn split_words(text: &str) -> HashSet<&str> {
    text.split(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        .filter(|w| w.len() > 1)
        .collect()
}

/// 计算从 last_retrieved 到现在的天数。
fn days_since_last_retrieved(last_retrieved: &Option<String>, now_epoch: f64) -> f64 {
    let last = last_retrieved
        .as_deref()
        .and_then(parse_iso_to_epoch)
        .unwrap_or(now_epoch);
    ((now_epoch - last) / 86400.0).max(0.0)
}

/// ISO 时间戳 → Unix epoch（秒）。简单解析，不依赖 chrono。
fn parse_iso_to_epoch(ts: &str) -> Option<f64> {
    let parts: Vec<&str> = ts.split(&['T', '-', ':']).collect();
    if parts.len() < 6 {
        return None;
    }
    let year: i64 = parts[0].parse().ok()?;
    let month: i64 = parts[1].parse().ok()?;
    let day: i64 = parts[2].parse().ok()?;
    let hour: i64 = parts[3].parse().ok()?;
    let min: i64 = parts[4].parse().ok()?;
    let sec: i64 = parts[5].parse().ok()?;

    let mut days = 0i64;
    for y in 1970..year {
        days += if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 366 } else { 365 };
    }
    let months_days: [i64; 12] = if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    for m in 0..(month - 1) as usize {
        days += months_days[m];
    }
    days += day - 1;

    Some((days as f64 * 86400.0) + (hour as f64 * 3600.0) + (min as f64 * 60.0) + sec as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::schema;

    /// 建内存库：真实 schema + 触发器（与生产一致）。
    fn setup_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        schema::create_tables(&conn).unwrap();
        schema::create_triggers(&conn).unwrap();
        conn
    }

    fn insert_event(conn: &rusqlite::Connection, id: &str, content: &str) {
        conn.execute(
            "INSERT INTO events (id, content, source, importance, decay_curve, created_at) \
             VALUES (?1, ?2, 'dialogue', 0.5, 'exponential', '2026-08-07T00:00:00')",
            rusqlite::params![id, content],
        )
        .unwrap();
    }

    /// 级联决策 bug（doc/项目代码详解.md §13 问题 2）复现：
    /// FTS5 rank 为负数且 ORDER BY rank 升序（最相关在前），
    /// 导致 (score0 - score1) 恒为负数 → 恒 < τ=0.05 → 恒走 Jaccard 全表兜底，
    /// BM25 直采纳路径成死代码。
    ///
    /// 期望行为：BM25 有明确头名时走直采纳路径——
    /// 只返回 FTS 命中的 2 条记忆（不掺无关条目），且头名排第一。
    #[test]
    fn clear_bm25_winner_takes_direct_path() {
        let conn = setup_db();
        // 2 条相关记忆：e1 词频高（BM25 明确头名），e2 词频低
        const WINNER: &str = "apple apple apple apple pie";
        const RUNNER_UP: &str = "apple orchard harvest festival";
        insert_event(&conn, "e1", WINNER);
        insert_event(&conn, "e2", RUNNER_UP);
        // 6 条完全无关的记忆：Jaccard 兜底会把它们凑进 top-5
        for i in 0..6 {
            insert_event(&conn, &format!("n{i}"), "zebra giraffe lion elephant tiger");
        }

        // 证据 1：FTS5 原始 rank——预期为负数，越相关越小（越负越靠前）
        let mut stmt = conn
            .prepare(
                "SELECT fts.rank, e.id FROM fts_events fts \
                 JOIN events e ON fts.rowid = e.rowid \
                 WHERE fts.content MATCH 'apple' ORDER BY fts.rank",
            )
            .unwrap();
        let ranks = stmt
            .query_map([], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, String>(1)?)))
            .unwrap();
        eprintln!("── FTS5 原始 rank（升序，最相关在最前）──");
        for r in ranks {
            let (rank, id) = r.unwrap();
            eprintln!("  rank = {rank:+.6}  id = {id}");
        }

        let results = search(&conn, "apple", 5, schema::now_epoch_secs()).unwrap();
        eprintln!("── search() 返回 {} 条 ──", results.len());
        for (i, c) in results.iter().enumerate() {
            eprintln!("  #{i}: {c}");
        }

        // 直采纳路径只返回 2 条 FTS 命中；Jaccard 兜底会凑满 5 条（含 3 条无关记忆）
        assert_eq!(
            results.len(),
            2,
            "返回了 {} 条结果（含无关记忆）——级联条件恒真，BM25 直采纳路径未生效",
            results.len()
        );
        // BM25 头名（rank 最负、相关性最高）应排第一
        assert_eq!(results[0], WINNER, "BM25 头名应排第一");
    }
}
