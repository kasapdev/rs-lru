# rs-lru

A small, zero-dependency Rust library implementing an O(1) LRU (least-recently-used)
cache. `get` and `put` are both amortized O(1), and the whole thing is built
without a single line of `unsafe` code.

## Why this is useful

An LRU cache is one of the most common "actually useful in production" data
structures — bounded in-memory caches for database rows, parsed configs,
computed results, HTTP responses, etc. The naive way to build one in Rust
(a real doubly-linked list of heap nodes, or a `Vec` you linearly scan) is
either painful to write safely or not actually O(1). `rs-lru` uses the
standard "arena + index-based intrusive linked list" trick to get real O(1)
`get`/`put` with only safe Rust: a `Vec` of slots plus a `HashMap<K, usize>`
stand in for pointers, so there's no `unsafe`, no reference-counting, and no
external crate — just `std::collections::HashMap` and `Vec`.

This crate intentionally does *not* try to be a general-purpose caching
framework: no TTLs, no async, no concurrent/shared access, no serialization.
It is exactly one data structure, done correctly, so you can either use it
directly for single-threaded caching or read the ~300 lines of `src/lib.rs`
to understand the technique.

## Usage

```rust
use rs_lru::LruCache;

fn main() {
    let mut cache = LruCache::new(2);
    cache.put("a", 1);
    cache.put("b", 2);

    // Reading "a" marks it as the most-recently-used entry.
    assert_eq!(cache.get(&"a"), Some(&1));

    // Inserting a third key evicts the least-recently-used entry.
    // At this point "b" hasn't been touched since it was inserted,
    // while "a" was just read, so "b" is evicted, not "a".
    cache.put("c", 3);
    assert_eq!(cache.get(&"b"), None);
    assert_eq!(cache.get(&"a"), Some(&1));
    assert_eq!(cache.get(&"c"), Some(&3));

    assert_eq!(cache.len(), 2);
    assert_eq!(cache.capacity(), 2);
}
```

Beyond `get`/`put`/`peek`, entries can be taken out and inspected:

```rust
use rs_lru::LruCache;

let mut cache = LruCache::new(3);
cache.put("a", 1);
cache.put("b", 2);
cache.put("c", 3);
cache.get(&"a");

// Iterate most- to least-recently-used without changing recency.
let keys: Vec<_> = cache.iter().map(|(k, _)| *k).collect();
assert_eq!(keys, ["a", "c", "b"]);

assert_eq!(cache.remove(&"c"), Some(3)); // remove a specific key
assert_eq!(cache.pop_lru(), Some(("b", 2))); // remove the oldest entry
cache.clear(); // drop everything, keep the capacity
assert!(cache.is_empty());
```

Add it to a project with:

```toml
[dependencies]
rs-lru = "1.1"
```

## How it works

The classic textbook LRU cache is a `HashMap<K, *Node>` plus a doubly-linked
list of nodes threaded through `prev`/`next` *pointers*, with the list kept
in recency order so the head is most-recently-used and the tail is
least-recently-used. Moving a node to the front on access, and dropping the
tail node on eviction, are both O(1) once you have the node — the `HashMap`
gives you that in O(1) too.

The problem is building that with real pointers in safe Rust: you'd need
`Rc<RefCell<Node>>` (reference counting + runtime borrow checks + a real risk
of leaking cycles) or raw pointers (`unsafe`). `rs-lru` sidesteps this with
the standard **arena + index-based intrusive linked list** technique:

- All nodes live in one `Vec<Option<Node<K, V>>>`, the *arena*. A node's
  "pointer" is just its `usize` index into this `Vec`.
- Each `Node` stores its key, its value, and `prev: Option<usize>` /
  `next: Option<usize>` — the arena indices of its neighbors in the recency
  list, instead of real pointers. `None` marks an end of the list.
- The cache keeps `head: Option<usize>` (most-recently-used) and
  `tail: Option<usize>` (least-recently-used) indices for the list's ends.
- A `HashMap<K, usize>` maps each live key straight to its arena index, so
  looking up a key's node is a single `HashMap` lookup, not a list walk.

Because indices are just integers, moving a node around the list is a few
field assignments (`detach` unlinks a node from wherever it is; `attach_front`
re-links it as the new head) — no allocation, no borrow-checker fights, no
`unsafe`. `get` looks up the index, calls `detach` + `attach_front` to make
the node the new head, and returns a reference to its value: O(1).

**Eviction and slot reuse.** When `put` inserts a new key while the cache is
at capacity, it evicts the tail node: unlink it from the list (O(1)), remove
its key from the `HashMap` (O(1) average), and vacate its arena slot. Rather
than physically removing the element from the `Vec` (which would require
shifting every later element and fixing up *all* their stored indices — an
O(n) operation, and easy to get subtly wrong), the vacated slot's index is
pushed onto a small `free: Vec<usize>` free list and the slot itself is set
to `None`. The next `put` that needs a new slot pops an index off `free` and
reuses it in place, instead of growing the arena; only once `free` is empty
does the arena actually grow with `Vec::push`. This keeps every arena index
that's currently referenced by the `HashMap` or the linked list permanently
stable — nothing ever moves — so `put` stays O(1) amortized (`Vec::push` is
amortized O(1); popping/pushing `free` is exactly O(1)) no matter how many
evictions have happened.

## Testing

```sh
cargo test
```

The test suite includes doctests plus unit tests that check specific,
named-entry eviction behavior rather than just "an entry got evicted" —
for example, filling a capacity-3 cache with `A, B, C`, refreshing `A` with
`get`, then inserting `D`, and asserting that `B` (and *only* `B`) was
evicted while `A`, `C`, and `D` all remain. Other tests cover the
capacity-1 edge case, overwrite semantics (value updates, `len()` stays
constant, recency is refreshed on overwrite), `peek` *not* affecting
recency, and slot recycling correctness across many evictions.

## Scope and honesty about limits

- Single-threaded only: `LruCache` is not `Sync`-safe for concurrent
  mutation; wrap it yourself (e.g. behind a `Mutex`) if you need that.
- No TTL/expiry — this is purely a size-bounded recency cache.
- `K` must be `Eq + Hash + Clone`. The `Clone` bound exists because a copy of
  each key is kept in the arena node *and* as the `HashMap` key; for large
  keys, consider wrapping them in an `Rc<K>` at your call site if cloning
  becomes a bottleneck.
- Capacity is fixed at construction and cannot be changed afterward.

## License

MIT. See [LICENSE](LICENSE).
