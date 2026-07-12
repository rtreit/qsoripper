//! Multi-client WinKeyer brokering.
//!
//! WinKeyer is a stateful byte protocol, not a delimiter-framed CAT dialect. This module
//! owns its parser, physical-device events, scheduling, and virtual client sessions rather
//! than routing keyer bytes through the radio backend.

mod actor;
mod broker;
mod face;
mod grpc;
mod protocol;

pub(crate) use actor::{spawn_supervised, BrokerHandle};
pub(crate) use face::{open_serial_face, run_serial_face, FacePermissions};
pub(crate) use grpc::bind_server;
