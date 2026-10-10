//! PID and creation identity prevent an orphan task surviving PID reuse.
pub fn stamp(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields: Vec<_> = stat[stat.rfind(')')? + 2..].split_whitespace().collect();
        if matches!(fields.first().copied(), Some("Z" | "X")) {
            return None;
        }
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        Some(format!("{}:{}", boot.trim(), fields.get(19)?))
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .ok()?;
        let text = String::from_utf8(output.stdout).ok()?;
        (output.status.success() && !text.trim().is_empty()).then(|| text.trim().to_owned())
    }
    #[cfg(target_os = "windows")]
    {
        use std::ffi::c_void;
        #[repr(C)]
        struct FileTime {
            low: u32,
            high: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
            fn GetProcessTimes(
                handle: *mut c_void,
                created: *mut FileTime,
                exited: *mut FileTime,
                kernel: *mut FileTime,
                user: *mut FileTime,
            ) -> i32;
            fn GetExitCodeProcess(handle: *mut c_void, code: *mut u32) -> i32;
            fn CloseHandle(handle: *mut c_void) -> i32;
        }
        unsafe {
            let handle = OpenProcess(0x1000, 0, pid);
            if handle.is_null() {
                return None;
            }
            let mut code = 0;
            let mut created = FileTime { low: 0, high: 0 };
            let mut exited = FileTime { low: 0, high: 0 };
            let mut kernel = FileTime { low: 0, high: 0 };
            let mut user = FileTime { low: 0, high: 0 };
            let alive = GetExitCodeProcess(handle, &mut code) != 0 && code == 259;
            let ok =
                GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0;
            CloseHandle(handle);
            (alive && ok).then(|| format!("{:08x}{:08x}", created.high, created.low))
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = pid;
        None
    }
}
