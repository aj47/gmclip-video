//! `gmclip-launch`: what the Windows Start menu shortcut runs.
//!
//! `gmclip-app.exe` is a console program, because it is also the MCP shim and
//! `gmclip-app call`, which talk over stdio. Started from the Start menu or Explorer, a
//! console program gets a console window of its own beside the app's window, and closing
//! that window ends gmclip. This launcher is a windows-subsystem program (no console),
//! and it starts the `gmclip-app` beside it with `CREATE_NO_WINDOW`: the app still has a
//! console, without a window, so the programs it starts (curl, tar, an agent client)
//! inherit that one instead of each opening their own, and Ctrl-Break still reaches an
//! agent's process group.
//!
//! It passes its arguments through and does not wait. On other systems it starts the app
//! the same way, without the flag.
#![forbid(unsafe_code)]
#![cfg_attr(windows, windows_subsystem = "windows")]

/// `CREATE_NO_WINDOW`: a console program started with it gets a console and no window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn main() {
    let name = if cfg!(windows) {
        "gmclip-app.exe"
    } else {
        "gmclip-app"
    };
    let Ok(me) = std::env::current_exe() else {
        std::process::exit(1);
    };
    let mut cmd = std::process::Command::new(me.with_file_name(name));
    cmd.args(std::env::args_os().skip(1));
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, CREATE_NO_WINDOW);
    if cmd.spawn().is_err() {
        std::process::exit(1);
    }
}
