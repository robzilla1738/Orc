//! Application-owned identifiers. Every entity gets its own newtype so a
//! `SessionId` can never be passed where a `RunId` is expected.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use ts_rs::TS;
use uuid::Uuid;

macro_rules! id_type {
    ($name:ident, $prefix:literal) => {
        /// Application-owned identifier (prefix `$prefix`).
        #[derive(
            Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS,
        )]
        #[ts(type = "string")]
        pub struct $name(String);

        impl $name {
            /// Generate a fresh ID (UUIDv7 — time-ordered).
            pub fn new() -> Self {
                Self(format!("{}_{}", $prefix, Uuid::now_v7().simple()))
            }

            /// Wrap an existing raw string without validation. Used when
            /// restoring persisted state.
            pub fn from_raw(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let expected = concat!($prefix, "_");
                if !s.starts_with(expected) {
                    return Err(IdParseError {
                        kind: stringify!($name),
                        input: s.to_string(),
                    });
                }
                Ok(Self(s.to_string()))
            }
        }
    };
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid {kind} id: '{input}'")]
pub struct IdParseError {
    pub kind: &'static str,
    pub input: String,
}

id_type!(ToolInstanceId, "tool");
id_type!(WorkspaceId, "ws");
id_type!(TaskId, "task");
id_type!(SessionId, "ses");
id_type!(RunId, "run");
id_type!(AttentionId, "att");
id_type!(ArtifactId, "art");
id_type!(EventId, "evt");
id_type!(OperationId, "op");
id_type!(StepId, "step");

/// Idempotency key attached to every side-effecting command. Recorded
/// before dispatch so a crash between dispatch and acknowledgment can be
/// reconciled instead of blindly retried.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(type = "string")]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn new() -> Self {
        Self(format!("idem_{}", Uuid::now_v7().simple()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for IdempotencyKey {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for IdempotencyKey {
    type Err = IdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if !s.starts_with("idem_") {
            return Err(IdParseError {
                kind: "IdempotencyKey",
                input: s.to_string(),
            });
        }
        Ok(Self(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_roundtrip_and_reject_wrong_prefix() {
        let sid = SessionId::new();
        assert!(sid.as_str().starts_with("ses_"));
        let parsed: SessionId = sid.as_str().parse().unwrap();
        assert_eq!(parsed, sid);
        assert!("ses_x".parse::<RunId>().is_err());
        assert!("run_x".parse::<SessionId>().is_err());
    }
}
