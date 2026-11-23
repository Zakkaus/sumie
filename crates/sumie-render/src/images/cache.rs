use std::collections::HashMap;

pub(super) struct Cache<T> {
    entries: HashMap<u64, Entry<T>>,
    budget: usize,
    bytes: usize,
    clock: u64,
    first_use: u64,
    pinned_bytes: usize,
}

struct Entry<T> {
    value: T,
    bytes: usize,
    used: u64,
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
        }
    }

    pub(super) const fn begin_frame(&mut self) {
        self.first_use = self.clock;
        self.pinned_bytes = 0;
    }

    pub(super) fn acquire(&mut self, key: u64, bytes: usize, upload: impl FnOnce() -> T) -> bool {
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.used < self.first_use {
                self.pinned_bytes += entry.bytes;
            }
            entry.used = self.clock;
            self.clock += 1;
            return true;
        }
        if bytes > self.budget - self.pinned_bytes {
            return false;
        }
        // A texture already selected for this pass must survive until submission.
        while self.bytes > self.budget - bytes {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, entry)| entry.used < self.first_use)
                .min_by_key(|(_, entry)| entry.used)
                .map(|(&key, _)| key)
                .expect("admission leaves enough unpinned space");
            self.bytes -= self.entries.remove(&oldest).unwrap().bytes;
        }
        self.entries.insert(
            key,
            Entry {
                value: upload(),
                bytes,
                used: self.clock,
            },
        );
        self.bytes += bytes;
        self.pinned_bytes += bytes;
        self.clock += 1;
        true
    }

    pub(super) fn get(&self, key: u64) -> &T {
        &self.entries[&key].value
    }
}

#[cfg(test)]
mod tests {
    use super::Cache;

    #[test]
    fn evicts_least_recently_used_image_within_budget() {
        let mut cache = Cache::new(8);
        cache.begin_frame();
        assert!(cache.acquire(1, 4, || 10));
        assert!(cache.acquire(2, 4, || 20));
        cache.begin_frame();
        assert!(cache.acquire(1, 4, || panic!("cache hit uploaded again")));
        cache.begin_frame();
        assert!(cache.acquire(3, 4, || 30));
        assert_eq!(cache.bytes, 8);
        assert_eq!(cache.get(1), &10);
        assert_eq!(cache.get(3), &30);
        assert!(!cache.entries.contains_key(&2));
        assert!(!cache.acquire(4, 8, || panic!("would evict a pinned image")));
    }
}
