#!/usr/bin/env python3

import os
import sys
import subprocess
import platform
import json
import asyncio
import aiofiles
from pathlib import Path
from datetime import datetime

# Benchmark configuration
WIDE_TOTAL_DIRS = 500
WIDE_FILES_PER_DIR = 500

DEEP_TOTAL_LEVELS = 12
DEEP_FILES_PER_LEVEL = 100
DEEP_BRANCHING_FACTOR = 2  # Number of subdirectories per level

# Global semaphore to limit concurrent file operations (initialized in main)
FILE_SEMAPHORE = None

async def create_file_async(file_path, content):
    """Create a single file asynchronously"""
    async with FILE_SEMAPHORE:
        async with aiofiles.open(file_path, 'wb') as f:
            await f.write(content)

async def create_directory_async(dir_path):
    """Create a single directory asynchronously"""
    await asyncio.to_thread(dir_path.mkdir, exist_ok=True)

async def create_wide_benchmark(base_dir):
    """Create benchmark with wide directory structure - tests many files/dirs performance"""
    bench_dir = base_dir / "wide"
    
    # Check if benchmark already exists
    if bench_dir.exists() and any(bench_dir.iterdir()):
        print(f"Wide benchmark already exists in {bench_dir}, skipping creation...")
        total_dirs = WIDE_TOTAL_DIRS
        files_per_dir = WIDE_FILES_PER_DIR
        print(f"Using existing {total_dirs} directories with {files_per_dir} files each ({total_dirs * files_per_dir:,} total files)")
        return bench_dir
    
    await create_directory_async(bench_dir)
    
    print(f"Creating wide benchmark in {bench_dir}...")
    total_dirs = WIDE_TOTAL_DIRS
    files_per_dir = WIDE_FILES_PER_DIR
    print(f"This will create {total_dirs} directories with {files_per_dir} files each ({total_dirs * files_per_dir:,} total files)...")
    
    # Create all directories first
    dir_tasks = []
    for i in range(total_dirs):
        subdir = bench_dir / f"dir_{i:03d}"
        dir_tasks.append(create_directory_async(subdir))
    
    await asyncio.gather(*dir_tasks)
    print("  All directories created, now creating files...")
    
    # Create files in batches to avoid overwhelming the system
    batch_size = 10000
    file_tasks = []
    
    for i in range(total_dirs):
        subdir = bench_dir / f"dir_{i:03d}"
        
        # Create files in each directory
        for j in range(files_per_dir):
            file_path = subdir / f"file_{j:03d}.txt"
            file_tasks.append(create_file_async(file_path, b"x" * 100))
            
            # Process in batches
            if len(file_tasks) >= batch_size:
                await asyncio.gather(*file_tasks)
                file_tasks = []
        
        # Progress indicator
        if (i + 1) % (total_dirs // 2) == 0:
            print(f"  Created files for {i + 1}/{total_dirs} directories...")
    
    # Process remaining files
    if file_tasks:
        await asyncio.gather(*file_tasks)
    
    print(f"Wide benchmark complete: {total_dirs} dirs, {total_dirs * files_per_dir:,} files")
    return bench_dir

async def create_deep_benchmark(base_dir):
    """Create benchmark with deep directory branching - tests tree traversal performance"""
    bench_dir = base_dir / "deep"
    
    # Check if benchmark already exists
    if bench_dir.exists() and any(bench_dir.iterdir()):
        print(f"Deep benchmark already exists in {bench_dir}, skipping creation...")
        total_levels = DEEP_TOTAL_LEVELS # 12
        files_per_level = DEEP_FILES_PER_LEVEL # 100
        branching_factor = DEEP_BRANCHING_FACTOR # 2
        # Estimate total directories (geometric series sum)
        total_dirs = sum(branching_factor ** level for level in range(total_levels))
        print(f"Using existing {total_levels} levels deep with branching factor {branching_factor}")
        print(f"Estimated {total_dirs:,} directories with {files_per_level} files each")
        return bench_dir
    
    await create_directory_async(bench_dir)
    
    print(f"Creating deep branching benchmark in {bench_dir}...")
    total_levels = DEEP_TOTAL_LEVELS
    files_per_level = DEEP_FILES_PER_LEVEL
    branching_factor = DEEP_BRANCHING_FACTOR
    
    # Estimate total directories for progress reporting
    estimated_dirs = sum(branching_factor ** level for level in range(total_levels))
    print(f"This will create a tree {total_levels} levels deep with branching factor {branching_factor}")
    print(f"Estimated {estimated_dirs:,} directories with {files_per_level} files each...")
    
    batch_size = 3000
    dirs_created = 0
    
    # Use a queue to manage directories to process (breadth-first creation)
    # Each item: (directory_path, current_depth)
    dirs_to_process = [(bench_dir, 0)]
    
    while dirs_to_process:
        current_batch = []
        
        # Process directories level by level
        next_level_dirs = []
        
        for dir_path, depth in dirs_to_process:
            # Create files in current directory
            file_tasks = []
            for i in range(files_per_level):
                file_path = dir_path / f"f{i}.txt"
                file_tasks.append(create_file_async(file_path, b"content" * 10))
                
                # Process files in batches
                if len(file_tasks) >= batch_size:
                    current_batch.append(asyncio.gather(*file_tasks))
                    file_tasks = []
            
            # Add remaining files for this directory
            if file_tasks:
                current_batch.append(asyncio.gather(*file_tasks))
            
            # Create subdirectories if we haven't reached max depth
            if depth < total_levels - 1:
                for branch in range(branching_factor):
                    subdir = dir_path / f"d{depth + 1}_b{branch}"
                    next_level_dirs.append((subdir, depth + 1))
        
        # Execute current batch of file operations
        if current_batch:
            await asyncio.gather(*current_batch)
        
        # Create all subdirectories for the next level
        if next_level_dirs:
            dir_tasks = []
            for subdir_path, _ in next_level_dirs:
                dir_tasks.append(create_directory_async(subdir_path))
            
            await asyncio.gather(*dir_tasks)
            dirs_created += len(next_level_dirs)
        
        # Update progress
        current_level = next_level_dirs[0][1] if next_level_dirs else total_levels
        if current_level % (total_levels // 4) == 0 or current_level == total_levels:
            print(f"  Completed level {current_level}/{total_levels}, created {dirs_created:,} directories...")
        
        # Set up for next iteration
        dirs_to_process = next_level_dirs
    
    total_files = dirs_created * files_per_level
    print(f"Deep branching benchmark complete: {total_levels} levels, {dirs_created:,} directories, {total_files:,} files")
    return bench_dir

async def main():
    global FILE_SEMAPHORE
    FILE_SEMAPHORE = asyncio.Semaphore(200)

    temp_dir = Path("temp")
    temp_dir.mkdir(exist_ok=True)
    
    # Create the two intensive benchmark scenarios
    print("\nSetting up benchmark scenarios...")
    
    await create_wide_benchmark(temp_dir)
    await create_deep_benchmark(temp_dir) 
    
    print("\nBenchmark setup complete.")

if __name__ == "__main__":
    asyncio.run(main())
