//! MCP server and local socket (SPEC §11, R-ARCH-5).
//!
//! - [`paths`]: the fixed per-user socket path and app data (R-ARCH-5a).
//! - [`wire`]: the opening exchange before MCP: peer checks, nonces, HMAC proofs.
//! - [`transport`]: the Unix socket, or the Windows named pipe (decisions/0019).
//! - [`server`]: `gmclip serve --headless` — the holder's accept loop and rmcp handler.
//! - [`client`]: the `gmclip mcp` stdio shim and the CLI's control requests.
//! - [`holder`]: open projects, launched runs, Owner challenges, connections.
//! - [`tools`]: the 16 task tools, discovery, `command_run` / `command_batch`, scopes.
//! - [`reads`]: projections with R-MCP-5 limits.
//! - [`canon`]: untrusted text (R-INJ-1/6).
//! - [`fsroots`]: cap-std file roots (R-FS-1).
//! - [`settings`]: Owner settings, audit and connection logs.
//! - [`project`]: project homes.
//! - [`connect`]: `owner connect claude-code|codex` config edits (R-MCP-2, R-AGT-4).
//! - [`collab`]: the comment-fix and thumbnail pass briefs (decision 0015).
//! - [`findpass`]: `ai_pass` / `pass_step` for find moments and the full sweep (A.01).
//! - [`copypass`]: `ai_pass` / `pass_step` for title and description drafts (0024).
#![forbid(unsafe_code)]

pub mod analysis;
pub mod canon;
pub mod client;
pub mod collab;
pub mod connect;
pub mod copypass;
pub mod cursor;
pub mod findpass;
pub mod fsroots;
pub mod holder;
pub mod matte;
pub mod media;
pub mod pathform;
pub mod paths;
pub mod project;
pub mod reads;
pub mod review;
pub mod server;
pub mod settings;
pub mod tools;
pub mod transport;
pub mod wire;
