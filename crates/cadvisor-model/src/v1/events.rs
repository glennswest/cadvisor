//! Event types mirroring upstream `lib/model/container.go` (Event section).

use serde::{Deserialize, Serialize};

use crate::GoTime;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Event {
    /// Absolute container name for which the event occurred.
    pub container_name: String,
    pub timestamp: GoTime,
    pub event_type: EventType,
    // Struct-typed with omitempty upstream -> always serializes.
    pub event_data: EventData,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventType {
    #[default]
    #[serde(rename = "oom")]
    Oom,
    #[serde(rename = "oomKill")]
    OomKill,
    #[serde(rename = "containerCreation")]
    ContainerCreation,
    #[serde(rename = "containerDeletion")]
    ContainerDeletion,
}

/// Extra information about an event. Only one variant is set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EventData {
    #[serde(rename = "oom", skip_serializing_if = "Option::is_none")]
    pub oom_kill: Option<OomKillEventData>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OomKillEventData {
    pub pid: i64,
    pub process_name: String,
    /// The constraint that triggered the OOM.
    pub constraint: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_wire_shape() {
        let e = Event {
            container_name: "/docker/abc".into(),
            event_type: EventType::OomKill,
            event_data: EventData {
                oom_kill: Some(OomKillEventData {
                    pid: 42,
                    process_name: "stress".into(),
                    constraint: "CONSTRAINT_MEMCG".into(),
                }),
            },
            ..Default::default()
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["event_type"], json!("oomKill"));
        assert_eq!(v["event_data"]["oom"]["process_name"], json!("stress"));

        // event_data always serializes, even when empty (Go struct omitempty no-op).
        let empty = serde_json::to_value(Event::default()).unwrap();
        assert_eq!(empty["event_data"], json!({}));
        assert_eq!(empty["event_type"], json!("oom"));
    }
}
