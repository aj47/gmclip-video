//! Owner commands from the app (R-OWN-1, R-OWN-1b; backlog U.11): the webview may only
//! *ask* (`owner_request{command, args}`); Rust works out in plain words what the command
//! will do — from the command and its validated arguments, never from caller prose —
//! shows it on the native sheet ([`crate::sheet`]), and only the sheet's button makes
//! the holder mint the single-use code (bound to the command and these exact arguments,
//! 60 s) that runs it. No sheet (the dev bridge, tests) → `Unauthorized`.

use serde_json::{Map, Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};

/// Commands the dev bridge refuses outright (R-OWN-1b, U.10/U.11): every Owner command
/// and every review-link setting that a native sheet decides. Without a window there is
/// no sheet, so these fail anyway; the bridge refuses them before they run, with one
/// answer. Sorted (a test checks it, and that each is an app command).
pub const DEV_BRIDGE_REFUSED: &[&str] = &[
    "agent_client_allow",
    "agent_client_set",
    "agent_default_set",
    "agents_pause",
    "clippy_pair",
    "clippy_send",
    "owner_request",
    "review_link_new",
    "review_link_role",
    "review_serve",
    "style_pick",
    "style_review",
];

/// Whether the dev bridge refuses this call: anything in [`DEV_BRIDGE_REFUSED`], except
/// `review_serve {on: false}` (stopping takes access away, like a revoke: no sheet).
pub fn dev_bridge_refuses(command: &str, args: &Value) -> bool {
    DEV_BRIDGE_REFUSED.contains(&command)
        && !(command == "review_serve" && args.get("on") == Some(&Value::Bool(false)))
}

fn printable(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(300).collect()
}

fn only(args: &Map<String, Value>, keys: &[&str]) -> AppResult<()> {
    match args.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(_) => Err(AppError::invalid(
            "an argument this Owner command does not take",
        )),
        None => Ok(()),
    }
}

fn path(args: &Map<String, Value>) -> AppResult<&str> {
    let p = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::invalid("a folder path"))?;
    // absolute for this OS (`/…`; Windows `C:\…`)
    if !std::path::Path::new(p).is_absolute() || p.len() > 1024 || p.chars().any(char::is_control) {
        return Err(AppError::invalid("an absolute folder path"));
    }
    Ok(p)
}

/// The sheet's title and lines for an Owner command the app offers.
pub fn effects(command: &str, args: &Value) -> AppResult<(String, Vec<String>)> {
    let a = args
        .as_object()
        .ok_or_else(|| AppError::invalid("arguments are an object"))?;
    Ok(match command {
        "roots_add" => {
            only(a, &["path", "write"])?;
            let p = printable(path(a)?);
            let write = a.get("write").and_then(Value::as_bool).unwrap_or(false);
            (
                "Let agents use this folder?".into(),
                vec![
                    format!("Agents connected to gmclip may read files in {p}."),
                    if write {
                        "They may also write files there (exports, shots).".to_owned()
                    } else {
                        "They may not write there.".into()
                    },
                    "You can remove it later.".into(),
                ],
            )
        }
        "roots_remove" => {
            only(a, &["path"])?;
            let p = printable(path(a)?);
            (
                "Stop agents using this folder?".into(),
                vec![format!("Agents will no longer read or write files in {p}.")],
            )
        }
        "recordings_set" => {
            only(a, &["path"])?;
            let p = printable(path(a)?);
            (
                "Use this as your recordings folder?".into(),
                vec![format!(
                    "{p} becomes the recordings folder; agents may read the files in it."
                )],
            )
        }
        "revoke" => {
            only(a, &["identity"])?;
            let id = a
                .get("identity")
                .and_then(Value::as_str)
                .filter(|i| i.starts_with("local:") && i.len() <= 80)
                .ok_or_else(|| AppError::invalid("a local:<client> identity"))?;
            (
                "Block this agent client?".into(),
                vec![format!(
                    "Calls from {} will be refused from now on.",
                    printable(id)
                )],
            )
        }
        "style_set" => {
            only(a, &["path", "sha256"])?;
            let p = printable(path(a)?);
            let sha = a
                .get("sha256")
                .and_then(Value::as_str)
                .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))
                .ok_or_else(|| AppError::invalid("the hash of the version shown"))?;
            (
                "Use this file as your style rules?".into(),
                vec![format!(
                    "Agents follow the style rules in {p} — the version you see now ({}…); gmclip only reads it, and an edit to it asks you again.",
                    &sha[..12]
                )],
            )
        }
        "profile_use" => {
            only(a, &["path"])?;
            let p = printable(path(a)?);
            (
                "Use this profile?".into(),
                vec![format!("New projects and agents use the profile in {p}.")],
            )
        }
        "review_link_new" => {
            only(a, &["login", "role"])?;
            let who = a
                .get("login")
                .and_then(Value::as_str)
                .filter(|l| !l.is_empty())
                .map(|l| format!(" (in tailnet mode, only {})", printable(l)))
                .unwrap_or_default();
            let editor = match a.get("role").and_then(Value::as_str) {
                None | Some("") | Some("reviewer") => false,
                Some("editor") => true,
                Some(_) => return Err(AppError::invalid("reviewer or editor")),
            };
            if editor {
                (
                    "Make an editor's review link?".into(),
                    vec![
                        format!(
                            "Anyone with the link{who} can watch your exports, comment on them and suggest edits for 7 days."
                        ),
                        "Suggested edits wait for you to accept or reject them; the link cannot start an agent or approve.".into(),
                        "The link is shown once, here; you can revoke it at any time.".into(),
                    ],
                )
            } else {
                (
                    "Make a review link?".into(),
                    vec![
                        format!(
                            "Anyone with the link{who} can watch your exports and leave review notes and comments for 7 days."
                        ),
                        "The link is shown once, here; you can revoke it at any time.".into(),
                    ],
                )
            }
        }
        "remote_send_set" => {
            only(a, &["off", "max_sends_per_day"])?;
            if a.get("off").and_then(Value::as_bool) == Some(true) {
                (
                    "Turn off remote approvals?".into(),
                    vec![
                        "\"Approve\" on a review link will always wait for a click in the app."
                            .into(),
                    ],
                )
            } else {
                let n = a
                    .get("max_sends_per_day")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                (
                    "Allow approvals from your phone?".into(),
                    vec![
                        format!(
                            "From your own tailnet login, \"Approve\" on a review link may hand up to {n} export(s) a day to Clippy's calendar without a click here."
                        ),
                        "Clippy still asks you before anything is posted.".into(),
                    ],
                )
            }
        }
        "review_tailnet" => {
            only(a, &["host", "owner_login"])?;
            let host = a.get("host").and_then(Value::as_str).unwrap_or_default();
            let login = a
                .get("owner_login")
                .and_then(Value::as_str)
                .unwrap_or_default();
            (
                "Serve review links on your tailnet?".into(),
                vec![
                    format!(
                        "Review links will open at https://{} through Tailscale (never Funnel).",
                        printable(host)
                    ),
                    format!(
                        "{} is recorded as you, the Owner (only you can start a remote fix).",
                        printable(login)
                    ),
                ],
            )
        }
        "remote_fix_set" => {
            only(a, &["off", "max_usd_per_run", "max_runs_per_day"])?;
            if a.get("off").and_then(Value::as_bool) == Some(true) {
                (
                    "Turn off remote fixes?".into(),
                    vec!["\"Fix with agent\" from a review link will always wait for a click in the app.".into()],
                )
            } else {
                let usd = a
                    .get("max_usd_per_run")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let runs = a
                    .get("max_runs_per_day")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                (
                    "Allow remote fixes?".into(),
                    vec![
                        format!(
                            "From your own tailnet login, \"Fix with agent\" may start up to {runs} run(s) a day without a click here."
                        ),
                        format!("Each run may spend at most ${usd:.2}."),
                    ],
                )
            }
        }
        "budgets_set" => {
            // R-COST-2: the caps named, each 0 < $ ≤ 1 000; the rest stay as they are
            let names = [
                "edit_pass_usd",
                "run_usd",
                "day_usd",
                "batch_usd",
                "ask_usd",
            ];
            only(a, &names)?;
            if a.is_empty() {
                return Err(AppError::invalid("at least one cap"));
            }
            let mut lines = Vec::new();
            for (k, label) in [
                ("edit_pass_usd", "An edit pass may spend"),
                ("run_usd", "Any other agent run may spend"),
                ("day_usd", "All AI together may spend"),
                ("batch_usd", "An overnight batch may spend"),
                ("ask_usd", "One Ask message may spend"),
            ] {
                if let Some(v) = a.get(k) {
                    let usd = v
                        .as_f64()
                        .filter(|x| x.is_finite() && *x > 0.0 && *x <= 1_000.0)
                        .ok_or_else(|| AppError::invalid("a cap of 0 < $ ≤ 1 000"))?;
                    let per = match k {
                        "edit_pass_usd" => " per run",
                        "run_usd" => " per client",
                        "day_usd" => " a day",
                        "batch_usd" => " in total",
                        _ => "",
                    };
                    lines.push(format!("{label} at most ${usd:.2}{per}."));
                }
            }
            lines.push(
                "Your own agent CLI runs on its own subscription: nothing gmclip runs today is billed by gmclip.".into(),
            );
            ("Change your AI spend limits?".into(), lines)
        }
        "providers_set" => {
            only(a, &["ai", "models"])?;
            let models: Vec<String> = match a.get("models") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(v)) if v.len() <= 32 => v
                    .iter()
                    .map(|m| {
                        m.as_str()
                            .filter(|m| gmclip_mcp::settings::valid_model_id(m))
                            .map(printable)
                            .ok_or_else(|| AppError::invalid("model ids"))
                    })
                    .collect::<AppResult<_>>()?,
                Some(_) => return Err(AppError::invalid("at most 32 model ids")),
            };
            match a.get("ai").and_then(Value::as_str) {
                Some("none") => (
                    "Turn AI off?".into(),
                    vec![
                        "No agent run starts, from the app or the terminal; every pass offers its local alternative.".into(),
                    ],
                ),
                Some("agent_cli") => (
                    "Use your own agent for AI?".into(),
                    vec![
                        "AI passes run through your own agent CLI (Claude Code, pi or opencode) on its own subscription, after your click.".into(),
                        if models.is_empty() {
                            "A run may use any model your agent offers.".to_owned()
                        } else {
                            format!("A run may name only these models: {}.", models.join(", "))
                        },
                    ],
                ),
                _ => return Err(AppError::invalid("none or agent_cli")),
            }
        }
        "telemetry_set" => {
            only(a, &["on"])?;
            match a.get("on").and_then(Value::as_bool) {
                Some(true) => (
                    "Allow anonymous usage events?".into(),
                    vec![
                        "gmclip may record event names, durations and the app version — never media, transcripts or file names.".into(),
                        "This build sends nothing; the choice is kept for the beta.".into(),
                    ],
                ),
                Some(false) => (
                    "Turn usage events off?".into(),
                    vec!["gmclip records no usage events.".into()],
                ),
                _ => return Err(AppError::invalid("on: true or false")),
            }
        }
        "review_loopback" => {
            only(a, &[])?;
            (
                format!("Keep review links on {} only?", crate::caps::THIS_MACHINE),
                vec!["Review links stop working from other devices (the tailnet setting is removed).".into()],
            )
        }
        _ => {
            return Err(AppError::new(
                "Unsupported",
                "the app does not offer that Owner command (use gmclip owner in a terminal)",
            ));
        }
    })
}

/// Asks the human on the native sheet, then runs the Owner command.
pub fn request(core: &AppCore, command: &str, args: &Value) -> AppResult<Value> {
    let (title, lines) = effects(command, args)?;
    // a folder that is not there is refused before the sheet asks (decision 0058: a
    // typed `/Movies/…` without the home folder was saved and granted nothing)
    if command == "roots_add"
        && let Some(p) = args.get("path").and_then(Value::as_str)
        && !std::path::Path::new(p).is_dir()
    {
        return Err(AppError::invalid(format!(
            "no folder at {} (type ~/… for one in your home folder)",
            printable(p)
        )));
    }
    let text = lines
        .iter()
        .map(|l| format!("• {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    if !crate::sheet::confirm(&title, &text, "Allow") {
        return Err(AppError::new(
            "Unauthorized",
            "not allowed (the Owner did not approve it on the sheet)",
        ));
    }
    core.agents.owner_call(command, args)
}

/// Shows the native "choose a file" panel for the style rules file.
pub type StylePicker = Box<dyn Fn() -> Option<std::path::PathBuf> + Send + Sync>;

static STYLE_PICKER: std::sync::OnceLock<StylePicker> = std::sync::OnceLock::new();

/// Installs the style-file panel (the window's main thread runs it); once per launch.
pub fn install_style_picker(f: StylePicker) {
    let _ = STYLE_PICKER.set(f);
}

/// The native panel for a Markdown style file (main thread only).
#[cfg(target_os = "macos")]
pub fn pick_style_on_main_thread() -> Option<std::path::PathBuf> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{NSArray, NSString};
    let mtm = MainThreadMarker::new()?;
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    panel.setMessage(Some(&NSString::from_str(
        "Choose the Markdown file with your style rules (gmclip only reads it)",
    )));
    panel.setPrompt(Some(&NSString::from_str("Use as style rules")));
    let exts: Vec<_> = ["md", "markdown", "txt"]
        .iter()
        .map(|e| NSString::from_str(e))
        .collect();
    #[allow(deprecated)]
    panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&exts)));
    if panel.runModal() != NSModalResponseOK {
        return None;
    }
    let url = panel.URLs().firstObject()?;
    Some(std::path::PathBuf::from(url.path()?.to_string()))
}

/// The system file dialog for the Markdown style file (W.9).
#[cfg(windows)]
pub fn pick_style_on_main_thread() -> Option<std::path::PathBuf> {
    gmclip_platform::windows::open_file_dialog(
        "Choose the Markdown file with your style rules (gmclip only reads it)",
        "Markdown",
        &["md", "markdown", "txt"],
        false,
    )
    .ok()?
    .into_iter()
    .next()
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn pick_style_on_main_thread() -> Option<std::path::PathBuf> {
    None
}

/// The style rules file, picked in the native panel (R-PROF-5, an Owner setting): the
/// file is read confined (absolute, symlinks in its folder resolved, the file itself not
/// a symlink, opened read-only with `O_NOFOLLOW`, ≤ 256 KB, UTF-8), and the native sheet
/// names it and the hash of exactly the version read; only the sheet's button pins it.
pub fn style_pick(core: &AppCore) -> AppResult<Value> {
    let pick = STYLE_PICKER.get().ok_or_else(|| {
        AppError::new(
            "Unsupported",
            "no file panel here (use gmclip owner style set <path> in a terminal)",
        )
    })?;
    let Some(path) = pick() else {
        return Ok(json!({"cancelled": true}));
    };
    style_pin(core, &path)
}

/// [`style_pick`] after the panel: preview, then the Owner sheet.
pub fn style_pin(core: &AppCore, path: &std::path::Path) -> AppResult<Value> {
    let (p, sha, _shown) = gmclip_core::profile::style::preview(path).map_err(AppError::invalid)?;
    request(
        core,
        "style_set",
        &json!({"path": p.to_string_lossy(), "sha256": sha}),
    )
}

/// The version of the style file the human last declined on the batched sheet (this
/// launch): not asked again until the file changes again or they ask to review.
static STYLE_DECLINED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The batched style-file sheet (R-PROF-3 M2): at launch and at a batch's start, every
/// change to the style file since it was accepted is shown as one diff on one native
/// sheet ("3 changes to AGENTS.md since 2026-10-14 — Accept all"). Until accepted,
/// agents keep the old text and hash. `review` asks even about a version declined
/// earlier in this launch (Settings' "Review changes…").
pub fn style_changes_sheet(core: &AppCore, review: bool) -> AppResult<Value> {
    let Some(h) = core.agents.holder() else {
        return Ok(json!({"pending": false}));
    };
    let Some(st) = gmclip_core::profile::style::status(&h.env.app_data) else {
        return Ok(json!({"pending": false}));
    };
    let current = match (&st.current, st.changed) {
        (Ok(c), true) => c.clone(),
        (Err(e), _) => return Ok(json!({"pending": false, "unreadable": e})),
        _ => return Ok(json!({"pending": false})),
    };
    let declined = STYLE_DECLINED
        .lock()
        .map(|g| g.as_deref() == Some(current.as_str()))
        .unwrap_or(false);
    if declined && !review {
        return Ok(json!({"pending": true, "asked": false, "sha256": current}));
    }
    let hunks = st.diff.matches("\n@@\n").count() + 1;
    let name = st
        .path
        .file_name()
        .map(|n| printable(&n.to_string_lossy()))
        .unwrap_or_else(|| "the style file".into());
    let title = format!(
        "{hunks} change{} to {name} since {}",
        if hunks == 1 { "" } else { "s" },
        crate::board::ymd(st.read_at)
    );
    let mut diff = gmclip_core::profile::style::visible(&st.diff);
    if diff.chars().count() > 3_000 {
        diff = diff.chars().take(3_000).collect::<String>() + "\n… (more in Settings)";
    }
    let text = format!(
        "Agents keep the version you accepted ({}…) until you accept this one ({}…).\n\n{diff}",
        &st.pinned_sha256[..12.min(st.pinned_sha256.len())],
        &current[..12.min(current.len())]
    );
    if !crate::sheet::confirm(&title, &text, "Accept all") {
        if let Ok(mut g) = STYLE_DECLINED.lock() {
            *g = Some(current.clone());
        }
        return Ok(json!({"pending": true, "asked": true, "accepted": false, "sha256": current}));
    }
    core.agents
        .owner_call("style_accept", &json!({"sha256": current}))?;
    if let Ok(mut g) = STYLE_DECLINED.lock() {
        *g = None;
    }
    Ok(json!({"pending": false, "asked": true, "accepted": true, "sha256": current}))
}

/// The Owner settings agents run under (read only).
pub fn settings(core: &AppCore) -> Value {
    match core.agents.holder() {
        Some(h) => {
            let s = h.settings();
            let style = gmclip_core::profile::style::status(&h.env.app_data).map(|st| {
                json!({"path": st.path, "sha256": st.pinned_sha256,
                       "changed_on_disk": st.changed, "unreadable": st.current.err()})
            });
            // the home folder agents read by default (decision 0058): Settings lists it
            // with the Owner's roots, and expands `~` in a typed folder to it
            json!({"read_roots": s.read_roots, "write_roots": s.write_roots, "recordings": s.recordings,
                   "home": h.user_home(), "home_readable": s.home_readable(),
                   "revoked": s.revoked, "export_dir": s.export_dir, "profile": s.profile, "style": style,
                   "budgets": s.budgets(), "providers": s.providers(), "telemetry": s.telemetry,
                   // the clients' probe takes seconds the first time (warmed at app_ready):
                   // until it is done this says so instead of holding Settings
                   "agent": crate::agent::client_status_cached(core)
                       .unwrap_or_else(|| json!({"found": false, "pending": true, "message": "Finding your agents…"}))})
        }
        None => json!({"note": "no agent server in this app"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // absolute folders as the platform writes them (a Unix path is not absolute on Windows)
    #[cfg(not(windows))]
    const MOVIES: &str = "/Users/aj/Movies";
    #[cfg(windows)]
    const MOVIES: &str = r"C:\Users\aj\Movies";
    #[cfg(not(windows))]
    const AGENTS_MD: &str = "/Users/aj/AGENTS.md";
    #[cfg(windows)]
    const AGENTS_MD: &str = r"C:\Users\aj\AGENTS.md";
    #[cfg(not(windows))]
    const PROFILE: &str = "/Users/aj/profile";
    #[cfg(windows)]
    const PROFILE: &str = r"C:\Users\aj\profile";

    /// A fixture string made at run time, for the fixture tables of `&str`.
    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    #[test]
    fn the_dev_bridge_refuses_every_owner_and_review_link_setting() {
        let mut sorted = DEV_BRIDGE_REFUSED.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, DEV_BRIDGE_REFUSED, "sorted, no duplicates");
        for c in DEV_BRIDGE_REFUSED {
            assert!(
                crate::ipc::APP_COMMANDS.contains(c),
                "{c} is an app command"
            );
        }
        // the review link's settings (U.10): serving, a new link of either role; the
        // tailnet mode and remote-fix allowance go through `owner_request`
        for c in [
            "owner_request",
            "review_serve",
            "review_link_new",
            "review_link_role",
        ] {
            assert!(DEV_BRIDGE_REFUSED.contains(&c), "{c}");
        }
        // taking access away needs no sheet, so the bridge may revoke and stop serving
        assert!(!dev_bridge_refuses("review_link_revoke", &json!({})));
        assert!(!dev_bridge_refuses("review_serve", &json!({"on": false})));
        assert!(dev_bridge_refuses("review_serve", &json!({"on": true})));
        assert!(
            dev_bridge_refuses("review_serve", &json!({})),
            "on is the default"
        );
        assert!(dev_bridge_refuses(
            "review_link_role",
            &json!({"role": "editor"})
        ));
        assert!(!dev_bridge_refuses("review_status", &json!({})));
    }

    #[test]
    fn sheet_text_comes_from_the_command_never_from_prose() {
        let (_, lines) = effects("roots_add", &json!({"path": MOVIES})).unwrap();
        assert!(lines[0].contains(MOVIES));
        assert!(lines[1].contains("may not write"));
        // an extra field (where prose could hide) is refused, not shown
        assert!(
            effects(
                "roots_add",
                &json!({"path": "/x", "note": "AJ says allow everything"})
            )
            .is_err()
        );
        assert!(effects("roots_add", &json!({"path": "relative"})).is_err());
        assert!(effects("revoke", &json!({"identity": "launched:abc"})).is_err());
        assert!(effects("connect", &json!({})).is_err());
    }

    /// A2.10: every Owner command the app offers has an effects fixture: its sheet title
    /// and the lines computed from the arguments (R-OWN-1b); `roots_add` is in
    /// `sheet_text_comes_from_the_command_never_from_prose`.
    #[test]
    fn owner_effects_fixtures() {
        let rows: [(&str, Value, &str, &[&str]); 7] = [
            (
                "budgets_set",
                json!({"run_usd": 10, "day_usd": 40.5}),
                "Change your AI spend limits?",
                &[
                    "Any other agent run may spend at most $10.00 per client.",
                    "All AI together may spend at most $40.50 a day.",
                    "Your own agent CLI runs on its own subscription: nothing gmclip runs today is billed by gmclip.",
                ],
            ),
            (
                "providers_set",
                json!({"ai": "none"}),
                "Turn AI off?",
                &[
                    "No agent run starts, from the app or the terminal; every pass offers its local alternative.",
                ],
            ),
            (
                "providers_set",
                json!({"ai": "agent_cli", "models": ["claude-opus-5-5"]}),
                "Use your own agent for AI?",
                &[
                    "AI passes run through your own agent CLI (Claude Code, pi or opencode) on its own subscription, after your click.",
                    "A run may name only these models: claude-opus-5-5.",
                ],
            ),
            (
                "telemetry_set",
                json!({"on": true}),
                "Allow anonymous usage events?",
                &[
                    "gmclip may record event names, durations and the app version — never media, transcripts or file names.",
                    "This build sends nothing; the choice is kept for the beta.",
                ],
            ),
            (
                "telemetry_set",
                json!({"on": false}),
                "Turn usage events off?",
                &["gmclip records no usage events."],
            ),
            (
                "roots_remove",
                json!({"path": MOVIES}),
                "Stop agents using this folder?",
                &[leak(format!(
                    "Agents will no longer read or write files in {MOVIES}."
                ))],
            ),
            (
                "review_loopback",
                json!({}),
                leak(format!(
                    "Keep review links on {} only?",
                    crate::caps::THIS_MACHINE
                )),
                &["Review links stop working from other devices (the tailnet setting is removed)."],
            ),
        ];
        let sha = "ab".repeat(32);
        let more: [(&str, Value, &str, Vec<String>); 11] = [
            (
                "recordings_set",
                json!({"path": MOVIES}),
                "Use this as your recordings folder?",
                vec![format!("{MOVIES} becomes the recordings folder; agents may read the files in it.")],
            ),
            (
                "revoke",
                json!({"identity": "local:codex"}),
                "Block this agent client?",
                vec!["Calls from local:codex will be refused from now on.".into()],
            ),
            (
                "style_set",
                json!({"path": AGENTS_MD, "sha256": sha}),
                "Use this file as your style rules?",
                vec![format!("Agents follow the style rules in {AGENTS_MD} — the version you see now ({}…); gmclip only reads it, and an edit to it asks you again.", &sha[..12])],
            ),
            (
                "profile_use",
                json!({"path": PROFILE}),
                "Use this profile?",
                vec![format!("New projects and agents use the profile in {PROFILE}.")],
            ),
            (
                "review_link_new",
                json!({}),
                "Make a review link?",
                vec![
                    "Anyone with the link can watch your exports and leave review notes and comments for 7 days.".into(),
                    "The link is shown once, here; you can revoke it at any time.".into(),
                ],
            ),
            (
                "review_link_new",
                json!({"role": "editor", "login": "aj@github"}),
                "Make an editor's review link?",
                vec![
                    "Anyone with the link (in tailnet mode, only aj@github) can watch your exports, comment on them and suggest edits for 7 days.".into(),
                    "Suggested edits wait for you to accept or reject them; the link cannot start an agent or approve.".into(),
                    "The link is shown once, here; you can revoke it at any time.".into(),
                ],
            ),
            (
                "remote_send_set",
                json!({"max_sends_per_day": 2}),
                "Allow approvals from your phone?",
                vec![
                    "From your own tailnet login, \"Approve\" on a review link may hand up to 2 export(s) a day to Clippy's calendar without a click here.".into(),
                    "Clippy still asks you before anything is posted.".into(),
                ],
            ),
            (
                "remote_send_set",
                json!({"off": true}),
                "Turn off remote approvals?",
                vec!["\"Approve\" on a review link will always wait for a click in the app.".into()],
            ),
            (
                "review_tailnet",
                json!({"host": "box.tail1.ts.net", "owner_login": "aj@github"}),
                "Serve review links on your tailnet?",
                vec![
                    "Review links will open at https://box.tail1.ts.net through Tailscale (never Funnel).".into(),
                    "aj@github is recorded as you, the Owner (only you can start a remote fix).".into(),
                ],
            ),
            (
                "remote_fix_set",
                json!({"max_usd_per_run": 2.0, "max_runs_per_day": 3}),
                "Allow remote fixes?",
                vec![
                    "From your own tailnet login, \"Fix with agent\" may start up to 3 run(s) a day without a click here.".into(),
                    "Each run may spend at most $2.00.".into(),
                ],
            ),
            (
                "remote_fix_set",
                json!({"off": true}),
                "Turn off remote fixes?",
                vec!["\"Fix with agent\" from a review link will always wait for a click in the app.".into()],
            ),
        ];
        for (cmd, args, title, lines) in rows {
            let (t, l) = effects(cmd, &args).unwrap_or_else(|e| panic!("{cmd}: {e:?}"));
            assert_eq!(t, title, "{cmd}");
            assert_eq!(l, lines, "{cmd}");
        }
        for (cmd, args, title, lines) in more {
            let (t, l) = effects(cmd, &args).unwrap_or_else(|e| panic!("{cmd}: {e:?}"));
            assert_eq!(t, title, "{cmd}");
            assert_eq!(l, lines, "{cmd}");
        }
        // refusals: an unknown cap, a cap out of range, an API key, a bad model id
        for (cmd, args) in [
            ("budgets_set", json!({"tokens": 5})),
            ("budgets_set", json!({"day_usd": 0})),
            ("budgets_set", json!({"day_usd": 5000})),
            ("budgets_set", json!({})),
            ("providers_set", json!({"ai": "anthropic", "key": "sk-..."})),
            (
                "providers_set",
                json!({"ai": "agent_cli", "models": ["--dangerous"]}),
            ),
            ("telemetry_set", json!({"on": "yes"})),
            ("updater_set", json!({"on": true})),
        ] {
            assert!(effects(cmd, &args).is_err(), "{cmd} {args}");
        }
    }
}
