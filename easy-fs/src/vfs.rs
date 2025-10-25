use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ,
};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};
/// Virtual filesystem layer over easy-fs
pub struct Inode {
    block_id: usize,
    block_offset: usize,
    inode_id: u32,
    fs: Arc<Mutex<EasyFileSystem>>,
    block_device: Arc<dyn BlockDevice>,
}

impl Inode {
    /// Create a vfs inode
    pub fn new(
        block_id: u32,
        block_offset: usize,
        inode_id: u32, // DirEntry中inode_id就是u32保存的，尽管外界会转为u64
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
    ) -> Self {
        Self {
            block_id: block_id as usize,
            block_offset,
            inode_id,
            fs,
            block_device,
        }
    }

    /// Call a function over a disk inode to read it
    /// 根据self的block_id/block_offset读取内容*并转换为DiskInode*，然后就能用f对DiskInode进行各种读操作了，经过f作用后返回V
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f) // 隐式推断出read要读取类型是DiskInode(也即f的参数类型)
    }
    /// Call a function over a disk inode to modify it
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// Find inode under a disk inode by name  在DiskInode这个目录中找名为name的文件，这个DiskInode对应文件中数据都是DirEntry数组(rCore中只有根目录DiskInode)
    fn find_inode_id(&self, name: &str, disk_inode: &DiskInode) -> Option<u32> {
        // assert it is a directory  只有根目录root_node才能调用find/find_inode_id(efs只有根目录一个目录)
        assert!(disk_inode.is_dir());
        let file_count = (disk_inode.size as usize) / DIRENT_SZ;
        let mut dirent = DirEntry::empty();
        for i in 0..file_count {
            assert_eq!(
                // disk_inode的read_at是读写DiskInode代表的文件内容
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                DIRENT_SZ,
            );
            if dirent.name() == name {
                return Some(dirent.inode_id() as u32);
            }
        }
        None
    }
    /// Find inode under current inode by name  
    pub fn find(&self, name: &str) -> Option<Arc<Inode>> {
        let fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            self.find_inode_id(name, disk_inode).map(|inode_id| {
                let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
                Arc::new(Self::new(
                    block_id,
                    block_offset,
                    inode_id,
                    self.fs.clone(),
                    self.block_device.clone(),
                ))
            })
        })
    }
    /// Increase the size of a DiskInode
    fn increase_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size < disk_inode.size {
            return;
        }
        let blocks_needed = disk_inode.blocks_num_needed(new_size);
        let mut v: Vec<u32> = Vec::new();
        for _ in 0..blocks_needed {
            v.push(fs.alloc_data());
        }
        disk_inode.increase_size(new_size, v, &self.block_device);
    }
    /// Decrease the size of a DiskInode 从末尾移除block
    fn decrease_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        _fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size >= disk_inode.size {
            return;
        }
        let _blocks_dealloc =
            DiskInode::total_blocks(disk_inode.size) - DiskInode::total_blocks(new_size);
        // 从末尾移除 - 其实不如搞个办法按顺序获取所有data blocks的block_id，然后移除末尾几个
        // TODO(scn): 为了简单起见，我暂时不回收data block - 模仿DiskInode::increase_size/clear_size都要命的长
        // 回收的时候，还要注意一级/二级间接索引块的回收！
        disk_inode.size = new_size;
    }
    /// Create inode under current inode by name 只有根目录root_node才能调用create(efs只有根目录一个目录)
    pub fn create(&self, name: &str) -> Option<Arc<Inode>> {
        let mut fs = self.fs.lock();
        let op = |root_inode: &DiskInode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created? 如果已经有同名文件
            self.find_inode_id(name, root_inode)
        };
        if self.read_disk_inode(op).is_some() {
            return None;
        }
        // create a new file
        // alloc a inode with an indirect block
        let new_inode_id = fs.alloc_inode();
        // initialize inode
        let (new_inode_block_id, new_inode_block_offset) = fs.get_disk_inode_pos(new_inode_id);
        get_block_cache(new_inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(new_inode_block_offset, |new_inode: &mut DiskInode| {
                new_inode.initialize(DiskInodeType::File, 1); // 文件或目录创建的时候，Inode的引用计数都为1
            });
        self.modify_disk_inode(|root_inode| {
            // append file in the dirent
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            // increase size
            self.increase_size(new_size as u32, root_inode, &mut fs);
            // write dirent
            let dirent = DirEntry::new(name, new_inode_id);
            root_inode.write_at(
                file_count * DIRENT_SZ,
                dirent.as_bytes(),
                &self.block_device,
            );
        });

        // 这个之前new_inode_block_id/new_inode_block_offset不是已经获取过了？
        let (block_id, block_offset) = fs.get_disk_inode_pos(new_inode_id);
        block_cache_sync_all();
        // return inode
        Some(Arc::new(Self::new(
            block_id,
            block_offset,
            new_inode_id,
            self.fs.clone(),
            self.block_device.clone(),
        )))
        // release efs lock automatically by compiler
    }

    /// 获取文件的硬链接数量
    pub fn nlinks(&self) -> u32 {
        let _fs = self.fs.lock(); // 是不是外界调用，fs就得上锁？
        self.read_disk_inode(|disk_inode| disk_inode.nlink)
    }

    /// 创建硬链接 - 注意，这个函数也只能根目录root_node调用
    pub fn link_at(&self, old_name: &str, new_name: &str) -> isize {
        // 上层已经确认old_name!=new_name，还要确认old_name存在，且new_name不存在
        let mut fs = self.fs.lock();

        // step1: 确认old_name文件存在，且new_name文件不存在，否则直接返回
        let (old_inode_id, new_inode_id) = self.read_disk_inode(|root_inode| {
            (
                self.find_inode_id(old_name, root_inode),
                self.find_inode_id(new_name, root_inode),
            )
        });
        if old_inode_id.is_none() || new_inode_id.is_some() {
            return -1;
        }

        // 其实step1/step2跟create很像的，要修改DiskInode(create是新建个DiskInode然后初始化)，要在root_inode之后加个DirEntry

        // step2: 找到旧文件的DiskInode并nlink+=1，然后写回
        let (old_inode_block_id, old_inode_block_offset) =
            fs.get_disk_inode_pos(old_inode_id.unwrap());
        // 获取旧文件DiskInode所在块
        get_block_cache(old_inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(old_inode_block_offset, |disk_inode: &mut DiskInode| {
                disk_inode.inc_nlink(); // 这个modify其实把OS Cache中的DiskInode缓存修改了，之后写回
            });

        // step3: 在root_inode根目录下创建新文件的DirEntry并且写入文件名和inode_id，但是不分配新的inode
        self.modify_disk_inode(|root_inode| {
            // append file in the dirent 学习Inode::create中在root_inode末尾加个DirEntry
            let de_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (de_count + 1) * DIRENT_SZ;
            // increase_size
            self.increase_size(new_size as u32, root_inode, &mut fs);
            // write dirent
            let dirent = DirEntry::new(new_name, old_inode_id.unwrap()); // 共享底层的Inode/DiskInode
            root_inode.write_at(de_count * DIRENT_SZ, dirent.as_bytes(), &self.block_device);
        });

        block_cache_sync_all();
        0
    }

    /// 删除硬链接 - 注意，这个函数也只能根目录root_inode调用
    /// 几个操作应该一气呵成，不能分多步，不然的话中间可能被其他文件操作打断
    pub fn unlink_at(&self, name: &str) -> isize {
        let mut fs = self.fs.lock();

        // step1: 确认name文件存在，不存在直接返回 - 如果存在，还需要返回是第几个DirEntry - 模仿find_inode_id写法
        let (inode_id, dirent_idx) = self.read_disk_inode(|root_inode| {
            assert!(root_inode.is_dir());
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let mut dirent = DirEntry::empty();
            for i in 0..file_count {
                assert_eq!(
                    root_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device),
                    DIRENT_SZ
                );

                if dirent.name() == name {
                    return (Some(dirent.inode_id() as u32), i);
                }
            }
            (None, 0)
        });
        if inode_id.is_none() {
            return -1;
        }

        // step2: 找到DiskInode并nlink-=1，然后写回
        let inode_id = inode_id.unwrap();
        let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
        // 获取文件DiskInode所在block
        let nlink = get_block_cache(block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(block_offset, |disk_inode: &mut DiskInode| {
                // disk_inode就是name对应文件的DiskInode
                if disk_inode.dec_nlink() == 0 {
                    // step3: 利用DiskInode删除文件 - 文件的data blocks回收，data block bitmap也顺便回收；inode bitmap回收(inode entry也即DiskInode应该不用清零，毕竟重新使用时会清零)
                    // 模仿Inode::clear()，只不过clear会上锁
                    let size = disk_inode.size;
                    let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
                    assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
                    for data_block_id in data_blocks_dealloc.into_iter() {
                        fs.dealloc_data(data_block_id); // data_block_id对应block回收，对应data bitmap也回收
                    }
                    // step4: 删除inode_id对应DiskInode(inode entry)这项
                    fs.dealloc_inode(inode_id);
                }
                disk_inode.nlink
            });

        if nlink == 0 {
            // step5: 删除文件在目录中的目录项DirEntry - DirEntry在根目录下顺序排列 用最后一条DirEntry覆盖name对应DirEntry，然后decrease_size(过程中可能回收几个block)
            self.modify_disk_inode(|root_inode| {
                // 学习Inode::create中在末尾加个DirEntry，改成覆盖
                let file_count = (root_inode.size as usize) / DIRENT_SZ;
                let new_size = (file_count - 1) * DIRENT_SZ;

                // 把[new_size, size]位置的DirEntry内容读出来
                let mut last_dirent = DirEntry::empty();
                assert_eq!(
                    root_inode.read_at(new_size, last_dirent.as_bytes_mut(), &self.block_device),
                    DIRENT_SZ
                );
                root_inode.write_at(
                    DIRENT_SZ * dirent_idx,
                    last_dirent.as_bytes(),
                    &self.block_device,
                );

                // 减少size大小，可能要回收block - 模仿increase_size写吧
                self.decrease_size(new_size as u32, root_inode, &mut fs);
            })
        }
        block_cache_sync_all();
        0
    }

    /// List inodes under current inode 应该也要确保只有目录才能调用ls吧，assert!(disk_inode.is_dir());
    pub fn ls(&self) -> Vec<String> {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut v: Vec<String> = Vec::new();
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                v.push(String::from(dirent.name()));
            }
            v
        })
    }
    /// Read data from current inode
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write data to current inode  注意Inode::write_at会自动sync同步，但DiskInode::write_at不会主动同步
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs);
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        block_cache_sync_all();
        size
    }
    /// Clear the data in current inode
    pub fn clear(&self) {
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            let size = disk_inode.size;
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
            for data_block in data_blocks_dealloc.into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        block_cache_sync_all();
    }

    /// 返回硬链接数量/是否是目录/inode_id
    pub fn stat(&self) -> (u32, bool, u64) {
        // 注意给外界调用的要上锁，哪怕是只读！
        let mut _fs = self.fs.lock();

        self.read_disk_inode(|disk_inode| {
            (disk_inode.nlink, disk_inode.is_dir(), self.inode_id as u64)
        })
    }
}

/*
TODO(scn): 我发现对Inode的读写都是传入&self不可变引用
但里面依然自由修改fs: Arc<Mutex<EasyFileSystem>>指向的文件系统
这点之后要好好搞清楚，也就是说能阻止直接修改Inode的各个成员，但可以调用fs的可变引用函数来修改fs本身？
确实！甚至create能self.fs.lock()生成一个可变的fs，之后fs.alloc_inode()也是会修改fs的
不过这些其实是Rust语言问题
*/
