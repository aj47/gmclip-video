//! gmclip core (SPEC §3): the project actor, the command registry and commands, undo,
//! runs, events.
//!
//! Core never depends on gmclip-render, gmclip-media or gmclip-gpu (`cargo xt deps`).
//! It reaches them only through the [`host::Renderer`] and [`host::MediaHost`] traits
//! it defines, so headless and CI builds link [`host::NullRenderer`] (R-ARCH-1).
#![forbid(unsafe_code)]

// `#[command]` emits `::gmclip_core::…` paths; this makes them resolve in this crate.
extern crate self as gmclip_core;

pub mod activity;
pub mod actor;
pub mod analysis;
pub mod anchors;
pub mod capmatch;
pub mod command;
pub mod commands;
pub mod cursor;
pub mod density;
pub mod events;
pub mod findpass;
pub mod fixture;
pub mod hooktext;
pub mod host;
pub mod library;
pub mod planner;
pub mod polish;
pub mod profile;
pub mod registry;
pub mod script;
pub mod services;
pub mod terms;
pub mod transcript;
pub mod tsgen;
pub mod zoom;

/// Job records, resume and machine-wide semaphores (SPEC §3.9). A12.
pub mod jobs {}
/// Lint: warnings, never refusals (SPEC §5.13): the [M1] agent set.
pub mod lint;
/// Agent pass definitions: versioned prompt assets (R-AI-5).
pub mod passes;

pub use actor::{CallOk, CallRequest, OpenOptions, OpenRun, ProjectHandle, Snapshot};
pub use command::{Caller, Command, CommandMeta, Cx, JobCommand, JobWork, Scope, Tx};
/// RenderPlan types, for hosts and the agent surface (which links core, not plan).
pub use gmclip_plan;
pub use gmclip_proto::CmdError;
/// Store options for opening a project (the holder chooses kind and host).
pub use gmclip_store::{
    HolderKind, OpenOptions as StoreOptions, ReadOnlyStore, StoreError, backup_into, local_host,
    write_fresh as write_fresh_store,
};
pub use host::{
    AnalysisHost, MediaHost, MediaRender, NoPlanner, NullAnalysisHost, NullMediaRender,
    NullRenderer, Planner, Renderer,
};
pub use registry::{ErasedCommand, REGISTRY};
