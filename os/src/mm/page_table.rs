//! Implementation of [`PageTableEntry`] and [`PageTable`].
use super::{
    frame_alloc, FrameTracker, PhysAddr, PhysPageNum, StepByOne, VPNRange, VirtAddr, VirtPageNum,
};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use bitflags::*;

bitflags! {
    /// page table entry flags
    pub struct PTEFlags: u8 {
        const V = 1 << 0;
        const R = 1 << 1;
        const W = 1 << 2;
        const X = 1 << 3;
        const U = 1 << 4;
        const G = 1 << 5;
        const A = 1 << 6;
        const D = 1 << 7;
    }
}

#[derive(Copy, Clone)]
#[repr(C)]
/// page table entry structure
pub struct PageTableEntry {
    /// bits of page table entry
    pub bits: usize,
}

impl PageTableEntry {
    /// Create a new page table entry
    pub fn new(ppn: PhysPageNum, flags: PTEFlags) -> Self {
        PageTableEntry {
            bits: ppn.0 << 10 | flags.bits as usize,
        }
    }
    /// Create an empty page table entry
    pub fn empty() -> Self {
        PageTableEntry { bits: 0 }
    }
    /// Get the physical page number from the page table entry
    pub fn ppn(&self) -> PhysPageNum {
        (self.bits >> 10 & ((1usize << 44) - 1)).into()
    }
    /// Get the flags from the page table entry
    pub fn flags(&self) -> PTEFlags {
        PTEFlags::from_bits(self.bits as u8).unwrap()
    }
    /// The page pointered by page table entry is valid?
    pub fn is_valid(&self) -> bool {
        (self.flags() & PTEFlags::V) != PTEFlags::empty()
    }
    /// The page pointered by page table entry is user accessible?
    pub fn is_user_accessible(&self) -> bool {
        (self.flags() & PTEFlags::U) != PTEFlags::empty()
    }
    /// The page pointered by page table entry is readable?
    pub fn readable(&self) -> bool {
        (self.flags() & PTEFlags::R) != PTEFlags::empty()
    }
    /// The page pointered by page table entry is writable?
    pub fn writable(&self) -> bool {
        (self.flags() & PTEFlags::W) != PTEFlags::empty()
    }
    /// The page pointered by page table entry is executable?
    pub fn executable(&self) -> bool {
        (self.flags() & PTEFlags::X) != PTEFlags::empty()
    }
}

/// page table structure
pub struct PageTable {
    root_ppn: PhysPageNum,
    frames: Vec<FrameTracker>,
}

/// Assume that it won't oom when creating/mapping.
impl PageTable {
    /// Create a new page table
    pub fn new() -> Self {
        let frame = frame_alloc().unwrap();
        PageTable {
            root_ppn: frame.ppn,
            frames: vec![frame],
        }
    }
    /// Temporarily used to get arguments from user space.
    pub fn from_token(satp: usize) -> Self {
        Self {
            root_ppn: PhysPageNum::from(satp & ((1usize << 44) - 1)),
            frames: Vec::new(),
        }
    }
    /// Find PageTableEntry by VirtPageNum, create a frame for a 4KB page table if not exist
    fn find_pte_create(&mut self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            if i == 2 {
                result = Some(pte);
                break;
            }
            if !pte.is_valid() {
                let frame = frame_alloc().unwrap();
                *pte = PageTableEntry::new(frame.ppn, PTEFlags::V);
                self.frames.push(frame);
            }
            ppn = pte.ppn();
        }
        result
    }
    /// Find PageTableEntry by VirtPageNum
    fn find_pte(&self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            if i == 2 {
                result = Some(pte);
                break;
            }
            if !pte.is_valid() {
                return None;
            }
            ppn = pte.ppn();
        }
        result
    }
    /// set the map between virtual page number and physical page number
    #[allow(unused)]
    pub fn map(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: PTEFlags) {
        let pte = self.find_pte_create(vpn).unwrap();
        assert!(!pte.is_valid(), "vpn {:?} is mapped before mapping", vpn);
        *pte = PageTableEntry::new(ppn, flags | PTEFlags::V);
    }
    /// remove the map between virtual page number and physical page number
    #[allow(unused)]
    pub fn unmap(&mut self, vpn: VirtPageNum) {
        let pte = self.find_pte(vpn).unwrap();
        assert!(pte.is_valid(), "vpn {:?} is invalid before unmapping", vpn);
        *pte = PageTableEntry::empty();
    }
    /// get the page table entry from the virtual page number
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PageTableEntry> {
        self.find_pte(vpn).map(|pte| *pte)
    }
    /// get the physical address from the virtual address
    pub fn translate_va(&self, va: VirtAddr) -> Option<PhysAddr> {
        self.find_pte(va.clone().floor()).map(|pte| {
            let aligned_pa: PhysAddr = pte.ppn().into();
            let offset = va.page_offset();
            let aligned_pa_usize: usize = aligned_pa.into();
            (aligned_pa_usize + offset).into()
        })
    }
    /// get the token from the page table
    pub fn token(&self) -> usize {
        8usize << 60 | self.root_ppn.0
    }

    /// 检查vpn页面是否已映射
    pub fn check_mapped_one(&self, vpn: VirtPageNum) -> bool {
        // 最后一级页表是not valid，要么就是没映射，还要就是映射了但已经被换出（总归当前PTE无效）
        if let Some(pte) = self.translate(vpn) {
            pte.is_valid()
        } else {
            false
        }
    }
}

/// Translate&Copy a ptr[u8] array with LENGTH len to a mutable u8 Vec through page table
pub fn translated_byte_buffer(token: usize, ptr: *const u8, len: usize) -> Vec<&'static mut [u8]> {
    let page_table = PageTable::from_token(token);
    let mut start = ptr as usize;
    let end = start + len;
    let mut v = Vec::new();
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.floor();
        let ppn = page_table.translate(vpn).unwrap().ppn();
        vpn.step();
        let mut end_va: VirtAddr = vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        if end_va.page_offset() == 0 {
            v.push(&mut ppn.get_bytes_array()[start_va.page_offset()..]);
        } else {
            v.push(&mut ppn.get_bytes_array()[start_va.page_offset()..end_va.page_offset()]);
        }
        start = end_va.into();
    }
    v
}

/// Translate&Copy a ptr[u8] array end with `\0` to a `String` Vec through page table
pub fn translated_str(token: usize, ptr: *const u8) -> String {
    let page_table = PageTable::from_token(token);
    let mut string = String::new();
    let mut va = ptr as usize;
    loop {
        let ch: u8 = *(page_table
            .translate_va(VirtAddr::from(va))
            .unwrap()
            .get_mut());
        if ch == 0 {
            break;
        }
        string.push(ch as char);
        va += 1;
    }
    string
}

#[allow(unused)]
/// Translate a ptr[u8] array through page table and return a reference of T
pub fn translated_ref<T>(token: usize, ptr: *const T) -> &'static T {
    let page_table = PageTable::from_token(token);
    page_table
        .translate_va(VirtAddr::from(ptr as usize))
        .unwrap()
        .get_ref()
}

// 这个函数似乎没考虑跨物理页的问题啊？是默认T结构一定在同一个物理页中？ => 确实有，要确保T是在同一个物理页内
/// Translate a ptr[u8] array through page table and return a mutable reference of T
pub fn translated_refmut<T>(token: usize, ptr: *mut T) -> &'static mut T {
    let page_table = PageTable::from_token(token);
    let va = ptr as usize;
    page_table
        .translate_va(VirtAddr::from(va))
        .unwrap()
        .get_mut()
}

/// An abstraction over a buffer passed from user space to kernel space
pub struct UserBuffer {
    /// A list of buffers
    pub buffers: Vec<&'static mut [u8]>,
}

impl UserBuffer {
    /// Constuct UserBuffer
    pub fn new(buffers: Vec<&'static mut [u8]>) -> Self {
        Self { buffers }
    }
    /// Get the length of the buffer
    pub fn len(&self) -> usize {
        let mut total: usize = 0;
        for b in self.buffers.iter() {
            total += b.len();
        }
        total
    }
}

impl IntoIterator for UserBuffer {
    type Item = *mut u8;
    type IntoIter = UserBufferIterator;
    fn into_iter(self) -> Self::IntoIter {
        UserBufferIterator {
            buffers: self.buffers,
            current_buffer: 0,
            current_idx: 0,
        }
    }
}

/// An iterator over a UserBuffer
pub struct UserBufferIterator {
    buffers: Vec<&'static mut [u8]>,
    current_buffer: usize,
    current_idx: usize,
}

impl Iterator for UserBufferIterator {
    type Item = *mut u8;
    fn next(&mut self) -> Option<Self::Item> {
        if self.current_buffer >= self.buffers.len() {
            None
        } else {
            let r = &mut self.buffers[self.current_buffer][self.current_idx] as *mut _;
            if self.current_idx + 1 == self.buffers[self.current_buffer].len() {
                self.current_idx = 0;
                self.current_buffer += 1;
            } else {
                self.current_idx += 1;
            }
            Some(r)
        }
    }
}

/// 检查start_vpn~end_vpn范围内所有页面都没有被映射（用于mmap前检查）
pub fn check_not_mapped(token: usize, start_vpn: VirtPageNum, end_vpn: VirtPageNum) -> bool {
    let page_table = PageTable::from_token(token);
    let vpn_range = VPNRange::new(start_vpn, end_vpn);
    // TODO(scn): 改成函数式写法
    for vpn in vpn_range {
        // 如果vpn已经被映射 - 说明有问题
        if page_table.check_mapped_one(vpn) {
            return false;
        }
    }
    true
}

// ------------ 以下是一些内核态读写用户态虚地址的方法

/// 传入用户态虚地址ptr（并要检查是否有写权限），未来将写入数据
pub fn translated_ua2write(
    token: usize,
    ptr: *const u8,
    len: usize,
) -> Option<Vec<&'static mut [u8]>> {
    let page_table = PageTable::from_token(token);
    let mut start = ptr as usize;
    let end = start + len;
    let mut v = Vec::new();
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.floor();
        if let Some(pte) = page_table.translate(vpn) {
            // 检查pte的valid/user_accessible/writable
            if pte.is_valid() && pte.is_user_accessible() && pte.writable() {
                let ppn = pte.ppn();
                vpn.step();
                let mut end_va: VirtAddr = vpn.into();
                end_va = end_va.min(VirtAddr::from(end));
                if end_va.page_offset() == 0 {
                    v.push(&mut ppn.get_bytes_array()[start_va.page_offset()..]);
                } else {
                    v.push(
                        &mut ppn.get_bytes_array()[start_va.page_offset()..end_va.page_offset()],
                    );
                }
                start = end_va.into();
            } else {
                return None;
            }
        } else {
            return None;
        }
    }
    Some(v)
}

/// 传入用户态虚地址ptr（并检查是否有读权限），未来将读取数据
pub fn translated_ua2read(token: usize, ptr: *const u8, len: usize) -> Option<Vec<&'static [u8]>> {
    let page_table = PageTable::from_token(token);
    let mut start = ptr as usize;
    let end = start + len;
    let mut v = Vec::new();
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.floor();
        if let Some(pte) = page_table.translate(vpn) {
            // 检查pte的valid/user_accessible/readable
            if pte.is_valid() && pte.is_user_accessible() && pte.readable() {
                let ppn = pte.ppn();
                vpn.step();
                let mut end_va: VirtAddr = vpn.into();
                end_va = end_va.min(VirtAddr::from(end));
                if end_va.page_offset() == 0 {
                    v.push(&ppn.get_bytes_array()[start_va.page_offset()..]);
                } else {
                    v.push(&ppn.get_bytes_array()[start_va.page_offset()..end_va.page_offset()]);
                }
                start = end_va.into();
            } else {
                return None;
            }
        } else {
            return None;
        }
    }
    Some(v)
}

/// 物理地址的src 拷贝数据到 用户态虚地址的dst - 内核态数据复制到用户空间虚地址，注意要检查虚地址是否有写权限
pub fn pa_copyto_uva(
    token: usize,
    src: *const u8,
    src_len: usize,
    dst: *const u8,
    dst_len: usize,
) -> bool {
    if let Some(dsts) = translated_ua2write(token, dst, dst_len) {
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
pub fn uva_copyto_pa(
    token: usize,
    src: *const u8,
    src_len: usize,
    dst: *mut u8,
    dst_len: usize,
) -> bool {
    if let Some(srcs) = translated_ua2read(token, src, src_len) {
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
