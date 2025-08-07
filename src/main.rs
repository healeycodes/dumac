use parking_lot::Mutex;
use rayon::prelude::*;
use std::collections::HashSet;
use std::env;
use std::ffi::CString;
use std::path::Path;
use std::sync::LazyLock;

// macOS-specific constants not in libc crate
const ATTR_CMN_ERROR: u32 = 0x20000000;
const VNON: u32 = 0;
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

// Max file handles open
const MAX_FILE_HANDLES: usize = 224;

// Sharded inode tracking
const SHARD_COUNT: usize = 128;

// File information for size calculation
#[derive(Debug)]
struct FileInfo {
    blocks: i64,
    inode: u64,
}

// Directory contents
#[derive(Debug)]
struct DirInfo {
    files: Vec<FileInfo>,
    subdirs: Vec<String>,
}

// Global sharded inode set for hardlink deduplication
static SEEN_INODES: LazyLock<[Mutex<HashSet<u64>>; SHARD_COUNT]> =
    LazyLock::new(|| std::array::from_fn(|_| Mutex::new(HashSet::new())));

fn shard_for_inode(inode: u64) -> usize {
    ((inode >> 8) % SHARD_COUNT as u64) as usize
}

// Clear all seen inodes (for testing)
#[cfg(test)]
pub fn clear_seen_inodes() {
    for shard in SEEN_INODES.iter() {
        shard.lock().clear();
    }
}

// Returns the blocks to add (blocks if newly seen, 0 if already seen)
fn check_and_add_inode(inode: u64, blocks: i64) -> i64 {
    let shard_idx = shard_for_inode(inode);
    let mut seen = SEEN_INODES[shard_idx].lock();
    if seen.insert(inode) {
        blocks // Inode was newly added, count the blocks
    } else {
        0 // Inode already seen, don't count
    }
}

// Convert bytes to 512-byte blocks (du default)
fn blocks_from_bytes(bytes: i64) -> i64 {
    (bytes + 511) / 512
}

// Convert blocks to human readable format (du -h style)
fn format_size(blocks: i64) -> String {
    let bytes = blocks * 512;

    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        let kb = bytes as f64 / 1024.0;
        if kb.fract() == 0.0 {
            format!("{}K", kb as i64)
        } else {
            format!("{:.1}K", kb)
        }
    } else if bytes < 1024 * 1024 * 1024 {
        let mb = bytes as f64 / (1024.0 * 1024.0);
        if mb.fract() == 0.0 {
            format!("{}M", mb as i64)
        } else {
            format!("{:.1}M", mb)
        }
    } else if bytes < 1024_i64.pow(4) {
        let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        if gb.fract() == 0.0 {
            format!("{}G", gb as i64)
        } else {
            format!("{:.1}G", gb)
        }
    } else {
        let tb = bytes as f64 / (1024.0 * 1024.0 * 1024.0 * 1024.0);
        if tb.fract() == 0.0 {
            format!("{}T", tb as i64)
        } else {
            format!("{:.1}T", tb)
        }
    }
}

fn is_dot_or_dotdot(filename: &str) -> bool {
    filename == "." || filename == ".."
}
fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("usage: {} directory", args[0]);
        std::process::exit(1);
    }

    let root_dir = &args[1];

    let max_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        // Limit to MAX_FILE_HANDLES to avoid file handle limit errors
        .min(MAX_FILE_HANDLES);
    let pool = rayon::ThreadPoolBuilder::new()
        // Larger than default stack size to handle deep directory trees
        .stack_size(16 * 1024 * 1024)
        .num_threads(max_threads)
        .build()
        .expect("Failed to build thread pool");

    let result = pool.install(|| calculate_size(root_dir.clone()));

    match result {
        Ok(total_blocks) => {
            println!("{}\t{}", format_size(total_blocks), root_dir);
        }
        Err(e) => {
            eprintln!("{}: {}", args[0], e);
            std::process::exit(1);
        }
    }
}

// Calculate total size recursively using rayon work stealing
pub fn calculate_size(root_dir: String) -> Result<i64, String> {
    // Get directory contents
    let dir_info = get_dir_info(&root_dir)?;

    let mut all_inodes = Vec::new();

    // Process files in this directory, deduplicating by inode
    let total_size: i64 = dir_info
        .files
        .iter()
        .map(|file| {
            let blocks = check_and_add_inode(file.inode, file.blocks);
            all_inodes.push(file.inode);
            blocks
        })
        .sum();

    // Process subdirectories in parallel
    let subdir_size = if !dir_info.subdirs.is_empty() {
        dir_info
            .subdirs
            .into_par_iter()
            .map(|subdir| {
                let subdir_path = Path::new(&root_dir)
                    .join(&subdir)
                    .to_string_lossy()
                    .to_string();
                calculate_size(subdir_path)
            })
            .map(|result| match result {
                Ok(size) => size,
                Err(e) => {
                    eprintln!("dumac: {}", e);
                    0
                }
            })
            .sum()
    } else {
        0
    };

    Ok(total_size + subdir_size)
}

fn get_dir_info(path: &str) -> Result<DirInfo, String> {
    // Open directory
    let c_path = CString::new(path).map_err(|_| format!("{}: Invalid path", path))?;
    let dirfd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY) };
    if dirfd == -1 {
        let errno = unsafe { *libc::__error() };
        let error_msg = match errno {
            libc::ENOENT => "No such file or directory",
            libc::EACCES => "Permission denied",
            libc::ENOTDIR => "Not a directory",
            _ => "Cannot access directory",
        };
        return Err(format!("{}: {}", path, error_msg));
    }

    // Set up attribute list for getattrlistbulk
    let mut attrlist = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT as u16,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS
            | libc::ATTR_CMN_NAME
            | ATTR_CMN_ERROR
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_FILEID,
        volattr: 0,
        dirattr: 0,
        fileattr: libc::ATTR_FILE_ALLOCSIZE,
        forkattr: 0,
    };

    let mut attrbuf = [0u8; 128 * 1024];
    let mut files = Vec::new();
    let mut subdirs = Vec::new();

    loop {
        let retcount = unsafe {
            libc::getattrlistbulk(
                dirfd,
                &mut attrlist as *mut libc::attrlist as *mut libc::c_void,
                attrbuf.as_mut_ptr() as *mut libc::c_void,
                attrbuf.len(),
                0,
            )
        };

        if retcount <= 0 {
            if retcount < 0 {
                let errno = unsafe { *libc::__error() };
                let error_msg = match errno {
                    libc::EACCES => "Permission denied",
                    libc::ENOENT => "No such file or directory",
                    _ => "Cannot read directory contents",
                };
                return Err(format!("{}: {}", path, error_msg));
            }
            break;
        }

        // Parse attribute buffer
        let mut entry_ptr = attrbuf.as_ptr();
        for _ in 0..retcount {
            unsafe {
                // Read entry length and advance to attribute data
                let entry_length = std::ptr::read_unaligned(entry_ptr as *const u32);
                let mut field_ptr = entry_ptr.add(std::mem::size_of::<u32>());

                // Read returned attributes bitmask
                let returned_attrs =
                    std::ptr::read_unaligned(field_ptr as *const libc::attribute_set_t);
                field_ptr = field_ptr.add(std::mem::size_of::<libc::attribute_set_t>());

                // Extract filename
                let mut filename: Option<String> = None;
                if returned_attrs.commonattr & libc::ATTR_CMN_NAME != 0 {
                    let name_start = field_ptr; // Save start of attrreference_t
                    let name_info =
                        std::ptr::read_unaligned(field_ptr as *const libc::attrreference_t);
                    field_ptr = field_ptr.add(std::mem::size_of::<libc::attrreference_t>());
                    let name_ptr = name_start.add(name_info.attr_dataoffset as usize);

                    if name_info.attr_length > 0 {
                        let name_slice = std::slice::from_raw_parts(
                            name_ptr,
                            (name_info.attr_length - 1) as usize,
                        );
                        if let Ok(name_str) = std::str::from_utf8(name_slice) {
                            if is_dot_or_dotdot(name_str) {
                                entry_ptr = entry_ptr.add(entry_length as usize);
                                continue;
                            }
                            filename = Some(name_str.to_string());
                        }
                    }
                }

                // Check for errors
                if returned_attrs.commonattr & ATTR_CMN_ERROR != 0 {
                    let error_code = std::ptr::read_unaligned(field_ptr as *const u32);
                    field_ptr = field_ptr.add(std::mem::size_of::<u32>());
                    if error_code != 0 {
                        if let Some(name) = &filename {
                            eprintln!("cannot access '{}/{}': error {}", path, name, error_code);
                        }
                        entry_ptr = entry_ptr.add(entry_length as usize);
                        continue;
                    }
                }

                // Get object type
                let obj_type = if returned_attrs.commonattr & libc::ATTR_CMN_OBJTYPE != 0 {
                    let obj_type = std::ptr::read_unaligned(field_ptr as *const u32);
                    field_ptr = field_ptr.add(std::mem::size_of::<u32>());
                    obj_type
                } else {
                    VNON
                };

                // Get inode
                let inode = if returned_attrs.commonattr & libc::ATTR_CMN_FILEID != 0 {
                    let inode = std::ptr::read_unaligned(field_ptr as *const u64);
                    field_ptr = field_ptr.add(std::mem::size_of::<u64>());
                    inode
                } else {
                    0
                };

                // Handle different file types
                match obj_type {
                    VREG => {
                        // Regular file - get allocation size
                        if returned_attrs.fileattr & libc::ATTR_FILE_ALLOCSIZE != 0 {
                            let alloc_size = std::ptr::read_unaligned(field_ptr as *const i64);
                            files.push(FileInfo {
                                blocks: blocks_from_bytes(alloc_size),
                                inode,
                            });
                        }
                    }
                    VDIR => {
                        // Directory - add to subdirectories list
                        if let Some(name) = filename {
                            subdirs.push(name);
                        }
                    }
                    VLNK => {
                        // Symlink - count the link itself as 1 (du default behavior)
                        files.push(FileInfo { blocks: 1, inode });
                    }
                    _ => {
                        // Other file types (devices, etc.) - treat as zero-size
                    }
                }

                // Move to next entry
                entry_ptr = entry_ptr.add(entry_length as usize);
            }
        }
    }

    // Close directory
    unsafe {
        libc::close(dirfd);
    }

    Ok(DirInfo { files, subdirs })
}
