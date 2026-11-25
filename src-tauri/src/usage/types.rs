//! Usage wire types and compact, in-memory recording identities.
use crate::calendar::CalendarDay;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageState {
    Active,
    Idle,
    Paused,
    Unknown,
}

impl UsageState {
    pub(crate) fn code(self) -> i64 {
        match self {
            Self::Active => 0,
            Self::Idle => 1,
            Self::Paused => 2,
            Self::Unknown => 3,
        }
    }

    pub(crate) fn from_code(code: i64) -> Self {
        match code {
            0 => Self::Active,
            1 => Self::Idle,
            2 => Self::Paused,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UsageAppIdentity {
    pub process_name: String,
    pub name: String,
    pub process_path: String,
    pub icon: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageIdentity {
    pub app: Arc<UsageAppIdentity>,
    pub title: String,
    pub url: Option<String>,
    pub category: String,
}

#[derive(Clone, Debug)]
pub(crate) struct UsageSpan {
    pub run_id: i64,
    pub sequence: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub state: UsageState,
    pub identity: Option<Arc<UsageIdentity>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LiveUsage {
    pub generation: u64,
    pub time_zone: Option<String>,
    pub spans: Vec<UsageSpan>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageInterval {
    pub start_ms: i64,
    pub end_ms: i64,
    pub state: UsageState,
    pub app_id: Option<String>,
    pub category: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageApp {
    pub id: String,
    pub process_name: String,
    pub name: String,
    pub icon: Option<String>,
    pub category: String,
    pub category_is_manual: bool,
    pub total_ms: i64,
    pub visits: usize,
    pub longest_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageCategory {
    pub category: String,
    pub total_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageHour {
    pub start_ms: i64,
    pub end_ms: i64,
    pub total_ms: i64,
    pub categories: Vec<UsageCategory>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageWeekDay {
    pub date: String,
    pub total_ms: i64,
    pub observed_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageDay {
    pub generation: u64,
    pub date: String,
    pub calendar: CalendarDay,
    pub recorded_since_ms: Option<i64>,
    pub total_ms: i64,
    pub observed_ms: i64,
    pub first_ms: Option<i64>,
    pub last_ms: Option<i64>,
    pub switches: usize,
    pub longest_ms: i64,
    pub apps: Vec<UsageApp>,
    pub categories: Vec<UsageCategory>,
    pub hours: Vec<UsageHour>,
    pub intervals: Vec<UsageInterval>,
    pub week: Vec<UsageWeekDay>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageDetail {
    pub label: String,
    pub total_ms: i64,
    pub first_ms: i64,
    pub last_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageAppDetails {
    pub date: String,
    pub app_id: String,
    pub titles: Vec<UsageDetail>,
    pub websites: Vec<UsageDetail>,
}
