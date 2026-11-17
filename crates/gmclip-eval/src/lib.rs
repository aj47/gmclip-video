//! gmclip agent evals (SPEC §17.1): the S7 launcher probe, the h79 grader and the
//! injection cases. A dev tool: never linked into the shipped app.
#![forbid(unsafe_code)]

pub mod clients;
pub mod h79;
pub mod injection;
pub mod mcp_stub;
pub mod s7;
pub mod stub_model;
