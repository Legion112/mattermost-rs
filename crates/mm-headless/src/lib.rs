//! Headless driver: runs `mm-core` against a live server with no GUI.
//!
//! **Outside the coverage gate.** This is what the Docker lab drives. It
//! feeds real REST responses and real WebSocket frames into the same
//! `mm_core` store the GUI uses, which is the one thing the unit tests with
//! a mock transport cannot do.
