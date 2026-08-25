//! Bounded output capture with head/tail retention (§20): a runaway
//! command (`yes`, a build tool stuck in a log-spam loop) must never be
//! allowed to exhaust memory, but the *useful* parts of long output — the
//! beginning (what command ran, initial errors) and the end (the final
//! failure, the actual assertion) — are both worth keeping over the noisy
//! middle.

use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedOutput {
    /// Lossily-decoded UTF-8 text: head, an elision marker if truncated,
    /// then tail.
    pub text: String,
    pub truncated: bool,
    pub total_bytes: u64,
}

impl CapturedOutput {
    pub fn empty() -> Self {
        Self {
            text: String::new(),
            truncated: false,
            total_bytes: 0,
        }
    }
}

pub struct CappedOutput {
    head_cap: usize,
    tail_cap: usize,
    /// Bytes seen so far, used while still under budget. Once the budget
    /// is exceeded this is split into `head`/`tail` and abandoned — see
    /// `push`.
    buf: Vec<u8>,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total_bytes: u64,
    truncated: bool,
}

impl CappedOutput {
    pub fn new(max_bytes: usize) -> Self {
        let head_cap = max_bytes / 2;
        let tail_cap = max_bytes - head_cap;
        Self {
            head_cap,
            tail_cap,
            buf: Vec::new(),
            head: Vec::new(),
            tail: VecDeque::new(),
            total_bytes: 0,
            truncated: false,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.total_bytes += chunk.len() as u64;

        if !self.truncated {
            self.buf.extend_from_slice(chunk);
            if self.buf.len() as u64 <= (self.head_cap + self.tail_cap) as u64 {
                return; // still within budget; nothing dropped yet
            }
            // Just crossed the threshold: snapshot head, seed the rolling
            // tail with whatever's left, and switch modes for good.
            self.truncated = true;
            let full = std::mem::take(&mut self.buf);
            let split = self.head_cap.min(full.len());
            self.head = full[..split].to_vec();
            self.tail = full[split..].iter().copied().collect();
            while self.tail.len() > self.tail_cap {
                self.tail.pop_front();
            }
            return;
        }

        self.tail.extend(chunk.iter().copied());
        while self.tail.len() > self.tail_cap {
            self.tail.pop_front();
        }
    }

    pub fn into_output(self) -> CapturedOutput {
        if !self.truncated {
            return CapturedOutput {
                text: String::from_utf8_lossy(&self.buf).into_owned(),
                truncated: false,
                total_bytes: self.total_bytes,
            };
        }

        let tail_bytes: Vec<u8> = self.tail.into_iter().collect();
        let elided = self.total_bytes - self.head.len() as u64 - tail_bytes.len() as u64;
        let marker = format!("\n... [{elided} bytes elided] ...\n");

        let mut combined = self.head;
        combined.extend_from_slice(marker.as_bytes());
        combined.extend_from_slice(&tail_bytes);

        CapturedOutput {
            text: String::from_utf8_lossy(&combined).into_owned(),
            truncated: true,
            total_bytes: self.total_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_output_is_not_truncated() {
        let mut cap = CappedOutput::new(1000);
        cap.push(b"hello");
        cap.push(b" world");
        let out = cap.into_output();
        assert_eq!(out.text, "hello world");
        assert!(!out.truncated);
        assert_eq!(out.total_bytes, 11);
    }

    #[test]
    fn oversized_output_keeps_head_and_tail() {
        let mut cap = CappedOutput::new(20); // head_cap=10, tail_cap=10
        cap.push(b"AAAAAAAAAA"); // 10 'A's
        cap.push(&vec![b'M'; 1000]); // middle, should be elided
        cap.push(b"ZZZZZZZZZZ"); // 10 'Z's
        let out = cap.into_output();

        assert!(out.truncated);
        assert!(out.text.starts_with("AAAAAAAAAA"));
        assert!(out.text.ends_with("ZZZZZZZZZZ"));
        assert!(out.text.contains("bytes elided"));
        assert_eq!(out.total_bytes, 1020);
    }

    #[test]
    fn exactly_at_budget_is_not_truncated() {
        let mut cap = CappedOutput::new(10); // head_cap=5, tail_cap=5
        cap.push(&[b'x'; 10]);
        let out = cap.into_output();
        assert!(!out.truncated);
        assert_eq!(out.text.len(), 10);
    }

    #[test]
    fn one_byte_over_budget_truncates() {
        let mut cap = CappedOutput::new(10);
        cap.push(&[b'x'; 11]);
        let out = cap.into_output();
        assert!(out.truncated);
    }

    #[test]
    fn no_content_duplication_between_head_and_tail() {
        let mut cap = CappedOutput::new(10); // head_cap=5, tail_cap=5
        cap.push(b"0123456789ABCDEF"); // 16 bytes, all in one push
        let out = cap.into_output();
        // head = "01234", tail = last 5 of the remainder = "BCDEF"
        assert!(out.text.starts_with("01234"));
        assert!(out.text.ends_with("BCDEF"));
        // the marker must appear exactly once, and total accounted bytes
        // (head + tail) must be less than total_bytes since some were
        // genuinely elided — not duplicated.
        assert_eq!(out.text.matches("bytes elided").count(), 1);
    }

    #[test]
    fn streamed_in_many_small_chunks_matches_one_big_push() {
        let mut streamed = CappedOutput::new(30);
        let mut bulk = CappedOutput::new(30);
        let data = b"the quick brown fox jumps over the lazy dog, repeatedly, many times over";

        for byte in data {
            streamed.push(&[*byte]);
        }
        bulk.push(data);

        assert_eq!(streamed.into_output(), bulk.into_output());
    }
}
