//! S4: How do FTS5 tokenizer configs handle statutory citations like §401(k)(2)(B)?
//!
//! Prints a match matrix: tokenizer config x query -> which sample docs matched.

use anyhow::Result;
use rusqlite::Connection;

const DOCS: &[&str] = &[
    "Under section 401(k)(2)(B)(i), a qualified cash or deferred arrangement must meet distribution requirements.",
    "See § 401(k) of the Internal Revenue Code for cash or deferred arrangements.",
    "A 401k plan lets employees defer part of their salary.",
    "Gross income defined. 26 U.S.C. § 61(a)(1) includes compensation for services.",
    "Attach Schedule B (Form 1040) if your taxable interest exceeds $1,500.",
    "For tax year 2026, see the Instructions for Form 1040, line 3a.",
    "Section 61(a) provides that gross income means all income from whatever source derived.",
];

const CONFIGS: &[(&str, &str)] = &[
    ("unicode61 (default)", "unicode61"),
    ("unicode61 tokenchars '§'", "unicode61 tokenchars '§'"),
    ("unicode61 tokenchars '§()'", "unicode61 tokenchars '§()'"),
    ("trigram", "trigram"),
];

const QUERIES: &[&str] = &[
    "401(k)",     // raw, unquoted: expected to be an FTS5 syntax error
    "\"401(k)\"", // phrase-quoted
    "\"§ 401(k)\"",
    "\"§401(k)(2)(B)\"",
    "401k",
    "\"section 401\"",
    "\"Form 1040\"",
    "\"61(a)\"",
];

fn main() -> Result<()> {
    println!("docs:");
    for (i, d) in DOCS.iter().enumerate() {
        println!("  [{}] {}", i + 1, &d[..d.len().min(80)]);
    }
    println!();
    print!("{:<28}", "query \\ tokenizer");
    for (label, _) in CONFIGS {
        print!("{label:<28}");
    }
    println!();

    let dbs: Vec<(&str, Connection)> = CONFIGS
        .iter()
        .map(|(label, tok)| {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(&format!("CREATE VIRTUAL TABLE t USING fts5(body, tokenize=\"{tok}\");"))
                .unwrap();
            for doc in DOCS {
                db.execute("INSERT INTO t(body) VALUES (?1)", [doc]).unwrap();
            }
            (*label, db)
        })
        .collect();

    for query in QUERIES {
        print!("{query:<28}");
        for (_, db) in &dbs {
            let cell = match matches(db, query) {
                Ok(ids) if ids.is_empty() => "-".to_string(),
                Ok(ids) => ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(","),
                Err(_) => "ERR".to_string(),
            };
            print!("{cell:<28}");
        }
        println!();
    }
    Ok(())
}

fn matches(db: &Connection, query: &str) -> Result<Vec<i64>> {
    let mut stmt = db.prepare("SELECT rowid FROM t WHERE t MATCH ?1 ORDER BY rowid")?;
    let ids = stmt
        .query_map([query], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}
