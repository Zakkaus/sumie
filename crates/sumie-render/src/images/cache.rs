use std::collections::HashMap;

const MIN_CHARGE: usize = 64 * 1024;
const MAX_TEXTURES: usize = 4096;

pub(super) struct Cache<T> {
    entries: HashMap<u64, Entry<T>>,
    budget: usize,
    bytes: usize,
    clock: u64,
    first_use: u64,
    pinned_bytes: usize,
    pinned_count: usize,
}

struct Entry<T> {
    value: T,
    size: [u32; 2],
    bytes: usize,
    used: u64,
    visible_frame: u64,
}

impl<T> Cache<T> {
    pub(super) fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            budget,
            bytes: 0,
            clock: 0,
            first_use: 0,
            pinned_bytes: 0,
            pinned_count: 0,
        }
    }

    pub(super) fn begin_frame(&mut self, visible: impl IntoIterator<Item = u64>) {
        self.first_use = self.clock;
        self.pinned_bytes = 0;
        self.clock += 1;
        self.pinned_count = 0;
        for key in visible {
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.visible_frame = self.first_use;
            }
        }
    }

    pub(super) fn acquire(
        &mut self,
        key: u64,
        size: [u32; 2],
        bytes: usize,
        upload: impl FnOnce(Option<T>) -> T,
    ) -> bool {
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.used < self.first_use {
                self.pinned_bytes += entry.bytes;
                self.pinned_count += 1;
            }
            entry.used = self.clock;
            self.clock += 1;
            return true;
        }
        let bytes = bytes.max(MIN_CHARGE);
        if bytes > self.budget - self.pinned_bytes || self.pinned_count == MAX_TEXTURES {
            return false;
        }
        let recycled = self.recycle(size);
        if recycled.is_none() {
            self.make_room(bytes);
            self.bytes += bytes;
        }
        self.entries.insert(
            key,
            Entry {
                value: upload(recycled),
                size,
                bytes,
                used: self.clock,
                visible_frame: self.first_use,
            },
        );
        self.pinned_bytes += bytes;
        self.pinned_count += 1;
        self.clock += 1;
        true
    }

    fn recycle(&mut self, size: [u32; 2]) -> Option<T> {
        if self.entries.len() == self.pinned_count {
            return None;
        }
        // A later pane or viewer may still need a visible generation we have not pinned yet.
        let oldest = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry.size == size
                    && entry.used < self.first_use
                    && entry.visible_frame != self.first_use
            })
            .min_by_key(|(_, entry)| entry.used)
            .map(|(&key, _)| key)?;
        Some(self.entries.remove(&oldest).unwrap().value)
    }

    fn make_room(&mut self, bytes: usize) {
        // A texture already selected for this pass must survive until submission.
        while self.bytes > self.budget - bytes || self.entries.len() == MAX_TEXTURES {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, entry)| entry.used < self.first_use)
                .min_by_key(|(_, entry)| (entry.visible_frame == self.first_use, entry.used))
                .map(|(&key, _)| key)
                .expect("admission leaves enough unpinned space");
            self.bytes -= self.entries.remove(&oldest).unwrap().bytes;
        }
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn get(&self, key: u64) -> &T {
        &self.entries[&key].value
    }
}

#[cfg(test)]
mod tests {
    use super::{Cache, MAX_TEXTURES, MIN_CHARGE};

    #[test]
    fn evicts_least_recently_used_image_within_budget() {
        let mut cache = Cache::new(2 * MIN_CHARGE);
        cache.begin_frame([1, 2]);
        assert!(cache.acquire(1, [1, 1], 4, |_| 10));
        assert!(cache.acquire(2, [1, 1], 4, |_| 20));
        cache.begin_frame([2, 3]);
        assert!(cache.acquire(2, [1, 1], 4, |_| panic!("cache hit uploaded again")));
        cache.begin_frame([1, 3]);
        assert!(cache.acquire(3, [1, 2], 8, |_| 30));
        assert_eq!(cache.bytes, 2 * MIN_CHARGE);
        assert_eq!(cache.get(1), &10);
        assert_eq!(cache.get(3), &30);
        assert!(!cache.entries.contains_key(&2));
        assert!(!cache.acquire(4, [256, 128], 2 * MIN_CHARGE, |_| {
            panic!("would evict a pinned image")
        }));
    }

    #[test]
    fn reuses_only_matching_textures_absent_from_the_frame() {
        let mut cache = Cache::new(4 * MIN_CHARGE);
        let size = [1, 2];
        cache.begin_frame([1, 2, 3]);
        for (key, extent) in [(1, size), (2, size), (3, [2, 1])] {
            assert!(cache.acquire(key, extent, 8, |_| key * 10));
        }

        cache.begin_frame([4, 2, 5]);
        assert!(cache.acquire(4, size, 8, |texture| texture.unwrap()));
        assert!(cache.acquire(5, size, 8, |texture| {
            assert!(texture.is_none());
            50
        }));
        assert!(cache.acquire(2, size, 8, |_| panic!("later pane lost its texture")));
        assert_eq!(cache.get(4), &10);
        assert_eq!(cache.get(2), &20);
        assert_eq!(cache.get(3), &30);
        assert!(!cache.entries.contains_key(&1));

        for generation in 6..120 {
            cache.begin_frame([generation, 2, 5]);
            assert!(cache.acquire(generation, size, 8, |texture| texture.unwrap()));
            assert_eq!(cache.get(generation), &10);
            assert_eq!(cache.len(), 4);
            assert_eq!(cache.bytes, 4 * MIN_CHARGE);
        }
    }

    #[test]
    fn tiny_textures_stop_at_the_object_limit() {
        let mut cache = Cache::new((MAX_TEXTURES + 1) * MIN_CHARGE);
        cache.begin_frame([]);
        for key in 0..MAX_TEXTURES as u64 {
            assert!(cache.acquire(key, [1, 1], 4, |_| ()));
        }
        assert!(!cache.acquire(MAX_TEXTURES as u64, [1, 1], 4, |_| {
            panic!("all textures pinned")
        }));
        cache.begin_frame([MAX_TEXTURES as u64]);
        assert!(cache.acquire(MAX_TEXTURES as u64, [1, 2], 8, |_| ()));
        assert_eq!(cache.len(), MAX_TEXTURES);
        assert!(!cache.entries.contains_key(&0));
    }
}
