//! Gap-free session sequence numbers.

use std::sync::{Arc, Mutex};

use rusty_protocol::ids::SessionId;

use crate::store::{SessionStore, StoreError};

pub struct SessionSequencer {
    store: Arc<dyn SessionStore>,
    session_id: SessionId,
    state: Mutex<SequencerState>,
}

enum SequencerState {
    Uninitialized,
    Ready { next: u64 },
}

impl SessionSequencer {
    pub fn new(store: Arc<dyn SessionStore>, session_id: SessionId) -> Self {
        Self {
            store,
            session_id,
            state: Mutex::new(SequencerState::Uninitialized),
        }
    }

    pub async fn next_sequence(&self) -> Result<u64, StoreError> {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match *state {
                SequencerState::Ready { next } => {
                    let current = next;
                    *state = SequencerState::Ready {
                        next: next.saturating_add(1),
                    };
                    return Ok(current);
                }
                SequencerState::Uninitialized => {}
            }
        }

        let committed = self.store.current_sequence(self.session_id).await?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match *state {
            SequencerState::Ready { next } => {
                let current = next;
                *state = SequencerState::Ready {
                    next: next.saturating_add(1),
                };
                Ok(current)
            }
            SequencerState::Uninitialized => {
                let next = committed.saturating_add(1);
                *state = SequencerState::Ready {
                    next: next.saturating_add(1),
                };
                Ok(next)
            }
        }
    }
}
