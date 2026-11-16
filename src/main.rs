use parking_lot::Mutex;
use rayon::prelude::*;
use rustc_hash::FxHashSet;
use std::cell::RefCell;
use std::env;
use std::ffi::CString;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;

// macOS-specific constants not in libc crate
const ATTR_CMN_ERROR: u32 = 0x20000000;
const VNON: u32 = 0;
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;
const DIR_MNTSTATUS_TRIGGER: u32 = 0x2;

// Max file handles open
const MAX_FILE_HANDLES: usize = 224;

// Sharded inode tracking
const SHARD_COUNT: usize = 128;

// Size of the attribute buffer handed to getattrlistbulk
const ATTR_BUF_SIZE: usize = 128 * 1024;

// Directory contents
#[derive(Debug)]
struct DirInfo {
    // Blocks used by the files in this directory (hardlinks already deduplicated)
    blocks: i64,
    // Subdirectories to descend into
    subdirs: Vec<SubDir>,
}

#[derive(Debug)]
struct SubDir {
    // Full path
    path: String,
    // The entry is a mount point, so the filesystem underneath may differ
    mount: bool,
}

// Count each APFS clone group's shared blocks once instead of once per file
static DEDUPE_CLONES: AtomicBool = AtomicBool::new(false);

pub fn set_dedupe_clones(enabled: bool) {
    DEDUPE_CLONES.store(enabled, Ordering::Relaxed);
}

thread_local! {
    // Reused per-thread so we don't zero 128 KiB for every directory
    static ATTR_BUF: RefCell<Vec<u8>> = RefCell::new(vec![0u8; ATTR_BUF_SIZE]);
}

type ShardedSet = [Mutex<FxHashSet<u64>>; SHARD_COUNT];

fn new_sharded_set() -> ShardedSet {
    std::array::from_fn(|_| Mutex::new(FxHashSet::default()))
}

// Global sharded inode set for hardlink deduplication
static SEEN_INODES: LazyLock<ShardedSet> = LazyLock::new(new_sharded_set);

// Global sharded set of APFS clone ids whose shared blocks have been counted.
// Kept apart from inodes: a clone id is the inode of the original file.
static SEEN_CLONES: LazyLock<ShardedSet> = LazyLock::new(new_sharded_set);

fn shard_for_inode(inode: u64) -> usize {
    ((inode >> 8) % SHARD_COUNT as u64) as usize
}

// Clear all seen inodes (for testing)
#[cfg(test)]
pub fn clear_seen_inodes() {
    for shard in SEEN_INODES.iter().chain(SEEN_CLONES.iter()) {
        shard.lock().clear();
    }
}

// Returns true the first time a key is inserted
fn insert_new(set: &ShardedSet, key: u64) -> bool {
    set[shard_for_inode(key)].lock().insert(key)
}

// Returns the blocks to add (blocks if newly seen, 0 if already seen)
fn check_and_add_inode(inode: u64, blocks: i64) -> i64 {
    if insert_new(&SEEN_INODES, inode) {
        blocks // Inode was newly added, count the blocks
    } else {
        0 // Inode already seen, don't count
    }
}

// Blocks a file adds to the total. Hardlinks are counted once. With clone
// deduplication on, a file's private blocks always count, while the blocks it
// shares with its clones count once per clone group (an estimate when clones
// have diverged in different places).
fn file_blocks_to_add(inode: u64, nlink: u32, blocks: i64, clone: Option<(i64, u64)>) -> i64 {
    let mut counted = if nlink > 1 {
        check_and_add_inode(inode, blocks)
    } else {
        blocks
    };
    if let Some((private_bytes, clone_id)) = clone {
        let private_blocks = blocks_from_bytes(private_bytes).min(counted);
        let shared = counted - private_blocks;
        if shared > 0 {
            counted = private_blocks + if insert_new(&SEEN_CLONES, clone_id) { shared } else { 0 };
        }
    }
    counted
}

// Convert bytes to 512-byte blocks (du default)
fn blocks_from_bytes(bytes: i64) -> i64 {
    (bytes + 511) / 512
}

// How sizes are printed, matching du's output units
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeFormat {
    // Block count in units of `unit` bytes, rounded up (default 512, -k, -m, -g, -B)
    Blocks { unit: i64 },
    // Human-readable with powers of `base` (1024 for -h, 1000 for --si)
    Human { base: i64 },
}

impl SizeFormat {
    pub fn format(self, blocks: i64) -> String {
        match self {
            SizeFormat::Blocks { unit } => ((blocks * 512 + unit - 1) / unit).to_string(),
            SizeFormat::Human { base } => format_human(blocks * 512, base),
        }
    }
}

// du -h style: one decimal place below 10 units, whole numbers from 10 up, rounded to
// the nearest. Anything that would print as 1000 or more moves up a unit, so 1000K
// is shown as 1.0M, as du does. Right-aligned in four columns, like du.
fn format_human(bytes: i64, base: i64) -> String {
    let suffixes = if base == 1000 {
        ['B', 'k', 'M', 'G', 'T', 'P']
    } else {
        ['B', 'K', 'M', 'G', 'T', 'P']
    };
    let base_f = base as f64;
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= base_f && unit + 1 < suffixes.len() {
        value /= base_f;
        unit += 1;
    }
    loop {
        let decimal = unit > 0 && value < 9.95;
        let rounded = if decimal {
            (value * 10.0).round() / 10.0
        } else {
            value.round()
        };
        if rounded >= 1000.0 && unit + 1 < suffixes.len() {
            value /= base_f;
            unit += 1;
            continue;
        }
        let number = if decimal {
            format!("{:.1}", value)
        } else {
            format!("{:.0}", value)
        };
        return format!("{:>3}{}", number, suffixes[unit]);
    }
}

// Block size from -B or BLOCKSIZE: a byte count with an optional k, m or g suffix
// (powers of 1024), rounded up to a multiple of 512 as du does.
pub fn parse_block_size(text: &str) -> Option<i64> {
    let lower = text.to_ascii_lowercase();
    let (digits, multiplier) = if let Some(d) = lower.strip_suffix('k') {
        (d, 1_i64 << 10)
    } else if let Some(d) = lower.strip_suffix('m') {
        (d, 1_i64 << 20)
    } else if let Some(d) = lower.strip_suffix('g') {
        (d, 1_i64 << 30)
    } else {
        (lower.as_str(), 1)
    };
    let count = digits.parse::<i64>().ok().filter(|&n| n > 0)?;
    let bytes = count.checked_mul(multiplier)?;
    Some((bytes + 511) / 512 * 512)
}

fn is_dot_or_dotdot(filename: &str) -> bool {
    filename == "." || filename == ".."
}

fn has_glob_chars(arg: &str) -> bool {
    arg.contains(['*', '?', '['])
}

// Expand glob patterns in the arguments. Arguments that exist literally, or that
// contain no glob characters, are passed through untouched. Returns the paths to
// measure and any error messages (patterns with no matches, invalid patterns).
pub fn expand_args(args: &[String]) -> (Vec<String>, Vec<String>) {
    let options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: true,
    };
    let mut paths = Vec::new();
    let mut errors = Vec::new();

    for arg in args {
        if !has_glob_chars(arg) || Path::new(arg).exists() {
            paths.push(arg.clone());
            continue;
        }
        match glob::glob_with(arg, options) {
            Ok(matches) => {
                let before = paths.len();
                paths.extend(
                    matches
                        .filter_map(Result::ok)
                        .map(|p| p.to_string_lossy().into_owned()),
                );
                if paths.len() == before {
                    errors.push(format!("{}: No such file or directory", arg));
                }
            }
            Err(e) => errors.push(format!("{}: invalid pattern: {}", arg, e.msg)),
        }
    }
    (paths, errors)
}

// Read an integer sysctl by name (macOS)
fn sysctl_usize(name: &str) -> Option<usize> {
    let c_name = CString::new(name).ok()?;
    let mut value: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    let rc = unsafe {
        libc::sysctlbyname(
            c_name.as_ptr(),
            &mut value as *mut libc::c_int as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && value > 0).then_some(value as usize)
}

// The walk is bound by kernel directory-lookup locks, so threads beyond the number
// of fast physical cores mostly spin: measured on an M-series Mac, 6 threads beat 12
// on wall time while using about half the CPU. Prefer performance cores, then
// physical cores, then logical cores. DUMAC_THREADS overrides for tuning.
fn worker_threads() -> usize {
    env::var("DUMAC_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .or_else(|| sysctl_usize("hw.perflevel0.physicalcpu"))
        .or_else(|| sysctl_usize("hw.physicalcpu"))
        .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()))
        .unwrap_or(1)
        // Limit to MAX_FILE_HANDLES to avoid file handle limit errors
        .min(MAX_FILE_HANDLES)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub dedupe_clones: bool,
    pub format: SizeFormat,
}

// Split options from paths. Short options can be combined (-hk), and -B takes its
// value attached or as the next argument. "--" ends option parsing, so a path that
// starts with a dash stays reachable. Later size options override earlier ones.
pub fn parse_flags(
    args: &[String],
    default_format: SizeFormat,
) -> Result<(Options, Vec<String>), String> {
    let mut options = Options {
        dedupe_clones: false,
        format: default_format,
    };
    let mut operands = Vec::new();
    let mut options_done = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if options_done || arg == "-" || !arg.starts_with('-') {
            operands.push(arg.clone());
            continue;
        }
        if arg == "--" {
            options_done = true;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            match long {
                "dedupe-clones" => options.dedupe_clones = true,
                "si" => options.format = SizeFormat::Human { base: 1000 },
                _ => return Err(format!("unrecognized option '{}'", arg)),
            }
            continue;
        }
        let cluster = &arg[1..];
        for (i, c) in cluster.char_indices() {
            match c {
                // dumac already prints one total per operand, which is what du -s prints
                's' => {}
                'h' => options.format = SizeFormat::Human { base: 1024 },
                'k' => options.format = SizeFormat::Blocks { unit: 1 << 10 },
                'm' => options.format = SizeFormat::Blocks { unit: 1 << 20 },
                'g' => options.format = SizeFormat::Blocks { unit: 1 << 30 },
                'B' => {
                    let attached = &cluster[i + 1..];
                    let value = if attached.is_empty() {
                        rest.next()
                            .ok_or_else(|| "option requires an argument -- 'B'".to_string())?
                            .as_str()
                    } else {
                        attached
                    };
                    let unit = parse_block_size(value)
                        .ok_or_else(|| format!("invalid block size '{}'", value))?;
                    options.format = SizeFormat::Blocks { unit };
                    break;
                }
                _ => return Err(format!("illegal option -- {}", c)),
            }
        }
    }
    Ok((options, operands))
}

// du's default unit is BLOCKSIZE when set, otherwise 512-byte blocks
fn default_size_format() -> SizeFormat {
    let unit = env::var("BLOCKSIZE")
        .ok()
        .and_then(|v| parse_block_size(&v))
        .unwrap_or(512);
    SizeFormat::Blocks { unit }
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let (options, operands) = match parse_flags(&args[1..], default_size_format()) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{}: {}", args[0], msg);
            std::process::exit(1);
        }
    };
    if operands.is_empty() {
        eprintln!(
            "usage: {} [-g | -h | -k | -m | --si | -B blocksize] [--dedupe-clones] path [path ...]",
            args[0]
        );
        std::process::exit(1);
    }
    set_dedupe_clones(options.dedupe_clones);

    let (roots, expand_errors) = expand_args(&operands);
    let mut failed = !expand_errors.is_empty();
    for e in &expand_errors {
        eprintln!("{}: {}", args[0], e);
    }

    let pool_threads = worker_threads();
    let pool = rayon::ThreadPoolBuilder::new()
        // Larger than default stack size to handle deep directory trees
        .stack_size(4 * 1024 * 1024)
        .num_threads(pool_threads)
        .build()
        .expect("Failed to build thread pool");

    // Roots are independent, so walk them concurrently. Collect before printing:
    // rayon's indexed collect preserves argument order, and buffering keeps
    // interleaved `println!` lines from splitting mid-record. A root that fails
    // still prints its error, in its original position relative to the others.
    let results: Vec<(String, Result<i64, String>)> = pool.install(|| {
        roots
            .into_par_iter()
            .map(|root| {
                let size = size_of_path(root.clone());
                (root, size)
            })
            .collect()
    });

    for (root, result) in results {
        match result {
            Ok(total_blocks) => println!("{}\t{}", options.format.format(total_blocks), root),
            Err(e) => {
                eprintln!("{}: {}", args[0], e);
                failed = true;
            }
        }
    }

    if failed {
        std::process::exit(1);
    }
}

// Size of a command line argument: directories are walked, anything else is
// measured directly. Unreadable paths fall through to calculate_size for its error.
// lstat, not stat: like du's default -P, a symlink operand is measured itself
// rather than followed to its target.
pub fn size_of_path(path: String) -> Result<i64, String> {
    let c_path = CString::new(path.as_str()).map_err(|_| format!("{}: Invalid path", path))?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::lstat(c_path.as_ptr(), &mut st) } == 0
        && (st.st_mode & libc::S_IFMT) != libc::S_IFDIR
    {
        let blocks = if st.st_nlink > 1 {
            check_and_add_inode(st.st_ino, st.st_blocks)
        } else {
            st.st_blocks
        };
        return Ok(blocks);
    }
    calculate_size(path)
}

// True when the entry count reported for a directory on this path can be trusted
// to mean "empty". Only done for writable APFS: the sealed system volume exposes
// firmlinks to the data volume (/Users, /Library, ...) as directories that report
// zero entries but open onto real content.
fn volume_trusts_empty(path: &str) -> bool {
    let Ok(c_path) = CString::new(path) else {
        return false;
    };
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut fs) } != 0 {
        return false;
    }
    let is_apfs = fs.f_fstypename.starts_with(&[b'a' as _, b'p' as _, b'f' as _, b's' as _, 0]);
    is_apfs && (fs.f_flags & libc::MNT_RDONLY as u32) == 0
}

// Calculate total size recursively using rayon work stealing
pub fn calculate_size(root_dir: String) -> Result<i64, String> {
    let trust_empty = volume_trusts_empty(&root_dir);
    walk(root_dir, trust_empty)
}

fn walk(path: String, trust_empty: bool) -> Result<i64, String> {
    let dir_info = get_dir_info(&path, trust_empty)?;

    // Process subdirectories in parallel
    let subdir_size: i64 = if dir_info.subdirs.is_empty() {
        0
    } else {
        dir_info
            .subdirs
            .into_par_iter()
            .map(|subdir| {
                // A mount point may sit on a different filesystem than its parent
                let trust = if subdir.mount {
                    volume_trusts_empty(&subdir.path)
                } else {
                    trust_empty
                };
                match walk(subdir.path, trust) {
                    Ok(size) => size,
                    Err(e) => {
                        eprintln!("dumac: {}", e);
                        0
                    }
                }
            })
            .sum()
    };

    Ok(dir_info.blocks + subdir_size)
}

fn get_dir_info(path: &str, trust_empty: bool) -> Result<DirInfo, String> {
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

    let result = ATTR_BUF.with(|buf| read_dir_entries(path, dirfd, &mut buf.borrow_mut(), trust_empty));

    // Close directory
    unsafe {
        libc::close(dirfd);
    }
    result
}

fn read_dir_entries(
    path: &str,
    dirfd: libc::c_int,
    attrbuf: &mut [u8],
    trust_empty: bool,
) -> Result<DirInfo, String> {
    let dedupe_clones = DEDUPE_CLONES.load(Ordering::Relaxed);

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
        // Entry count and mount status let us skip empty directories entirely
        dirattr: libc::ATTR_DIR_ENTRYCOUNT | libc::ATTR_DIR_MOUNTSTATUS,
        fileattr: libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE,
        // With FSOPT_ATTR_CMN_EXTENDED this group is the extended common attributes
        forkattr: if dedupe_clones {
            libc::ATTR_CMNEXT_PRIVATESIZE | libc::ATTR_CMNEXT_CLONEID
        } else {
            0
        },
    };
    let options = if dedupe_clones {
        libc::FSOPT_ATTR_CMN_EXTENDED as u64
    } else {
        0
    };

    // Prefix (with trailing separator) shared by every child path
    let mut prefix = String::with_capacity(path.len() + 1);
    prefix.push_str(path);
    if !prefix.ends_with('/') {
        prefix.push('/');
    }

    let mut blocks: i64 = 0;
    let mut subdirs = Vec::new();

    loop {
        let retcount = unsafe {
            libc::getattrlistbulk(
                dirfd,
                &mut attrlist as *mut libc::attrlist as *mut libc::c_void,
                attrbuf.as_mut_ptr() as *mut libc::c_void,
                attrbuf.len(),
                options,
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

                // Extract filename (borrowed from the buffer, no allocation)
                let mut filename: Option<&str> = None;
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
                            filename = Some(name_str);
                        }
                    }
                }

                // Check for errors
                if returned_attrs.commonattr & ATTR_CMN_ERROR != 0 {
                    let error_code = std::ptr::read_unaligned(field_ptr as *const u32);
                    field_ptr = field_ptr.add(std::mem::size_of::<u32>());
                    if error_code != 0 {
                        if let Some(name) = filename {
                            eprintln!("cannot access '{}{}': error {}", prefix, name, error_code);
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

                // Directory attributes: entry count, then mount status
                let mut entry_count: Option<u32> = None;
                let mut mount_status: Option<u32> = None;
                if obj_type == VDIR {
                    if returned_attrs.dirattr & libc::ATTR_DIR_ENTRYCOUNT != 0 {
                        entry_count = Some(std::ptr::read_unaligned(field_ptr as *const u32));
                        field_ptr = field_ptr.add(std::mem::size_of::<u32>());
                    }
                    if returned_attrs.dirattr & libc::ATTR_DIR_MOUNTSTATUS != 0 {
                        mount_status = Some(std::ptr::read_unaligned(field_ptr as *const u32));
                        field_ptr = field_ptr.add(std::mem::size_of::<u32>());
                    }
                }

                // Link count comes first among the file attributes. Only files with
                // more than one link can be duplicates, so only they touch the inode set.
                let mut nlink: u32 = 2; // assume shared if the kernel didn't say
                if obj_type == VREG || obj_type == VLNK {
                    if returned_attrs.fileattr & libc::ATTR_FILE_LINKCOUNT != 0 {
                        nlink = std::ptr::read_unaligned(field_ptr as *const u32);
                        field_ptr = field_ptr.add(std::mem::size_of::<u32>());
                    }
                }

                // Handle different file types
                match obj_type {
                    VREG | VLNK => {
                        // File attributes cover every non-directory object, symlinks
                        // included, so use the reported allocation rather than assuming
                        // a block per link. APFS stores short targets inline and reports
                        // zero, which is what du counts.
                        if returned_attrs.fileattr & libc::ATTR_FILE_ALLOCSIZE != 0 {
                            let alloc_size = std::ptr::read_unaligned(field_ptr as *const i64);
                            field_ptr = field_ptr.add(std::mem::size_of::<i64>());
                            let file_blocks = blocks_from_bytes(alloc_size);

                            // Extended attributes follow the file group: private size, clone id
                            let mut clone: Option<(i64, u64)> = None;
                            if dedupe_clones
                                && returned_attrs.forkattr & libc::ATTR_CMNEXT_PRIVATESIZE != 0
                                && returned_attrs.forkattr & libc::ATTR_CMNEXT_CLONEID != 0
                            {
                                let private = std::ptr::read_unaligned(field_ptr as *const i64);
                                field_ptr = field_ptr.add(std::mem::size_of::<i64>());
                                let clone_id = std::ptr::read_unaligned(field_ptr as *const u64);
                                clone = Some((private, clone_id));
                            }
                            blocks += file_blocks_to_add(inode, nlink, file_blocks, clone);
                        }
                    }
                    VDIR => {
                        // Directory - record its full path, unless it is provably empty.
                        // A mount point or autofs trigger can report zero entries yet
                        // have content mounted over it.
                        let mount = mount_status.map_or(false, |m| m & libc::DIR_MNTSTATUS_MNTPOINT != 0);
                        let provably_empty = trust_empty
                            && entry_count == Some(0)
                            && mount_status.map_or(false, |m| m & (libc::DIR_MNTSTATUS_MNTPOINT | DIR_MNTSTATUS_TRIGGER) == 0);
                        if let (Some(name), false) = (filename, provably_empty) {
                            let mut child = String::with_capacity(prefix.len() + name.len());
                            child.push_str(&prefix);
                            child.push_str(name);
                            subdirs.push(SubDir { path: child, mount });
                        }
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

    Ok(DirInfo { blocks, subdirs })
}
