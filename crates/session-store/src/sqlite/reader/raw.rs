//! The unprocessed record stream, for diagnostics.

use rusty_protocol::ids::SessionId;

use super::{checkout, events::all_events, snapshot::load_snapshot, ReadPool};
use crate::store::{RawRecord, StoreError};

/// Every stored event followed by the latest snapshot, exactly as stored.
pub(in crate::sqlite) fn raw_records(
    pool: &ReadPool,
    id: SessionId,
) -> Result<Vec<RawRecord>, StoreError> {
    let connection = checkout(pool)?;
    let mut records: Vec<RawRecord> = all_events(&connection, id)?
        .into_iter()
        .map(RawRecord::Event)
        .collect();
    if let Some(snapshot) = load_snapshot(&connection, id, "load raw snapshot")? {
        records.push(RawRecord::Snapshot(snapshot));
    }
    Ok(records)
}
