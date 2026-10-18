//! Per conversation, what the model was shown in earlier runs, so a page that changed since then is
//! marked as changed. Injected context is rebuilt for every request and never stored in history, so
//! an unchanged page is sent again each time; the ledger only says what is different.
use std::{collections::HashMap, sync::Mutex};

use super::blocks::{Injected, Seen};

/// Conversations tracked at once; the oldest is forgotten first.
const MAX_CONVERSATIONS: usize = 64;

#[derive(Default)]
struct Conversation {
    run: String,
    /// Digests shown in runs before the current one.
    previous: Seen,
    /// Digests shown so far in the current run.
    current: Seen,
}

#[derive(Default)]
pub struct ContextLedger {
    state: Mutex<(Vec<String>, HashMap<String, Conversation>)>,
}

impl ContextLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// What earlier runs of this conversation were shown. Stable for every request of one run, so the
    /// text of a block does not change between its turns.
    pub fn seen(&self, conversation: &str, run: &str) -> Seen {
        let Ok(mut state) = self.state.lock() else {
            return Seen::new();
        };
        let (order, map) = &mut *state;
        let entry = Self::entry(order, map, conversation);
        Self::rotate(entry, run);
        entry.previous.clone()
    }

    /// Remembers what the current run was shown.
    pub fn record(&self, conversation: &str, run: &str, injected: &[Injected]) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let (order, map) = &mut *state;
        let entry = Self::entry(order, map, conversation);
        Self::rotate(entry, run);
        for item in injected.iter().filter(|item| item.excerpt) {
            entry.current.insert(item.page.clone(), item.digest.clone());
        }
    }

    fn rotate(entry: &mut Conversation, run: &str) {
        if entry.run != run {
            let finished = std::mem::take(&mut entry.current);
            entry.previous.extend(finished);
            entry.run = run.to_owned();
        }
    }

    fn entry<'a>(
        order: &mut Vec<String>,
        map: &'a mut HashMap<String, Conversation>,
        conversation: &str,
    ) -> &'a mut Conversation {
        if !map.contains_key(conversation) {
            order.push(conversation.to_owned());
            if order.len() > MAX_CONVERSATIONS {
                let oldest = order.remove(0);
                map.remove(&oldest);
            }
        }
        map.entry(conversation.to_owned()).or_default()
    }
}
