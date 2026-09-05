//! License bodies for the catalog's models (§4.21 "license surfacing").
//!
//! `ModelCard` carries only the SPDX-ish `license_name` and an optional
//! `license_url`. The runtime works fully offline, so an in-app license
//! acceptance prompt (`model_install { accept_license }`) needs the license
//! *text* available without a network fetch. Every distinct `license_name`
//! in `catalog.json` has its body bundled here via `include_str!`.
//!
//! These are the standard, verbatim license texts. If a future catalog entry
//! introduces a `license_name` with no match here, [`license_text`] returns
//! `None` and the client falls back to `license_url`.

/// The full body of `license_name`, when it is one Core bundles.
///
/// Matching is exact on the SPDX-ish name used in `catalog.json`
/// (`Apache-2.0`, `MIT`, `Llama-3.1-Community`).
pub fn license_text(license_name: &str) -> Option<&'static str> {
    match license_name {
        "Apache-2.0" => Some(include_str!("licenses/Apache-2.0.txt")),
        "MIT" => Some(include_str!("licenses/MIT.txt")),
        "Llama-3.1-Community" => Some(include_str!("licenses/Llama-3.1-Community.txt")),
        _ => None,
    }
}

/// Whether Core can show the full license body for `license_name` locally.
pub fn has_license_text(license_name: &str) -> bool {
    license_text(license_name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Catalog;

    #[test]
    fn every_catalog_license_has_bundled_text() {
        let catalog = Catalog::embedded().unwrap();
        for card in catalog.cards() {
            assert!(
                license_text(&card.license_name).is_some(),
                "no bundled license text for `{}` (model `{}`) — add \
                 crates/valyria-model-registry/src/licenses/{}.txt",
                card.license_name,
                card.id,
                card.license_name,
            );
        }
    }

    #[test]
    fn bundled_texts_are_non_trivial() {
        for name in ["Apache-2.0", "MIT", "Llama-3.1-Community"] {
            let text = license_text(name).unwrap();
            assert!(text.len() > 200, "{name} text looks truncated");
        }
    }

    #[test]
    fn unknown_license_has_no_text() {
        assert!(license_text("BSD-Nonexistent-9.0").is_none());
        assert!(!has_license_text("BSD-Nonexistent-9.0"));
    }
}
