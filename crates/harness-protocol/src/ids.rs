//! Strongly typed UUID identifiers and UTC timestamps.

use std::fmt::{self, Display, Formatter};
use std::ops::Deref;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! newtype_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[repr(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub const fn as_uuid(&self) -> Uuid {
                self.0
            }

            /// Derives a reproducible ID for deterministic core transitions.
            pub fn derived(seed: Uuid, sequence: u64, namespace: u64) -> Self {
                let value = seed.as_u128() ^ ((sequence as u128) << 64) ^ namespace as u128;
                Self(Uuid::from_u128(value))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

newtype_id!(SessionId);
newtype_id!(AgentId);
newtype_id!(RunId);
newtype_id!(RequestId);
newtype_id!(ToolCallId);
newtype_id!(MessageId);
newtype_id!(EventId);
newtype_id!(PermissionId);
newtype_id!(ToolId);
newtype_id!(IntegrationId);
newtype_id!(ConfigurationId);
newtype_id!(ModelId);
newtype_id!(BackendId);
newtype_id!(ContextProviderId);
newtype_id!(ContextCheckpointId);
newtype_id!(ContextItemId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Timestamp(DateTime<Utc>);

impl Timestamp {
    pub fn now() -> Self {
        Self(Utc::now())
    }

    pub const fn from_datetime(value: DateTime<Utc>) -> Self {
        Self(value)
    }

    pub const fn as_datetime(&self) -> &DateTime<Utc> {
        &self.0
    }

    /// Produces a stable timestamp suitable for ordered deterministic transitions.
    pub fn from_sequence(sequence: u64) -> Self {
        let seconds = i64::try_from(sequence).unwrap_or(i64::MAX);
        Self(DateTime::from_timestamp(seconds, 0).expect("valid deterministic timestamp"))
    }
}

impl Deref for Timestamp {
    type Target = DateTime<Utc>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! assert_id_roundtrips {
        ($($type:ty),+ $(,)?) => {$({
            let id = <$type>::new();
            let parsed: $type = id.to_string().parse().expect("parse displayed UUID");
            assert_eq!(parsed, id);
            let json = serde_json::to_string(&id).expect("serialize ID");
            let decoded: $type = serde_json::from_str(&json).expect("deserialize ID");
            assert_eq!(decoded, id);
        })+};
    }

    #[test]
    fn every_id_display_parse_and_json_round_trips() {
        assert_id_roundtrips!(
            SessionId,
            AgentId,
            RunId,
            RequestId,
            ToolCallId,
            MessageId,
            EventId,
            PermissionId,
            ToolId,
            IntegrationId,
            ConfigurationId,
            ModelId,
            BackendId,
            ContextProviderId,
            ContextCheckpointId,
            ContextItemId,
        );
    }

    #[test]
    fn derived_ids_are_reproducible() {
        let seed = AgentId::new().as_uuid();
        assert_eq!(RunId::derived(seed, 1, 2), RunId::derived(seed, 1, 2));
        assert_ne!(RunId::derived(seed, 1, 2), RunId::derived(seed, 2, 2));
    }

    #[test]
    fn timestamp_now_is_within_observed_interval() {
        let before = Utc::now();
        let timestamp = Timestamp::now();
        let after = Utc::now();
        assert!(*timestamp.as_datetime() >= before && *timestamp.as_datetime() <= after);
    }
}
