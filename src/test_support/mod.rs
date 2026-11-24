#![allow(dead_code)]

pub mod encoder;
pub mod lines;
pub mod session;
pub mod store;

use crate::timestamp::Timestamp;

pub fn timestamp(value: &str) -> Timestamp {
    value.parse().expect("an RFC 3339 timestamp")
}
