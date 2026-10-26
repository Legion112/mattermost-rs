//! The client state machine: store, reducers, unread computation, notification
//! policy and drafts.
//!
//! **Inside the coverage gate, at 100%.** Nothing here performs I/O and nothing
//! here reads a clock: the reducer takes the receive timestamp on its envelope
//! and timers are data, so `reduce` is a total function of its inputs. That is
//! what lets a recorded trace replay byte-for-byte, and it is why this crate
//! needs no clock fake at all.

pub mod unread;
