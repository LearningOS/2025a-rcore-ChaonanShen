//! Semaphore

use crate::sync::UPSafeCell;
use crate::syscall::is_deadlock_detect_enabled;
use crate::task::{block_current_and_run_next, current_task, wakeup_task, TaskControlBlock};
use alloc::{collections::VecDeque, sync::Arc};

/// semaphore structure
pub struct Semaphore {
    /// semaphore inner
    pub inner: UPSafeCell<SemaphoreInner>,
}

pub struct SemaphoreInner {
    pub count: isize,
    pub wait_queue: VecDeque<Arc<TaskControlBlock>>,
}

impl Semaphore {
    /// Create a new semaphore
    pub fn new(res_count: usize) -> Self {
        trace!("kernel: Semaphore::new");
        Self {
            inner: unsafe {
                UPSafeCell::new(SemaphoreInner {
                    count: res_count as isize,
                    wait_queue: VecDeque::new(),
                })
            },
        }
    }

    /// up operation of semaphore
    pub fn up(&self) {
        trace!("kernel: Semaphore::up");
        let mut inner = self.inner.exclusive_access();
        inner.count += 1;
        if inner.count <= 0 {
            if let Some(task) = inner.wait_queue.pop_front() {
                wakeup_task(task); // 这些wakeup_task和block/suspend_and_run_next都是和TaskManager合作，死锁检测下也可以和TaskManager合作
            }
        }
    }

    /// down operation of semaphore
    pub fn down(&self) -> isize {
        trace!("kernel: Semaphore::down");
        let mut inner = self.inner.exclusive_access();
        inner.count -= 1;
        if inner.count < 0 {
            inner.wait_queue.push_back(current_task().unwrap());
            drop(inner);
            block_current_and_run_next();

            // 正常的wakeup_task后依然继续获取锁执行
            // 但发现某一时候(目前在waittid时检测)所有线程都blocked
            if is_deadlock_detect_enabled()
                && current_task()
                    .unwrap()
                    .inner_exclusive_access()
                    .found_deadlock
            {
                let mut inner = self.inner.exclusive_access();
                inner.count += 1; // 被wakeup_deadlock_task唤醒的进程，已经不再semaphore的等待队列之中了
                return -0xdead;
            }
        }
        0
    }
}
