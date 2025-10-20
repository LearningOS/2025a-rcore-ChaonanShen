//! Process management syscalls
use crate::{
    mm::{VirtAddr, VirtPageNum},
    task::{change_program_brk, check_not_mapped, exit_current_and_run_next, suspend_current_and_run_next, task_mmap},
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(_ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time"); // 写入TimeVal地址
    0
}

/// TODO: Finish sys_trace to pass testcases
/// HINT: You might reimplement it with virtual memory management.
pub fn sys_trace(_trace_request: usize, _id: usize, _data: usize) -> isize {
    trace!("kernel: sys_trace");
    -1
}

// YOUR JOB: Implement mmap.
/// 当前task页表中，将[start, start+len)段映射到物理页 使用
/// 出错类型：都返回-1
/// 1. start没有按页对齐
/// 2. [start, start+len)中已经存在被映射的页
/// 3. prot & !0x7 != 0或者prot & 0x7 = 0 - prot 0-R 1-W 2-X 其他位必须为0
/// 4. 物理内存不足
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    trace!("kernel: sys_mmap");

    let start_va: VirtAddr = start.into();
    // 检查start_va是否对齐
    if !start_va.aligned() {
        return -1;
    }
    // 检查prot位是否正确
    if prot & !0x7 != 0 || prot & 0x7 == 0 {
        return -1;
    }
    // 检查是否已经存在被映射的页面
    let start_vpn: VirtPageNum = start_va.into();
    let end_va: VirtAddr = (start + len).into();
    let end_vpn: VirtPageNum = end_va.ceil();
    if !check_not_mapped(start_vpn, end_vpn) {
        return -1;
    }

    if task_mmap(start_va, end_va, prot) {
        0 // 操作成功返回0
    } else {
        -1
    }
}

// YOUR JOB: Implement munmap.
pub fn sys_munmap(_start: usize, _len: usize) -> isize {
    trace!("kernel: sys_munmap");
    -1
}
/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
