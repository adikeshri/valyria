//! Contradiction detection between instruction sources.
//!
//! This is a *heuristic*, and deliberately a conservative one: it looks
//! for two directive lines that share a topic but carry opposite polarity
//! ("always use tabs" vs. "never use tabs"). It will miss subtle
//! conflicts and it will not invent one from vague prose. When it does
//! fire, resolution is not a judgement call — the higher-authority source
//! wins, every time, and the loser is reported so a client can show it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::source::InstructionSource;

/// Two directive lines that appear to contradict each other. `winner` is
/// always the higher-authority source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionConflict {
    /// The shared salient words, sorted — what the two lines are both
    /// "about".
    pub topic: String,
    pub winner: PathBuf,
    pub winner_line: String,
    pub loser: PathBuf,
    pub loser_line: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Polarity {
    Positive,
    Negative,
}

/// Words that flip a directive negative. Checked as whole words.
const NEGATIVE_MARKERS: &[&str] = &[
    "never", "don't", "dont", "avoid", "no", "not", "without", "disallow", "forbid",
];
/// Words that make a directive an affirmative requirement.
const POSITIVE_MARKERS: &[&str] = &[
    "always", "must", "should", "require", "prefer", "use", "ensure", "do",
];
/// Low-signal words dropped before building a topic key.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "to", "of", "in", "on", "for", "and", "or", "is", "are", "be", "this",
    "that", "it", "with", "when", "as", "at", "by", "we", "you", "your", "our", "please", "all",
    "any", "every", "code", "should", "must",
];

/// Scan every pair of directive sources for contradicting lines. `sources`
/// must already be in authority order (highest first); the earlier of a
/// pair is the winner.
pub fn detect(sources: &[InstructionSource]) -> Vec<InstructionConflict> {
    let per_source: Vec<Vec<(String, String, Polarity)>> = sources
        .iter()
        .map(|s| {
            if s.is_directive() {
                directive_lines(&s.body)
            } else {
                Vec::new()
            }
        })
        .collect();

    let mut out = Vec::new();
    for i in 0..sources.len() {
        for j in (i + 1)..sources.len() {
            for (topic_a, line_a, pol_a) in &per_source[i] {
                for (topic_b, line_b, pol_b) in &per_source[j] {
                    if topic_a == topic_b && !topic_a.is_empty() && pol_a != pol_b {
                        out.push(InstructionConflict {
                            topic: topic_a.clone(),
                            winner: sources[i].origin.clone(),
                            winner_line: line_a.clone(),
                            loser: sources[j].origin.clone(),
                            loser_line: line_b.clone(),
                        });
                    }
                }
            }
        }
    }
    out
}

/// Pull `(topic_key, original_line, polarity)` from every line that reads
/// like a directive (contains a polarity marker).
fn directive_lines(body: &str) -> Vec<(String, String, Polarity)> {
    let mut out = Vec::new();
    for raw in body.lines() {
        let line = raw
            .trim()
            .trim_start_matches(['-', '*', '+', '#', '>'])
            .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')')
            .trim();
        if line.is_empty() {
            continue;
        }
        let words: Vec<String> = line
            .split(|c: char| !c.is_alphanumeric() && c != '\'')
            .filter(|w| !w.is_empty())
            .map(|w| w.to_lowercase())
            .collect();
        if words.is_empty() {
            continue;
        }

        let negative = words.iter().any(|w| NEGATIVE_MARKERS.contains(&w.as_str()));
        let positive = words.iter().any(|w| POSITIVE_MARKERS.contains(&w.as_str()));
        let polarity = match (positive, negative) {
            (_, true) => Polarity::Negative,
            (true, false) => Polarity::Positive,
            (false, false) => continue, // not a directive-shaped line
        };

        let mut salient: Vec<String> = words
            .into_iter()
            .filter(|w| {
                !STOPWORDS.contains(&w.as_str())
                    && !NEGATIVE_MARKERS.contains(&w.as_str())
                    && !POSITIVE_MARKERS.contains(&w.as_str())
                    && w.len() > 1
            })
            .collect();
        salient.sort();
        salient.dedup();
        if salient.is_empty() {
            continue;
        }
        out.push((salient.join(" "), line.to_string(), polarity));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::Authority;
    use crate::source::FileFingerprint;

    fn src(authority: Authority, origin: &str, body: &str) -> InstructionSource {
        InstructionSource {
            trust: authority.trust(),
            authority,
            origin: origin.into(),
            body: body.to_string(),
            truncated: false,
            bytes_on_disk: body.len() as u64,
            fingerprint: FileFingerprint {
                mtime_ms: 0,
                len: body.len() as u64,
            },
        }
    }

    #[test]
    fn opposite_polarity_on_the_same_topic_is_a_conflict_won_by_authority() {
        let sources = vec![
            src(
                Authority::Agents,
                "AGENTS.md",
                "- Never use tabs for indentation.",
            ),
            src(
                Authority::Claude,
                "CLAUDE.md",
                "- Always use tabs for indentation.",
            ),
        ];
        let conflicts = detect(&sources);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].winner, PathBuf::from("AGENTS.md"));
        assert_eq!(conflicts[0].loser, PathBuf::from("CLAUDE.md"));
        assert!(conflicts[0].topic.contains("tabs"));
    }

    #[test]
    fn agreeing_lines_are_not_a_conflict() {
        let sources = vec![
            src(
                Authority::Agents,
                "AGENTS.md",
                "- Always run the tests before committing.",
            ),
            src(
                Authority::Claude,
                "CLAUDE.md",
                "- You must run the tests before committing.",
            ),
        ];
        assert!(detect(&sources).is_empty());
    }

    #[test]
    fn unrelated_topics_do_not_collide() {
        let sources = vec![
            src(Authority::Agents, "AGENTS.md", "- Never use tabs."),
            src(
                Authority::Claude,
                "CLAUDE.md",
                "- Always write doc comments.",
            ),
        ];
        assert!(detect(&sources).is_empty());
    }

    #[test]
    fn advisory_sources_do_not_participate() {
        let sources = vec![
            src(Authority::Claude, "CLAUDE.md", "- Always use tabs."),
            src(
                Authority::Advisory,
                "README.md",
                "We never use tabs in this project.",
            ),
        ];
        assert!(detect(&sources).is_empty());
    }

    #[test]
    fn prose_without_a_polarity_marker_is_ignored() {
        let sources = vec![
            src(
                Authority::Agents,
                "AGENTS.md",
                "The parser lives in src/parser.rs.",
            ),
            src(
                Authority::Claude,
                "CLAUDE.md",
                "The parser is in a different place.",
            ),
        ];
        assert!(detect(&sources).is_empty());
    }
}
