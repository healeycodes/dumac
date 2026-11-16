use std::fs::{self, File, hard_link};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::sync::{Mutex, MutexGuard};
use tempfile::TempDir;

// The inode and clone sets are process-global, so tests that clear or depend on
// them must not overlap.
static GLOBAL_STATE: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    GLOBAL_STATE.lock().unwrap_or_else(|e| e.into_inner())
}

// Import the main module
#[path = "../src/main.rs"]
mod main;

use main::calculate_size;

#[test]
fn test_basic_file_size_calculation() {
    let _guard = serial();
    // Clear the seen inodes cache to ensure test isolation
    main::clear_seen_inodes();
    
    // Create a temporary directory
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path();
    
    // Create a file with known content
    let file_path = temp_path.join("test_file.txt");
    let mut file = File::create(&file_path).expect("Failed to create test file");
    
    // Write 1000 bytes
    let content = "a".repeat(1000);
    file.write_all(content.as_bytes()).expect("Failed to write to file");
    file.sync_all().expect("Failed to sync file");
    drop(file);
    
    // Calculate size
    let result = calculate_size(temp_path.to_string_lossy().to_string());
    assert!(result.is_ok(), "calculate_size should succeed");
    
    let total_blocks = result.unwrap();
    
    // 1000 bytes should be at least 2 blocks (1000 + 511) / 512 = 2 blocks
    // But filesystem allocation might be larger
    assert!(total_blocks >= 2, "Should have at least 2 blocks for 1000 bytes, got {}", total_blocks);
    
    // Cleanup happens automatically when TempDir is dropped
}

#[test]
fn test_nested_directories() {
    let _guard = serial();
    // Clear the seen inodes cache to ensure test isolation
    main::clear_seen_inodes();
    
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path();
    
    // Create nested directory structure
    let subdir = temp_path.join("subdir");
    fs::create_dir(&subdir).expect("Failed to create subdir");
    
    // Create files in both root and subdir
    let root_file = temp_path.join("root.txt");
    let mut file1 = File::create(&root_file).expect("Failed to create root file");
    file1.write_all(b"hello").expect("Failed to write to root file");
    drop(file1);
    
    let sub_file = subdir.join("sub.txt");
    let mut file2 = File::create(&sub_file).expect("Failed to create sub file");
    file2.write_all(b"world").expect("Failed to write to sub file");
    drop(file2);
    
    // Calculate total size
    let result = calculate_size(temp_path.to_string_lossy().to_string());
    assert!(result.is_ok(), "calculate_size should succeed for nested dirs");
    
    let total_blocks = result.unwrap();
    // Should have blocks for both files (minimum 2 blocks total)
    assert!(total_blocks >= 2, "Should have at least 2 blocks for two files, got {}", total_blocks);
}

#[test]
fn test_hardlink_deduplication() {
    let _guard = serial();
    // Clear the seen inodes cache to ensure test isolation
    main::clear_seen_inodes();
    
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path();
    
    // Create original file with substantial content
    let original_file = temp_path.join("original.txt");
    let mut file = File::create(&original_file).expect("Failed to create original file");
    
    // Write 2048 bytes (should be 4 blocks: (2048 + 511) / 512 = 4)
    let content = "x".repeat(2048);
    file.write_all(content.as_bytes()).expect("Failed to write to original file");
    file.sync_all().expect("Failed to sync original file");
    drop(file);
    
    // Calculate size with just the original file
    let size_original = calculate_size(temp_path.to_string_lossy().to_string())
        .expect("Failed to calculate size for original");
    
    // Create hard link to the same file
    let hardlink_file = temp_path.join("hardlink.txt");
    hard_link(&original_file, &hardlink_file).expect("Failed to create hard link");
    
    // Verify the hardlink was created successfully
    let original_metadata = fs::metadata(&original_file).expect("Failed to get original metadata");
    let hardlink_metadata = fs::metadata(&hardlink_file).expect("Failed to get hardlink metadata");
    assert_eq!(original_metadata.ino(), hardlink_metadata.ino(), "Hardlink should have same inode");
    
    // Clear cache again before second calculation to test deduplication logic
    main::clear_seen_inodes();
    
    // Calculate size again - should be the same due to deduplication
    let size_with_hardlink = calculate_size(temp_path.to_string_lossy().to_string())
        .expect("Failed to calculate size with hardlink");
    
    // The total size should be the same because hardlinks should be deduplicated
    assert_eq!(
        size_original, 
        size_with_hardlink,
        "Hardlinked files should not double-count blocks. Original: {}, With hardlink: {}",
        size_original,
        size_with_hardlink
    );
    
    // Verify the original size is reasonable (at least 4 blocks for 2048 bytes)
    assert!(size_original >= 4, "Should have at least 4 blocks for 2048 bytes, got {}", size_original);
} 

use main::{expand_args, size_of_path};
use std::process::Command;

fn write_file(path: &std::path::Path, bytes: usize) {
    let mut f = File::create(path).expect("Failed to create file");
    f.write_all(&vec![b'a'; bytes]).expect("Failed to write file");
}

#[test]
fn test_expand_args_glob_matches() {
    let _guard = serial();
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::create_dir(root.join("sub1")).unwrap();
    fs::create_dir(root.join("sub2")).unwrap();
    fs::create_dir(root.join("other")).unwrap();
    fs::create_dir(root.join(".hidden")).unwrap();

    let pattern = format!("{}/sub*", root.display());
    let (paths, errors) = expand_args(&[pattern]);
    assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
    assert_eq!(
        paths,
        vec![
            format!("{}/sub1", root.display()),
            format!("{}/sub2", root.display())
        ]
    );

    // Leading dots are not matched by a bare wildcard, like a shell
    let (paths, _) = expand_args(&[format!("{}/*", root.display())]);
    assert!(paths.iter().all(|p| !p.contains(".hidden")));
}

#[test]
fn test_expand_args_recursive_glob() {
    let _guard = serial();
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::create_dir_all(root.join("a/build")).unwrap();
    fs::create_dir_all(root.join("b/c/build")).unwrap();

    let (paths, errors) = expand_args(&[format!("{}/**/build", root.display())]);
    assert!(errors.is_empty());
    assert_eq!(paths.len(), 2, "got {:?}", paths);
}

#[test]
fn test_expand_args_no_match_and_literal() {
    let _guard = serial();
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    let (paths, errors) = expand_args(&[format!("{}/nothing*", root.display())]);
    assert!(paths.is_empty());
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("No such file or directory"));

    // A literal path that happens to contain glob characters is not expanded
    let weird = root.join("we[ir]d*");
    fs::create_dir(&weird).unwrap();
    let arg = weird.to_string_lossy().to_string();
    let (paths, errors) = expand_args(&[arg.clone()]);
    assert!(errors.is_empty());
    assert_eq!(paths, vec![arg]);

    // Plain nonexistent paths pass through so the size step reports the error
    let (paths, errors) = expand_args(&["/definitely/not/here".to_string()]);
    assert!(errors.is_empty());
    assert_eq!(paths.len(), 1);
}

#[test]
fn test_size_of_path_file_root() {
    let _guard = serial();
    main::clear_seen_inodes();
    let temp_dir = TempDir::new().unwrap();
    let file = temp_dir.path().join("f.txt");
    write_file(&file, 4096);

    let blocks = size_of_path(file.to_string_lossy().to_string()).unwrap();
    assert!(blocks >= 8, "4096 bytes should be at least 8 blocks, got {}", blocks);
}

#[test]
fn test_symlink_root_is_not_followed() {
    let _guard = serial();
    main::clear_seen_inodes();
    let temp_dir = TempDir::new().unwrap();
    let target = temp_dir.path().join("target");
    fs::create_dir(&target).unwrap();
    write_file(&target.join("big"), 1 << 20);
    let link = temp_dir.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    // Like du -P, the link contributes its own blocks, not the 1 MiB behind it
    let blocks = size_of_path(link.to_string_lossy().to_string()).unwrap();
    assert!(blocks < 2048, "symlink root should not be followed, got {} blocks", blocks);
}

#[test]
fn test_hardlink_shared_across_roots_counted_once() {
    let _guard = serial();
    main::clear_seen_inodes();
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::create_dir(root.join("a")).unwrap();
    fs::create_dir(root.join("b")).unwrap();
    write_file(&root.join("a/file"), 8192);
    hard_link(root.join("a/file"), root.join("b/link")).unwrap();

    let first = size_of_path(root.join("a").to_string_lossy().to_string()).unwrap();
    let second = size_of_path(root.join("b").to_string_lossy().to_string()).unwrap();
    assert!(first >= 16);
    assert_eq!(second, 0, "hardlink already counted in the first root");
}

#[test]
fn test_cli_multiple_paths_and_quoted_glob() {
    let _guard = serial();
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::create_dir(root.join("sub1")).unwrap();
    fs::create_dir(root.join("sub2")).unwrap();
    write_file(&root.join("sub1/x"), 10_000);
    write_file(&root.join("sub2/y"), 10_000);

    let pattern = format!("{}/sub*", root.display());
    let out = Command::new(env!("CARGO_BIN_EXE_dumac"))
        .arg(&pattern)
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.lines().count(), 2, "got {:?}", stdout);
    assert!(stdout.contains("sub1") && stdout.contains("sub2"));

    let out = Command::new(env!("CARGO_BIN_EXE_dumac"))
        .arg(root.join("sub1"))
        .arg(format!("{}/nomatch*", root.display()))
        .output()
        .unwrap();
    assert!(!out.status.success(), "no-match pattern should fail");
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1);
}

#[test]
fn test_symlinks_report_allocated_size() {
    let _guard = serial();
    main::clear_seen_inodes();

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path();

    let links: Vec<_> = (0..100)
        .map(|i| {
            let link = temp_path.join(format!("link_{}", i));
            std::os::unix::fs::symlink(format!("/some/target/path/file_{}", i), &link)
                .expect("Failed to create symlink");
            link
        })
        .collect();

    // du counts a symlink's own st_blocks, which is 0 where the filesystem stores
    // short targets inline. Derive the expectation so this holds off APFS too.
    let expected_blocks: i64 = links
        .iter()
        .map(|link| {
            fs::symlink_metadata(link)
                .expect("Failed to stat symlink")
                .blocks() as i64
        })
        .sum();

    let total_blocks = calculate_size(temp_path.to_string_lossy().to_string())
        .expect("calculate_size should succeed");

    assert_eq!(
        total_blocks, expected_blocks,
        "symlinks should contribute their real allocated size, not a fixed block each"
    );
}


use main::{parse_block_size, parse_flags, set_dedupe_clones, SizeFormat};

// du's default: 512-byte blocks
const BLOCKS_512: SizeFormat = SizeFormat::Blocks { unit: 512 };

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn test_parse_flags() {
    let (opts, paths) = parse_flags(&args(&["--dedupe-clones", "a", "b"]), BLOCKS_512).unwrap();
    assert!(opts.dedupe_clones);
    assert_eq!(paths, args(&["a", "b"]));

    let (opts, paths) = parse_flags(&args(&["a"]), BLOCKS_512).unwrap();
    assert!(!opts.dedupe_clones);
    assert_eq!(paths, args(&["a"]));

    // "--" ends option parsing so odd paths remain reachable
    let (opts, paths) = parse_flags(&args(&["--", "--dedupe-clones", "-h"]), BLOCKS_512).unwrap();
    assert!(!opts.dedupe_clones);
    assert_eq!(paths, args(&["--dedupe-clones", "-h"]));

    assert!(parse_flags(&args(&["--nope", "a"]), BLOCKS_512).is_err());
    assert!(parse_flags(&args(&["-x", "a"]), BLOCKS_512).is_err());
}

#[test]
fn test_parse_flags_size_format() {
    let format = |v: &[&str]| parse_flags(&args(v), BLOCKS_512).unwrap().0.format;

    assert_eq!(format(&["a"]), BLOCKS_512);
    assert_eq!(format(&["-h", "a"]), SizeFormat::Human { base: 1024 });
    assert_eq!(format(&["--si", "a"]), SizeFormat::Human { base: 1000 });
    assert_eq!(format(&["-k", "a"]), SizeFormat::Blocks { unit: 1024 });
    assert_eq!(format(&["-m", "a"]), SizeFormat::Blocks { unit: 1 << 20 });
    assert_eq!(format(&["-g", "a"]), SizeFormat::Blocks { unit: 1 << 30 });
    assert_eq!(format(&["-B", "4k", "a"]), SizeFormat::Blocks { unit: 4096 });
    assert_eq!(format(&["-B4k", "a"]), SizeFormat::Blocks { unit: 4096 });

    // -s is accepted (dumac already prints one total per operand), so du's -sh works
    assert_eq!(format(&["-sh", "a"]), SizeFormat::Human { base: 1024 });
    assert_eq!(format(&["-s", "a"]), BLOCKS_512);

    // Options combine, and the last size option wins
    assert_eq!(format(&["-hk", "a"]), SizeFormat::Blocks { unit: 1024 });
    assert_eq!(format(&["-kh", "a"]), SizeFormat::Human { base: 1024 });
    assert_eq!(format(&["-k", "--si", "a"]), SizeFormat::Human { base: 1000 });
    assert_eq!(format(&["--si", "-B", "1m", "a"]), SizeFormat::Blocks { unit: 1 << 20 });

    // The default is what BLOCKSIZE supplies, and a size option overrides it
    let default = SizeFormat::Blocks { unit: 1024 };
    let (opts, _) = parse_flags(&args(&["a"]), default).unwrap();
    assert_eq!(opts.format, default);
    let (opts, _) = parse_flags(&args(&["-h", "a"]), default).unwrap();
    assert_eq!(opts.format, SizeFormat::Human { base: 1024 });

    assert!(parse_flags(&args(&["-B"]), BLOCKS_512).is_err());
    assert!(parse_flags(&args(&["-B", "zero", "a"]), BLOCKS_512).is_err());
    assert!(parse_flags(&args(&["-Q", "a"]), BLOCKS_512).is_err());
}

#[test]
fn test_parse_block_size() {
    assert_eq!(parse_block_size("512"), Some(512));
    assert_eq!(parse_block_size("1k"), Some(1024));
    assert_eq!(parse_block_size("1K"), Some(1024));
    assert_eq!(parse_block_size("2m"), Some(2 << 20));
    assert_eq!(parse_block_size("1g"), Some(1 << 30));
    // Rounded up to a multiple of 512, as du does
    assert_eq!(parse_block_size("1000"), Some(1024));
    assert_eq!(parse_block_size("513"), Some(1024));
    assert_eq!(parse_block_size("0"), None);
    assert_eq!(parse_block_size("k"), None);
    assert_eq!(parse_block_size("1x"), None);
    assert_eq!(parse_block_size(""), None);
}

#[test]
fn test_block_counts_match_du_units() {
    // 4 KiB file, as du -k, -m and -g report it
    let four_kib = SizeFormat::Blocks { unit: 1024 }.format(8);
    assert_eq!(four_kib, "4");
    assert_eq!(SizeFormat::Blocks { unit: 1 << 20 }.format(8), "1");
    assert_eq!(SizeFormat::Blocks { unit: 1 << 30 }.format(8), "1");
    // Partial units round up: 1 GiB + 100 MiB is 2 GiB
    assert_eq!(SizeFormat::Blocks { unit: 1 << 30 }.format(2_099_200), "2");
    // Default output is 512-byte blocks
    assert_eq!(BLOCKS_512.format(8), "8");
}

#[test]
fn test_human_sizes_match_du() {
    let human = SizeFormat::Human { base: 1024 };
    // Blocks are 512 bytes; the byte counts in the comments are what du -h printed
    assert_eq!(human.format(0), "  0B");
    assert_eq!(human.format(8), "4.0K"); // 4096 bytes
    assert_eq!(human.format(208), "104K"); // 106496 bytes
    assert_eq!(human.format(1016), "508K"); // 520192 bytes
    assert_eq!(human.format(8000), "3.9M"); // 4096000 bytes
    assert_eq!(human.format(20560), " 10M"); // 10526720 bytes
    assert_eq!(human.format(2_097_152), "1.0G"); // 1 GiB
    assert_eq!(human.format(2_097_152 * 40), " 40G");

    let si = SizeFormat::Human { base: 1000 };
    assert_eq!(si.format(8), "4.1k"); // 4096 bytes
    assert_eq!(si.format(80), " 41k"); // 40960 bytes
    assert_eq!(si.format(8000), "4.1M"); // 4096000 bytes
}

#[test]
fn test_human_sizes_roll_over_to_next_unit() {
    // du shows 1000 KiB as 1.0M, so anything printing as 1000 or more moves up a unit
    let human = SizeFormat::Human { base: 1024 };
    assert_eq!(human.format(2000), "1.0M"); // 1000 KiB
    assert_eq!(human.format(2040), "1.0M"); // 1020 KiB
    assert_eq!(human.format(1960), "980K"); // 980 KiB stays in K
    assert_eq!(human.format(2047), "1.0M"); // 1023.5 KiB
}

#[test]
fn test_empty_directories_do_not_change_totals() {
    let _guard = serial();
    main::clear_seen_inodes();
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Empty dirs, a chain of empty dirs, a dir holding only a dotfile, and a dir
    // holding only a subdirectory that holds a file.
    fs::create_dir(root.join("empty")).unwrap();
    fs::create_dir_all(root.join("chain/a/b/c")).unwrap();
    fs::create_dir(root.join("dotonly")).unwrap();
    write_file(&root.join("dotonly/.hidden"), 5000);
    fs::create_dir_all(root.join("wrapper/inner")).unwrap();
    write_file(&root.join("wrapper/inner/f"), 9000);
    write_file(&root.join("top"), 3000);

    let expected: i64 = ["dotonly/.hidden", "wrapper/inner/f", "top"]
        .iter()
        .map(|f| fs::metadata(root.join(f)).unwrap().blocks() as i64)
        .sum();
    let total = calculate_size(root.to_string_lossy().to_string()).unwrap();
    assert_eq!(total, expected);
}

#[test]
fn test_dedupe_clones_counts_shared_blocks_once() {
    let _guard = serial();
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::create_dir(root.join("d")).unwrap();
    let original = root.join("d/original");
    let mut data = vec![0u8; 1 << 20];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i.wrapping_mul(2654435761) >> 7) as u8; // incompressible-ish, no sparse holes
    }
    File::create(&original).unwrap().write_all(&data).unwrap();

    let cloned = ["c1", "c2"].iter().all(|name| {
        Command::new("cp")
            .arg("-c")
            .arg(&original)
            .arg(root.join("d").join(name))
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    });
    if !cloned {
        eprintln!("skipping: filesystem does not support clonefile");
        return;
    }

    let path = root.join("d").to_string_lossy().to_string();
    main::clear_seen_inodes();
    set_dedupe_clones(false);
    let plain = calculate_size(path.clone()).unwrap();

    main::clear_seen_inodes();
    set_dedupe_clones(true);
    let deduped = calculate_size(path).unwrap();
    set_dedupe_clones(false);

    let one_file = (1 << 20) / 512;
    assert!(plain >= 3 * one_file, "plain total counts every clone: {}", plain);
    assert!(
        deduped < 2 * one_file,
        "clones should share one copy: plain {} deduped {}",
        plain,
        deduped
    );
}

#[test]
fn test_cli_dedupe_clones_flag() {
    let _guard = serial();
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    write_file(&root.join("f"), 10_000);

    let out = Command::new(env!("CARGO_BIN_EXE_dumac"))
        .arg("--dedupe-clones")
        .arg(root)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1);

    let out = Command::new(env!("CARGO_BIN_EXE_dumac"))
        .arg("--unknown")
        .arg(root)
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn test_cli_size_format_flags() {
    let _guard = serial();
    let temp_dir = TempDir::new().unwrap();
    let file = temp_dir.path().join("one_mib");
    write_file(&file, 1 << 20);
    let run = |args: &[&str], blocksize: Option<&str>| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dumac"));
        cmd.env_remove("BLOCKSIZE").args(args).arg(&file);
        if let Some(value) = blocksize {
            cmd.env("BLOCKSIZE", value);
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{:?}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).split('\t').next().unwrap().to_string()
    };

    // A 1 MiB file is 2048 512-byte blocks
    assert_eq!(run(&[], None), "2048");
    assert_eq!(run(&["-k"], None), "1024");
    assert_eq!(run(&["-m"], None), "1");
    assert_eq!(run(&["-B", "4k"], None), "256");
    assert_eq!(run(&["-h"], None), "1.0M");
    assert_eq!(run(&["--si"], None), "1.0M");
    assert_eq!(run(&[], Some("1k")), "1024");
    // Flags override BLOCKSIZE
    assert_eq!(run(&["-m"], Some("1k")), "1");

    let out = Command::new(env!("CARGO_BIN_EXE_dumac"))
        .args(["-B", "zero"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(!out.status.success());
}
