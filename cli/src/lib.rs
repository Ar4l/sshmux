//! Library surface of the sshmux CLI, so the relay's security invariant can be
//! covered by integration tests (see `tests/relay_gate.rs`).

pub mod hostkey;
pub mod relay;
pub mod trust;
pub mod tunnel;
