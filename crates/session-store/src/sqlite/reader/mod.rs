//! Reads run on pooled connections and never touch the writer's channel. WAL mode lets them
//! proceed while a write is in flight.

mod catalog;
mod events;
mod load;
mod raw;
mod sequence;
mod snapshot;

use std::path::Path;

use r2d2_sqlite::SqliteConnectionManager;

pub(super) use catalog::list_sessions;
pub(super) use events::events_since;
pub(super) use load::load_session;
pub(super) use raw::raw_records;
pub(super) use sequence::current_sequence;

use super::schema::BUSY_TIMEOUT;
use crate::store::StoreError;

pub(super) type ReadPool = r2d2::Pool<SqliteConnectionManager>;

/// Connections kept for reading.
const READ_POOL_MAX_SIZE: u32 = 8;

pub(super) fn build_read_pool(path: &Path) -> Result<ReadPool, StoreError> {
    let manager = SqliteConnectionManager::file(path).with_init(|connection| {
        connection.busy_timeout(BUSY_TIMEOUT)?;
        Ok(())
    });
    r2d2::Pool::builder()
        .max_size(READ_POOL_MAX_SIZE)
        .build(manager)
        .map_err(|error| StoreError::Backend(format!("build read pool: {error}")))
}

fn checkout(
    pool: &ReadPool,
) -> Result<r2d2::PooledConnection<SqliteConnectionManager>, StoreError> {
    pool.get()
        .map_err(|error| StoreError::Backend(format!("acquire read connection: {error}")))
}
