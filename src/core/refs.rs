//! Opaque ref store with TTL (poka-yoke, spec §4.5).
//!
//! `act_on_element` / `window_control` / targeted `screenshot` only accept
//! refs minted here by a recent observe/inspect/window_query in this session.
//! Fabricated or expired refs fail closed; callers re-observe.

use crate::types::Ref;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const REF_TTL: Duration = Duration::from_secs(300);

pub struct RefStore {
    inner: Mutex<Inner>,
}

struct Inner {
    live: HashMap<String, Instant>,
    windows: HashSet<String>,
}

impl RefStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                live: HashMap::new(),
                windows: HashSet::new(),
            }),
        }
    }

    /// Mint element refs; returns the input ids as [`Ref`]s after registering.
    pub async fn mint_elements(&self, ids: Vec<String>) -> Vec<Ref> {
        let mut inner = self.inner.lock().await;
        let now = Instant::now();
        ids.into_iter()
            .map(|id| {
                inner.live.insert(id.clone(), now);
                Ref(id)
            })
            .collect()
    }

    pub async fn mint_windows(&self, ids: Vec<String>) -> Vec<Ref> {
        let mut inner = self.inner.lock().await;
        let now = Instant::now();
        ids.into_iter()
            .map(|id| {
                inner.live.insert(id.clone(), now);
                inner.windows.insert(id.clone());
                Ref(id)
            })
            .collect()
    }

    /// Validate without consuming. Prunes expired entries on each check.
    pub async fn check(&self, r: &Ref) -> bool {
        let mut inner = self.inner.lock().await;
        let now = Instant::now();
        inner.live.retain(|_, t| now.duration_since(*t) <= REF_TTL);
        if !inner.live.contains_key(&r.0) {
            return false;
        }
        inner.live.insert(r.0.clone(), now);
        true
    }

    pub async fn is_window(&self, r: &Ref) -> bool {
        self.inner.lock().await.windows.contains(&r.0)
    }
}

impl Default for RefStore {
    fn default() -> Self {
        Self::new()
    }
}
