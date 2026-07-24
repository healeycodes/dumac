use fsevent_stream::ffi::{
    kFSEventStreamCreateFlagFileEvents, kFSEventStreamCreateFlagNoDefer, FSEventsGetCurrentEventId,
};
use fsevent_stream::flags::StreamFlags;
use fsevent_stream::stream::create_event_stream;
use futures_util::StreamExt;
use parking_lot::Mutex;
use rayon::prelude::*;
use std::collections::HashSet;
use std::env;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

// macOS-specific constants not in libc crate
const ATTR_CMN_ERROR: u32 = 0x20000000;
const VNON: u32 = 0;
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

// Max file handles open
const MAX_FILE_HANDLES: usize = 224;
const DEFAULT_THREADS: usize = 6;

// Sharded inode tracking
const SHARD_COUNT: usize = 32;

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

// Clear all seen inodes before a new scan.
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

#[derive(Debug)]
struct CacheEntry {
    blocks: i64,
    event_id: u64,
}

fn cache_file(root: &Path) -> std::path::PathBuf {
    let hash = root
        .as_os_str()
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    env::temp_dir()
        .join("dumac-cache")
        .join(format!("{hash:016x}"))
}

fn read_cache(root: &Path) -> Option<CacheEntry> {
    let contents = fs::read_to_string(cache_file(root)).ok()?;
    let mut fields = contents.lines();
    let event_id = fields.next()?.parse().ok()?;
    let blocks = fields.next()?.parse().ok()?;
    let cached_root = fields.next()?.to_owned();
    (cached_root == root.to_string_lossy()).then_some(CacheEntry { blocks, event_id })
}

fn write_cache(root: &Path, blocks: i64, event_id: u64) {
    let file = cache_file(root);
    if let Some(parent) = file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(
        file,
        format!("{event_id}\n{blocks}\n{}\n", root.to_string_lossy()),
    );
}

async fn changed_since(root: &Path, event_id: u64) -> bool {
    let flags = kFSEventStreamCreateFlagNoDefer | kFSEventStreamCreateFlagFileEvents;
    let Ok((stream, mut handler)) = create_event_stream([root], event_id, Duration::ZERO, flags)
    else {
        return true;
    };
    let mut events = stream.into_flatten();
    let changed = match tokio::time::timeout(Duration::from_secs(2), events.next()).await {
        Ok(Some(event)) if event.flags.contains(StreamFlags::HISTORY_DONE) => false,
        Ok(Some(_)) | Ok(None) | Err(_) => true,
    };
    handler.abort();
    changed
}

fn cache_changed(root: &Path, event_id: u64) -> bool {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_or(true, |runtime| {
            runtime.block_on(changed_since(root, event_id))
        })
}

fn calculate_with_pool(pool: &rayon::ThreadPool, root: String) -> Result<i64, String> {
    clear_seen_inodes();
    pool.install(|| calculate_size(root))
}

fn cached_or_calculate(pool: &rayon::ThreadPool, root: &Path) -> Result<(i64, bool), String> {
    if let Some(entry) = read_cache(root) {
        if !cache_changed(root, entry.event_id) {
            return Ok((entry.blocks, true));
        }
    }

    for _ in 0..2 {
        let event_id = unsafe { FSEventsGetCurrentEventId() };
        let blocks = calculate_with_pool(pool, root.to_string_lossy().to_string())?;
        if !cache_changed(root, event_id) {
            write_cache(root, blocks, event_id);
            return Ok((blocks, false));
        }
    }

    calculate_with_pool(pool, root.to_string_lossy().to_string()).map(|blocks| (blocks, false))
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let (cache_mode, root_dir) = match args.as_slice() {
        [_program, root_dir] => (false, root_dir),
        [program, flag, root_dir] if flag == "--cache" => (true, root_dir),
        [program, ..] => {
            eprintln!("usage: {program} [--cache] directory");
            std::process::exit(1);
        }
        [] => unreachable!(),
    };

    if root_dir.is_empty() {
        eprintln!("usage: {} directory", args[0]);
        std::process::exit(1);
    }

    let available_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        // Limit to MAX_FILE_HANDLES to avoid file handle limit errors
        .min(MAX_FILE_HANDLES);
    let thread_count = env::var("DUMAC_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&value| value > 0)
        .map(|value| value.min(MAX_FILE_HANDLES))
        .unwrap_or_else(|| available_threads.min(DEFAULT_THREADS));
    let pool = rayon::ThreadPoolBuilder::new()
        // Larger than default stack size to handle deep directory trees
        .stack_size(16 * 1024 * 1024)
        .num_threads(thread_count)
        .build()
        .expect("Failed to build thread pool");

    let result = if cache_mode {
        fs::canonicalize(root_dir)
            .map_err(|_| format!("{}: Cannot access directory", root_dir))
            .and_then(|root| cached_or_calculate(&pool, &root).map(|(blocks, _)| blocks))
    } else {
        calculate_with_pool(&pool, root_dir.clone())
    };

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

    // Process files in this directory, deduplicating by inode
    let total_size: i64 = dir_info
        .files
        .iter()
        .map(|file| check_and_add_inode(file.inode, file.blocks))
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
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
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
                    VREG if returned_attrs.fileattr & libc::ATTR_FILE_ALLOCSIZE != 0 => {
                        // Regular file - get allocation size
                        let alloc_size = std::ptr::read_unaligned(field_ptr as *const i64);
                        files.push(FileInfo {
                            blocks: blocks_from_bytes(alloc_size),
                            inode,
                        });
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
