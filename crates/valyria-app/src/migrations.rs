//! `valyria-app`'s own tiny slice of `workspace.db` schema. Versions
//! 200-299 are reserved for this crate, continuing the hundred-block
//! convention started in `valyria-task` (`valyria-events` owns 1-99,
//! `valyria-task` owns 100-199, `valyria-index` owns 300-399,
//! `valyria-graph` owns 400-499, `valyria-embed` owns 500-599,
//! `valyria-memory` owns 600-699, `valyria-verify` owns 700-799,
//! `valyria-plan` owns 800-899).

use valyria_store::Migration;

pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 200,
    description: "create workspace_meta table",
    sql: "CREATE TABLE workspace_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
}];

/// The full migration slice for a workspace's shared `workspace.db`:
/// every crate's migrations, concatenated in one place so `Store::open`
/// only ever needs to be called once. `valyria-app` is the natural owner
/// of this concatenation (§4.1) — no lower-layer crate may depend on
/// another at the same layer to build it themselves.
pub fn workspace_migrations() -> Vec<Migration> {
    let mut all: Vec<Migration> = valyria_events::MIGRATIONS.to_vec();
    all.extend(valyria_task::MIGRATIONS.iter().copied());
    all.extend(MIGRATIONS.iter().copied());
    all.extend(valyria_index::MIGRATIONS.iter().copied());
    all.extend(valyria_graph::MIGRATIONS.iter().copied());
    all.extend(valyria_embed::MIGRATIONS.iter().copied());
    all.extend(valyria_memory::MIGRATIONS.iter().copied());
    all.extend(valyria_verify::MIGRATIONS.iter().copied());
    all.extend(valyria_plan::MIGRATIONS.iter().copied());
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_version_collisions_across_the_whole_slice() {
        let all = workspace_migrations();
        let mut versions: Vec<i64> = all.iter().map(|m| m.version).collect();
        let before = versions.len();
        versions.sort_unstable();
        versions.dedup();
        assert_eq!(
            versions.len(),
            before,
            "duplicate migration version in {all:?}"
        );
    }

    #[test]
    fn applies_cleanly_to_a_fresh_store() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let mut conn = conn;
        valyria_store::run_migrations(&mut conn, &workspace_migrations()).unwrap();
        let applied = valyria_store::applied_versions(&conn).unwrap();
        assert!(applied.contains(&1));
        assert!(applied.contains(&100));
        assert!(applied.contains(&200));
        assert!(applied.contains(&300));
        assert!(applied.contains(&400));
        assert!(applied.contains(&500));
        assert!(applied.contains(&600));
        assert!(applied.contains(&700));
        assert!(applied.contains(&800));
    }

    #[test]
    fn every_crates_block_is_represented() {
        // A crate that grows tables but is never added to the
        // concatenation above fails silently at runtime — the table simply
        // does not exist — so the concatenation is asserted here rather
        // than trusted.
        let versions: Vec<i64> = workspace_migrations().iter().map(|m| m.version).collect();
        for (crate_name, block) in [
            ("valyria-events", 1..100),
            ("valyria-task", 100..200),
            ("valyria-app", 200..300),
            ("valyria-index", 300..400),
            ("valyria-graph", 400..500),
            ("valyria-embed", 500..600),
            ("valyria-memory", 600..700),
            ("valyria-verify", 700..800),
            ("valyria-plan", 800..900),
        ] {
            assert!(
                versions.iter().any(|v| block.contains(v)),
                "no migrations from {crate_name} reached workspace.db"
            );
        }
    }
}
