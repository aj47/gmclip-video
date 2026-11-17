//! The at-exit end (`apple::exit::end_now_if_main_returned`) keeps a test harness's
//! status: this binary runs itself as a child with the handler registered; a child whose
//! test passed ends through the handler with 0, one whose test failed keeps libtest's 101
//! (its exit is `std::process::exit` inside `main`, so the handler lets it go on).
#![cfg(target_os = "macos")]

use std::process::Command;

const CHILD: &str = "GMCLIP_EXIT_STATUS_CHILD";

extern "C" fn end() {
    println!(
        "exit-status child: main returned = {}",
        gmclip_gpu::apple::exit::main_has_returned()
    );
    gmclip_gpu::apple::exit::end_now_if_main_returned();
    println!("exit-status child: went on to the normal exit");
}

fn as_child() -> bool {
    std::env::var_os(CHILD).is_some()
}

#[test]
fn child_passes() {
    if as_child() {
        assert!(gmclip_gpu::apple::exit::at_exit(end));
    }
}

#[test]
fn child_fails() {
    if as_child() {
        assert!(gmclip_gpu::apple::exit::at_exit(end));
        panic!("this child fails on purpose");
    }
}

fn child(name: &str) -> (Option<i32>, String) {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--test-threads=1", "--nocapture"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn main_returning_ends_with_0_and_a_failed_run_keeps_101() {
    if as_child() {
        return;
    }
    let (code, out) = child("child_passes");
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("main returned = true"), "{out}");
    assert!(!out.contains("went on"), "{out}");
    let (code, out) = child("child_fails");
    assert_eq!(code, Some(101), "{out}");
    assert!(out.contains("main returned = false"), "{out}");
    assert!(out.contains("went on to the normal exit"), "{out}");
}

#[test]
fn a_test_thread_is_not_main_returning() {
    assert!(!gmclip_gpu::apple::exit::main_has_returned());
}
