//! Process CPU time, shared by the worker and its measurement harness.
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

#[allow(dead_code)] // The OCR worker shares this module for CPU timing only.
pub fn memory_pressure() -> bool {
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: the initialized struct has the required size and a writable pointer.
    unsafe { GlobalMemoryStatusEx(&mut status) }.is_ok()
        && (status.dwMemoryLoad >= 90 || status.ullAvailPhys < 768 * 1024 * 1024)
}

pub fn cpu_ms() -> f64 {
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: GetCurrentProcess is a borrowed pseudo-handle and all four
    // pointers refer to initialized writable FILETIME structures.
    if unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    }
    .is_err()
    {
        return f64::NAN;
    }
    let ticks =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    (ticks(kernel) + ticks(user)) as f64 / 10_000.0
}
