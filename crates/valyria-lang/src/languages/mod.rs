//! The shipped [`LanguageProvider`](crate::provider::LanguageProvider)
//! implementations, one module per language.
//!
//! Each is ~40 lines of metadata pointing at a `queries/<lang>/`
//! directory. That is the whole cost of adding a language: no change to
//! extraction, indexing, the graph, or search — which is the property D9
//! exists to buy.
//!
//! The set here is the tier-1 list from the build plan's decision 6, cut
//! to the five ecosystems named there (Rust, TypeScript/JavaScript,
//! Python, Go, Java). The remaining tier-1 candidates (C/C++, C#, Ruby,
//! PHP, Kotlin, Swift) and the tier-2 structure-only set are additive:
//! each is a directory and a provider, behind its own cargo feature.

use std::sync::Arc;

use crate::provider::LanguageProvider;

#[cfg(feature = "lang-go")]
pub mod go;
#[cfg(feature = "lang-java")]
pub mod java;
#[cfg(feature = "lang-javascript")]
pub mod javascript;
#[cfg(feature = "lang-python")]
pub mod python;
#[cfg(feature = "lang-rust")]
pub mod rust;
#[cfg(feature = "lang-typescript")]
pub mod typescript;

/// Every provider compiled into this build.
///
/// `Vec::new()` plus conditional pushes rather than a literal: which
/// providers exist depends on the `lang-*` cargo features, and a vec
/// literal cannot be assembled from `cfg`-gated elements.
#[allow(unused_mut, clippy::vec_init_then_push)]
pub fn builtin_providers() -> Vec<Arc<dyn LanguageProvider>> {
    let mut providers: Vec<Arc<dyn LanguageProvider>> = Vec::new();

    #[cfg(feature = "lang-rust")]
    providers.push(Arc::new(rust::Rust));
    #[cfg(feature = "lang-python")]
    providers.push(Arc::new(python::Python));
    #[cfg(feature = "lang-go")]
    providers.push(Arc::new(go::Go));
    #[cfg(feature = "lang-java")]
    providers.push(Arc::new(java::Java));
    #[cfg(feature = "lang-javascript")]
    providers.push(Arc::new(javascript::JavaScript));
    #[cfg(feature = "lang-typescript")]
    {
        providers.push(Arc::new(typescript::TypeScript));
        providers.push(Arc::new(typescript::Tsx));
    }

    providers
}
