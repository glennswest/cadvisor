//! Custom-metric types mirroring upstream `lib/model/metric.go`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::GoTime;
use crate::omit::is_zero;

/// Upstream string enum: "gauge" | "cumulative".
pub type MetricType = String;
pub const METRIC_GAUGE: &str = "gauge";
pub const METRIC_CUMULATIVE: &str = "cumulative";

/// Upstream string enum: "int" | "float".
pub type DataType = String;
pub const INT_TYPE: &str = "int";
pub const FLOAT_TYPE: &str = "float";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub metric_type: MetricType,
    pub format: DataType,
    pub units: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricValBasic {
    pub timestamp: GoTime,
    #[serde(skip_serializing_if = "is_zero")]
    pub int_value: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub float_value: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricVal {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    pub timestamp: GoTime,
    #[serde(skip_serializing_if = "is_zero")]
    pub int_value: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub float_value: f64,
}
