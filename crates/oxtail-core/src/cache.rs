//! LRU block cache over a [`ReadAt`].
//!
//! Blocks are [`BLOCK_SIZE`] bytes and aligned. Only **full** blocks are
//! cached, so a growing tail is never served stale. The cache is bounded by
//! total bytes and keyed by `(generation, block index)`; bumping the
//! generation (truncation/rotation) makes older entries unreachable and
//! [`BlockCache::clear`] frees them.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lru::LruCache;
use parking_lot::Mutex;

use crate::source::ReadAt;

/// Size of a cache block: 256 KiB.
pub const BLOCK_SIZE: usize = 256 * 1024;

/// Default cache capacity: 64 MiB.
pub const DEFAULT_CACHE_BYTES: usize = 64 * 1024 * 1024;

type Key = (u64, u64);

struct Inner {
    map: LruCache<Key, Arc<[u8]>>,
    used: usize,
}

/// Thread-safe LRU cache of file blocks, bounded by total bytes.
pub struct BlockCache {
    inner: Mutex<Inner>,
    capacity: usize,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl BlockCache {
    /// Creates a cache holding at most `capacity_bytes` (at least one block).
    pub fn new(capacity_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                map: LruCache::unbounded(),
                used: 0,
            }),
            capacity: capacity_bytes.max(BLOCK_SIZE),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Bytes currently held.
    pub fn used_bytes(&self) -> usize {
        self.inner.lock().used
    }

    /// Number of blocks currently held.
    pub fn block_count(&self) -> usize {
        self.inner.lock().map.len()
    }

    /// `(hits, misses)` since creation.
    pub fn stats(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }

    /// Drops every cached block (called on truncation and rotation).
    pub fn clear(&self) {
        let mut g = self.inner.lock();
        g.map.clear();
        g.used = 0;
    }

    fn get(&self, key: Key) -> Option<Arc<[u8]>> {
        let hit = self.inner.lock().map.get(&key).cloned();
        if hit.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        hit
    }

    fn insert(&self, key: Key, block: Arc<[u8]>) {
        let mut g = self.inner.lock();
        let add = block.len();
        if let Some(old) = g.map.put(key, block) {
            g.used -= old.len();
        }
        g.used += add;
        while g.used > self.capacity {
            match g.map.pop_lru() {
                Some((_, b)) => g.used -= b.len(),
                None => break,
            }
        }
    }

    /// Reads through the cache: like [`ReadAt::read_at`] on `src`, but full
    /// blocks are served from / stored in the cache under `generation`.
    pub fn read(
        &self,
        src: &dyn ReadAt,
        generation: u64,
        mut offset: u64,
        buf: &mut [u8],
    ) -> io::Result<usize> {
        let mut done = 0;
        while done < buf.len() {
            let idx = offset / BLOCK_SIZE as u64;
            let in_block = (offset % BLOCK_SIZE as u64) as usize;
            let block = match self.get((generation, idx)) {
                Some(b) => b,
                None => {
                    let mut data = vec![0u8; BLOCK_SIZE];
                    let n = read_full(src, idx * BLOCK_SIZE as u64, &mut data)?;
                    data.truncate(n);
                    let block: Arc<[u8]> = data.into();
                    if n == BLOCK_SIZE {
                        self.insert((generation, idx), block.clone());
                    }
                    block
                }
            };
            if in_block >= block.len() {
                break;
            }
            let n = (block.len() - in_block).min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&block[in_block..in_block + n]);
            done += n;
            offset += n as u64;
            if block.len() < BLOCK_SIZE {
                break; // short block: end of source
            }
        }
        Ok(done)
    }
}

/// Reads until `buf` is full or EOF; returns bytes read.
fn read_full(src: &dyn ReadAt, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match src.read_at(offset + got as u64, &mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

/// A [`ReadAt`] that reads through a shared [`BlockCache`] using the current
/// value of a shared generation counter.
#[derive(Clone)]
pub struct CachedSource {
    inner: Arc<dyn ReadAt>,
    cache: Arc<BlockCache>,
    generation: Arc<AtomicU64>,
}

impl CachedSource {
    /// Wraps `inner`; cache keys use `generation.load()` at read time.
    pub fn new(inner: Arc<dyn ReadAt>, cache: Arc<BlockCache>, generation: Arc<AtomicU64>) -> Self {
        Self {
            inner,
            cache,
            generation,
        }
    }
}

impl ReadAt for CachedSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let generation = self.generation.load(Ordering::Acquire);
        self.cache.read(&*self.inner, generation, offset, buf)
    }

    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemSource;

    #[test]
    fn reads_match_source_and_hit_cache() {
        let data: Vec<u8> = (0..(BLOCK_SIZE * 3 + 100))
            .map(|i| (i % 251) as u8)
            .collect();
        let src = MemSource::new(data.clone());
        let cache = BlockCache::new(BLOCK_SIZE * 8);
        let mut buf = vec![0u8; 1000];
        let off = BLOCK_SIZE as u64 - 500;
        let n = cache.read(&src, 0, off, &mut buf).unwrap();
        assert_eq!(n, 1000);
        assert_eq!(&buf[..], &data[off as usize..off as usize + 1000]);
        let (_, m0) = cache.stats();
        cache.read(&src, 0, off, &mut buf).unwrap();
        let (h1, m1) = cache.stats();
        assert_eq!(m0, m1);
        assert!(h1 >= 2);
        // Tail read across the partial last block.
        let mut tail = vec![0u8; 500];
        let n = cache
            .read(&src, 0, data.len() as u64 - 50, &mut tail)
            .unwrap();
        assert_eq!(n, 50);
        assert_eq!(&tail[..50], &data[data.len() - 50..]);
        assert_eq!(
            cache.read(&src, 0, data.len() as u64, &mut tail).unwrap(),
            0
        );
    }

    #[test]
    fn evicts_and_bounds_memory() {
        let data = vec![7u8; BLOCK_SIZE * 10];
        let src = MemSource::new(data);
        let cache = BlockCache::new(BLOCK_SIZE * 3);
        let mut buf = [0u8; 16];
        for i in 0..10u64 {
            cache
                .read(&src, 0, i * BLOCK_SIZE as u64, &mut buf)
                .unwrap();
            assert!(cache.used_bytes() <= BLOCK_SIZE * 3);
        }
        assert_eq!(cache.block_count(), 3);
        cache.clear();
        assert_eq!(cache.used_bytes(), 0);
    }

    #[test]
    fn growing_tail_is_never_stale() {
        let src = Arc::new(MemSource::new(b"abc".to_vec()));
        let cache = Arc::new(BlockCache::new(BLOCK_SIZE * 2));
        let generation = Arc::new(AtomicU64::new(0));
        let cs = CachedSource::new(src.clone(), cache, generation);
        let mut buf = [0u8; 16];
        assert_eq!(cs.read_at(0, &mut buf).unwrap(), 3);
        src.append(b"def");
        assert_eq!(cs.read_at(0, &mut buf).unwrap(), 6);
        assert_eq!(&buf[..6], b"abcdef");
    }
}
