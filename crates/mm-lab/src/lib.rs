//! Docker integration lab.
//!
//! **Outside the coverage gate**, and not built at all by `cargo test
//! --workspace`: the lab tests sit behind `required-features = ["lab"]`.
//!
//! Modelled on go-tun: a typed harness over the Docker Engine API rather
//! than a compose file, internal bridge networks only, per-run name
//! prefixes, and readiness as a ladder of polled labelled facts.
