//! `gmclip-app`: the Mac app.
#![forbid(unsafe_code)]

fn main() {
    let _ = gmclip_app::START.set(std::time::Instant::now());
    // `gmclip-app mcp` (the stdio shim a client starts) and `serve --headless` (the holder
    // the shim starts when no app runs): the app is gmclip for an agent too.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("mcp") => {
            if let Err(e) = gmclip_app::agent::run_mcp_shim() {
                eprintln!("gmclip: {e}");
                std::process::exit(1);
            }
            return;
        }
        // `gmclip-app call <tool|command> '<json>'` (decision 0050): one call through the
        // running gmclip, as the agent session `GMCLIP_SESSION` names (one identity, one
        // run across calls); the result's JSON on stdout, exit 1 when it is an error.
        Some("call") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: gmclip-app call <tool|command> '<json>'");
                std::process::exit(2);
            };
            let json = match args.get(2).map(|s| serde_json::from_str(s)) {
                None => Ok(serde_json::json!({})),
                Some(r) => r,
            };
            let Ok(json) = json else {
                eprintln!("gmclip: the arguments are not JSON");
                std::process::exit(2);
            };
            match gmclip_app::agent::run_call(name, json) {
                Ok((v, failed)) => {
                    println!("{v}");
                    if failed {
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("gmclip: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("bundle-fonts") => {
            let Some(dir) = args.get(1) else {
                eprintln!("usage: gmclip-app bundle-fonts <dir>");
                std::process::exit(2);
            };
            match gmclip_app::fonts::export(std::path::Path::new(dir)) {
                Ok(n) => println!("{n} font(s) in {dir}"),
                Err(e) => {
                    eprintln!("gmclip: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        // The stand-in MCP server the one-time isolation check (S7) starts a new Claude
        // Code build against: it records what the client asked and serves nothing real.
        Some("mcp-stub") => {
            let Some(rec) = args.get(1) else {
                eprintln!("usage: gmclip-app mcp-stub <record> [--tools a,b] [--responses f]");
                std::process::exit(2);
            };
            let opt = |n: &str| {
                args.iter()
                    .position(|a| a == n)
                    .and_then(|i| args.get(i + 1))
            };
            let only: Option<Vec<String>> =
                opt("--tools").map(|t| t.split(',').map(str::to_owned).collect());
            if let Err(e) = gmclip_eval::mcp_stub::run(
                std::path::Path::new(rec),
                only.as_deref(),
                opt("--responses").map(std::path::Path::new),
            ) {
                eprintln!("gmclip: {e}");
                std::process::exit(1);
            }
            return;
        }
        // `gmclip-app lab-project <fixture.json>` (dev-bridge builds; the e2e runner): a
        // committed project fixture becomes a project home in the library, printed as
        // `{"home": …}` for the page to open.
        #[cfg(feature = "dev-bridge")]
        Some("lab-project") => {
            let Some(fx) = args.get(1) else {
                eprintln!("usage: gmclip-app lab-project <fixture.json>");
                std::process::exit(2);
            };
            let r = gmclip_app::core::Dirs::from_env().and_then(|d| {
                let core = gmclip_app::core::AppCore::new(d);
                gmclip_app::labfixture::create(&core, std::path::Path::new(fx))
            });
            match r {
                Ok((home, summary)) => {
                    println!("{}", serde_json::json!({"home": home, "fixture": summary}))
                }
                Err(e) => {
                    eprintln!("gmclip: {}", e.message);
                    std::process::exit(1);
                }
            }
            return;
        }
        // `gmclip-app perf [--video <mp4>]` (U.18 / A2.05): the in-process §13 rows;
        // `gmclip-app perf --scrub <source> [--full-proxy]` (H.05): the scrub proxy, scrub
        // and exact-seek rows; `gmclip-app perf --asr <video>` (U.18): memory while a
        // whole analysis runs; `gmclip-app perf --open <video>` (lane K5): time to the
        // first frame of a long recording before its proxy exists.
        Some("perf") => {
            let arg = |k: &str| {
                args.iter()
                    .position(|a| a == k)
                    .and_then(|i| args.get(i + 1))
                    .map(std::path::PathBuf::from)
            };
            let video = arg("--video");
            #[cfg(any(target_os = "macos", windows))]
            let r = if let Some(v) = arg("--asr") {
                gmclip_app::perf::asr(&v)
            } else if let Some(v) = arg("--open") {
                gmclip_app::perf::open_first_frame(&v)
            } else {
                match arg("--scrub") {
                    Some(src) => gmclip_app::perf::scrub(
                        &src,
                        &gmclip_app::perf::ScrubOptions {
                            full_proxy: args.iter().any(|a| a == "--full-proxy"),
                        },
                    ),
                    None => gmclip_app::perf::run(video.as_deref()),
                }
            };
            #[cfg(not(any(target_os = "macos", windows)))]
            let r = match arg("--asr") {
                Some(v) => gmclip_app::perf::asr(&v),
                None => gmclip_app::perf::run(video.as_deref()),
            };
            match r {
                Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()),
                Err(e) => {
                    eprintln!("gmclip: {}", e.message);
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("serve") if args.iter().any(|a| a == "--headless") => {
            if let Err(e) = gmclip_app::agent::run_headless() {
                eprintln!("gmclip: {e}");
                std::process::exit(1);
            }
            return;
        }
        _ => {}
    }
    let opts = match gmclip_app::parse_args(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("gmclip: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = gmclip_app::run(opts) {
        eprintln!("gmclip: {e}");
        std::process::exit(1);
    }
}
