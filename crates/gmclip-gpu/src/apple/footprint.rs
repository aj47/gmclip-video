//! Process memory as Activity Monitor and jetsam see it (`phys_footprint`), for the
//! import/drop leak test (H-01) and the export memory budget.

#[repr(C)]
#[derive(Default)]
struct RusageInfoV0 {
    ri_uuid: [u8; 16],
    ri_user_time: u64,
    ri_system_time: u64,
    ri_pkg_idle_wkups: u64,
    ri_interrupt_wkups: u64,
    ri_pageins: u64,
    ri_wired_size: u64,
    ri_resident_size: u64,
    ri_phys_footprint: u64,
    ri_proc_start_abstime: u64,
    ri_proc_exit_abstime: u64,
}

const RUSAGE_INFO_V0: i32 = 0;

unsafe extern "C" {
    // libproc.h (libSystem).
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut RusageInfoV0) -> i32;
}

/// This process's `phys_footprint` in bytes.
pub fn phys_footprint() -> Option<u64> {
    let mut info = RusageInfoV0::default();
    // SAFETY: `info` is a correctly sized, writable `rusage_info_v0` for flavor 0.
    let rc = unsafe { proc_pid_rusage(std::process::id() as i32, RUSAGE_INFO_V0, &mut info) };
    (rc == 0).then_some(info.ri_phys_footprint)
}
