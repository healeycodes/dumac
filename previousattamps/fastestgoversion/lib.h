#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/attr.h>
#include <sys/stat.h>
#include <sys/vnode.h>
#include <unistd.h>
#include <fcntl.h>
#include <stddef.h>
#include <errno.h>

// Growth factor for arrays and arena expansion
#define GROWTH_FACTOR 1.25

// Initial capacities for dynamic arrays
#define INITIAL_FILE_CAPACITY 512
#define INITIAL_SUBDIR_CAPACITY 512

// Convert bytes to 512-byte blocks (du default)
static inline long blocks_from_bytes(off_t bytes) {
    return (bytes + 511) / 512;
}

// Fast check for "." and ".." without strcmp overhead
static inline int is_dot_or_dotdot(const char* filename) {
    return (filename[0] == '.' && filename[1] == '\0') || 
           (filename[0] == '.' && filename[1] == '.' && filename[2] == '\0');
}

// File info structure to track size and inode
typedef struct {
    long blocks;
    uint64_t inode;
} file_info_t;

// Get directory info: file information and subdirectory names
typedef struct {
    file_info_t *files;
    int file_count;
    char **subdirs;
    int subdir_count;
} dir_info_t;

dir_info_t* get_dir_info(int dirfd, const char* path) {
    struct attrlist attrList = {0};
    attrList.bitmapcount = ATTR_BIT_MAP_COUNT;
    attrList.commonattr = ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_ERROR | ATTR_CMN_OBJTYPE | ATTR_CMN_FILEID;
    attrList.fileattr = ATTR_FILE_ALLOCSIZE;

    char attrBuf[128 * 1024];

    // Allocate initial arrays
    int file_capacity = INITIAL_FILE_CAPACITY;
    file_info_t *files = (file_info_t *)malloc(file_capacity * sizeof(file_info_t));
    if (!files) {
        fprintf(stderr, "dumac: failed to allocate files array: %s\n", strerror(errno));
        return NULL;
    }
    int file_count = 0;

    int subdir_capacity = INITIAL_SUBDIR_CAPACITY;
    char **subdirs = (char **)malloc(subdir_capacity * sizeof(char*));
    if (!subdirs) {
        fprintf(stderr, "dumac: failed to allocate subdirs array: %s\n", strerror(errno));
        free(files);
        return NULL;
    }
    int subdir_count = 0;

    for (;;) {
        int retcount = getattrlistbulk(dirfd, &attrList, attrBuf, sizeof(attrBuf), 0);
        if (retcount <= 0) {
            if (retcount < 0)
                fprintf(stderr, "dumac: getattrlistbulk failed: %s\n", strerror(errno));
            break;
        }

        char *entry = attrBuf;
        for (int i = 0; i < retcount; i++) {
            char *field = entry + sizeof(uint32_t);
            entry += *(uint32_t *)entry;

            attribute_set_t returned = *(attribute_set_t *)field;
            field += sizeof(attribute_set_t);

            // Extract filename first so we can use it in error reporting
            char *filename = NULL;
            u_int32_t filename_length = 0;
            if (returned.commonattr & ATTR_CMN_NAME) {
                attrreference_t name_info = *(attrreference_t *)field;
                filename = (field += sizeof(attrreference_t)) + name_info.attr_dataoffset - sizeof(attrreference_t);
                filename_length = name_info.attr_length;
                if (is_dot_or_dotdot(filename))
                    continue;
            }

            if (returned.commonattr & ATTR_CMN_ERROR && *(uint32_t *)(field += sizeof(uint32_t), field - sizeof(uint32_t))) {
                if (filename) {
                    fprintf(stderr, "dumac: cannot access '%s/%s': %s\n", path, filename, strerror(errno));
                } else {
                    fprintf(stderr, "dumac: error accessing entry in '%s'\n", path);
                }
                continue; // Skip entries with errors
            }

            fsobj_type_t obj_type = (returned.commonattr & ATTR_CMN_OBJTYPE) ?
                *(fsobj_type_t *)(field += sizeof(fsobj_type_t), field - sizeof(fsobj_type_t)) : VNON;

            uint64_t inode = (returned.commonattr & ATTR_CMN_FILEID) ?
                *(uint64_t *)(field += sizeof(uint64_t), field - sizeof(uint64_t)) : 0;

            if (obj_type == VREG && (returned.fileattr & ATTR_FILE_ALLOCSIZE)) {
                // Grow files array if needed
                if (file_count >= file_capacity) {
                    int new_file_capacity = (int)(file_capacity * GROWTH_FACTOR);
                    file_info_t *new_files = (file_info_t *)realloc(files, new_file_capacity * sizeof(file_info_t));
                    if (!new_files) {
                        fprintf(stderr, "dumac: realloc failed for files: %s\n", strerror(errno));
                        free(files);
                        for (int j = 0; j < subdir_count; j++) {
                            free(subdirs[j]);
                        }
                        free(subdirs);
                        return NULL;
                    }
                    files = new_files;
                    file_capacity = new_file_capacity;
                }
                // Store file info
                files[file_count].blocks = blocks_from_bytes(*(off_t *)field);
                files[file_count].inode = inode;
                file_count++;
            } else if (obj_type == VLNK) {
                // Symbolic links: count as 0 blocks (like du default behavior)
                // We don't follow symlinks, just acknowledge their existence
            } else if (obj_type == VDIR && filename) {
                // Grow subdirs array if needed
                if (subdir_count >= subdir_capacity) {
                    int new_subdir_capacity = (int)(subdir_capacity * GROWTH_FACTOR);
                    char **new_subdirs = (char **)realloc(subdirs, new_subdir_capacity * sizeof(char*));
                    if (!new_subdirs) {
                        fprintf(stderr, "dumac: realloc failed for subdirs: %s\n", strerror(errno));
                        free(files);
                        for (int j = 0; j < subdir_count; j++) {
                            free(subdirs[j]);
                        }
                        free(subdirs);
                        return NULL;
                    }
                    subdirs = new_subdirs;
                    subdir_capacity = new_subdir_capacity;
                }
                // Store subdirectory name using malloc
                subdirs[subdir_count] = (char *)malloc(filename_length);
                if (!subdirs[subdir_count]) {
                    fprintf(stderr, "dumac: malloc failed for filename: %s\n", strerror(errno));
                    free(files);
                    for (int j = 0; j < subdir_count; j++) {
                        free(subdirs[j]);
                    }
                    free(subdirs);
                    return NULL;
                }
                memcpy(subdirs[subdir_count], filename, filename_length);
                subdir_count++;
            }
        }
    }

    // Allocate result struct
    dir_info_t *result = (dir_info_t *)malloc(sizeof(dir_info_t));
    if (!result) {
        free(files);
        for (int j = 0; j < subdir_count; j++) {
            free(subdirs[j]);
        }
        free(subdirs);
        return NULL;
    }
    result->files = files;
    result->file_count = file_count;
    result->subdirs = subdirs;
    result->subdir_count = subdir_count;

    return result;
}

void free_dir_info(dir_info_t *info) {
    if (info) {
        if (info->files) {
            free(info->files);
        }
        if (info->subdirs) {
            for (int i = 0; i < info->subdir_count; i++) {
                free(info->subdirs[i]);
            }
            free(info->subdirs);
        }
        free(info);
    }
}
