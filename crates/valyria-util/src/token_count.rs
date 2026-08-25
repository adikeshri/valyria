//! Token counting trait. The context engine's budget allocator (§11) needs
//! a token count for every candidate item; the *real* count depends on the
//! target model's tokenizer, which only exists once `valyria-model` and a
//! loaded model are available (Phase 9). Everything before that can budget
//! against [`HeuristicTokenCounter`], and nothing above this trait needs to
//! change when a real tokenizer-backed counter is substituted in.

pub trait TokenCounter: Send + Sync {
    fn count(&self, text: &str) -> usize;
}

/// Roughly 4 characters per token for English prose and most source code —
/// a standard approximation used as a placeholder until a model-specific
/// tokenizer is wired in. Deliberately rounds up: overestimating a budget
/// fails safe (triggers the allocator's "narrow the task" path);
/// underestimating risks overflowing a real context window.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicTokenCounter;

impl TokenCounter for HeuristicTokenCounter {
    fn count(&self, text: &str) -> usize {
        text.chars()
            .count()
            .div_ceil(4)
            .max(if text.is_empty() { 0 } else { 1 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_is_zero_tokens() {
        assert_eq!(HeuristicTokenCounter.count(""), 0);
    }

    #[test]
    fn nonempty_string_is_at_least_one_token() {
        assert_eq!(HeuristicTokenCounter.count("a"), 1);
    }

    #[test]
    fn scales_roughly_with_length() {
        let short = HeuristicTokenCounter.count("hello");
        let long = HeuristicTokenCounter.count(&"hello world ".repeat(100));
        assert!(long > short * 50);
    }
}
