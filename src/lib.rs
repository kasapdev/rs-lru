//! `rs-lru` is a small, dependency-free O(1) LRU (least-recently-used) cache.
//!
//! # Example
//!
//! ```
//! use rs_lru::LruCache;
//!
//! let mut cache = LruCache::new(2);
//! cache.put("a", 1);
//! cache.put("b", 2);
//!
//! // Touching "a" makes it the most-recently-used entry.
//! assert_eq!(cache.get(&"a"), Some(&1));
//!
//! // Inserting a third key evicts the least-recently-used entry, "b".
//! cache.put("c", 3);
//! assert_eq!(cache.get(&"b"), None);
//! assert_eq!(cache.get(&"a"), Some(&1));
//! assert_eq!(cache.get(&"c"), Some(&3));
//! ```
//!
//! See the crate README for a detailed explanation of the arena-based,
//! index-linked-list technique used internally to get O(1) `get`/`put`
//! without `unsafe` code.

use std::collections::HashMap;
use std::hash::Hash;

/// One occupied slot in the cache's arena.
///
/// `prev`/`next` are arena indices (not pointers) that thread this node
/// into the intrusive doubly-linked recency list. `None` means "this end
/// of the list" (i.e. this node is currently the head or the tail).
struct Node<K, V> {
    key: K,
    value: V,
    prev: Option<usize>,
    next: Option<usize>,
}

/// An O(1)-operations LRU (least-recently-used) cache.
///
/// `LruCache<K, V>` holds at most `capacity` key/value pairs. When a new
/// key is inserted while the cache is already at capacity, the entry that
/// has gone the longest without being read or written is evicted to make
/// room.
///
/// Both [`LruCache::get`] and [`LruCache::put`] run in amortized O(1) time.
/// See the crate-level docs and the README for how this is achieved
/// without `unsafe` code.
///
/// # Example
///
/// ```
/// use rs_lru::LruCache;
///
/// let mut cache: LruCache<i32, &str> = LruCache::new(3);
/// cache.put(1, "one");
/// cache.put(2, "two");
/// cache.put(3, "three");
/// assert_eq!(cache.len(), 3);
///
/// cache.put(4, "four"); // evicts key 1, the least-recently-used entry
/// assert!(!cache.contains_key(&1));
/// assert!(cache.contains_key(&4));
/// ```
pub struct LruCache<K, V> {
    /// Maps each live key to the arena index holding its node.
    map: HashMap<K, usize>,
    /// Backing storage for all nodes. A `None` entry is a vacated slot
    /// sitting on `free`, available for reuse.
    arena: Vec<Option<Node<K, V>>>,
    /// Indices of vacated arena slots, recycled by future `put` calls so
    /// the arena never needs to shift elements around on eviction.
    free: Vec<usize>,
    /// Index of the most-recently-used node, or `None` if the cache is empty.
    head: Option<usize>,
    /// Index of the least-recently-used node, or `None` if the cache is empty.
    tail: Option<usize>,
    /// Maximum number of entries this cache will hold at once.
    capacity: usize,
}

impl<K, V> LruCache<K, V>
where
    K: Eq + Hash + Clone,
{
    /// Creates a new, empty cache that holds at most `capacity` entries.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is `0`. A zero-capacity cache cannot hold any
    /// entry, and every `put` would have to evict the entry it just
    /// inserted, so this is treated as a programmer error rather than a
    /// runtime condition to handle gracefully.
    ///
    /// # Example
    ///
    /// ```
    /// use rs_lru::LruCache;
    ///
    /// let cache: LruCache<String, i32> = LruCache::new(16);
    /// assert_eq!(cache.capacity(), 16);
    /// assert_eq!(cache.len(), 0);
    /// ```
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "LruCache capacity must be greater than zero");
        Self {
            map: HashMap::new(),
            arena: Vec::new(),
            free: Vec::new(),
            head: None,
            tail: None,
            capacity,
        }
    }

    /// Returns the number of entries currently stored in the cache.
    ///
    /// This is always `<= capacity()`.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Returns `true` if the cache currently holds no entries.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Returns the maximum number of entries this cache can hold.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns `true` if `key` is currently present in the cache.
    ///
    /// This does not affect recency ordering.
    pub fn contains_key(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    /// Looks up `key` without affecting recency ordering.
    ///
    /// Prefer [`LruCache::get`] for normal cache reads; use `peek` only
    /// when you specifically want to inspect an entry without counting it
    /// as a "use" for eviction purposes.
    ///
    /// # Example
    ///
    /// ```
    /// use rs_lru::LruCache;
    ///
    /// let mut cache = LruCache::new(2);
    /// cache.put("a", 1);
    /// assert_eq!(cache.peek(&"a"), Some(&1));
    /// ```
    pub fn peek(&self, key: &K) -> Option<&V> {
        let &idx = self.map.get(key)?;
        Some(&self.node(idx).value)
    }

    /// Looks up `key`, returning its value if present.
    ///
    /// A successful lookup refreshes the entry's recency, moving it to the
    /// most-recently-used end of the internal list so it is the last
    /// candidate considered for eviction.
    ///
    /// # Example
    ///
    /// ```
    /// use rs_lru::LruCache;
    ///
    /// let mut cache = LruCache::new(2);
    /// cache.put("a", 1);
    /// cache.put("b", 2);
    ///
    /// assert_eq!(cache.get(&"a"), Some(&1)); // "a" is now most-recently-used
    /// cache.put("c", 3); // evicts "b", not "a"
    /// assert_eq!(cache.get(&"b"), None);
    /// assert_eq!(cache.get(&"a"), Some(&1));
    /// ```
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let &idx = self.map.get(key)?;
        self.touch(idx);
        Some(&self.node(idx).value)
    }

    /// Inserts a key/value pair, or updates the value of an existing key.
    ///
    /// * If `key` is new and the cache is already at capacity, the
    ///   least-recently-used entry is evicted first to make room.
    /// * If `key` already exists, its value is replaced and its recency is
    ///   refreshed, but the cache's `len()` does not change.
    ///
    /// # Example
    ///
    /// ```
    /// use rs_lru::LruCache;
    ///
    /// let mut cache = LruCache::new(1);
    /// cache.put("a", 1);
    /// cache.put("a", 2); // overwrite: no eviction, len stays 1
    /// assert_eq!(cache.len(), 1);
    /// assert_eq!(cache.get(&"a"), Some(&2));
    ///
    /// cache.put("b", 3); // "a" is now evicted, cache still holds 1 entry
    /// assert_eq!(cache.len(), 1);
    /// assert_eq!(cache.get(&"a"), None);
    /// assert_eq!(cache.get(&"b"), Some(&3));
    /// ```
    pub fn put(&mut self, key: K, value: V) {
        if let Some(&idx) = self.map.get(&key) {
            self.node_mut(idx).value = value;
            self.touch(idx);
            return;
        }

        if self.map.len() >= self.capacity {
            self.evict_lru();
        }

        let idx = self.alloc_slot(key.clone(), value);
        self.map.insert(key, idx);
        self.attach_front(idx);
    }

    // ---- internal helpers -------------------------------------------------

    /// Borrows the node at `idx`.
    ///
    /// # Panics
    ///
    /// Panics if `idx` does not point at an occupied slot. This is an
    /// internal invariant violation (a bug in this crate), not something
    /// that can happen from safe, correct use of the public API.
    fn node(&self, idx: usize) -> &Node<K, V> {
        self.arena[idx]
            .as_ref()
            .expect("rs-lru: arena index referenced by map/list must be occupied")
    }

    /// Mutably borrows the node at `idx`. See [`LruCache::node`] for the panic invariant.
    fn node_mut(&mut self, idx: usize) -> &mut Node<K, V> {
        self.arena[idx]
            .as_mut()
            .expect("rs-lru: arena index referenced by map/list must be occupied")
    }

    /// Allocates a slot for `key`/`value`, recycling a vacated slot from
    /// the free list when one is available, and appending to the arena
    /// otherwise. Returns the slot's arena index. The returned node is not
    /// yet linked into the recency list.
    fn alloc_slot(&mut self, key: K, value: V) -> usize {
        let node = Node {
            key,
            value,
            prev: None,
            next: None,
        };
        if let Some(idx) = self.free.pop() {
            self.arena[idx] = Some(node);
            idx
        } else {
            self.arena.push(Some(node));
            self.arena.len() - 1
        }
    }

    /// Unlinks the node at `idx` from the recency list, patching up its
    /// neighbors (and `head`/`tail` as needed). Does not touch the map or
    /// the arena slot itself.
    fn detach(&mut self, idx: usize) {
        let (prev, next) = {
            let n = self.node(idx);
            (n.prev, n.next)
        };

        match prev {
            Some(p) => self.node_mut(p).next = next,
            None => self.head = next,
        }
        match next {
            Some(n) => self.node_mut(n).prev = prev,
            None => self.tail = prev,
        }

        let n = self.node_mut(idx);
        n.prev = None;
        n.next = None;
    }

    /// Links the node at `idx` in as the new head (most-recently-used end)
    /// of the recency list. Assumes `idx` is currently unlinked (e.g. just
    /// detached, or freshly allocated).
    fn attach_front(&mut self, idx: usize) {
        let old_head = self.head;
        {
            let n = self.node_mut(idx);
            n.prev = None;
            n.next = old_head;
        }
        match old_head {
            Some(h) => self.node_mut(h).prev = Some(idx),
            None => self.tail = Some(idx),
        }
        self.head = Some(idx);
    }

    /// Moves the node at `idx` to the most-recently-used end of the
    /// recency list, marking it as just used.
    fn touch(&mut self, idx: usize) {
        if self.head == Some(idx) {
            return; // already the most-recently-used entry
        }
        self.detach(idx);
        self.attach_front(idx);
    }

    /// Evicts the least-recently-used entry (the current tail), removing
    /// it from the recency list, the map, and freeing its arena slot for
    /// reuse. No-op if the cache is empty.
    fn evict_lru(&mut self) {
        let Some(tail_idx) = self.tail else {
            return;
        };
        self.detach(tail_idx);
        let removed = self.arena[tail_idx]
            .take()
            .expect("rs-lru: tail index must be occupied");
        self.map.remove(&removed.key);
        self.free.push(tail_idx);
    }
}

#[cfg(test)]
mod tests {
    use super::LruCache;

    #[test]
    fn basic_get_put() {
        let mut cache = LruCache::new(2);
        assert_eq!(cache.get(&"a"), None);

        cache.put("a", 1);
        assert_eq!(cache.get(&"a"), Some(&1));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn eviction_order_is_exact() {
        // Capacity 3: insert A, B, C (A is least-recently-used at this point).
        let mut cache = LruCache::new(3);
        cache.put("A", 1);
        cache.put("B", 2);
        cache.put("C", 3);
        // Recency, most- to least-recently-used: C, B, A.

        // Refresh A: now B is the least-recently-used entry.
        assert_eq!(cache.get(&"A"), Some(&1));
        // Recency, most- to least-recently-used: A, C, B.

        // Insert a 4th entry, forcing an eviction.
        cache.put("D", 4);

        // B specifically must have been evicted; A, C, D specifically must remain.
        assert!(
            !cache.contains_key(&"B"),
            "B was the true least-recently-used entry and should have been evicted"
        );
        assert!(
            cache.contains_key(&"A"),
            "A was refreshed and should survive"
        );
        assert!(cache.contains_key(&"C"), "C should survive");
        assert!(
            cache.contains_key(&"D"),
            "D was just inserted and should be present"
        );
        assert_eq!(cache.len(), 3);

        assert_eq!(cache.get(&"A"), Some(&1));
        assert_eq!(cache.get(&"C"), Some(&3));
        assert_eq!(cache.get(&"D"), Some(&4));
        assert_eq!(cache.get(&"B"), None);
    }

    #[test]
    fn capacity_one_edge_case() {
        let mut cache = LruCache::new(1);

        cache.put("X", 1);
        assert_eq!(cache.get(&"X"), Some(&1));
        assert_eq!(cache.len(), 1);

        // Inserting a second key evicts the first.
        cache.put("Y", 2);
        assert_eq!(cache.len(), 1);
        assert!(!cache.contains_key(&"X"));
        assert_eq!(cache.get(&"Y"), Some(&2));

        // get on the current sole key works and doesn't disturb anything.
        assert_eq!(cache.get(&"Y"), Some(&2));
        assert_eq!(cache.len(), 1);

        // Repeated puts to the same key don't evict anything (there's
        // nothing else to evict, and the key itself must survive).
        cache.put("Y", 3);
        cache.put("Y", 4);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(&"Y"), Some(&4));
        assert!(cache.contains_key(&"Y"));
    }

    #[test]
    fn overwrite_updates_value_keeps_len_and_refreshes_recency() {
        let mut cache = LruCache::new(3);
        cache.put("A", 1);
        cache.put("B", 2);
        cache.put("C", 3);
        // Recency, most- to least-recently-used: C, B, A (A is LRU).

        let len_before = cache.len();
        cache.put("A", 100); // overwrite existing key
        assert_eq!(cache.peek(&"A"), Some(&100), "value must be updated");
        assert_eq!(cache.len(), len_before, "overwrite must not change len()");
        assert_eq!(cache.len(), 3);
        // Recency, most- to least-recently-used: A, C, B (B is now LRU).

        // Fill with one new key, forcing exactly one eviction. If the
        // overwrite refreshed A's recency, B (not A) must be evicted.
        cache.put("D", 4);

        assert!(
            !cache.contains_key(&"B"),
            "B was least-recently-used after the overwrite and should be evicted"
        );
        assert!(
            cache.contains_key(&"A"),
            "A was refreshed by the overwrite and must survive the eviction"
        );
        assert!(cache.contains_key(&"C"));
        assert!(cache.contains_key(&"D"));
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.get(&"A"), Some(&100));
    }

    #[test]
    fn peek_does_not_affect_recency() {
        let mut cache = LruCache::new(2);
        cache.put("a", 1);
        cache.put("b", 2);
        // Recency, most- to least-recently-used: b, a (a is LRU).

        // Peeking "a" must NOT refresh it.
        assert_eq!(cache.peek(&"a"), Some(&1));

        cache.put("c", 3); // should evict "a", since peek didn't refresh it
        assert!(!cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert!(cache.contains_key(&"c"));
    }

    #[test]
    fn free_list_recycles_slots_across_many_evictions() {
        // Push far more entries than capacity through the cache to
        // exercise slot recycling repeatedly; the cache must still behave
        // correctly (only ever holding `capacity` live entries, always the
        // most recently inserted ones).
        let mut cache = LruCache::new(4);
        for i in 0..1000 {
            cache.put(i, i * 10);
            assert!(cache.len() <= 4);
        }
        assert_eq!(cache.len(), 4);
        for i in 996..1000 {
            assert_eq!(cache.get(&i), Some(&(i * 10)));
        }
        for i in 0..996 {
            assert_eq!(cache.get(&i), None);
        }
    }

    #[test]
    fn capacity_and_len_accessors() {
        let mut cache: LruCache<i32, i32> = LruCache::new(5);
        assert_eq!(cache.capacity(), 5);
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());

        cache.put(1, 1);
        assert_eq!(cache.len(), 1);
        assert!(!cache.is_empty());
        assert_eq!(cache.capacity(), 5); // capacity never changes
    }

    #[test]
    #[should_panic(expected = "capacity must be greater than zero")]
    fn zero_capacity_panics() {
        let _cache: LruCache<i32, i32> = LruCache::new(0);
    }
}
