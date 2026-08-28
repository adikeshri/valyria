//! The model pool and admission control (§4.22, §41). Loading a second
//! model on a memory-constrained machine may require evicting the first.
//! Eviction is **LRU within role priority**: the lowest-priority,
//! least-recently-used resident model goes first, so the primary coder —
//! the task's critical path — survives pressure from an embedder or
//! autocomplete model.
//!
//! This is bookkeeping only: it decides *what* to evict and emits
//! [`PoolEvent`]s. Actually unloading weights is the runtime adapter's job.

use crate::role::Role;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolEvent {
    Loaded {
        id: String,
        footprint_bytes: u64,
    },
    Evicted {
        id: String,
        reason: EvictReason,
    },
    /// Emitted whenever admitting a model required evicting another —
    /// clients surface this so a user understands why throughput dipped.
    ResourcePressure {
        requested_bytes: u64,
        budget_bytes: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvictReason {
    MemoryPressure,
    Manual,
}

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error(
        "model {id:?} needs {needed} bytes but the pool budget is {budget}; it will not fit even with everything else evicted"
    )]
    WontFit {
        id: String,
        needed: u64,
        budget: u64,
    },
}

#[derive(Debug, Clone)]
struct Resident {
    id: String,
    role: Role,
    footprint: u64,
    last_used: u64,
}

#[derive(Debug)]
pub struct ModelPool {
    budget_bytes: u64,
    residents: Vec<Resident>,
    tick: u64,
}

impl ModelPool {
    /// `budget_bytes` is the measured memory the runtime is allowed to
    /// spend on model weights — from `valyria_hardware` available memory
    /// minus the runtime's own working-set reservation.
    pub fn new(budget_bytes: u64) -> Self {
        Self {
            budget_bytes,
            residents: Vec::new(),
            tick: 0,
        }
    }

    pub fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    pub fn used_bytes(&self) -> u64 {
        self.residents.iter().map(|r| r.footprint).sum()
    }

    pub fn loaded_ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.residents.iter().map(|r| r.id.clone()).collect();
        v.sort();
        v
    }

    pub fn is_loaded(&self, id: &str) -> bool {
        self.residents.iter().any(|r| r.id == id)
    }

    /// Request that `id` be resident. Returns the events that describe what
    /// happened — possibly an eviction cascade and a `ResourcePressure`
    /// notice, possibly nothing if it was already loaded.
    pub fn admit(
        &mut self,
        id: &str,
        role: Role,
        footprint_bytes: u64,
    ) -> Result<Vec<PoolEvent>, PoolError> {
        self.tick += 1;

        if let Some(r) = self.residents.iter_mut().find(|r| r.id == id) {
            r.last_used = self.tick;
            r.role = role;
            return Ok(Vec::new());
        }

        if footprint_bytes > self.budget_bytes {
            return Err(PoolError::WontFit {
                id: id.to_string(),
                needed: footprint_bytes,
                budget: self.budget_bytes,
            });
        }

        let mut events = Vec::new();
        let mut evicted_any = false;
        let incoming_priority = role.priority();
        while self.used_bytes() + footprint_bytes > self.budget_bytes {
            // Only evict models at or below the incoming role's priority —
            // a background embedder never displaces the primary coder.
            let Some(victim) = self.pick_victim(incoming_priority) else {
                return Err(PoolError::WontFit {
                    id: id.to_string(),
                    needed: footprint_bytes,
                    budget: self.budget_bytes,
                });
            };
            let victim_id = self.residents[victim].id.clone();
            self.residents.remove(victim);
            events.push(PoolEvent::Evicted {
                id: victim_id,
                reason: EvictReason::MemoryPressure,
            });
            evicted_any = true;
        }

        if evicted_any {
            events.insert(
                0,
                PoolEvent::ResourcePressure {
                    requested_bytes: footprint_bytes,
                    budget_bytes: self.budget_bytes,
                },
            );
        }

        self.residents.push(Resident {
            id: id.to_string(),
            role,
            footprint: footprint_bytes,
            last_used: self.tick,
        });
        events.push(PoolEvent::Loaded {
            id: id.to_string(),
            footprint_bytes,
        });
        Ok(events)
    }

    /// Manually unload a model (e.g. task finished, role rebound).
    pub fn evict(&mut self, id: &str) -> Option<PoolEvent> {
        let pos = self.residents.iter().position(|r| r.id == id)?;
        self.residents.remove(pos);
        Some(PoolEvent::Evicted {
            id: id.to_string(),
            reason: EvictReason::Manual,
        })
    }

    /// Among residents at or below `ceiling` priority: lowest priority
    /// first, then least-recently-used. `None` if nothing is eligible —
    /// which is how a higher-priority resident is protected from eviction.
    fn pick_victim(&self, ceiling: u8) -> Option<usize> {
        self.residents
            .iter()
            .enumerate()
            .filter(|(_, r)| r.role.priority() <= ceiling)
            .min_by(|(_, a), (_, b)| {
                a.role
                    .priority()
                    .cmp(&b.role.priority())
                    .then(a.last_used.cmp(&b.last_used))
            })
            .map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;

    #[test]
    fn load_within_budget_emits_only_loaded() {
        let mut pool = ModelPool::new(16 * GB);
        let ev = pool.admit("coder", Role::PrimaryCoder, 7 * GB).unwrap();
        assert_eq!(
            ev,
            vec![PoolEvent::Loaded {
                id: "coder".into(),
                footprint_bytes: 7 * GB
            }]
        );
        assert_eq!(pool.used_bytes(), 7 * GB);
    }

    #[test]
    fn re_admitting_a_loaded_model_is_a_no_op_touch() {
        let mut pool = ModelPool::new(16 * GB);
        pool.admit("coder", Role::PrimaryCoder, 7 * GB).unwrap();
        assert!(pool
            .admit("coder", Role::PrimaryCoder, 7 * GB)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn coder_plus_embedder_coexist_on_16gb() {
        let mut pool = ModelPool::new(16 * GB);
        pool.admit("coder", Role::PrimaryCoder, 7 * GB).unwrap();
        let ev = pool.admit("embed", Role::Embedder, GB).unwrap();
        // No eviction — this is the §4.22 design constraint.
        assert_eq!(
            ev,
            vec![PoolEvent::Loaded {
                id: "embed".into(),
                footprint_bytes: GB
            }]
        );
        assert_eq!(pool.loaded_ids(), vec!["coder", "embed"]);
    }

    #[test]
    fn pressure_evicts_lowest_priority_first_and_keeps_the_coder() {
        let mut pool = ModelPool::new(10 * GB);
        pool.admit("coder", Role::PrimaryCoder, 6 * GB).unwrap();
        pool.admit("embed", Role::Embedder, 2 * GB).unwrap();
        pool.admit("rerank", Role::Reranker, GB).unwrap();

        // A planner needs 3 GB; used is 9, budget 10 → must free ≥2 GB from
        // models at or below the planner's priority (never the coder).
        let ev = pool.admit("planner", Role::Planner, 3 * GB).unwrap();

        assert_eq!(
            ev[0],
            PoolEvent::ResourcePressure {
                requested_bytes: 3 * GB,
                budget_bytes: 10 * GB
            }
        );
        let evicted: Vec<&str> = ev
            .iter()
            .filter_map(|e| match e {
                PoolEvent::Evicted { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        // rerank (priority 30) then embed (priority 35) — coder never.
        assert_eq!(evicted, vec!["rerank", "embed"]);
        assert!(pool.is_loaded("coder"));
        assert!(pool.is_loaded("planner"));
    }

    #[test]
    fn lru_breaks_ties_within_a_priority() {
        let mut pool = ModelPool::new(3 * GB);
        pool.admit("a", Role::Reranker, GB).unwrap(); // tick 1
        pool.admit("b", Role::Reranker, GB).unwrap(); // tick 2
        pool.admit("a", Role::Reranker, GB).unwrap(); // tick 3 — touch a
                                                      // Now admit c (1 GB) — used 2, budget 3, fits without eviction.
        pool.admit("c", Role::Reranker, GB).unwrap();
        // used 3 == budget. Admit d → evict LRU of equal priority = b.
        let ev = pool.admit("d", Role::Reranker, GB).unwrap();
        let evicted: Vec<&str> = ev
            .iter()
            .filter_map(|e| match e {
                PoolEvent::Evicted { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(evicted, vec!["b"]);
    }

    #[test]
    fn a_low_priority_model_will_not_evict_a_higher_priority_one() {
        let mut pool = ModelPool::new(8 * GB);
        pool.admit("coder", Role::PrimaryCoder, 6 * GB).unwrap();
        // An embedder needs 4 GB; the only way to fit is evicting the
        // coder, which outranks it — so admission is refused.
        assert!(matches!(
            pool.admit("embed", Role::Embedder, 4 * GB),
            Err(PoolError::WontFit { .. })
        ));
        assert!(pool.is_loaded("coder"));
        assert!(!pool.is_loaded("embed"));
    }

    #[test]
    fn a_model_bigger_than_the_whole_budget_wont_fit() {
        let mut pool = ModelPool::new(4 * GB);
        assert!(matches!(
            pool.admit("huge", Role::PrimaryCoder, 8 * GB),
            Err(PoolError::WontFit { .. })
        ));
    }

    #[test]
    fn manual_evict_returns_an_event_and_frees_space() {
        let mut pool = ModelPool::new(8 * GB);
        pool.admit("x", Role::Summarizer, 3 * GB).unwrap();
        assert_eq!(
            pool.evict("x"),
            Some(PoolEvent::Evicted {
                id: "x".into(),
                reason: EvictReason::Manual
            })
        );
        assert_eq!(pool.used_bytes(), 0);
        assert!(pool.evict("x").is_none());
    }
}
