//! Process memory inspection and optional working-set trimming.

use windows::Win32::System::ProcessStatus::{
    EmptyWorkingSet, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::GetCurrentProcess;

#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryInfo {
    pub working_set: u64,
    pub private_usage: u64,
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
    })
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
