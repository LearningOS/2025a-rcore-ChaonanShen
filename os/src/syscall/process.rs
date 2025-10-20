//! Process management syscalls
use crate::{
    mm::{ VirtAddr, VirtPageNum},
    task::{
        change_program_brk, check_not_mapped, exit_current_and_run_next,
        suspend_current_and_run_next, task_mmap, task_munmap, task_translated_byte_buffer,
    },
    timer::get_time_us,
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
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time"); // 写入TimeVal地址
    let us = get_time_us();
    let tv = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };

    // ts是用户态的虚拟地址，得转换为物理页的字节数组才能得到真正内容
    let src = unsafe { core::slice::from_raw_parts(&tv as *const TimeVal as *const u8, core::mem::size_of::<TimeVal>()) };
    let dsts = task_translated_byte_buffer(ts as *const u8, core::mem::size_of::<TimeVal>());

    let mut idx = 0;
    for dst in dsts {
        // get能够安全返回切片
        if let Some(sub_src) = src.get(idx..idx+dst.len()) {
            dst.copy_from_slice(sub_src);
            idx += dst.len();
        } else {
            return -1;
        }
    }
    0
}

/// YOUR JOB: Finish sys_trace to pass testcases
/// HINT: You might reimplement it with virtual memory management.
pub fn sys_trace(_trace_request: usize, _id: usize, _data: usize) -> isize {
    trace!("kernel: sys_trace");
    -1
}

// YOUR JOB: Implement mmap.
/// 当前task的TCB的MemorySet(包含页表)中，将[start, start+len)段映射到物理页
/// 出错类型：都返回-1
/// 1. start没有按页对齐
/// 2. [start, start+len)中已经存在被映射的页（说明当前mmap跟之前的mmap重叠了）
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
/// 当前task的TCB的MemorySet(包含页表)中，将[start, start+len)区域解除映射
/// 出错类型：都返回-1
/// 1. start没有按页对齐
/// 2. [start, start+len)中有页面没有映射过（说明当前munmap把一些没有mmap过的区域也包含进去了）
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel: sys_munmap");

    let start_va: VirtAddr = start.into();
    // 检查start_va是否对齐
    if !start_va.aligned() {
        return -1;
    }

    // task_munmap会检查之前是否存在这个映射(通过查找MapArea)，如果存在就删除(页表相关也删除)，不存在就算出错
    let end_va: VirtAddr = (start + len).into();
    if task_munmap(start_va, end_va) {
        0
    } else {
        -1
    }
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
