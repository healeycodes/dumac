package main

import (
	"fmt"
	"os"
	"path/filepath"
	"syscall"
)

func main() {
	if len(os.Args) < 2 {
		fmt.Println("Usage: dumac <root_directory>")
		os.Exit(1)
	}
	rootDir := os.Args[1]
	res, err := handleDir(rootDir)
	if err != nil {
		fmt.Println("Error:", err)
		os.Exit(1)
	}

	fmt.Println(res.size, res.path)
}

type Result struct {
	path     string
	size     int64
	children []Result
}

func handleDir(rootDir string) (Result, error) {
	dirTree := Result{
		path:     rootDir,
		size:     0,
		children: []Result{},
	}
	dir, err := os.Open(rootDir)
	if err != nil {
		return Result{}, err
	}
	defer dir.Close()

	files, err := dir.Readdir(0)
	if err != nil {
		return Result{}, err
	}

	for _, file := range files {
		if file.IsDir() {
			child, err := handleDir(filepath.Join(rootDir, file.Name()))
			if err != nil {
				return Result{}, err
			}
			dirTree.children = append(dirTree.children, child)
			dirTree.size += child.size
		} else {
			dirTree.size += file.Sys().(*syscall.Stat_t).Blocks
		}
	}
	return dirTree, nil
}
