//! Spawning the client: no shell, an empty environment plus the whitelist, the prompt on
//! stdin, stdout to the transcript file, and a wall clock.
//!
//! `std::process::Command` is `posix_spawn` on macOS whenever it can be (no `pre_exec`
//! hook is used here, so it can). Signals go through `/bin/kill` by absolute path (no
//! `unsafe` `kill(2)` in this crate). Two groupings:
//! - [`Grouping::Own`] (harnesses): the client gets its own process group; the wall clock
//!   and the client's exit signal the whole group, so its MCP children die with it.
//! - [`Grouping::Foreground`] (`gmclip agent run` at a terminal): the client stays in the
//!   terminal's foreground group, so Ctrl-C and a closed terminal reach it as they reach
//!   the launcher (std has no signal handler without `unsafe`, so the launcher could not
//!   forward them). The wall clock then signals the client's process tree.
//!
//! Windows (decisions/0019): [`Grouping::Own`] starts the client in a new process group
//! (Ctrl-C at the launcher's console does not reach it), and the wall clock stops the
//! client's tree with `%SystemRoot%\System32\taskkill.exe /T` (polite, then `/F`). The
//! launcher holds the child's handle until then, so its pid cannot have been reused.
//! The client also runs in a kill-on-close **job object** (W.9,
//! `gmclip_platform::windows::Job`): what it leaves running when it exits on its own (an
//! MCP server, whose parent is gone, so no tree leads to it) is ended with the job when
//! the run ends, and if the launcher itself dies the OS ends them. A grandchild the
//! client starts in the instant before it is assigned escapes (std spawns it running);
//! MCP servers start well after. A batch file is refused as the program
//! (CVE-2024-24576; [`super::resolve`] never yields one).
//!
//! TCC (R-AGT-2) is NOT disclaimed here: `responsibility_spawnattrs_setdisclaim` is a
//! private SPI call that needs `unsafe` FFI, and `unsafe` is confined to `gmclip-gpu`
//! (SPEC §3.1). The run record says so (`tcc_disclaimed: false`); see `spikes/S7`.

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Where the client's process group is (see the module docs).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Grouping {
    Own,
    Foreground,
}

/// The outcome of one child run.
#[derive(Debug)]
pub struct Finished {
    pub status: Option<ExitStatus>,
    pub timed_out: bool,
    pub elapsed: Duration,
    pub pid: u32,
}

/// Grace between SIGTERM and SIGKILL on a wall-clock stop.
pub const KILL_GRACE: Duration = Duration::from_secs(5);

/// Runs `argv` (argv[0] an absolute path) to completion or until `wall_clock`.
#[allow(clippy::too_many_arguments)]
pub fn run(
    argv: &[OsString],
    env: &[(OsString, OsString)],
    cwd: &Path,
    stdin: &[u8],
    stdout_to: &Path,
    stderr_to: &Path,
    wall_clock: Duration,
    grouping: Grouping,
) -> std::io::Result<Finished> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::other("empty argv"))?;
    if !Path::new(program).is_absolute() {
        return Err(std::io::Error::other("the client must be an absolute path"));
    }
    #[cfg(windows)]
    if super::winshim::is_batch(Path::new(program)) {
        return Err(std::io::Error::other(
            "a batch file is never run as the client (CVE-2024-24576)",
        ));
    }
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(std::fs::File::create(stdout_to)?))
        .stderr(Stdio::from(std::fs::File::create(stderr_to)?));
    #[cfg(unix)]
    if grouping == Grouping::Own {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    if grouping == Grouping::Own {
        use std::os::windows::process::CommandExt;
        /// `CREATE_NEW_PROCESS_GROUP`
        const NEW_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(NEW_GROUP);
    }
    let signal = |pid: u32, sig: &str| match grouping {
        Grouping::Own => signal_group(pid, sig),
        Grouping::Foreground => signal_tree(pid, sig),
    };
    let started = Instant::now();
    let mut child = cmd.spawn()?;
    let pid = child.id();
    // everything the client starts from here on is in the job, and ends with the run
    #[cfg(windows)]
    let job = gmclip_platform::windows::Job::kill_on_close()
        .and_then(|j| j.assign(&child).map(|()| j))
        .ok();
    if let Some(mut si) = child.stdin.take() {
        let bytes = stdin.to_vec();
        // a separate thread: a child that never reads stdin must not block the clock
        std::thread::spawn(move || {
            let _ = si.write_all(&bytes);
        });
    }
    loop {
        if let Some(status) = child.try_wait()? {
            // the client is gone; anything it left in its group (an MCP server holding
            // GMCLIP_RUN in its environment) goes with it
            #[cfg(unix)]
            if grouping == Grouping::Own {
                signal_group(pid, "-TERM");
            }
            #[cfg(windows)]
            if let Some(j) = &job {
                let _ = j.terminate(1);
            }
            return Ok(Finished {
                status: Some(status),
                timed_out: false,
                elapsed: started.elapsed(),
                pid,
            });
        }
        if started.elapsed() >= wall_clock {
            signal(pid, "-TERM");
            // Windows: taskkill cannot ask a console program to stop; Ctrl-Break to the
            // client's own group is the polite stop (the SIGTERM to a group). Without a
            // console to send it from (a GUI launcher) there is none: no grace, the job
            // ends now rather than leaving everything running for KILL_GRACE.
            #[cfg(windows)]
            let polite =
                grouping == Grouping::Own && gmclip_platform::windows::ctrl_break(pid).is_ok();
            #[cfg(not(windows))]
            let polite = true;
            let grace = Instant::now();
            while polite && grace.elapsed() < KILL_GRACE {
                if child.try_wait()?.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            signal(pid, "-KILL");
            #[cfg(windows)]
            if let Some(j) = &job {
                let _ = j.terminate(1);
            }
            let _ = child.kill();
            let status = child.wait().ok();
            return Ok(Finished {
                status,
                timed_out: true,
                elapsed: started.elapsed(),
                pid,
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `pid` and every descendant (`/usr/bin/pgrep -P`, walked), parents first: a parent
/// signalled after its child could run its next command in between.
#[cfg(unix)]
fn tree(pid: u32) -> Vec<u32> {
    let mut out = vec![pid];
    let kids = Command::new("/usr/bin/pgrep")
        .env_clear()
        .args(["-P", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    for k in kids.lines().filter_map(|l| l.trim().parse::<u32>().ok()) {
        out.extend(tree(k));
    }
    out
}

fn signal_tree(pid: u32, sig: &str) {
    #[cfg(windows)]
    taskkill_tree(pid, sig);
    #[cfg(unix)]
    for p in tree(pid) {
        let _ = Command::new("/bin/kill")
            .env_clear()
            .args([sig, &p.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (pid, sig);
}

/// `taskkill /T` on `pid`'s tree: `-TERM` asks (a window gets `WM_CLOSE`), `-KILL` ends
/// it (`/F`). By absolute path, with an empty environment but `SystemRoot`.
#[cfg(windows)]
fn taskkill_tree(pid: u32, sig: &str) {
    let root = std::env::var_os("SystemRoot")
        .filter(|r| !r.is_empty())
        .map_or_else(
            || std::path::PathBuf::from(r"C:\Windows"),
            std::path::PathBuf::from,
        );
    let mut c = Command::new(root.join("System32").join("taskkill.exe"));
    c.env_clear()
        .env("SystemRoot", &root)
        .args(["/T", "/PID", &pid.to_string()]);
    if sig == "-KILL" {
        c.arg("/F");
    }
    let _ = c
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub(crate) fn signal_group(pgid: u32, sig: &str) {
    // Windows: a process group is not a kill target; the tree is
    #[cfg(windows)]
    taskkill_tree(pgid, sig);
    #[cfg(unix)]
    {
        let _ = Command::new("/bin/kill")
            .env_clear()
            .args([sig, "--", &format!("-{pgid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pgid, sig);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("gmclip-spawn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn env_is_exactly_the_given_one_and_stdin_arrives() {
        let d = tmp("env");
        let argv: Vec<OsString> = ["/bin/sh", "-c", "env | sort; cat"]
            .iter()
            .map(Into::into)
            .collect();
        let env = vec![
            ("A_KEY".into(), "1".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ];
        let f = run(
            &argv,
            &env,
            &d,
            b"from-stdin",
            &d.join("o"),
            &d.join("e"),
            Duration::from_secs(20),
            Grouping::Own,
        )
        .unwrap();
        assert!(f.status.unwrap().success() && !f.timed_out);
        let out = std::fs::read_to_string(d.join("o")).unwrap();
        let keys: Vec<&str> = out
            .lines()
            .filter_map(|l| l.split_once('=').map(|x| x.0))
            .collect();
        // sh itself adds PWD/SHLVL/_; nothing from the test process leaks
        for k in &keys {
            assert!(
                ["A_KEY", "PATH", "PWD", "SHLVL", "_", "OLDPWD"].contains(k),
                "{k}"
            );
        }
        assert!(out.ends_with("from-stdin"), "{out}");
    }

    #[test]
    fn the_wall_clock_kills_the_whole_group() {
        let d = tmp("wall");
        // a grandchild that would outlive a plain kill of the child
        let marker = d.join("survived");
        let script = format!("(sleep 3; touch {}) & sleep 60", marker.display());
        let argv: Vec<OsString> = ["/bin/sh".into(), "-c".into(), script.into()].to_vec();
        let f = run(
            &argv,
            &[],
            &d,
            b"",
            &d.join("o"),
            &d.join("e"),
            Duration::from_millis(400),
            Grouping::Own,
        )
        .unwrap();
        assert!(f.timed_out);
        assert!(f.elapsed < Duration::from_secs(8), "{:?}", f.elapsed);
        std::thread::sleep(Duration::from_secs(4));
        assert!(!marker.exists(), "the grandchild survived the wall clock");
    }

    #[test]
    fn foreground_wall_clock_kills_the_tree() {
        let d = tmp("fg");
        let marker = d.join("survived");
        let script = format!("(sleep 3; touch {}) & sleep 60", marker.display());
        let argv: Vec<OsString> = ["/bin/sh".into(), "-c".into(), script.into()].to_vec();
        let f = run(
            &argv,
            &[],
            &d,
            b"",
            &d.join("o"),
            &d.join("e"),
            Duration::from_millis(400),
            Grouping::Foreground,
        )
        .unwrap();
        assert!(f.timed_out);
        std::thread::sleep(Duration::from_secs(4));
        assert!(!marker.exists(), "a descendant survived the wall clock");
    }

    #[test]
    fn relative_programs_are_refused() {
        let d = tmp("rel");
        let argv: Vec<OsString> = vec!["sh".into()];
        assert!(
            run(
                &argv,
                &[],
                &d,
                b"",
                &d.join("o"),
                &d.join("e"),
                Duration::from_secs(1),
                Grouping::Own,
            )
            .is_err()
        );
    }
}

/// Windows only (W.9): these await the PC; `tools/check_windows.sh` type-checks them.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn cmd() -> OsString {
        std::env::var_os("SystemRoot")
            .map_or_else(
                || std::path::PathBuf::from(r"C:\Windows"),
                std::path::PathBuf::from,
            )
            .join("System32")
            .join("cmd.exe")
            .into_os_string()
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("gmclip-spawn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn env() -> Vec<(OsString, OsString)> {
        ["SystemRoot", "PATH", "COMSPEC"]
            .iter()
            .filter_map(|k| Some(((*k).into(), std::env::var_os(k)?)))
            .collect()
    }

    /// `cmd /c start "" /b cmd /c "ping … && echo x > <marker>"`: a detached process that
    /// writes `marker` `secs` seconds later unless it is stopped (`&&`: an interrupted ping
    /// writes nothing; cmd.exe would otherwise run the next command after a Ctrl-Break).
    /// One argument per word: Rust escapes an embedded quote as `\"`, which cmd.exe does
    /// not read (the outer cmd then hangs).
    fn detached(marker: &std::path::Path, secs: u64) -> Vec<OsString> {
        assert!(
            !marker.to_string_lossy().contains(' '),
            "{}",
            marker.display()
        );
        vec![
            cmd(),
            "/c".into(),
            "start".into(),
            "".into(),
            "/b".into(),
            cmd(),
            "/c".into(),
            format!(
                "ping -n {} 127.0.0.1 >nul && echo x > {}",
                secs + 1,
                marker.display()
            )
            .into(),
        ]
    }

    /// The client exits at once, leaving a detached process that would write a marker
    /// three seconds later: the job ends it with the run, so the marker never appears.
    #[test]
    fn what_a_client_leaves_behind_ends_with_the_run() {
        let d = tmp("job");
        let marker = d.join("survived");
        let argv = detached(&marker, 3);
        let f = run(
            &argv,
            &env(),
            &d,
            b"",
            &d.join("o"),
            &d.join("e"),
            Duration::from_secs(20),
            Grouping::Own,
        )
        .unwrap();
        assert!(!f.timed_out);
        std::thread::sleep(Duration::from_secs(5));
        assert!(!marker.exists(), "the detached process outlived the run");
    }

    /// The wall clock ends the client and everything in its job.
    #[test]
    fn the_wall_clock_ends_the_job() {
        let d = tmp("jobwall");
        let marker = d.join("survived");
        // past the wall clock and the grace: ping ignores Ctrl-Break (it prints its
        // statistics and goes on), so only the forced stop at the end of the grace ends it
        let mut argv = detached(&marker, KILL_GRACE.as_secs() + 3);
        argv.extend(["&", "ping", "-n", "60", "127.0.0.1", ">nul"].map(OsString::from));
        let f = run(
            &argv,
            &env(),
            &d,
            b"",
            &d.join("o"),
            &d.join("e"),
            Duration::from_millis(500),
            Grouping::Own,
        )
        .unwrap();
        assert!(f.timed_out);
        std::thread::sleep(Duration::from_secs(5));
        assert!(
            !marker.exists(),
            "a process in the job outlived the wall clock"
        );
    }
}
