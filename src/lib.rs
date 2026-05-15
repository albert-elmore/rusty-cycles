//! Library surface for tests and tooling. The binary (`main.rs`) adds audio + CLI.

pub mod banks;
pub mod dsl;
pub use dsl::TempoWrap;
pub mod event_spec;
pub mod pat;
