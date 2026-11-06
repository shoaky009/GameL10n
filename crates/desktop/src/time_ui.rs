//! Formats stored timestamps for display in the operating system's time zone.

use chrono::{DateTime, Local};

pub fn local_timestamp(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|date| date.with_timezone(&Local).to_rfc3339())
        .unwrap_or_else(|_| value.to_owned())
}
