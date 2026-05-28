//! Kenwood CAT backend family. The TS-590 is the first concrete model; other
//! Kenwood radios share the command grammar and slot in via the same parsing
//! and formatting helpers.

pub(crate) mod ts590;

pub(crate) use ts590::Ts590Backend;
