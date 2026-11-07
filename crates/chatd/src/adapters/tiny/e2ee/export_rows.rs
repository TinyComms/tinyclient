//! Export ordering on disk. Only one cache line/message is opened at a time. The temporary
//! database holds sealed cache values and ordering metadata, never plaintext messages or keys.

use super::{Opened, CACHED};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use e2ee::{store::CacheKey, Error, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub(super) struct Rows {
    // Close SQLite before deleting its file, including on Windows.
    db: Connection,
    _file: tempfile::NamedTempFile,
    pub total: u64,
}

fn sql(error: rusqlite::Error) -> Error {
    Error::Io(format!("History export index: {error}"))
}
fn io(error: std::io::Error) -> Error {
    Error::Io(error.to_string())
}

// An archive record is at most 1 MiB. Allow base64 overhead and ids without permitting a
// damaged append-only log line to grow a buffer without bound.
const LINE_MAX: usize = 2 * 1024 * 1024;

fn line(reader: &mut impl BufRead, out: &mut Vec<u8>) -> std::io::Result<Option<bool>> {
    out.clear();
    let mut oversized = false;
    let mut any = false;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(any.then_some(!oversized));
        }
        any = true;
        let end = buf.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(buf.len(), |at| at + 1);
        if !oversized {
            if out.len() + count > LINE_MAX {
                oversized = true;
                out.clear();
            } else {
                out.extend_from_slice(&buf[..count]);
            }
        }
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(!oversized));
        }
    }
}

impl Rows {
    pub fn scan(
        dir: &Path,
        conversations: Vec<String>,
        key: &CacheKey,
        keep: impl Fn(&Opened) -> bool,
    ) -> Result<Self> {
        let file = tempfile::NamedTempFile::new_in(dir).map_err(io)?;
        let mut db = Connection::open(file.path()).map_err(sql)?;
        db.execute_batch(
            "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;
            PRAGMA cache_size=-2048; PRAGMA temp_store=FILE;
            CREATE TABLE export_rows(conv TEXT NOT NULL, id TEXT NOT NULL, at INTEGER NOT NULL,
                sealed BLOB NOT NULL, PRIMARY KEY(conv,id)) WITHOUT ROWID;
            CREATE INDEX export_order ON export_rows(at DESC,conv,id);",
        )
        .map_err(sql)?;
        {
            let transaction = db.transaction().map_err(sql)?;
            let mut put = transaction
                .prepare(
                    "INSERT INTO export_rows VALUES (?1,?2,?3,?4)
                ON CONFLICT(conv,id) DO UPDATE SET at=excluded.at,sealed=excluded.sealed",
                )
                .map_err(sql)?;
            let mut remove = transaction
                .prepare("DELETE FROM export_rows WHERE conv=?1 AND id=?2")
                .map_err(sql)?;
            let mut buffer = Vec::new();
            for conv in conversations {
                let file = match std::fs::File::open(dir.join(format!("cache-{conv}.log"))) {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(io(error)),
                };
                let mut reader = BufReader::new(file);
                while let Some(valid_size) = line(&mut reader, &mut buffer).map_err(io)? {
                    if !valid_size {
                        continue;
                    }
                    let Ok(text) = std::str::from_utf8(&buffer) else {
                        continue;
                    };
                    let Some((id, encoded)) = text.trim_end_matches(['\r', '\n']).split_once(' ')
                    else {
                        continue;
                    };
                    if !super::super::safe_id(id) {
                        continue;
                    }
                    let Ok(sealed) = STANDARD.decode(encoded) else {
                        continue;
                    };
                    let Ok(plain) = e2ee::store::open_cache_value(key, id.as_bytes(), &sealed)
                    else {
                        continue;
                    };
                    let Ok(opened) = serde_json::from_slice::<Opened>(&plain) else {
                        continue;
                    };
                    // A later valid record replaces the earlier one, even when it now falls
                    // outside the chosen scope. Torn/unauthenticated records never replace it.
                    if keep(&opened) {
                        put.execute(params![conv, id, opened.at, sealed])
                            .map_err(sql)?;
                    } else {
                        remove.execute(params![conv, id]).map_err(sql)?;
                    }
                }
            }
            drop(put);
            drop(remove);
            transaction.commit().map_err(sql)?;
        }
        let total: i64 = db
            .query_row(
                "SELECT coalesce(sum(min(n,?1)),0)
            FROM (SELECT count(*) n FROM export_rows GROUP BY conv)",
                [CACHED as i64],
                |row| row.get(0),
            )
            .map_err(sql)?;
        Ok(Self {
            db,
            _file: file,
            total: total as u64,
        })
    }

    /// Returning false stops after the last record that fits. The index supplies global newest
    /// order; the small counts map holds only conversation ids, rather than history plaintext.
    pub fn visit(
        &self,
        key: &CacheKey,
        mut visit: impl FnMut(&str, &str, Opened) -> Result<bool>,
    ) -> Result<()> {
        let mut counts = HashMap::<String, usize>::new();
        let mut query = self
            .db
            .prepare("SELECT conv,id,sealed FROM export_rows ORDER BY at DESC,conv,id")
            .map_err(sql)?;
        let mut rows = query.query([]).map_err(sql)?;
        while let Some(row) = rows.next().map_err(sql)? {
            let conv: String = row.get(0).map_err(sql)?;
            let count = counts.entry(conv.clone()).or_default();
            if *count >= CACHED {
                continue;
            }
            *count += 1;
            let id: String = row.get(1).map_err(sql)?;
            let sealed: Vec<u8> = row.get(2).map_err(sql)?;
            let plain = e2ee::store::open_cache_value(key, id.as_bytes(), &sealed)?;
            let opened =
                serde_json::from_slice(&plain).map_err(|_| Error::Malformed("export cache row"))?;
            if !visit(&conv, &id, opened)? {
                break;
            }
        }
        Ok(())
    }
}
