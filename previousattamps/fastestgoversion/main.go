package main

// #cgo CFLAGS: -O3 -march=native -Wall -flto
// #include "lib.h"
import "C"
import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"runtime/pprof"
	"sync"
	"sync/atomic"
	"unsafe"
)

func main() {
	if os.Getenv("DUMAC_PROFILE") == "1" {
		f, err := os.Create("cpu.prof")
		if err != nil {
			fmt.Fprintf(os.Stderr, "dumac: %v\n", err)
			os.Exit(1)
		}
		defer f.Close()
		pprof.StartCPUProfile(f)
		defer pprof.StopCPUProfile()
	}

	if runtime.GOOS != "darwin" {
		fmt.Fprintf(os.Stderr, "dumac: only supported on macos\n")
		os.Exit(1)
	}

	if len(os.Args) < 2 {
		fmt.Fprintf(os.Stderr, "usage: dumac directory\n")
		os.Exit(1)
	}
	rootDir := os.Args[1]

	size := handleDir(rootDir)
	fmt.Println(size)
}

func handleDir(rootDir string) int64 {
	dir, err := os.Open(rootDir)
	if err != nil {
		panic(err)
	}
	defer dir.Close()
	dirfd := int(dir.Fd())

	cPath := C.CString(rootDir)
	defer C.free(unsafe.Pointer(cPath))

	info := C.get_dir_info(C.int(dirfd), cPath)
	defer C.free_dir_info(info)

	size := int64(0)
	// Process files in this directory and deduplicate by inode
	if info.file_count > 0 {
		files := (*[1 << 30]C.file_info_t)(unsafe.Pointer(info.files))[:info.file_count:info.file_count]

		for _, file := range files {
			size += int64(file.blocks) * 512
		}
	}

	// Process subdirectories recursively
	if info.subdir_count > 0 {
		var wg sync.WaitGroup
		var totalSize int64
		subdirs := (*[1 << 30]*C.char)(unsafe.Pointer(info.subdirs))[:info.subdir_count:info.subdir_count]

		for i, subdir := range subdirs {
			wg.Add(1)
			go func(index int) {
				defer wg.Done()
				childSize := handleDir(filepath.Join(rootDir, C.GoString(subdir)))
				atomic.AddInt64(&totalSize, childSize)
			}(i)
		}
		wg.Wait()

		size += atomic.LoadInt64(&totalSize)
	}

	return size
}
