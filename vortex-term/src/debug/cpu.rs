//! The CPU time of a debugged thread or process, read from the OS.
//!
//! Linux gives the time of one thread (`/proc/<pid>/task/<tid>/stat`); macOS only of the whole
//! process (`proc_pidinfo`). Elsewhere there is no reading.

use std::time::Duration;

/// What a reading covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Thread,
    Process,
}

/// CPU time (user + system) of thread `thread` of process `pid` when the OS gives per-thread
/// times and the thread exists under that id, else of the whole process. None where
/// unsupported or when the process can't be read.
pub fn cpu_time(pid: u32, thread: Option<u64>) -> Option<(Duration, Scope)> {
    imp::cpu_time(pid, thread)
}

/// The utime and stime (fields 14 and 15, counting from 1) of a `/proc/.../stat` line, in clock
/// ticks. The comm field (2) may hold spaces and parentheses, so the fields are counted after
/// the last `)`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_stat_ticks(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // `rest` starts with field 3 (state): utime is the 12th and stime the 13th field of it.
    let mut fields = rest.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    utime.checked_add(stime)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{parse_stat_ticks, Scope};
    use std::time::Duration;

    fn read(path: &str) -> Option<Duration> {
        let ticks = parse_stat_ticks(&std::fs::read_to_string(path).ok()?)?;
        // SAFETY: sysconf has no preconditions.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let hz = if hz > 0 { hz as u64 } else { 100 };
        Some(Duration::from_nanos(
            (ticks as u128 * 1_000_000_000 / hz as u128) as u64,
        ))
    }

    pub fn cpu_time(pid: u32, thread: Option<u64>) -> Option<(Duration, Scope)> {
        if let Some(thread) = thread {
            if let Some(time) = read(&format!("/proc/{pid}/task/{thread}/stat")) {
                return Some((time, Scope::Thread));
            }
        }
        read(&format!("/proc/{pid}/stat")).map(|time| (time, Scope::Process))
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Scope;
    use std::{ffi::c_void, mem, time::Duration};

    pub fn cpu_time(pid: u32, _thread: Option<u64>) -> Option<(Duration, Scope)> {
        // SAFETY: `info` is plain data that proc_pidinfo fills, at most `size` bytes of it.
        let mut info: libc::proc_taskinfo = unsafe { mem::zeroed() };
        let size = mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        let written = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTASKINFO,
                0,
                &mut info as *mut libc::proc_taskinfo as *mut c_void,
                size,
            )
        };
        if written != size {
            return None;
        }
        let nanos = info.pti_total_user.checked_add(info.pti_total_system)?;
        Some((Duration::from_nanos(nanos), Scope::Process))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use super::Scope;
    use std::time::Duration;

    pub fn cpu_time(_pid: u32, _thread: Option<u64>) -> Option<(Duration, Scope)> {
        None
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn the_comm_may_hold_spaces_and_parentheses() {
        let line = "42 ((a) b)) S 1 42 42 0 -1 4194304 100 0 0 0 7 5 0 0 20 0 1 0 100 1000 10";
        assert_eq!(parse_stat_ticks(line), Some(12));
        assert_eq!(parse_stat_ticks("garbage"), None);
        assert_eq!(parse_stat_ticks("1 (x) S 1 2"), None);
    }

    #[test]
    fn reads_the_own_process_and_main_thread() {
        let pid = std::process::id();
        let (_, scope) = cpu_time(pid, None).unwrap();
        assert_eq!(scope, Scope::Process);
        // The main thread's id is the pid; the test runs on another thread but may read it.
        let (_, scope) = cpu_time(pid, Some(pid as u64)).unwrap();
        assert_eq!(scope, Scope::Thread);
    }

    #[test]
    fn a_bogus_thread_falls_back_to_the_process() {
        let (_, scope) = cpu_time(std::process::id(), Some(u32::MAX as u64)).unwrap();
        assert_eq!(scope, Scope::Process);
        assert!(cpu_time(u32::MAX, None).is_none());
    }

    #[test]
    fn the_reading_grows_while_a_thread_works() {
        let tid = unsafe { libc::syscall(libc::SYS_gettid) } as u64;
        let pid = std::process::id();
        let (before, scope) = cpu_time(pid, Some(tid)).unwrap();
        assert_eq!(scope, Scope::Thread);
        let start = std::time::Instant::now();
        let mut x = 0u64;
        while start.elapsed() < Duration::from_millis(200) {
            x = std::hint::black_box(x.wrapping_add(1));
        }
        let (after, _) = cpu_time(pid, Some(tid)).unwrap();
        assert!(after > before, "{before:?} -> {after:?}");
    }
}
