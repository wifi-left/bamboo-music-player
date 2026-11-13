//! Process memory inspection and optional working-set trimming.

use windows::Win32::System::ProcessStatus::{
    EmptyWorkingSet, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{
    GR_GDIOBJECTS, GR_USEROBJECTS, GetCurrentProcess, GetGuiResources, GetProcessHandleCount,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryInfo {
    pub working_set: u64,
    pub private_usage: u64,
    pub peak_working_set: u64,
    pub pagefile_usage: u64,
    pub page_faults: u32,
}

pub fn process_memory() -> Option<MemoryInfo> {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    unsafe {
        let ok = K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            counters.cb,
        );
        if !ok.as_bool() {
            return None;
        }
    }
    Some(MemoryInfo {
        working_set: counters.WorkingSetSize as u64,
        private_usage: counters.PrivateUsage as u64,
        peak_working_set: counters.PeakWorkingSetSize as u64,
        pagefile_usage: counters.PagefileUsage as u64,
        page_faults: counters.PageFaultCount,
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemMemory {
    pub total: u64,
    pub available: u64,
}

pub fn system_memory() -> Option<SystemMemory> {
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    Some(SystemMemory {
        total: status.ullTotalPhys,
        available: status.ullAvailPhys,
    })
}

pub fn handle_count() -> Option<u32> {
    let mut count = 0;
    unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) }
        .ok()
        .map(|_| count)
}

/// GDI and USER object counts for the process, as shown by Task Manager.
/// Zero is a valid count for a process with no GUI objects yet.
pub fn gui_resources() -> (u32, u32) {
    let process = unsafe { GetCurrentProcess() };
    let gdi = unsafe { GetGuiResources(process, GR_GDIOBJECTS) };
    let user = unsafe { GetGuiResources(process, GR_USEROBJECTS) };
    (gdi, user)
}

/// Trims the process working set. Pages remain valid but are moved to standby;
/// this reduces the Task Manager figure and is safe to call while idle.
pub fn trim_working_set() {
    unsafe {
        if let Err(err) = EmptyWorkingSet(GetCurrentProcess()) {
            tracing::warn!("EmptyWorkingSet failed: {err}");
        }
    }
}

pub fn format_mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

pub fn format_gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_metrics_are_readable() {
        let memory = process_memory().expect("process memory counters");
        assert!(memory.working_set > 0);
        assert!(memory.peak_working_set >= memory.working_set);
        let system = system_memory().expect("system memory status");
        assert!(system.total > 0);
        assert!(system.available <= system.total);
        assert!(handle_count().is_some(), "handle count");
        let _ = gui_resources();
    }
}
