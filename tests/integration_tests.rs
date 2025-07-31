use std::fs::{self, File, hard_link};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use tempfile::TempDir;

// Import the main module
#[path = "../src/main.rs"]
mod main;

use main::calculate_size;

#[tokio::test]
async fn test_basic_file_size_calculation() {
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
    let result = calculate_size(temp_path.to_string_lossy().to_string()).await;
    assert!(result.is_ok(), "calculate_size should succeed");
    
    let total_blocks = result.unwrap();
    
    // 1000 bytes should be at least 2 blocks (1000 + 511) / 512 = 2 blocks
    // But filesystem allocation might be larger
    assert!(total_blocks >= 2, "Should have at least 2 blocks for 1000 bytes, got {}", total_blocks);
    
    // Cleanup happens automatically when TempDir is dropped
}

#[tokio::test]
async fn test_nested_directories() {
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
    let result = calculate_size(temp_path.to_string_lossy().to_string()).await;
    assert!(result.is_ok(), "calculate_size should succeed for nested dirs");
    
    let total_blocks = result.unwrap();
    // Should have blocks for both files (minimum 2 blocks total)
    assert!(total_blocks >= 2, "Should have at least 2 blocks for two files, got {}", total_blocks);
}

#[tokio::test]
async fn test_hardlink_deduplication() {
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
    let size_original = calculate_size(temp_path.to_string_lossy().to_string()).await
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
    let size_with_hardlink = calculate_size(temp_path.to_string_lossy().to_string()).await
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