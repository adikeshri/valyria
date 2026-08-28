//! `valyria-graph` — layer 2 (Repository intelligence).
//!
//! The typed knowledge graph (§13): files, modules, symbols and tests as
//! nodes; contains, defines, imports, calls and tests as typed edges.
//! Derived entirely from one [`Generation`](valyria_types::Generation) of
//! the index, so it is always safe to throw away and recompute and can
//! never become a second, disagreeing source of truth.
//!
//! Two things distinguish it from a naive reference graph:
//!
//! **Edges carry confidence.** Without a type checker, a call can only be
//! resolved by name, and names collide. Rather than dropping the uncertain
//! edges or presenting them as facts, each records how it was derived
//! ([`Confidence`]), so ranking can prefer the certain ones and
//! `--explain` can say why a file was pulled into context.
//!
//! **References that leave the repository are kept.** An import of
//! `serde` binds to no node, but "this file depends on serde" is a real
//! fact; it is recorded as an [`UnresolvedRef`] instead of being
//! discarded.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod build;
pub mod error;
pub mod migrations;
pub mod model;
pub mod resolve;
pub mod store;

pub use build::{build, BuiltGraph, GraphInput};
pub use error::{GraphError, Result};
pub use migrations::MIGRATIONS;
pub use model::{
    Confidence, Direction, Edge, EdgeKind, GraphStats, ImpactSet, Node, NodeId, NodeKind, Subgraph,
    UnresolvedRef,
};
pub use resolve::{resolve_call, resolve_import, FileLookup, Resolution, SymbolLookup};
pub use store::{adjacency, GraphStore, DEFAULT_DEPTH};
