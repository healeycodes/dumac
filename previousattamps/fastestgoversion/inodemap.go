package main

import "sync"

const shardCount = 256

type inodeShard struct {
	mu sync.Mutex
	m  map[uint64]struct{}
}

var shards [shardCount]*inodeShard

func init() {
	for i := range shards {
		shards[i] = &inodeShard{m: make(map[uint64]struct{})}
	}
}

// Returns true if the inode was not seen before
func checkAndAddInode(inode uint64) bool {
	shard := shards[inode%shardCount]
	shard.mu.Lock()
	defer shard.mu.Unlock()
	if _, exists := shard.m[inode]; exists {
		return false
	}
	shard.m[inode] = struct{}{}
	return true
}
