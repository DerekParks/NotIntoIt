//! S1: Is brute-force sqlite-vec KNN fast enough at 200k int8[384] vectors,
//! with and without metadata filtering, on this machine? Also times FTS5 over
//! real Title 26 chunks and an end-to-end hybrid (embed + KNN + FTS + RRF) query.
//!
//! Real data where it matters: chunk texts come from data/usc26.xml; the first
//! N_REAL_EMBED vectors are real bge-small embeddings (validates the pipeline),
//! the rest are random int8 padding — identical compute cost for a brute-force
//! scan, which is what we're timing.

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use rand::RngExt;
use rusqlite::{Connection, ffi::sqlite3_auto_extension, params};
use spikes::{Timings, chunk_text, i8_bytes, quantize_i8, strip_tags};
use std::collections::HashMap;
use std::time::Instant;

const DIM: usize = 384;
const N_TOTAL: usize = 200_000;
const N_REAL_EMBED: usize = 512;
const K: usize = 100;
const REPS: usize = 20;
const BGE_QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";

/// (natural-language query for embedding, keyword form for FTS MATCH)
const QUERIES: &[(&str, &str)] = &[
    ("What is included in gross income?", "\"gross income\""),
    ("retirement plan contribution limits", "retirement AND contribution"),
    ("capital gains on sale of a home", "\"capital gain\" OR residence"),
];

fn main() -> Result<()> {
    unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ())));
    }

    // -- corpus --------------------------------------------------------------
    let t = Instant::now();
    let xml = std::fs::read_to_string("data/usc26.xml").context("data/usc26.xml missing")?;
    let text = strip_tags(&xml);
    let chunks = chunk_text(&text, 1000);
    println!(
        "corpus: {:.1} MB xml -> {:.1} MB text -> {} chunks (~1000 chars) in {:.1?}",
        xml.len() as f64 / 1e6,
        text.len() as f64 / 1e6,
        chunks.len(),
        t.elapsed()
    );

    // -- embeddings (S2 validation happens here too: ort must init + run) -----
    let t = Instant::now();
    let mut model = TextEmbedding::try_new(
        TextInitOptions::new(EmbeddingModel::BGESmallENV15Q)
            .with_cache_dir("data/models".into())
            .with_show_download_progress(true),
    )?;
    println!("model init (bge-small-en-v1.5 int8 onnx): {:.1?}", t.elapsed());

    let t = Instant::now();
    let real_texts: Vec<&str> = chunks.iter().take(N_REAL_EMBED).map(|s| s.as_str()).collect();
    let real_embs = model.embed(&real_texts, Some(32))?;
    let dt = t.elapsed().as_secs_f64();
    println!(
        "embedded {} real chunks in {:.1}s ({:.0} chunks/s), dim={}",
        real_embs.len(),
        dt,
        real_embs.len() as f64 / dt,
        real_embs[0].len()
    );
    assert_eq!(real_embs[0].len(), DIM);

    // -- build vec0 table ------------------------------------------------------
    let _ = std::fs::remove_file("data/spike_s1.db");
    let mut db = Connection::open("data/spike_s1.db")?;
    let vec_version: String = db.query_row("SELECT vec_version()", [], |r| r.get(0))?;
    println!("sqlite-vec {vec_version}");

    db.execute_batch(
        "CREATE VIRTUAL TABLE chunks_vec USING vec0(
            embedding int8[384] distance_metric=cosine,
            source text,
            year integer
        );",
    )?;

    let sources = ["usc", "cfr", "pub"];
    let mut rng = rand::rng();
    let t = Instant::now();
    let tx = db.transaction()?;
    {
        let mut stmt =
            tx.prepare("INSERT INTO chunks_vec(rowid, embedding, source, year) VALUES (?1, vec_int8(?2), ?3, ?4)")?;
        for i in 0..N_TOTAL {
            let v: Vec<i8> = if i < real_embs.len() {
                quantize_i8(&real_embs[i])
            } else {
                (0..DIM).map(|_| rng.random_range(-127i32..=127) as i8).collect()
            };
            stmt.execute(params![
                i as i64,
                i8_bytes(&v),
                sources[i % sources.len()],
                2020 + (i % 7) as i64,
            ])?;
        }
    }
    tx.commit()?;
    println!("inserted {} int8[{}] vectors in {:.1?}", N_TOTAL, DIM, t.elapsed());

    // -- build FTS5 table over all real chunk texts ----------------------------
    let t = Instant::now();
    db.execute_batch("CREATE VIRTUAL TABLE chunks_fts USING fts5(body, tokenize=\"unicode61 tokenchars '§'\");")?;
    let tx = db.transaction()?;
    {
        let mut stmt = tx.prepare("INSERT INTO chunks_fts(rowid, body) VALUES (?1, ?2)")?;
        for (i, chunk) in chunks.iter().enumerate() {
            stmt.execute(params![i as i64, chunk])?;
        }
    }
    tx.commit()?;
    println!("FTS5 indexed {} real chunks in {:.1?}", chunks.len(), t.elapsed());

    let db_mb = std::fs::metadata("data/spike_s1.db")?.len() as f64 / 1e6;
    println!("db size on disk: {db_mb:.1} MB\n");

    // -- query timings ---------------------------------------------------------
    let embed_query = |model: &mut TextEmbedding, q: &str| -> Result<Vec<i8>> {
        let embs = model.embed(&[format!("{BGE_QUERY_PREFIX}{q}")], None)?;
        Ok(quantize_i8(&embs[0]))
    };

    let knn = |db: &Connection, qv: &[i8], filter: bool| -> Result<Vec<(i64, f64)>> {
        let sql = if filter {
            "SELECT rowid, distance FROM chunks_vec
             WHERE embedding MATCH vec_int8(?1) AND k = ?2 AND source = 'usc' ORDER BY distance"
        } else {
            "SELECT rowid, distance FROM chunks_vec
             WHERE embedding MATCH vec_int8(?1) AND k = ?2 ORDER BY distance"
        };
        let mut stmt = db.prepare_cached(sql)?;
        let rows = stmt
            .query_map(params![i8_bytes(qv), K as i64], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    };

    let fts = |db: &Connection, q: &str| -> Result<Vec<(i64, f64)>> {
        let mut stmt =
            db.prepare_cached("SELECT rowid, rank FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY rank LIMIT ?2")?;
        let rows = stmt
            .query_map(params![q, K as i64], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    };

    let mut t_knn = Timings::new();
    let mut t_knn_filtered = Timings::new();
    let mut t_fts = Timings::new();
    let mut t_hybrid = Timings::new();

    for (nl_query, fts_query) in QUERIES {
        let qv = embed_query(&mut model, nl_query)?;
        for _ in 0..REPS {
            let t = Instant::now();
            let hits = knn(&db, &qv, false)?;
            t_knn.push(t.elapsed());
            assert_eq!(hits.len(), K);

            let t = Instant::now();
            let hits = knn(&db, &qv, true)?;
            t_knn_filtered.push(t.elapsed());
            assert_eq!(hits.len(), K);
            assert!(hits.iter().all(|(id, _)| id % 3 == 0), "filter leaked");

            let t = Instant::now();
            let hits = fts(&db, fts_query)?;
            t_fts.push(t.elapsed());
            assert!(!hits.is_empty(), "FTS found nothing for {fts_query}");

            // end-to-end hybrid: embed + KNN + FTS + RRF fuse
            let t = Instant::now();
            let qv2 = embed_query(&mut model, nl_query)?;
            let vec_hits = knn(&db, &qv2, false)?;
            let fts_hits = fts(&db, fts_query)?;
            let fused = rrf(&[&vec_hits, &fts_hits], 60.0);
            t_hybrid.push(t.elapsed());
            assert!(fused.len() >= K);
        }
    }

    println!(
        "query timings ({} queries x {REPS} reps, k={K}, {N_TOTAL} vectors):",
        QUERIES.len()
    );
    t_knn.report("KNN int8 brute-force, unfiltered");
    t_knn_filtered.report("KNN int8 brute-force, source='usc' filter");
    t_fts.report(&format!("FTS5 MATCH over {} real chunks", chunks.len()));
    t_hybrid.report("hybrid end-to-end (embed+KNN+FTS+RRF)");

    Ok(())
}

/// Reciprocal rank fusion over ranked result lists.
fn rrf(lists: &[&Vec<(i64, f64)>], k: f64) -> Vec<(i64, f64)> {
    let mut scores: HashMap<i64, f64> = HashMap::new();
    for list in lists {
        for (rank, (id, _)) in list.iter().enumerate() {
            *scores.entry(*id).or_default() += 1.0 / (k + rank as f64 + 1.0);
        }
    }
    let mut fused: Vec<(i64, f64)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1));
    fused
}
