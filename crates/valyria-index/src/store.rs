//! [`IndexStore`]: persistence and point-in-time reads.
//!
//! Every write goes through [`IndexStore::write_generation`], which is the
//! only place that assigns a generation number. Every read takes a
//! [`Generation`] and sees exactly the rows that were live at it — the
//! snapshot isolation D8 asks for, implemented as a `WHERE` clause rather
//! than a lock.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension, Row};
use valyria_lang::{Span, SymbolKind};
use valyria_store::Store;
use valyria_types::Generation;
use valyria_util::ContentHash;

use crate::error::{IndexError, Result};
use crate::record::{
    CallRecord, FileRecord, GenerationInfo, GenerationStage, ImportRecord, IndexDelta, IndexStats,
    RelPath, SymbolRecord, TestRecord,
};
use crate::scan::ScannedFile;

/// A store call's result: the outer layer is the actor's own failure mode,
/// the inner one is this crate's. Keeping them separate lets a closure
/// running on the actor thread return a domain error (an unknown
/// generation, say) without pretending it was a SQLite failure.
type Call<T> = valyria_store::Result<Result<T>>;

/// How to read a publish: what the absence of a path means, and whether
/// an unchanged file still needs rewriting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishOptions {
    /// Treat any live path absent from `scanned` as deleted. Set for a
    /// full rebuild, which is what makes a rebuild converge on the truth
    /// even if the incremental pipeline previously missed a delete;
    /// cleared for an incremental update, where absence just means "not
    /// part of this change".
    pub authoritative: bool,
    /// Rewrite every scanned path even when its content hash is unchanged.
    /// The bootstrap's second stage needs this: the file bytes are
    /// identical to the files-only generation, but the rows must be
    /// rewritten to carry the symbols that stage did not extract.
    pub force_rewrite: bool,
    pub stage: GenerationStage,
}

impl PublishOptions {
    /// A full rebuild: absent means deleted.
    pub fn full() -> Self {
        Self {
            authoritative: true,
            force_rewrite: false,
            stage: GenerationStage::Complete,
        }
    }

    /// An incremental update: only the named paths are in scope.
    pub fn incremental() -> Self {
        Self {
            authoritative: false,
            force_rewrite: false,
            stage: GenerationStage::Complete,
        }
    }

    pub fn stage(mut self, stage: GenerationStage) -> Self {
        self.stage = stage;
        self
    }

    pub fn force_rewrite(mut self) -> Self {
        self.force_rewrite = true;
        self
    }
}

#[derive(Clone)]
pub struct IndexStore {
    store: Arc<Store>,
}

// `valyria_store::Store` owns a channel and a thread handle and does not
// implement `Debug`; the useful thing to print here is which database the
// index lives in.
impl std::fmt::Debug for IndexStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexStore")
            .field("db", &self.store.db_path())
            .finish()
    }
}

impl IndexStore {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// The newest published generation, or `None` when this workspace has
    /// never been indexed.
    pub async fn current(&self) -> Result<Option<GenerationInfo>> {
        self.store
            .call(|conn| Ok(current_generation(conn)?))
            .await
            .map_err(IndexError::from)
    }

    /// The newest published generation, or [`IndexError::NotIndexed`].
    /// The convenience form for callers that cannot proceed without one.
    pub async fn current_generation(&self) -> Result<Generation> {
        self.current()
            .await?
            .map(|info| info.generation)
            .ok_or(IndexError::NotIndexed)
    }

    pub async fn generations(&self) -> Result<Vec<GenerationInfo>> {
        self.store
            .call(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT generation, stage, file_count, symbol_count, created_at_ms
                     FROM index_generation ORDER BY generation",
                )?;
                let rows = stmt
                    .query_map([], generation_info_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .map_err(IndexError::from)
    }

    /// Publish a new generation.
    ///
    /// `scanned` is the complete content for every path it mentions and
    /// `removed` names paths known to be gone; [`PublishOptions`] says how
    /// to read the gaps between them.
    ///
    /// Publishing nothing is not a generation: if no file was added,
    /// modified or removed, the current generation stands. Otherwise every
    /// no-op filesystem event would invalidate every step's snapshot.
    pub async fn write_generation(
        &self,
        scanned: Vec<ScannedFile>,
        removed: Vec<RelPath>,
        options: PublishOptions,
    ) -> Result<IndexDelta> {
        self.store
            .call(move |conn| {
                let tx = conn.transaction()?;
                let delta = apply_generation(&tx, &scanned, &removed, options)?;
                tx.commit()?;
                Ok(Ok(delta))
            })
            .await
            .map_err(IndexError::from)?
    }

    pub async fn files(&self, generation: Generation) -> Result<Vec<FileRecord>> {
        self.query_at(generation, move |conn, g| {
            let mut stmt = conn.prepare(
                "SELECT path, language, content_hash, size_bytes, line_count, is_binary,
                        has_parse_errors
                 FROM index_file
                 WHERE valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)
                 ORDER BY path",
            )?;
            let rows = stmt
                .query_map([g], file_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn file(&self, generation: Generation, path: &str) -> Result<Option<FileRecord>> {
        let path = path.to_string();
        self.query_at(generation, move |conn, g| {
            conn.query_row(
                "SELECT path, language, content_hash, size_bytes, line_count, is_binary,
                        has_parse_errors
                 FROM index_file
                 WHERE path = ?2 AND valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)",
                params![g, path],
                file_from_row,
            )
            .optional()
        })
        .await
    }

    pub async fn symbols_in(
        &self,
        generation: Generation,
        path: &str,
    ) -> Result<Vec<SymbolRecord>> {
        let path = path.to_string();
        self.query_at(generation, move |conn, g| {
            let mut stmt = conn.prepare(
                "SELECT path, name, kind, symbol_path, start_byte, end_byte, start_line, end_line,
                        name_start_byte, name_end_byte, name_start_line, signature, doc
                 FROM index_symbol
                 WHERE path = ?2 AND valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)
                 ORDER BY start_byte",
            )?;
            let rows = stmt
                .query_map(params![g, path], symbol_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Every symbol with this exact name, across the repository. The
    /// lookup behind "go to definition" and behind the graph's call
    /// resolution.
    pub async fn symbols_named(
        &self,
        generation: Generation,
        name: &str,
    ) -> Result<Vec<SymbolRecord>> {
        let name = name.to_string();
        self.query_at(generation, move |conn, g| {
            let mut stmt = conn.prepare(
                "SELECT path, name, kind, symbol_path, start_byte, end_byte, start_line, end_line,
                        name_start_byte, name_end_byte, name_start_line, signature, doc
                 FROM index_symbol
                 WHERE name = ?2 AND valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)
                 ORDER BY path, start_byte",
            )?;
            let rows = stmt
                .query_map(params![g, name], symbol_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Resolve a symbol path (`Parser::parse`), optionally narrowed to one
    /// file. Several files can define the same path, so this returns every
    /// match and lets the caller decide — the symbol-aware edit strategy
    /// refuses to guess when there is more than one.
    pub async fn symbols_by_path(
        &self,
        generation: Generation,
        symbol_path: &str,
        file: Option<&str>,
    ) -> Result<Vec<SymbolRecord>> {
        let symbol_path = symbol_path.to_string();
        let file = file.map(|f| f.to_string());
        self.query_at(generation, move |conn, g| {
            let mut stmt = conn.prepare(
                "SELECT path, name, kind, symbol_path, start_byte, end_byte, start_line, end_line,
                        name_start_byte, name_end_byte, name_start_line, signature, doc
                 FROM index_symbol
                 WHERE symbol_path = ?2
                   AND (?3 IS NULL OR path = ?3)
                   AND valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)
                 ORDER BY path, start_byte",
            )?;
            let rows = stmt
                .query_map(params![g, symbol_path, file], symbol_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn all_symbols(&self, generation: Generation) -> Result<Vec<SymbolRecord>> {
        self.query_at(generation, move |conn, g| {
            let mut stmt = conn.prepare(
                "SELECT path, name, kind, symbol_path, start_byte, end_byte, start_line, end_line,
                        name_start_byte, name_end_byte, name_start_line, signature, doc
                 FROM index_symbol
                 WHERE valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)
                 ORDER BY path, start_byte",
            )?;
            let rows = stmt
                .query_map([g], symbol_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn imports_of(
        &self,
        generation: Generation,
        path: &str,
    ) -> Result<Vec<ImportRecord>> {
        let path = path.to_string();
        self.query_at(generation, move |conn, g| {
            let mut stmt = conn.prepare(
                "SELECT path, raw_path, start_line FROM index_import
                 WHERE path = ?2 AND valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)
                 ORDER BY start_line, raw_path",
            )?;
            let rows = stmt
                .query_map(params![g, path], import_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn all_imports(&self, generation: Generation) -> Result<Vec<ImportRecord>> {
        self.query_at(generation, move |conn, g| {
            let mut stmt = conn.prepare(
                "SELECT path, raw_path, start_line FROM index_import
                 WHERE valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)
                 ORDER BY path, start_line, raw_path",
            )?;
            let rows = stmt
                .query_map([g], import_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn all_calls(&self, generation: Generation) -> Result<Vec<CallRecord>> {
        self.query_at(generation, move |conn, g| {
            let mut stmt = conn.prepare(
                "SELECT path, name, enclosing_symbol_path, start_line FROM index_call
                 WHERE valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)
                 ORDER BY path, start_line, name",
            )?;
            let rows = stmt
                .query_map([g], call_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn calls_in(&self, generation: Generation, path: &str) -> Result<Vec<CallRecord>> {
        let path = path.to_string();
        self.query_at(generation, move |conn, g| {
            let mut stmt = conn.prepare(
                "SELECT path, name, enclosing_symbol_path, start_line FROM index_call
                 WHERE path = ?2 AND valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)
                 ORDER BY start_line, name",
            )?;
            let rows = stmt
                .query_map(params![g, path], call_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn tests(&self, generation: Generation) -> Result<Vec<TestRecord>> {
        self.query_at(generation, move |conn, g| {
            let mut stmt = conn.prepare(
                "SELECT path, name, symbol_path, start_line FROM index_test
                 WHERE valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)
                 ORDER BY path, start_line",
            )?;
            let rows = stmt
                .query_map([g], test_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Full-text symbol lookup against the *current* generation (§4.14's
    /// FTS5 entry point). Historical generations are served from
    /// [`Self::symbols_named`] instead — keeping retired rows out of FTS
    /// is what keeps it fast.
    pub async fn search_symbols(&self, query: &str, limit: usize) -> Result<Vec<SymbolRecord>> {
        let Some(current) = self.current().await? else {
            return Ok(Vec::new());
        };
        let Some(fts_query) = fts_query_for(query) else {
            return Ok(Vec::new());
        };
        let g = current.generation.0 as i64;

        self.store
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT s.path, s.name, s.kind, s.symbol_path, s.start_byte, s.end_byte,
                            s.start_line, s.end_line, s.name_start_byte, s.name_end_byte, s.name_start_line,
                            s.signature, s.doc
                     FROM index_symbol_fts f
                     JOIN index_symbol s
                       ON s.path = f.path AND s.symbol_path = f.symbol_path
                     WHERE index_symbol_fts MATCH ?1
                       AND s.valid_from <= ?2 AND (s.valid_to IS NULL OR s.valid_to > ?2)
                     ORDER BY rank
                     LIMIT ?3",
                )?;
                let rows = stmt
                    .query_map(params![fts_query, g, limit as i64], symbol_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(Ok(rows))
            })
            .await
            .map_err(IndexError::from)?
    }

    pub async fn stats(&self, generation: Generation) -> Result<IndexStats> {
        let info = self
            .generations()
            .await?
            .into_iter()
            .find(|i| i.generation == generation)
            .ok_or(IndexError::UnknownGeneration(generation))?;

        self.query_at(generation, move |conn, g| {
            let count = |table: &str, extra: &str| -> rusqlite::Result<u64> {
                conn.query_row(
                    &format!(
                        "SELECT COUNT(*) FROM {table}
                         WHERE valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1) {extra}"
                    ),
                    [g],
                    |row| row.get::<_, i64>(0),
                )
                .map(|n| n as u64)
            };

            Ok(IndexStats {
                generation: Generation(g as u64),
                stage: info.stage,
                files: count("index_file", "")?,
                symbols: count("index_symbol", "")?,
                imports: count("index_import", "")?,
                calls: count("index_call", "")?,
                tests: count("index_test", "")?,
                files_with_parse_errors: count("index_file", "AND has_parse_errors = 1")?,
                files_without_language: count("index_file", "AND language IS NULL")?,
            })
        })
        .await
    }

    /// Drop every row retired before `keep_from`, and the generation rows
    /// describing those snapshots. Reads at a pruned generation then fail
    /// loudly with [`IndexError::UnknownGeneration`] rather than silently
    /// answering from newer data.
    ///
    /// The current generation is never pruned, whatever is asked for.
    pub async fn prune_before(&self, keep_from: Generation) -> Result<u64> {
        let Some(current) = self.current().await? else {
            return Ok(0);
        };
        let keep_from = keep_from.min(current.generation);
        let g = keep_from.0 as i64;

        self.store
            .call(move |conn| {
                let tx = conn.transaction()?;
                let mut removed = 0u64;
                for table in [
                    "index_file",
                    "index_symbol",
                    "index_import",
                    "index_call",
                    "index_test",
                ] {
                    removed += tx.execute(
                        &format!(
                            "DELETE FROM {table} WHERE valid_to IS NOT NULL AND valid_to <= ?1"
                        ),
                        [g],
                    )? as u64;
                }
                tx.execute("DELETE FROM index_generation WHERE generation < ?1", [g])?;
                tx.commit()?;
                Ok(Ok(removed))
            })
            .await
            .map_err(IndexError::from)?
    }

    /// Run `f` against the store, having first checked that `generation`
    /// was actually published.
    async fn query_at<T, F>(&self, generation: Generation, f: F) -> Result<T>
    where
        F: FnOnce(&Connection, i64) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let g = generation.0 as i64;
        self.store
            .call(move |conn| -> Call<T> {
                let known: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM index_generation WHERE generation = ?1)",
                    [g],
                    |row| row.get(0),
                )?;
                if !known {
                    return Ok(Err(IndexError::UnknownGeneration(generation)));
                }
                Ok(Ok(f(conn, g)?))
            })
            .await
            .map_err(IndexError::from)?
    }
}

// --- write path -------------------------------------------------------

fn current_generation(conn: &Connection) -> rusqlite::Result<Option<GenerationInfo>> {
    conn.query_row(
        "SELECT generation, stage, file_count, symbol_count, created_at_ms
         FROM index_generation ORDER BY generation DESC LIMIT 1",
        [],
        generation_info_from_row,
    )
    .optional()
}

fn apply_generation(
    tx: &rusqlite::Transaction<'_>,
    scanned: &[ScannedFile],
    removed: &[RelPath],
    options: PublishOptions,
) -> rusqlite::Result<IndexDelta> {
    let previous = current_generation(tx)?;
    let generation = previous
        .map(|info| info.generation.next())
        .unwrap_or(Generation(1));
    let g = generation.0 as i64;

    let live = live_file_hashes(tx)?;

    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut unchanged = BTreeSet::new();
    for file in scanned {
        match live.get(&file.record.path) {
            None => added.push(file),
            Some(hash) if *hash != file.record.content_hash || options.force_rewrite => {
                modified.push(file)
            }
            Some(_) => {
                unchanged.insert(file.record.path.clone());
            }
        }
    }

    let mut gone: BTreeSet<RelPath> = removed
        .iter()
        .filter(|path| live.contains_key(*path))
        .cloned()
        .collect();
    if options.authoritative {
        let present: BTreeSet<&RelPath> = scanned.iter().map(|f| &f.record.path).collect();
        for path in live.keys() {
            if !present.contains(path) {
                gone.insert(path.clone());
            }
        }
    }

    // Nothing changed: keep the existing generation rather than minting a
    // new one that would invalidate every in-flight step's snapshot for no
    // reason. A first-ever index is the exception — an empty repository
    // still needs a generation to read at.
    if added.is_empty() && modified.is_empty() && gone.is_empty() {
        if let Some(previous) = previous {
            return Ok(IndexDelta {
                generation: previous.generation,
                ..Default::default()
            });
        }
    }

    // Retire everything about a path that changed or vanished. Unchanged
    // files keep their existing rows, which is the whole reason rows are
    // keyed by path rather than by a per-generation file id.
    let retiring: Vec<&RelPath> = modified
        .iter()
        .map(|f| &f.record.path)
        .chain(gone.iter())
        .collect();
    for path in &retiring {
        retire_path(tx, path, g)?;
    }

    for file in added.iter().chain(modified.iter()) {
        insert_file(tx, file, g)?;
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let file_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM index_file
             WHERE valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)",
        [g],
        |row| row.get(0),
    )?;
    let symbol_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM index_symbol
             WHERE valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)",
        [g],
        |row| row.get(0),
    )?;

    tx.execute(
        "INSERT INTO index_generation (generation, created_at_ms, stage, file_count, symbol_count)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![g, now_ms, options.stage.as_str(), file_count, symbol_count],
    )?;

    Ok(IndexDelta {
        generation,
        added: added.iter().map(|f| f.record.path.clone()).collect(),
        modified: modified.iter().map(|f| f.record.path.clone()).collect(),
        removed: gone.into_iter().collect(),
    })
}

fn live_file_hashes(
    tx: &rusqlite::Transaction<'_>,
) -> rusqlite::Result<BTreeMap<RelPath, ContentHash>> {
    let mut stmt =
        tx.prepare("SELECT path, content_hash FROM index_file WHERE valid_to IS NULL")?;
    let rows = stmt
        .query_map([], |row| {
            let path: String = row.get(0)?;
            let hash: String = row.get(1)?;
            Ok((path, hash))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(rows
        .into_iter()
        .filter_map(|(path, hash)| ContentHash::from_str(&hash).ok().map(|h| (path, h)))
        .collect())
}

fn retire_path(tx: &rusqlite::Transaction<'_>, path: &str, g: i64) -> rusqlite::Result<()> {
    for table in [
        "index_file",
        "index_symbol",
        "index_import",
        "index_call",
        "index_test",
    ] {
        tx.execute(
            &format!("UPDATE {table} SET valid_to = ?1 WHERE path = ?2 AND valid_to IS NULL"),
            params![g, path],
        )?;
    }
    tx.execute("DELETE FROM index_symbol_fts WHERE path = ?1", [path])?;
    Ok(())
}

fn insert_file(tx: &rusqlite::Transaction<'_>, file: &ScannedFile, g: i64) -> rusqlite::Result<()> {
    let record = &file.record;
    tx.execute(
        "INSERT INTO index_file
            (path, language, content_hash, size_bytes, line_count, is_binary,
             has_parse_errors, valid_from, valid_to)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)",
        params![
            record.path,
            record.language,
            record.content_hash.to_hex(),
            record.size_bytes as i64,
            record.line_count as i64,
            record.is_binary as i64,
            record.has_parse_errors as i64,
            g,
        ],
    )?;

    for symbol in &file.facts.symbols {
        tx.execute(
            "INSERT INTO index_symbol
                (path, name, kind, symbol_path, start_byte, end_byte, start_line, end_line,
                 name_start_byte, name_end_byte, name_start_line, signature, doc,
                 valid_from, valid_to)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, NULL)",
            params![
                record.path,
                symbol.name,
                symbol.kind.as_str(),
                symbol.symbol_path,
                symbol.span.start_byte as i64,
                symbol.span.end_byte as i64,
                symbol.span.start_line as i64,
                symbol.span.end_line as i64,
                symbol.name_span.start_byte as i64,
                symbol.name_span.end_byte as i64,
                symbol.name_span.start_line as i64,
                symbol.signature,
                symbol.doc,
                g,
            ],
        )?;

        tx.execute(
            "INSERT INTO index_symbol_fts (name, symbol_path, path, kind) VALUES (?1, ?2, ?3, ?4)",
            params![
                symbol.name,
                symbol.symbol_path,
                record.path,
                symbol.kind.as_str()
            ],
        )?;
    }

    for import in &file.facts.imports {
        tx.execute(
            "INSERT INTO index_import (path, raw_path, start_line, valid_from, valid_to)
             VALUES (?1, ?2, ?3, ?4, NULL)",
            params![
                record.path,
                import.raw_path,
                import.span.start_line as i64,
                g
            ],
        )?;
    }

    for call in &file.facts.calls {
        tx.execute(
            "INSERT INTO index_call
                (path, name, enclosing_symbol_path, start_line, valid_from, valid_to)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![
                record.path,
                call.name,
                call.enclosing_symbol_path,
                call.span.start_line as i64,
                g
            ],
        )?;
    }

    for test in &file.facts.tests {
        tx.execute(
            "INSERT INTO index_test (path, name, symbol_path, start_line, valid_from, valid_to)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![
                record.path,
                test.name,
                test.symbol_path,
                test.span.start_line as i64,
                g
            ],
        )?;
    }

    Ok(())
}

// --- row decoding -----------------------------------------------------

fn generation_info_from_row(row: &Row<'_>) -> rusqlite::Result<GenerationInfo> {
    let stage: String = row.get(1)?;
    Ok(GenerationInfo {
        generation: Generation(row.get::<_, i64>(0)? as u64),
        // An unrecognized stage string can only come from a newer release
        // writing to an older one's database; treating it as complete is
        // the reading that degrades least badly.
        stage: GenerationStage::parse(&stage).unwrap_or(GenerationStage::Complete),
        file_count: row.get::<_, i64>(2)? as u64,
        symbol_count: row.get::<_, i64>(3)? as u64,
        created_at_ms: row.get(4)?,
    })
}

fn file_from_row(row: &Row<'_>) -> rusqlite::Result<FileRecord> {
    let hash: String = row.get(2)?;
    Ok(FileRecord {
        path: row.get(0)?,
        language: row.get(1)?,
        content_hash: ContentHash::from_str(&hash).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?,
        size_bytes: row.get::<_, i64>(3)? as u64,
        line_count: row.get::<_, i64>(4)? as u32,
        is_binary: row.get::<_, i64>(5)? != 0,
        has_parse_errors: row.get::<_, i64>(6)? != 0,
    })
}

fn symbol_from_row(row: &Row<'_>) -> rusqlite::Result<SymbolRecord> {
    let kind: String = row.get(2)?;
    Ok(SymbolRecord {
        path: row.get(0)?,
        name: row.get(1)?,
        kind: SymbolKind::parse(&kind).unwrap_or(SymbolKind::Variable),
        symbol_path: row.get(3)?,
        span: Span {
            start_byte: row.get::<_, i64>(4)? as usize,
            end_byte: row.get::<_, i64>(5)? as usize,
            start_line: row.get::<_, i64>(6)? as u32,
            end_line: row.get::<_, i64>(7)? as u32,
        },
        name_span: Span {
            start_byte: row.get::<_, i64>(8)? as usize,
            end_byte: row.get::<_, i64>(9)? as usize,
            // An identifier never spans lines, so one stored line covers
            // both ends of the name span.
            start_line: row.get::<_, i64>(10)? as u32,
            end_line: row.get::<_, i64>(10)? as u32,
        },
        signature: row.get(11)?,
        doc: row.get(12)?,
    })
}

fn import_from_row(row: &Row<'_>) -> rusqlite::Result<ImportRecord> {
    Ok(ImportRecord {
        path: row.get(0)?,
        raw_path: row.get(1)?,
        start_line: row.get::<_, i64>(2)? as u32,
    })
}

fn call_from_row(row: &Row<'_>) -> rusqlite::Result<CallRecord> {
    Ok(CallRecord {
        path: row.get(0)?,
        name: row.get(1)?,
        enclosing_symbol_path: row.get(2)?,
        start_line: row.get::<_, i64>(3)? as u32,
    })
}

fn test_from_row(row: &Row<'_>) -> rusqlite::Result<TestRecord> {
    Ok(TestRecord {
        path: row.get(0)?,
        name: row.get(1)?,
        symbol_path: row.get(2)?,
        start_line: row.get::<_, i64>(3)? as u32,
    })
}

/// Turn a user's search words into an FTS5 `MATCH` expression.
///
/// User input is never interpolated raw: FTS5 has its own operator syntax
/// (`NEAR`, `*`, `-`, `"`), and a stray quote in a search box would be a
/// syntax error rather than a search. Each alphanumeric run becomes a
/// quoted prefix term, which is also what makes `par` find `parse`.
fn fts_query_for(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{term}\"*"))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_queries_are_built_from_words_not_interpolated_raw() {
        assert_eq!(fts_query_for("parse"), Some("\"parse\"*".to_string()));
        assert_eq!(
            fts_query_for("Parser::parse"),
            Some("\"Parser\"* OR \"parse\"*".to_string())
        );
    }

    #[test]
    fn fts_operator_characters_in_user_input_cannot_reach_the_matcher() {
        // A bare `"` or `*` would be an FTS5 syntax error; splitting on
        // non-alphanumerics drops them before they get there.
        assert_eq!(
            fts_query_for("\"a* NEAR b"),
            Some("\"a\"* OR \"NEAR\"* OR \"b\"*".to_string())
        );
    }

    #[test]
    fn a_query_with_no_searchable_characters_matches_nothing() {
        assert_eq!(fts_query_for("   "), None);
        assert_eq!(fts_query_for("***"), None);
    }
}
