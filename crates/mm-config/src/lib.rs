//! On-disk configuration, schema migrations and Electron config import.
//!
//! **Inside the coverage gate, at 100%.** All filesystem access goes through
//! the `Fs` trait, so the tests use an in-memory implementation and need no
//! temporary directories -- which is also what makes the permission-denied
//! and rename-failed paths reachable in CI.
