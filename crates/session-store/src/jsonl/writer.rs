//! The task that appends to one session's file.
//!
//! One task per session keeps appends to a file ordered and lets a caller wait for its own record
//! to be written. The file is synced every `sync_interval` appends and when the task ends. After a
//! write or sync failure the task reports the error to the caller and to everything still queued,
//! then stops; the next append starts a fresh task.

use std::path::PathBuf;

use rusty_protocol::ids::SessionId;
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, oneshot},
};

use super::record::JsonlRecord;
use crate::store::StoreError;

/// Appends that may queue before callers wait for room.
const QUEUE_CAPACITY: usize = 64;

pub(super) struct WriteCommand {
    record: JsonlRecord,
    reply: oneshot::Sender<Result<(), StoreError>>,
}

pub(super) type WriterQueue = mpsc::Sender<WriteCommand>;

/// Starts the writer task for the file at `path`.
pub(super) fn spawn_writer(path: PathBuf, sync_interval: u32) -> WriterQueue {
    let (queue, commands) = mpsc::channel(QUEUE_CAPACITY);
    tokio::spawn(run_writer(path, commands, sync_interval));
    queue
}

/// Queues `record` and waits until the writer has written it.
pub(super) async fn write_through(
    queue: &WriterQueue,
    session_id: SessionId,
    record: JsonlRecord,
) -> Result<(), StoreError> {
    let (reply, acknowledgement) = oneshot::channel();
    queue
        .send(WriteCommand { record, reply })
        .await
        .map_err(|_| {
            StoreError::Backend(format!(
                "session writer task for {session_id} is not running"
            ))
        })?;
    acknowledgement.await.map_err(|_| {
        StoreError::Backend(format!(
            "session writer task for {session_id} terminated before acknowledging"
        ))
    })?
}

async fn run_writer(path: PathBuf, mut commands: mpsc::Receiver<WriteCommand>, sync_interval: u32) {
    let mut file = match open_for_append(&path).await {
        Ok(file) => file,
        Err(error) => return fail_queued(commands, error),
    };
    let mut appends_since_sync: u32 = 0;
    while let Some(command) = commands.recv().await {
        let written = write_record(&mut file, &command.record).await;
        appends_since_sync = appends_since_sync.saturating_add(1);
        let outcome = match written {
            Ok(()) if appends_since_sync >= sync_interval => {
                appends_since_sync = 0;
                file.sync_data()
                    .await
                    .map_err(|error| StoreError::Io(error.into()))
            }
            other => other,
        };
        match outcome {
            Ok(()) => {
                let _ = command.reply.send(Ok(()));
            }
            Err(error) => {
                let _ = command.reply.send(Err(error.clone()));
                return fail_queued(commands, error);
            }
        }
    }
    let _ = file.sync_data().await;
}

async fn open_for_append(path: &PathBuf) -> Result<tokio::fs::File, StoreError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| StoreError::Io(error.into()))?;
    }
    tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|error| StoreError::Io(error.into()))
}

async fn write_record(file: &mut tokio::fs::File, record: &JsonlRecord) -> Result<(), StoreError> {
    let mut line = Vec::new();
    serde_json::to_writer(&mut line, record)?;
    line.push(b'\n');
    file.write_all(&line).await?;
    Ok(())
}

fn fail_queued(mut commands: mpsc::Receiver<WriteCommand>, error: StoreError) {
    while let Ok(command) = commands.try_recv() {
        let _ = command.reply.send(Err(error.clone()));
    }
}
