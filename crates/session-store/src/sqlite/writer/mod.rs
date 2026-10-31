//! The single writer: SQLite serializes writers whatever the driver, so every mutation is sent
//! to one background task that owns the only write connection.
//!
//! Each command runs in its own transaction and is acknowledged through a `oneshot`. A failed
//! command is reported to its caller and does not stop the writer: one constraint violation must
//! not take the store down for every other session. When the last sender is dropped the writer
//! compacts the WAL and exits.

mod append;
mod prune;
mod session_row;
mod snapshot;

use rusqlite::Connection;
use rusty_protocol::ids::SessionId;
use tokio::sync::{mpsc, oneshot};

use crate::store::{DurableSessionEvent, DurableSessionSnapshot, StoreError};

/// Write requests that may queue before callers wait for room.
const WRITE_CHANNEL_CAPACITY: usize = 1024;

enum WriteCommand {
    Append {
        event: DurableSessionEvent,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    SaveSnapshot {
        snapshot: DurableSessionSnapshot,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    Prune {
        session_id: SessionId,
        sequence: u64,
        reply: oneshot::Sender<Result<u64, StoreError>>,
    },
}

/// The caller's side of the writer task.
pub(super) struct WriterHandle {
    commands: mpsc::Sender<WriteCommand>,
}

impl WriterHandle {
    /// Starts the writer task on `connection`.
    pub(super) fn spawn(connection: Connection) -> Self {
        let (commands, receiver) = mpsc::channel(WRITE_CHANNEL_CAPACITY);
        tokio::task::spawn_blocking(move || run_writer(connection, receiver));
        Self { commands }
    }

    pub(super) async fn append(&self, event: DurableSessionEvent) -> Result<(), StoreError> {
        self.submit(|reply| WriteCommand::Append { event, reply })
            .await
    }

    pub(super) async fn save_snapshot(
        &self,
        snapshot: DurableSessionSnapshot,
    ) -> Result<(), StoreError> {
        self.submit(|reply| WriteCommand::SaveSnapshot { snapshot, reply })
            .await
    }

    pub(super) async fn prune(
        &self,
        session_id: SessionId,
        sequence: u64,
    ) -> Result<u64, StoreError> {
        self.submit(|reply| WriteCommand::Prune {
            session_id,
            sequence,
            reply,
        })
        .await
    }

    async fn submit<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T, StoreError>>) -> WriteCommand,
    ) -> Result<T, StoreError> {
        let (reply, acknowledgement) = oneshot::channel();
        self.commands
            .send(command(reply))
            .await
            .map_err(|_| StoreError::Backend("SQLite writer task is not running".into()))?;
        acknowledgement
            .await
            .map_err(|_| StoreError::Backend("SQLite writer task terminated".into()))?
    }
}

fn run_writer(mut connection: Connection, mut commands: mpsc::Receiver<WriteCommand>) {
    while let Some(command) = commands.blocking_recv() {
        match command {
            WriteCommand::Append { event, reply } => {
                let _ = reply.send(append::append_event(&mut connection, event));
            }
            WriteCommand::SaveSnapshot { snapshot, reply } => {
                let _ = reply.send(snapshot::save_snapshot(&mut connection, snapshot));
            }
            WriteCommand::Prune {
                session_id,
                sequence,
                reply,
            } => {
                let _ = reply.send(prune::prune_events(&mut connection, session_id, sequence));
            }
        }
    }
    let _ = connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
}
