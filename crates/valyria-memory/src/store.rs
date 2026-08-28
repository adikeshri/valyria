//! [`MemoryStore`]: persistence, decay-aware retrieval, and the
//! inspect/purge surface `valyria clean --memory` is built on.
//!
//! Row parsing that can fail with [`MemoryError::Corrupt`] always happens
//! *outside* the store actor closure — the closure only ever returns
//! `rusqlite`/`StoreError`, and a [`RawRow`] carries the untyped columns
//! across the boundary to be validated in async context.

use std::collections::HashSet;
use std::sync::Arc;

use rusqlite::params;
use valyria_store::Store;
use valyria_types::MemoryId;

use crate::entry::{MemoryAuthor, MemoryEntry, MemoryKind, MemoryScope};
use crate::error::{MemoryError, Result};

/// A retrieval request against one or more scopes.
#[derive(Debug, Clone)]
pub struct RetrievalRequest {
    /// Which scopes to draw from. A `Session` or `Task` scope must name
    /// the exact id; `Repository` and `User` match all rows of that tier.
    pub scopes: Vec<MemoryScope>,
    /// Free text — the task intent, an error message, a topic. Scored by
    /// term overlap against each entry's text.
    pub query: String,
    pub now_ms: i64,
    pub half_life_ms: i64,
    /// Cap on ranked (non-pinned) results.
    pub limit: usize,
    /// Ranked entries below this decayed confidence are dropped. Pinned
    /// (session) entries ignore this.
    pub min_effective_confidence: f64,
}

impl RetrievalRequest {
    pub fn new(query: impl Into<String>, now_ms: i64) -> Self {
        Self {
            scopes: Vec::new(),
            query: query.into(),
            now_ms,
            half_life_ms: crate::entry::DEFAULT_HALF_LIFE_MS,
            limit: 8,
            min_effective_confidence: 0.15,
        }
    }

    pub fn scope(mut self, scope: MemoryScope) -> Self {
        self.scopes.push(scope);
        self
    }

    pub fn scopes(mut self, scopes: impl IntoIterator<Item = MemoryScope>) -> Self {
        self.scopes.extend(scopes);
        self
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn min_effective_confidence(mut self, min: f64) -> Self {
        self.min_effective_confidence = min;
        self
    }
}

/// One ranked entry and the numbers behind its rank.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredMemory {
    pub entry: MemoryEntry,
    /// Term overlap with the query, `[0, 1]`.
    pub relevance: f64,
    /// Decayed confidence at `now_ms`.
    pub effective_confidence: f64,
    /// `relevance * effective_confidence` — what the entries are sorted by.
    pub score: f64,
}

/// The result of [`MemoryStore::retrieve`]: session memory that is always
/// shown, and everything else ranked and truncated to the limit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RetrievedMemory {
    /// Session-scoped entries — placed in the prompt header unconditionally.
    pub pinned: Vec<MemoryEntry>,
    /// Task/repository/user entries, best first.
    pub ranked: Vec<ScoredMemory>,
}

impl RetrievedMemory {
    pub fn is_empty(&self) -> bool {
        self.pinned.is_empty() && self.ranked.is_empty()
    }
}

/// Counts for `storage.inspect` (§4.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryStats {
    pub live: u64,
    pub retired: u64,
    pub session: u64,
    pub task: u64,
    pub repository: u64,
    pub user: u64,
}

/// What [`MemoryStore::purge`] should delete.
#[derive(Debug, Clone)]
pub enum PurgeScope {
    /// Every entry.
    All,
    /// Every entry in one tier (id-bearing scopes match that exact id).
    Scope(MemoryScope),
    /// Only entries already retired.
    Retired,
}

pub struct MemoryStore {
    store: Arc<Store>,
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryStore").finish_non_exhaustive()
    }
}

impl MemoryStore {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Insert an entry. Returns its id. Fails loudly on an out-of-range
    /// confidence rather than clamping silently — the constructors clamp,
    /// so a bad value here means a caller built the struct by hand wrong.
    pub async fn write(&self, entry: MemoryEntry) -> Result<MemoryId> {
        self.write_all(vec![entry]).await.map(|ids| ids[0])
    }

    /// Write many entries in one transaction (used by extraction).
    pub async fn write_all(&self, entries: Vec<MemoryEntry>) -> Result<Vec<MemoryId>> {
        for e in &entries {
            if !(0.0..=1.0).contains(&e.confidence) {
                return Err(MemoryError::BadConfidence(e.confidence));
            }
        }
        let ids: Vec<MemoryId> = entries.iter().map(|e| e.id).collect();
        self.store
            .call(move |conn| {
                let tx = conn.transaction()?;
                {
                    let mut stmt = tx.prepare(
                        "INSERT INTO memory_entry
                         (id, scope_kind, scope_id, author, kind, text, provenance,
                          confidence, created_ms, last_seen_ms, uses, retired, retired_reason)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                    )?;
                    for e in &entries {
                        stmt.execute(params![
                            e.id.to_string(),
                            e.scope.kind_str(),
                            e.scope.id_str(),
                            e.author.as_str(),
                            e.kind.as_str(),
                            e.text,
                            e.provenance,
                            e.confidence,
                            e.created_ms,
                            e.last_seen_ms,
                            e.uses,
                            e.retired as i64,
                            e.retired_reason,
                        ])?;
                    }
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        Ok(ids)
    }

    pub async fn get(&self, id: MemoryId) -> Result<Option<MemoryEntry>> {
        let key = id.to_string();
        let raw: Option<RawRow> = self
            .store
            .call(move |conn| {
                let mut stmt = conn.prepare(&format!("{SELECT_COLUMNS} WHERE id = ?1"))?;
                let mut rows = stmt.query(params![key])?;
                match rows.next()? {
                    Some(row) => Ok(Some(RawRow::read(row)?)),
                    None => Ok(None),
                }
            })
            .await?;
        raw.map(RawRow::into_entry).transpose()
    }

    /// Mark an entry seen *now* and bump its use count — decay is measured
    /// from `last_seen_ms`, so this is how a still-true memory stays alive.
    pub async fn reinforce(&self, id: MemoryId, now_ms: i64) -> Result<()> {
        let key = id.to_string();
        self.store
            .call(move |conn| {
                conn.execute(
                    "UPDATE memory_entry SET last_seen_ms = ?2, uses = uses + 1
                     WHERE id = ?1 AND retired = 0",
                    params![key, now_ms],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Retire an entry: it stops being retrieved and its effective
    /// confidence goes to zero. Idempotent.
    pub async fn retire(&self, id: MemoryId, reason: impl Into<String>) -> Result<()> {
        let key = id.to_string();
        let reason = reason.into();
        self.store
            .call(move |conn| {
                conn.execute(
                    "UPDATE memory_entry SET retired = 1, retired_reason = ?2 WHERE id = ?1",
                    params![key, reason],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Retire every live entry for which `predicate` holds — the hook for
    /// "this memory was contradicted by evidence" (§4.19). Returns the
    /// number retired.
    pub async fn retire_matching(
        &self,
        predicate: impl Fn(&MemoryEntry) -> bool,
        reason: impl Into<String>,
    ) -> Result<u64> {
        let reason = reason.into();
        let live = self.load_live().await?;
        let doomed: Vec<String> = live
            .into_iter()
            .filter(|e| predicate(e))
            .map(|e| e.id.to_string())
            .collect();
        if doomed.is_empty() {
            return Ok(0);
        }
        let n = doomed.len() as u64;
        self.store
            .call(move |conn| {
                let tx = conn.transaction()?;
                for id in &doomed {
                    tx.execute(
                        "UPDATE memory_entry SET retired = 1, retired_reason = ?2 WHERE id = ?1",
                        params![id, reason],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        Ok(n)
    }

    /// Decay-aware retrieval. Session entries come back pinned and
    /// unfiltered; everything else is scored by term overlap times decayed
    /// confidence, filtered by `min_effective_confidence`, and truncated
    /// to `limit`.
    pub async fn retrieve(&self, req: RetrievalRequest) -> Result<RetrievedMemory> {
        let query_terms: HashSet<String> = terms(&req.query).into_iter().collect();
        let entries = self.load_live().await?;

        let mut pinned = Vec::new();
        let mut ranked = Vec::new();

        for entry in entries {
            if !req.scopes.iter().any(|w| scope_matches(w, &entry.scope)) {
                continue;
            }
            if entry.scope.is_pinned() {
                pinned.push(entry);
                continue;
            }
            let eff = entry.effective_confidence(req.now_ms, req.half_life_ms);
            if eff < req.min_effective_confidence {
                continue;
            }
            let relevance = jaccard(&query_terms, &terms(&entry.text).into_iter().collect());
            if relevance <= 0.0 {
                continue;
            }
            let score = relevance * eff;
            ranked.push(ScoredMemory {
                entry,
                relevance,
                effective_confidence: eff,
                score,
            });
        }

        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.entry.last_seen_ms.cmp(&a.entry.last_seen_ms))
                .then_with(|| a.entry.id.cmp(&b.entry.id))
        });
        ranked.truncate(req.limit);

        pinned.sort_by(|a, b| {
            b.last_seen_ms
                .cmp(&a.last_seen_ms)
                .then_with(|| a.id.cmp(&b.id))
        });

        Ok(RetrievedMemory { pinned, ranked })
    }

    /// Delete entries. Returns how many rows were removed. This is a hard
    /// delete — the row is gone, not retired.
    pub async fn purge(&self, scope: PurgeScope) -> Result<u64> {
        let n = self
            .store
            .call(move |conn| {
                let n = match scope {
                    PurgeScope::All => conn.execute("DELETE FROM memory_entry", [])?,
                    PurgeScope::Retired => {
                        conn.execute("DELETE FROM memory_entry WHERE retired = 1", [])?
                    }
                    PurgeScope::Scope(s) => match s.id_str() {
                        Some(id) => conn.execute(
                            "DELETE FROM memory_entry WHERE scope_kind = ?1 AND scope_id = ?2",
                            params![s.kind_str(), id],
                        )?,
                        None => conn.execute(
                            "DELETE FROM memory_entry WHERE scope_kind = ?1",
                            params![s.kind_str()],
                        )?,
                    },
                };
                Ok(n as u64)
            })
            .await?;
        Ok(n)
    }

    pub async fn stats(&self) -> Result<MemoryStats> {
        let s = self
            .store
            .call(|conn| {
                let mut s = MemoryStats::default();
                let mut stmt = conn.prepare(
                    "SELECT scope_kind, retired, COUNT(*) FROM memory_entry \
                     GROUP BY scope_kind, retired",
                )?;
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    let kind: String = row.get(0)?;
                    let retired: i64 = row.get(1)?;
                    let count = row.get::<_, i64>(2)? as u64;
                    if retired == 1 {
                        s.retired += count;
                    } else {
                        s.live += count;
                    }
                    match kind.as_str() {
                        "session" => s.session += count,
                        "task" => s.task += count,
                        "repository" => s.repository += count,
                        "user" => s.user += count,
                        _ => {}
                    }
                }
                Ok(s)
            })
            .await?;
        Ok(s)
    }

    async fn load_live(&self) -> Result<Vec<MemoryEntry>> {
        let raws: Vec<RawRow> = self
            .store
            .call(|conn| {
                let mut stmt = conn.prepare(&format!("{SELECT_COLUMNS} WHERE retired = 0"))?;
                let rows = stmt
                    .query_map([], RawRow::read)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        raws.into_iter().map(RawRow::into_entry).collect()
    }
}

fn scope_matches(wanted: &MemoryScope, have: &MemoryScope) -> bool {
    match (wanted, have) {
        (MemoryScope::Repository, MemoryScope::Repository) => true,
        (MemoryScope::User, MemoryScope::User) => true,
        (MemoryScope::Session(a), MemoryScope::Session(b)) => a == b,
        (MemoryScope::Task(a), MemoryScope::Task(b)) => a == b,
        _ => false,
    }
}

const SELECT_COLUMNS: &str = "SELECT id, scope_kind, scope_id, author, kind, text, provenance, \
     confidence, created_ms, last_seen_ms, uses, retired, retired_reason FROM memory_entry";

/// The columns of one `memory_entry` row, untyped — read inside the store
/// closure (infallible `row.get`), validated into a [`MemoryEntry`]
/// afterward.
struct RawRow {
    id: String,
    scope_kind: String,
    scope_id: Option<String>,
    author: String,
    kind: String,
    text: String,
    provenance: String,
    confidence: f64,
    created_ms: i64,
    last_seen_ms: i64,
    uses: i64,
    retired: i64,
    retired_reason: Option<String>,
}

impl RawRow {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            scope_kind: row.get(1)?,
            scope_id: row.get(2)?,
            author: row.get(3)?,
            kind: row.get(4)?,
            text: row.get(5)?,
            provenance: row.get(6)?,
            confidence: row.get(7)?,
            created_ms: row.get(8)?,
            last_seen_ms: row.get(9)?,
            uses: row.get(10)?,
            retired: row.get(11)?,
            retired_reason: row.get(12)?,
        })
    }

    fn into_entry(self) -> Result<MemoryEntry> {
        let id: MemoryId = self
            .id
            .parse()
            .map_err(|_| MemoryError::Corrupt(self.id.clone(), "id"))?;
        Ok(MemoryEntry {
            id,
            scope: MemoryScope::from_row(&self.scope_kind, self.scope_id.as_deref(), &self.id)?,
            author: MemoryAuthor::parse(&self.author, &self.id)?,
            kind: MemoryKind::parse(&self.kind, &self.id)?,
            text: self.text,
            provenance: self.provenance,
            confidence: self.confidence,
            created_ms: self.created_ms,
            last_seen_ms: self.last_seen_ms,
            uses: self.uses as u32,
            retired: self.retired != 0,
            retired_reason: self.retired_reason,
        })
    }
}

/// Words too common to carry retrieval signal.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "in", "on", "for", "is", "are", "be", "it", "this",
    "that", "with", "as", "at", "by", "do", "how", "i", "we", "you", "where", "what", "when",
    "which", "there", "here", "was", "were", "has", "have", "had",
];

/// Lowercased alphanumeric tokens of length > 1, minus stopwords.
fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() > 1)
        .map(|t| t.to_lowercase())
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{MemoryKind, DEFAULT_HALF_LIFE_MS};
    use valyria_types::{SessionId, TaskId};

    async fn store() -> MemoryStore {
        let s = Store::open_in_memory(crate::MIGRATIONS).unwrap();
        MemoryStore::new(Arc::new(s))
    }

    #[tokio::test]
    async fn write_then_get_round_trips() {
        let m = store().await;
        let e = MemoryEntry::agent(
            MemoryScope::Repository,
            MemoryKind::Command,
            "cargo test --workspace passes",
            "task_abc",
            0.7,
            1000,
        );
        let id = m.write(e.clone()).await.unwrap();
        let got = m.get(id).await.unwrap().unwrap();
        assert_eq!(got, e);
    }

    #[tokio::test]
    async fn retrieve_ranks_by_relevance_times_decayed_confidence() {
        let m = store().await;
        m.write(MemoryEntry::agent(
            MemoryScope::Repository,
            MemoryKind::Command,
            "the parser lives in src/parser.rs",
            "t",
            0.9,
            0,
        ))
        .await
        .unwrap();
        m.write(MemoryEntry::agent(
            MemoryScope::Repository,
            MemoryKind::ArchNote,
            "the http client retries three times",
            "t",
            0.9,
            0,
        ))
        .await
        .unwrap();

        let got = m
            .retrieve(
                RetrievalRequest::new("where is the parser code", 0).scope(MemoryScope::Repository),
            )
            .await
            .unwrap();
        assert_eq!(got.ranked.len(), 1);
        assert!(got.ranked[0].entry.text.contains("parser"));
    }

    #[tokio::test]
    async fn decayed_entries_fall_below_the_confidence_floor() {
        let m = store().await;
        m.write(MemoryEntry::agent(
            MemoryScope::Repository,
            MemoryKind::Command,
            "run make build to compile",
            "t",
            0.5,
            0,
        ))
        .await
        .unwrap();

        let got = m
            .retrieve(
                RetrievalRequest::new("how do I build", 4 * DEFAULT_HALF_LIFE_MS)
                    .scope(MemoryScope::Repository),
            )
            .await
            .unwrap();
        assert!(got.ranked.is_empty());
    }

    #[tokio::test]
    async fn reinforce_resets_decay() {
        let m = store().await;
        let id = m
            .write(MemoryEntry::agent(
                MemoryScope::Repository,
                MemoryKind::Command,
                "run make build to compile the project",
                "t",
                0.5,
                0,
            ))
            .await
            .unwrap();
        m.reinforce(id, 4 * DEFAULT_HALF_LIFE_MS).await.unwrap();
        let got = m
            .retrieve(
                RetrievalRequest::new("how do I build the project", 4 * DEFAULT_HALF_LIFE_MS)
                    .scope(MemoryScope::Repository),
            )
            .await
            .unwrap();
        assert_eq!(got.ranked.len(), 1);
    }

    #[tokio::test]
    async fn session_memory_is_pinned_and_ignores_relevance_and_confidence() {
        let m = store().await;
        let sid = SessionId::new();
        m.write(MemoryEntry::agent(
            MemoryScope::Session(sid),
            MemoryKind::Freeform,
            "user prefers terse explanations",
            "runtime",
            0.01,
            0,
        ))
        .await
        .unwrap();
        let got = m
            .retrieve(
                RetrievalRequest::new("completely unrelated query", 10 * DEFAULT_HALF_LIFE_MS)
                    .scope(MemoryScope::Session(sid)),
            )
            .await
            .unwrap();
        assert_eq!(got.pinned.len(), 1);
        assert!(got.ranked.is_empty());
    }

    #[tokio::test]
    async fn retire_removes_an_entry_from_retrieval() {
        let m = store().await;
        let id = m
            .write(MemoryEntry::agent(
                MemoryScope::Repository,
                MemoryKind::Pitfall,
                "the flaky network test sometimes fails",
                "t",
                0.9,
                0,
            ))
            .await
            .unwrap();
        m.retire(id, "fixed in task_xyz").await.unwrap();
        let got = m
            .retrieve(RetrievalRequest::new("flaky network test", 0).scope(MemoryScope::Repository))
            .await
            .unwrap();
        assert!(got.ranked.is_empty());
        assert!(m.get(id).await.unwrap().unwrap().retired);
    }

    #[tokio::test]
    async fn scopes_are_isolated() {
        let m = store().await;
        let t1 = TaskId::new();
        let t2 = TaskId::new();
        m.write(MemoryEntry::agent(
            MemoryScope::Task(t1),
            MemoryKind::Freeform,
            "task one note about the widget",
            "t",
            0.9,
            0,
        ))
        .await
        .unwrap();
        let got = m
            .retrieve(RetrievalRequest::new("widget", 0).scope(MemoryScope::Task(t2)))
            .await
            .unwrap();
        assert!(got.ranked.is_empty(), "task 2 must not see task 1's memory");
    }

    #[tokio::test]
    async fn purge_by_scope_and_stats() {
        let m = store().await;
        for i in 0..3 {
            m.write(MemoryEntry::agent(
                MemoryScope::Repository,
                MemoryKind::Freeform,
                format!("repo note {i}"),
                "t",
                0.9,
                0,
            ))
            .await
            .unwrap();
        }
        m.write(MemoryEntry::user(
            MemoryScope::User,
            MemoryKind::Convention,
            "global convention",
            0,
        ))
        .await
        .unwrap();

        let s = m.stats().await.unwrap();
        assert_eq!(s.repository, 3);
        assert_eq!(s.user, 1);
        assert_eq!(s.live, 4);

        let removed = m
            .purge(PurgeScope::Scope(MemoryScope::Repository))
            .await
            .unwrap();
        assert_eq!(removed, 3);
        assert_eq!(m.stats().await.unwrap().live, 1);
    }

    #[tokio::test]
    async fn retire_matching_retires_contradicted_entries() {
        let m = store().await;
        m.write(MemoryEntry::agent(
            MemoryScope::Repository,
            MemoryKind::Command,
            "npm test is the test command",
            "t",
            0.8,
            0,
        ))
        .await
        .unwrap();
        let n = m
            .retire_matching(|e| e.text.contains("npm test"), "project switched to pnpm")
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn out_of_range_confidence_is_rejected() {
        let m = store().await;
        let mut e = MemoryEntry::agent(
            MemoryScope::Repository,
            MemoryKind::Command,
            "x",
            "t",
            0.5,
            0,
        );
        e.confidence = 1.5;
        assert!(matches!(
            m.write(e).await,
            Err(MemoryError::BadConfidence(_))
        ));
    }
}
