//! Windows resource measurements. Missing counters remain unknown, never zero.

use crate::background_policy::{ExecutionEnvironment, ResourceSample};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{FILETIME, HANDLE};
use windows::Win32::System::JobObjects::{
    JobObjectCpuRateControlInformation, QueryInformationJobObject, SetInformationJobObject,
    JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOB_OBJECT_CPU_RATE_CONTROL_ENABLE,
    JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::GetProcessTimes;

pub struct ResourceSampler {
    system: sysinfo::System,
    disk: Option<DiskCounter>,
}

impl ResourceSampler {
    pub fn new() -> Self {
        let mut system = sysinfo::System::new();
        system.refresh_cpu();
        system.refresh_memory();
        Self {
            system,
            disk: DiskCounter::new(),
        }
    }

    pub fn environment(&self) -> ExecutionEnvironment {
        ExecutionEnvironment {
            logical_cores: self.system.cpus().len(),
            physical_memory_bytes: self.system.total_memory(),
            runtime: format!(
                "{}:{}:{}:ml{}:ort1.24.2:budget-v2:spin0:{}:{}:{}",
                std::env::consts::OS,
                std::env::consts::ARCH,
                sysinfo::System::os_version().unwrap_or_default(),
                crate::ml_protocol::ML_PROTOCOL_VERSION,
                std::env::var("CARBONPAPER_ONNX_INTRA_THREADS").unwrap_or_default(),
                self.system
                    .cpus()
                    .first()
                    .map(|cpu| format!("{}:{}", cpu.vendor_id(), cpu.brand()))
                    .unwrap_or_default(),
                env!("CARGO_PKG_VERSION")
            ),
        }
    }

    pub fn sample(&mut self, at_ms: u64) -> ResourceSample {
        self.system.refresh_cpu();
        self.system.refresh_memory();
        ResourceSample {
            at_ms,
            total_cpu_percent: (!self.system.cpus().is_empty())
                .then(|| self.system.global_cpu_info().cpu_usage()),
            busiest_core_percent: self
                .system
                .cpus()
                .iter()
                .map(|c| c.cpu_usage())
                .max_by(f32::total_cmp),
            available_memory_bytes: (self.system.total_memory() > 0)
                .then(|| self.system.available_memory()),
            disk_latency_ms: self.disk.as_mut().and_then(DiskCounter::sample),
        }
    }
}

struct DiskCounter {
    query: isize,
    counter: isize,
}

impl DiskCounter {
    fn new() -> Option<Self> {
        let mut query = 0;
        let mut counter = 0;
        // SAFETY: PDH receives initialized out-pointers and a static null-terminated
        // English counter path. This object exclusively owns the resulting query.
        unsafe {
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return None;
            }
            if PdhAddEnglishCounterW(
                query,
                w!("\\PhysicalDisk(_Total)\\Avg. Disk sec/Transfer"),
                0,
                &mut counter,
            ) != 0
            {
                PdhCloseQuery(query);
                return None;
            }
            PdhCollectQueryData(query);
        }
        Some(Self { query, counter })
    }
    fn sample(&mut self) -> Option<f64> {
        let mut value = PDH_FMT_COUNTERVALUE::default();
        // SAFETY: both handles belong to this live query; PDH writes a double
        // because PDH_FMT_DOUBLE was requested. A failed status is not a sample.
        unsafe {
            if PdhCollectQueryData(self.query) != 0
                || PdhGetFormattedCounterValue(self.counter, PDH_FMT_DOUBLE, None, &mut value) != 0
                || value.CStatus > 1
            {
                return None;
            }
            let ms = value.Anonymous.doubleValue * 1000.0;
            (ms.is_finite() && ms >= 0.0).then_some(ms)
        }
    }
}
impl Drop for DiskCounter {
    fn drop(&mut self) {
        // SAFETY: the query has one owner and is no longer sampled during drop.
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

/// Job limits count a percentage of the whole machine, not one logical CPU.
pub fn set_cpu_rate(job: HANDLE, percent: Option<u32>) -> Result<(), String> {
    let mut current = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default();
    // Windows rejects ControlFlags=0 with ERROR_INVALID_PARAMETER when CPU
    // rate control has never been enabled for this Job Object. Query first so
    // restoring an already-unlimited worker is an idempotent operation.
    unsafe {
        QueryInformationJobObject(
            job,
            JobObjectCpuRateControlInformation,
            &mut current as *mut _ as *mut _,
            std::mem::size_of_val(&current) as u32,
            None,
        )
    }
    .map_err(|e| format!("worker_budget: failed to read current CPU budget: {e}"))?;

    let enabled = current.ControlFlags.0 & JOB_OBJECT_CPU_RATE_CONTROL_ENABLE.0 != 0;
    if percent.is_none() && !enabled {
        return Ok(());
    }

    let mut info = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default();
    if let Some(percent) = percent {
        if !(1..=100).contains(&percent) {
            return Err("invalid worker CPU budget".into());
        }
        info.ControlFlags =
            JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
        info.Anonymous.CpuRate = percent * 100;

        if current.ControlFlags == info.ControlFlags
            // SAFETY: HARD_CAP selects the CpuRate member of the Win32 union.
            && unsafe { current.Anonymous.CpuRate == info.Anonymous.CpuRate }
        {
            return Ok(());
        }
    }
    // SAFETY: info is the documented structure for this information class and
    // job remains owned by the child supervisor throughout this call.
    unsafe {
        SetInformationJobObject(
            job,
            JobObjectCpuRateControlInformation,
            &info as *const _ as *const _,
            std::mem::size_of_val(&info) as u32,
        )
    }
    .map_err(|e| format!("worker_budget: {e}"))
}

#[derive(Debug, Clone, Copy)]
pub struct ProcessUsage {
    pub cpu_ms: f64,
    pub private_bytes: u64,
    pub peak_private_bytes: u64,
}

impl ProcessUsage {
    /// A previous model's lifetime peak is not this operation's extra memory.
    /// Include the OS peak only when this interval advanced it; otherwise use
    /// the private samples collected while the operation is executing.
    pub fn peak_since(self, before: Option<Self>) -> u64 {
        if before.is_none_or(|before| self.peak_private_bytes > before.peak_private_bytes) {
            self.peak_private_bytes
        } else {
            self.private_bytes
        }
    }
}

pub fn process_usage(process: HANDLE) -> Option<ProcessUsage> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let mut memory = PROCESS_MEMORY_COUNTERS_EX::default();
    memory.cb = std::mem::size_of_val(&memory) as u32;
    // SAFETY: all out-pointers are writable and correctly sized. The caller
    // keeps the process handle alive; neither API takes ownership of it.
    unsafe {
        GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user).ok()?;
        GetProcessMemoryInfo(process, &mut memory as *mut _ as *mut _, memory.cb).ok()?;
    }
    let ticks =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    Some(ProcessUsage {
        cpu_ms: (ticks(kernel) + ticks(user)) as f64 / 10_000.0,
        private_bytes: memory.PrivateUsage as u64,
        peak_private_bytes: memory.PrivateUsage.max(memory.PeakPagefileUsage) as u64,
    })
}
