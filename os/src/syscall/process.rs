//! Process management syscalls
//!
use alloc::sync::Arc;

use crate::{
    fs::{open_file, OpenFlags},
    mm::{
        check_not_mapped, pa_copyto_uva, translated_refmut, translated_str, VirtAddr, VirtPageNum,
    },
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next,
        suspend_current_and_run_next, task_mmap, task_munmap,
    },
    timer::get_time_us,
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

pub fn sys_getpid() -> isize {
    trace!("kernel: sys_getpid pid:{}", current_task().unwrap().pid.0);
    current_task().unwrap().pid.0 as isize
}

pub fn sys_fork() -> isize {
    trace!("kernel:pid[{}] sys_fork", current_task().unwrap().pid.0);
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    // 要让子进程中fork返回0就这么简单
    // 注意子进程和父进程状态完全相同，并且子进程是模拟新进程那样，TaskContext用goto_trap_return，TrapContext就完全是父进程复制来的，所以回到用户态的状态(内核栈是新分配的，用户栈完全复制一样的，所以从syscall的下一个命令返回 - 因为sepc已经+=4)
    // 所以子进程下一次调度回去时，也是从一个syscall的trap返回，并且返回值是下边设置的x[10](a0)=0
    trap_cx.x[10] = 0; // TODO(scn): 这个为啥不直接在task::fork()中做了
                       // add new task to scheduler
                       // 这样直接把子进程放到队尾，按照目前FIFO调度设计，子进程必然晚于父进程执行啊（除非父进程调用yield）
    add_task(new_task);
    new_pid as isize
}

pub fn sys_exec(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        task.exec(all_data.as_slice());
        0
    } else {
        -1
    }
}

/// YOUR JOB: Implement spawn.
/// HINT: fork + exec =/= spawn
pub fn sys_spawn(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_spawn", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(elf_data) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let new_task = current_task()
            .unwrap()
            .spawn(elf_data.read_all().as_slice());
        let new_pid = new_task.pid.0;
        add_task(new_task);
        new_pid as isize
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    //trace!("kernel: sys_waitpid");
    let task = current_task().unwrap();
    // find a child process

    // ---- access current PCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after being removed from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child PCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        // exit_code_ptr是个用户态虚地址，写入的话需要进行转换
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB automatically
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel:pid[{}] sys_get_time", current_task().unwrap().pid.0);

    let us = get_time_us();

    // 目标就是把内核态的tv拷贝到用户态的ts虚地址（把ts虚地址转为物理地址上字节序列，然后就能直接复制了）
    let tv = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };

    let sz = core::mem::size_of::<TimeVal>();
    if pa_copyto_uva(current_user_token(), &tv as *const TimeVal as *const u8, sz, ts as *const u8, sz) {
        0
    } else {
        -1
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
    trace!("kernel:pid[{}] sys_mmap", current_task().unwrap().pid.0);

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
    if !check_not_mapped(current_user_token(), start_vpn, end_vpn) {
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
    trace!("kernel:pid[{}] sys_munmap", current_task().unwrap().pid.0);

    let start_va: VirtAddr = start.into();
    // 检查start_va是否对齐
    if !start_va.aligned() {
        return -1;
    }

    // task_munmap会检查之前是否存在这个映射(通过查找MapArea)，如果存在就删除(页表相关也删除)，不存在对应MapArea就算出错
    let end_va: VirtAddr = (start + len).into();
    if task_munmap(start_va, end_va) {
        0
    } else {
        -1
    }
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().pid.0);
    if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

// YOUR JOB: Set task priority.
pub fn sys_set_priority(_prio: isize) -> isize {
    trace!(
        "kernel:pid[{}] sys_set_priority NOT IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    -1
}
