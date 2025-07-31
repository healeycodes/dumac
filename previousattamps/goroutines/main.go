package main

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime/pprof"
	"syscall"
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

	if len(os.Args) < 2 {
		fmt.Println("Usage: dumac <root_directory>")
		os.Exit(1)
	}
	rootDir := os.Args[1]
	ch := make(chan int64)
	go handleDir(rootDir, ch)
	size := <-ch
	fmt.Println(size)
}

type Result struct {
	error    error
	path     string
	size     int64
	children []Result
}

var sem = make(chan struct{}, 16)

func handleDir(rootDir string, ch chan int64) {
	size := int64(0)
	dir, err := os.Open(rootDir)
	if err != nil {
		panic(err)
	}
	defer dir.Close()

	files, err := dir.Readdir(0)
	if err != nil {
		panic(err)
	}

	for _, file := range files {
		sem <- struct{}{}
		if file.IsDir() {
			childCh := make(chan int64)
			go handleDir(filepath.Join(rootDir, file.Name()), childCh)
			childSize := <-childCh
			size += childSize
		} else {
			size += file.Sys().(*syscall.Stat_t).Blocks * 512
		}
		<-sem
	}
	ch <- size
}
