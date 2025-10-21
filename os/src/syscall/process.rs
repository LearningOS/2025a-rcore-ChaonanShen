//! Process management syscalls
use crate::{
    mm::{translated_ua2read, translated_ua2write, VirtAddr, VirtPageNum},
    task::{
        change_program_brk, check_not_mapped, current_user_token, exit_current_and_run_next,
        suspend_current_and_run_next, task_get_syscall_count, task_mmap, task_munmap,
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

/// 物理地址的src 拷贝数据到 用户态虚地址的dst - 内核态数据复制到用户空间虚地址，注意要检查虚地址是否有写权限
#[allow(unused)]
fn pa_copyto_uva(src: *const u8, src_len: usize, dst: *const u8, dst_len: usize) -> bool {
    if let Some(dsts) = translated_ua2write(current_user_token(), dst, dst_len) {
        let src = unsafe { core::slice::from_raw_parts(src, src_len) };

        let mut idx = 0;
        for dst in dsts {
            // get能够安全返回切片
            if let Some(sub_src) = src.get(idx..idx + dst.len()) {
                dst.copy_from_slice(sub_src);
                idx += dst.len();
            } else {
                return false;
            }
        }
        true
    } else {
        false
    }
}

/// 用户态虚地址的src 拷贝数据到 物理地址的dst - 从用户空间虚地址读取数据到内核态，注意要检查虚地址是否有读权限
#[allow(unused)]
fn uva_copyto_pa(src: *const u8, src_len: usize, dst: *mut u8, dst_len: usize) -> bool {
    if let Some(srcs) = translated_ua2read(current_user_token(), src, src_len) {
        let dst = unsafe { core::slice::from_raw_parts_mut(dst, dst_len) };

        let mut idx = 0;
        for src in srcs {
            // get能够安全返回切片
            if let Some(sub_dst) = dst.get_mut(idx..idx + src.len()) {
                sub_dst.copy_from_slice(src);
                idx += src.len();
            } else {
                return false;
            }
        }
        true
    } else {
        false
    }
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time"); // 写入TimeVal地址
    let us = get_time_us();

    // 目标就是把内核态的tv拷贝到用户态的ts虚地址（把ts虚地址转为物理地址上字节序列，然后就能直接复制了）
    let tv = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };

    let sz = core::mem::size_of::<TimeVal>();
    if pa_copyto_uva(&tv as *const TimeVal as *const u8, sz, ts as *const u8, sz) {
        0
    } else {
        -1
    }
}

/// YOUR JOB: Finish sys_trace to pass testcases
/// HINT: You might reimplement it with virtual memory management.
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    match trace_request {
        // 其实读写一个u8没必要用uva_copyto_pa这样的跨物理页复制，但这里为了统一就在sys_get_time/sys_trace里一起用了
        0 => {
            // 把id当作&u8用户态虚地址 读取其中数据
            let sz = core::mem::size_of::<u8>();
            let mut dst: u8 = 0;
            if uva_copyto_pa(id as *const u8, sz, &mut dst as *mut u8, sz) {
                dst as isize
            } else {
                -1
            }
        }
        1 => {
            // 把id当作&u8用户态虚地址 向其中写入数据data(取最低字节u8写入)
            let sz = core::mem::size_of::<u8>();
            let data = data as u8;
            if pa_copyto_uva(&data as *const u8, sz, id as *const u8, sz) {
                0
            } else {
                -1
            }
        }
        2 => task_get_syscall_count(id),
        _ => -1,
    }
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
