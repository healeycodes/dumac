package main

import (
	"fmt"
	"os"
	"path/filepath"
	"syscall"
	"unsafe"
)

// macOS getattrlistbulk syscall number
const SYS_GETATTRLISTBULK = 461

// Attribute constants from sys/attr.h
const (
	ATTR_BIT_MAP_COUNT      = 5
	ATTR_CMN_RETURNED_ATTRS = 0x80000000
	ATTR_CMN_NAME           = 0x00000001
	ATTR_CMN_ERROR          = 0x20000000
	ATTR_CMN_OBJTYPE        = 0x00000002
	ATTR_FILE_ALLOCSIZE     = 0x00000400
)

// File system object types
const (
	VNON  = 0
	VREG  = 1
	VDIR  = 2
	VBLK  = 3
	VCHR  = 4
	VLNK  = 5
	VSOCK = 6
	VFIFO = 7
	VBAD  = 8
)

// Structures for getattrlistbulk
type attrlist struct {
	bitmapcount uint16
	reserved    uint16
	commonattr  uint32
	volattr     uint32
	dirattr     uint32
	fileattr    uint32
	forkattr    uint32
}

type attributeSet struct {
	commonattr uint32
	volattr    uint32
	dirattr    uint32
	fileattr   uint32
	forkattr   uint32
}

type attrreference struct {
	attr_dataoffset int32
	attr_length     uint32
}

// Convert bytes to 512 bytes blocks (like du)
func blocksFromBytes(bytes int64) int64 {
	return (bytes + 511) / 512
}

// DirInfo holds directory information
type DirInfo struct {
	TotalBlocks int64
	Subdirs     []string
}

// getDirInfo gets directory info using direct syscall to getattrlistbulk
func getDirInfo(path string) (*DirInfo, error) {
	fd, err := syscall.Open(path, syscall.O_RDONLY, 0)
	if err != nil {
		return nil, fmt.Errorf("failed to open %s: %v", path, err)
	}
	defer syscall.Close(fd)

	// Set up attribute list
	attrList := attrlist{
		bitmapcount: ATTR_BIT_MAP_COUNT,
		commonattr:  ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_ERROR | ATTR_CMN_OBJTYPE,
		fileattr:    ATTR_FILE_ALLOCSIZE,
	}

	attrBuf := make([]byte, 128*1024)
	var totalBlocks int64
	var subdirs []string

	for {
		// Make the syscall: getattrlistbulk(fd, &attrList, attrBuf, sizeof(attrBuf), 0)
		r1, _, errno := syscall.RawSyscall6(
			SYS_GETATTRLISTBULK,
			uintptr(fd),
			uintptr(unsafe.Pointer(&attrList)),
			uintptr(unsafe.Pointer(&attrBuf[0])),
			uintptr(len(attrBuf)),
			0, // options
			0, // unused
		)

		retcount := int(r1)
		if retcount <= 0 {
			if retcount < 0 && errno != 0 {
				return nil, fmt.Errorf("getattrlistbulk failed: %v", errno)
			}
			break
		}

		// Parse the attribute buffer
		entry := attrBuf
		for i := 0; i < retcount; i++ {
			if len(entry) < 4 {
				break
			}

			// Get entry length and move to field data
			entryLen := *(*uint32)(unsafe.Pointer(&entry[0]))
			if entryLen == 0 || int(entryLen) > len(entry) {
				break
			}

			field := entry[4:] // Skip entry length

			// Get returned attributes
			if len(field) < 20 { // sizeof(attributeSet) = 20
				break
			}
			returned := *(*attributeSet)(unsafe.Pointer(&field[0]))
			field = field[20:]

			// Check for error
			if returned.commonattr&ATTR_CMN_ERROR != 0 {
				if len(field) < 4 {
					break
				}
				errCode := *(*uint32)(unsafe.Pointer(&field[0]))
				field = field[4:]
				if errCode != 0 {
					// Skip this entry
					entry = entry[entryLen:]
					continue
				}
			}

			var filename string
			// Get filename
			if returned.commonattr&ATTR_CMN_NAME != 0 {
				if len(field) < 8 { // sizeof(attrreference) = 8
					break
				}
				nameInfo := *(*attrreference)(unsafe.Pointer(&field[0]))
				namePtr := unsafe.Pointer(uintptr(unsafe.Pointer(&field[0])) + uintptr(nameInfo.attr_dataoffset))
				nameBytes := (*[256]byte)(namePtr)

				// Find null terminator
				nameLen := 0
				for nameLen < 256 && nameBytes[nameLen] != 0 {
					nameLen++
				}
				if nameLen > 0 {
					filename = string(nameBytes[:nameLen])
				}
				field = field[8:]
			}

			// Skip . and ..
			if filename == "." || filename == ".." {
				entry = entry[entryLen:]
				continue
			}

			// Skip object type field (we'll use ATTR_FILE_ALLOCSIZE to distinguish files/dirs)
			if returned.commonattr&ATTR_CMN_OBJTYPE != 0 {
				if len(field) < 4 {
					break
				}
				field = field[4:] // Skip object type
			}

			// Handle directories and regular files
			// If ATTR_FILE_ALLOCSIZE is present, it's a regular file
			// If not present, it's likely a directory
			if returned.fileattr&ATTR_FILE_ALLOCSIZE != 0 {
				// Regular file
				if len(field) < 8 {
					break
				}
				allocSize := *(*int64)(unsafe.Pointer(&field[0]))
				totalBlocks += blocksFromBytes(allocSize)
				field = field[8:]
			} else if filename != "" {
				// Directory (no file size attribute)
				subdirs = append(subdirs, filename)
			}

			// Move to next entry
			entry = entry[entryLen:]
		}
	}

	return &DirInfo{
		TotalBlocks: totalBlocks,
		Subdirs:     subdirs,
	}, nil
}

func main() {
	if len(os.Args) < 2 {
		fmt.Println("Usage: dumac <root_directory>")
		os.Exit(1)
	}
	rootDir := os.Args[1]

	ch := make(chan Result)
	printCh := make(chan string)
	go handleDir(rootDir, ch, printCh)

	go func() {
		for print := range printCh {
			fmt.Print(print)
		}
	}()

	dirTree := <-ch
	if dirTree.error != nil {
		fmt.Println("Error:", dirTree.error)
		os.Exit(1)
	}
}

type Result struct {
	error    error
	path     string
	size     int64
	children []Result
}

func handleDir(rootDir string, ch chan Result, printCh chan string) {
	info, err := getDirInfo(rootDir)
	if err != nil {
		ch <- Result{error: fmt.Errorf("failed to read directory: %s: %v", rootDir, err)}
		return
	}

	dirTree := Result{
		path:     rootDir,
		size:     info.TotalBlocks,
		children: make([]Result, 0, len(info.Subdirs)),
	}

	childCh := make(chan Result)
	if len(info.Subdirs) > 0 {
		for _, subdir := range info.Subdirs {
			go handleDir(filepath.Join(rootDir, subdir), childCh, printCh)
		}
	}

	for range len(info.Subdirs) {
		child := <-childCh
		dirTree.children = append(dirTree.children, child)
		dirTree.size += child.size
	}

	printCh <- fmt.Sprintf("%d\t%s\n", dirTree.size, dirTree.path)
	ch <- dirTree
}
