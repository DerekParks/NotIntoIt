//! Hybrid retrieval over one SQLite file: `chunks` content, `chunks_fts` (FTS5), `chunks_vec`
//! (sqlite-vec `int8[384]`, cosine), and `meta` provenance, fused with reciprocal rank fusion.

use anyhow::{Context, Result};
use nii_core::{DocChunk, SourceKind};
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::path::Path;

pub const DIM: usize = 384;
pub const SCHEMA_VERSION: u32 = 2;
const RRF_K: f64 = 60.0;
/// Candidates fetched from each retriever before fusion.
const CANDIDATES: usize = 100;

/// Register sqlite-vec for every connection opened by this process.
/// Idempotent; call before `open`.
pub fn register_sqlite_vec() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::os::raw::c_int,
        >(sqlite_vec::sqlite3_vec_init as *const ())));
    });
}

pub fn open(db_path: &Path) -> Result<Connection> {
    register_sqlite_vec();
    let conn = Connection::open(db_path).with_context(|| format!("opening {}", db_path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    Ok(conn)
}

pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS chunks (
            id INTEGER PRIMARY KEY,
            source TEXT NOT NULL,
            citation TEXT NOT NULL,
            section_path TEXT NOT NULL,
            heading TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            url TEXT,
            text TEXT NOT NULL,
            caveat TEXT
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
            heading, text,
            content='chunks', content_rowid='id'
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS chunks_vec USING vec0(
            embedding int8[{DIM}] distance_metric=cosine,
            source text
        );
        CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        "
    ))?;
    set_meta(conn, "schema_version", &SCHEMA_VERSION.to_string())?;
    Ok(())
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
    let mut rows = stmt.query_map([key], |r| r.get::<_, String>(0))?;
    Ok(rows.next().transpose()?)
}

/// Append chunks with their (f32, unit-norm) embeddings to the index.
pub fn add_chunks(conn: &mut Connection, chunks: &[DocChunk], embeddings: &[Vec<f32>]) -> Result<()> {
    anyhow::ensure!(chunks.len() == embeddings.len(), "chunks/embeddings length mismatch");
    let tx = conn.transaction()?;
    {
        let mut ins_chunk = tx.prepare_cached(
            "INSERT INTO chunks(source, citation, section_path, heading, ordinal, url, text, caveat)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        let mut ins_fts = tx.prepare_cached("INSERT INTO chunks_fts(rowid, heading, text) VALUES (?1, ?2, ?3)")?;
        let mut ins_vec =
            tx.prepare_cached("INSERT INTO chunks_vec(rowid, embedding, source) VALUES (?1, vec_int8(?2), ?3)")?;
        for (chunk, embedding) in chunks.iter().zip(embeddings) {
            anyhow::ensure!(embedding.len() == DIM, "embedding dim {} != {DIM}", embedding.len());
            ins_chunk.execute(params![
                chunk.source.as_str(),
                chunk.citation,
                chunk.section_path,
                chunk.heading,
                chunk.ordinal,
                chunk.url,
                chunk.text,
                chunk.caveat,
            ])?;
            let id = tx.last_insert_rowid();
            ins_fts.execute(params![id, chunk.heading, chunk.text])?;
            let q = quantize_i8(embedding);
            ins_vec.execute(params![id, i8_bytes(&q), chunk.source.as_str()])?;
        }
    }
    tx.commit()?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub id: i64,
    pub score: f64,
    pub source: SourceKind,
    pub citation: String,
    pub section_path: String,
    pub heading: String,
    pub url: Option<String>,
    pub text: String,
    pub caveat: Option<String>,
}

/// Hybrid search: FTS5 + vector KNN fused with reciprocal rank fusion.
pub fn search(
    conn: &Connection,
    query_text: &str,
    query_embedding: &[f32],
    source_filter: Option<SourceKind>,
    limit: usize,
) -> Result<Vec<Hit>> {
    let fts_ranked = fts_search(conn, query_text, source_filter, CANDIDATES)?;
    let vec_ranked = vec_search(conn, query_embedding, source_filter, CANDIDATES)?;

    let mut scores: HashMap<i64, f64> = HashMap::new();
    for ranked in [&fts_ranked, &vec_ranked] {
        for (rank, id) in ranked.iter().enumerate() {
            *scores.entry(*id).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
    }
    let mut fused: Vec<(i64, f64)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1));

    let mut stmt = conn.prepare_cached(
        "SELECT source, citation, section_path, heading, url, text, caveat
         FROM chunks WHERE id = ?1",
    )?;
    let mut seen_citations = std::collections::HashSet::new();
    let mut hits = Vec::with_capacity(limit);
    for (id, score) in fused {
        if hits.len() >= limit {
            break;
        }
        let hit = stmt.query_row([id], |r| {
            Ok(Hit {
                id,
                score,
                source: {
                    let raw: String = r.get(0)?;
                    SourceKind::parse(&raw).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            format!("unknown chunk source {raw:?}").into(),
                        )
                    })?
                },
                citation: r.get(1)?,
                section_path: r.get(2)?,
                heading: r.get(3)?,
                url: r.get(4)?,
                text: r.get(5)?,
                caveat: r.get(6)?,
            })
        })?;
        if seen_citations.insert(hit.citation.clone()) {
            hits.push(hit);
        }
    }
    Ok(hits)
}

fn fts_search(conn: &Connection, query: &str, source_filter: Option<SourceKind>, k: usize) -> Result<Vec<i64>> {
    let match_expr = to_fts_query(query);
    if match_expr.is_empty() {
        return Ok(Vec::new());
    }
    let sql = match source_filter {
        Some(_) => {
            "SELECT f.rowid FROM chunks_fts f JOIN chunks c ON c.id = f.rowid
             WHERE chunks_fts MATCH ?1 AND c.source = ?3 ORDER BY f.rank LIMIT ?2"
        }
        None => "SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY rank LIMIT ?2",
    };
    let mut stmt = conn.prepare_cached(sql)?;
    let ids = match source_filter {
        Some(s) => stmt
            .query_map(params![match_expr, k as i64, s.as_str()], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?,
        None => stmt
            .query_map(params![match_expr, k as i64], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?,
    };
    Ok(ids)
}

fn vec_search(
    conn: &Connection,
    query_embedding: &[f32],
    source_filter: Option<SourceKind>,
    k: usize,
) -> Result<Vec<i64>> {
    let q = quantize_i8(query_embedding);
    let sql = match source_filter {
        Some(_) => {
            "SELECT rowid FROM chunks_vec
             WHERE embedding MATCH vec_int8(?1) AND k = ?2 AND source = ?3 ORDER BY distance"
        }
        None => {
            "SELECT rowid FROM chunks_vec
             WHERE embedding MATCH vec_int8(?1) AND k = ?2 ORDER BY distance"
        }
    };
    let mut stmt = conn.prepare_cached(sql)?;
    let ids = match source_filter {
        Some(s) => stmt
            .query_map(params![i8_bytes(&q), k as i64, s.as_str()], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?,
        None => stmt
            .query_map(params![i8_bytes(&q), k as i64], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?,
    };
    Ok(ids)
}

/// Natural language -> FTS5 MATCH expression: every token phrase-quoted (so `401(k)` is literal)
/// and OR-joined.
pub fn to_fts_query(input: &str) -> String {
    input
        .split_whitespace()
        .map(|token| token.trim_matches(|c: char| c == '"' || c == '\''))
        .filter(|token| token.chars().any(char::is_alphanumeric))
        .map(|token| format!("\"{}\"", token.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn quantize_i8(v: &[f32]) -> Vec<i8> {
    v.iter()
        .map(|&x| (x * 127.0).round().clamp(-127.0, 127.0) as i8)
        .collect()
}

fn i8_bytes(v: &[i8]) -> &[u8] {
    // SAFETY: i8 and u8 have identical size and alignment.
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_embedding(seed: usize) -> Vec<f32> {
        // Deterministic pseudo-vector, unit-ish norm.
        let mut v: Vec<f32> = (0..DIM)
            .map(|i| ((seed * 31 + i * 7) % 13) as f32 / 13.0 - 0.5)
            .collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter_mut().for_each(|x| *x /= norm);
        v
    }

    fn chunk(citation: &str, heading: &str, text: &str) -> DocChunk {
        DocChunk {
            source: SourceKind::Usc,
            citation: citation.into(),
            section_path: "Subtitle A".into(),
            heading: heading.into(),
            ordinal: 0,
            url: None,
            text: text.into(),
            caveat: Some(format!("caveat for {citation}")),
        }
    }

    #[test]
    fn fts_query_sanitizes_citations() {
        assert_eq!(to_fts_query("401(k) limits"), "\"401(k)\" OR \"limits\"");
        assert_eq!(to_fts_query("  "), "");
        assert_eq!(to_fts_query("\"quoted\""), "\"quoted\"");
    }

    #[test]
    fn end_to_end_index_and_hybrid_search() {
        let mut conn = {
            register_sqlite_vec();
            let c = Connection::open_in_memory().unwrap();
            init_schema(&c).unwrap();
            c
        };
        let chunks = vec![
            chunk(
                "26 U.S.C. § 61",
                "Gross income defined",
                "gross income means all income from whatever source derived",
            ),
            chunk(
                "26 U.S.C. § 401",
                "Qualified pension plans",
                "a trust forming part of a stock bonus, pension, or profit-sharing plan such as a 401(k)",
            ),
            chunk(
                "26 U.S.C. § 121",
                "Exclusion of gain from sale of principal residence",
                "gross income shall not include gain from the sale or exchange of property",
            ),
        ];
        let embeddings: Vec<Vec<f32>> = (0..chunks.len()).map(fake_embedding).collect();
        add_chunks(&mut conn, &chunks, &embeddings).unwrap();

        // FTS side should surface the 401(k) chunk for a citation-style query;
        // vector side gets the exact stored embedding -> distance 0 for its row.
        let hits = search(&conn, "401(k) plan", &fake_embedding(1), None, 3).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(
            hits[0].citation,
            "26 U.S.C. § 401",
            "hits: {:?}",
            hits.iter().map(|h| &h.citation).collect::<Vec<_>>()
        );

        // Source filter returns only matching sources (all usc here).
        let hits = search(&conn, "gross income", &fake_embedding(0), Some(SourceKind::Usc), 3).unwrap();
        assert!(hits.iter().all(|h| h.source == SourceKind::Usc));
        assert!(
            hits.iter()
                .all(|h| h.caveat.as_deref() == Some(&format!("caveat for {}", h.citation)))
        );

        // Meta round-trip.
        set_meta(&conn, "model", "bge-small-en-v1.5-q").unwrap();
        assert_eq!(
            get_meta(&conn, "model").unwrap().as_deref(),
            Some("bge-small-en-v1.5-q")
        );

        // A chunk whose source this build doesn't know is an error, never relabelled as statute.
        conn.execute(
            "UPDATE chunks SET source = 'treaty' WHERE citation = '26 U.S.C. § 401'",
            [],
        )
        .unwrap();
        let err = search(&conn, "401(k) plan", &fake_embedding(1), None, 3)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown chunk source \"treaty\""), "{err}");
    }
}
