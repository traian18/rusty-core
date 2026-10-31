//! Caching extracted facts by file content, so a refresh re-reads only what changed.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use crate::import_graph::model::ExtractedFile;

pub(super) type ContentHash = [u8; 32];

pub(super) type CacheKey = (String, ContentHash, &'static str, u32, usize);

pub(super) type CacheEntries = (HashMap<CacheKey, Arc<ExtractedFile>>, VecDeque<CacheKey>);

/// Bounded per-file extraction cache; entries are pure functions of their key.
pub struct ExtractionCache {
    pub(super) capacity: usize,
    pub(super) entries: Mutex<CacheEntries>,
    pub(super) hits: AtomicU64,
    pub(super) misses: AtomicU64,
}

/// Cumulative cache counters since the cache was created.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
}

pub(super) fn hex(hash: &ContentHash) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl ExtractionCache {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Mutex::default(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub(super) fn get(&self, key: &CacheKey) -> Option<Arc<ExtractedFile>> {
        let found = self
            .entries
            .lock()
            .ok()
            .and_then(|entries| entries.0.get(key).cloned());
        let counter = if found.is_some() {
            &self.hits
        } else {
            &self.misses
        };
        counter.fetch_add(1, Ordering::Relaxed);
        found
    }

    pub(super) fn insert(&self, key: CacheKey, value: Arc<ExtractedFile>) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        if entries.0.insert(key.clone(), value).is_none() {
            entries.1.push_back(key);
        }
        while entries.1.len() > self.capacity {
            if let Some(oldest) = entries.1.pop_front() {
                entries.0.remove(&oldest);
            }
        }
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }
}
