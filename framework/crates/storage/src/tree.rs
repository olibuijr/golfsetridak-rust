//! A copy-on-write B+tree over the [`Pager`].
//!
//! Every mutation rewrites the path from the touched leaf back to the root into
//! freshly allocated pages, leaving the old pages untouched. The old tree stays
//! the committed truth until [`BTree::commit`] swaps the root pointer in the
//! meta page (see [`crate::meta`]) and fsyncs. A crash before that swap simply
//! leaves the previous tree intact — no torn updates, no WAL.
//!
//! Pages the rewrite orphans are recycled. They are held in `pending_free`
//! until commit (they still belong to the *committed* tree until the swap
//! lands), then folded into the durable free list. Allocation draws from the
//! committed free list first and extends the file only when it is empty.
//!
//! Keys and values are arbitrary byte strings; keys order lexicographically.
//! Deletion removes the entry and copies the path but does not yet merge
//! underfull nodes — a documented, correctness-preserving simplification
//! (rebalancing is a later step on the roadmap).

use std::io;
use std::path::Path;

use crate::meta::{Meta, FIRST_TREE_PAGE, META_SLOT_0, META_SLOT_1};
use crate::node::{Node, Val};
use crate::pager::{PageId, Pager, PAGE_SIZE};

/// A persistent, crash-safe ordered map backed by a single file.
pub struct BTree {
    pager: Pager,
    root: PageId,
    txn_id: u64,
    /// Pages free in the committed tree, available to hand out now.
    free: Vec<PageId>,
    /// Pages orphaned by the in-flight transaction; freed at commit.
    pending_free: Vec<PageId>,
}

/// Outcome of inserting into a subtree: either the subtree was rewritten in
/// place, or it split into two and a separator must rise to the parent.
enum Ins {
    Updated(PageId),
    Split {
        left: PageId,
        sep: Vec<u8>,
        right: PageId,
    },
}

impl BTree {
    /// Open the tree at `path`, creating an empty one if the file is new.
    pub fn open(path: impl AsRef<Path>) -> io::Result<BTree> {
        let mut pager = Pager::open(path)?;
        if pager.page_count()? == 0 {
            return Self::initialize(pager);
        }
        let meta = Self::load_meta(&mut pager)?;
        Ok(BTree {
            pager,
            root: meta.root,
            txn_id: meta.txn_id,
            free: meta.free,
            pending_free: Vec::new(),
        })
    }

    /// Lay down a fresh database: reserve the two meta slots, write an empty
    /// leaf root, and commit both meta slots so either survives a first crash.
    fn initialize(mut pager: Pager) -> io::Result<BTree> {
        // Pages must be created in order; reserve slot 0, slot 1, then root.
        let s0 = pager.allocate()?;
        let s1 = pager.allocate()?;
        debug_assert_eq!((s0, s1), (META_SLOT_0, META_SLOT_1));
        let root = pager.allocate()?;
        debug_assert_eq!(root, FIRST_TREE_PAGE);
        pager.write_page(root, &Node::Leaf(Vec::new()).encode())?;

        // Seed both slots; the odd-txn slot wins, so it is the live one.
        pager.write_page(
            META_SLOT_0,
            &Meta {
                txn_id: 0,
                root,
                free: vec![],
            }
            .encode(),
        )?;
        pager.write_page(
            META_SLOT_1,
            &Meta {
                txn_id: 1,
                root,
                free: vec![],
            }
            .encode(),
        )?;
        pager.sync()?;

        Ok(BTree {
            pager,
            root,
            txn_id: 1,
            free: Vec::new(),
            pending_free: Vec::new(),
        })
    }

    /// Read both meta slots and choose the valid one with the highest txn id.
    fn load_meta(pager: &mut Pager) -> io::Result<Meta> {
        let m0 = Meta::decode(&pager.read_page(META_SLOT_0)?);
        let m1 = Meta::decode(&pager.read_page(META_SLOT_1)?);
        match (m0, m1) {
            (Some(a), Some(b)) => Ok(if a.txn_id >= b.txn_id { a } else { b }),
            (Some(a), None) => Ok(a),
            (None, Some(b)) => Ok(b),
            (None, None) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "both meta slots are corrupt — database is unrecoverable",
            )),
        }
    }

    // ---- public map operations -------------------------------------------

    /// Look up `key`, returning a copy of its value if present.
    pub fn get(&mut self, key: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let mut id = self.root;
        loop {
            match self.read_node(id)? {
                Node::Leaf(entries) => {
                    return match entries.binary_search_by(|(k, _)| k.as_slice().cmp(key)) {
                        Ok(i) => Ok(Some(self.load_value(&entries[i].1)?)),
                        Err(_) => Ok(None),
                    };
                }
                Node::Interior { keys, children } => {
                    id = children[route(&keys, key)];
                }
            }
        }
    }

    /// Insert or replace `key`'s value. Durable only after [`BTree::commit`].
    pub fn insert(&mut self, key: &[u8], value: &[u8]) -> io::Result<()> {
        match self.insert_rec(self.root, key, value)? {
            Ins::Updated(new_root) => self.root = new_root,
            Ins::Split { left, sep, right } => {
                self.root = self.write_node(&Node::Interior {
                    keys: vec![sep],
                    children: vec![left, right],
                })?;
            }
        }
        Ok(())
    }

    /// Remove `key`. Returns whether it was present. Durable after `commit`.
    pub fn delete(&mut self, key: &[u8]) -> io::Result<bool> {
        let (new_root, removed) = self.delete_rec(self.root, key)?;
        if removed {
            self.root = new_root;
        }
        Ok(removed)
    }

    /// Collect every `(key, value)` with `start <= key < end`, in key order.
    pub fn range(&mut self, start: &[u8], end: &[u8]) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut out = Vec::new();
        self.range_rec(self.root, start, end, &mut out)?;
        Ok(out)
    }

    /// Make every mutation since the last commit durable, then atomically swap
    /// in the new root by writing the next meta slot.
    pub fn commit(&mut self) -> io::Result<()> {
        // 1. The new tree's data pages must hit disk before the meta names them.
        self.pager.sync()?;

        // 2. Now the orphaned pages are safe to recycle; fold them in (deduped,
        //    so a page can never be handed out twice).
        for id in self.pending_free.drain(..).collect::<Vec<_>>() {
            if !self.free.contains(&id) {
                self.free.push(id);
            }
        }

        // 3. Swap the root by writing the slot for the next transaction, then
        //    fsync so the swap itself is durable.
        let meta = Meta {
            txn_id: self.txn_id + 1,
            root: self.root,
            free: self.free.clone(),
        };
        self.pager.write_page(meta.slot(), &meta.encode())?;
        self.pager.sync()?;
        self.txn_id = meta.txn_id;
        Ok(())
    }

    // ---- recursion --------------------------------------------------------

    fn insert_rec(&mut self, id: PageId, key: &[u8], value: &[u8]) -> io::Result<Ins> {
        match self.read_node(id)? {
            Node::Leaf(mut entries) => {
                let val = self.store_value(value)?;
                match entries.binary_search_by(|(k, _)| k.as_slice().cmp(key)) {
                    Ok(i) => entries[i].1 = val,
                    Err(i) => entries.insert(i, (key.to_vec(), val)),
                }
                self.free_page(id);
                self.write_leaf_split(entries)
            }
            Node::Interior { keys, mut children } => {
                let idx = route(&keys, key);
                let outcome = self.insert_rec(children[idx], key, value)?;
                self.free_page(id);
                match outcome {
                    Ins::Updated(child) => {
                        children[idx] = child;
                        Ok(Ins::Updated(
                            self.write_node(&Node::Interior { keys, children })?,
                        ))
                    }
                    Ins::Split { left, sep, right } => {
                        let mut keys = keys;
                        children[idx] = left;
                        children.insert(idx + 1, right);
                        keys.insert(idx, sep);
                        self.write_interior_split(keys, children)
                    }
                }
            }
        }
    }

    /// Write a leaf, splitting in half if it overflows a page.
    fn write_leaf_split(&mut self, entries: Vec<(Vec<u8>, Val)>) -> io::Result<Ins> {
        let node = Node::Leaf(entries);
        if node.fits() {
            return Ok(Ins::Updated(self.write_node(&node)?));
        }
        let Node::Leaf(entries) = node else {
            unreachable!()
        };
        let mid = entries.len() / 2;
        let sep = entries[mid].0.clone();
        let right_entries = entries[mid..].to_vec();
        let left_entries = entries[..mid].to_vec();
        let left = self.write_node(&Node::Leaf(left_entries))?;
        let right = self.write_node(&Node::Leaf(right_entries))?;
        Ok(Ins::Split { left, sep, right })
    }

    /// Write an interior node, splitting and lifting a separator if it overflows.
    fn write_interior_split(
        &mut self,
        keys: Vec<Vec<u8>>,
        children: Vec<PageId>,
    ) -> io::Result<Ins> {
        let node = Node::Interior { keys, children };
        if node.fits() {
            return Ok(Ins::Updated(self.write_node(&node)?));
        }
        let Node::Interior { keys, children } = node else {
            unreachable!()
        };
        let mid = keys.len() / 2; // this key rises to the parent
        let sep = keys[mid].clone();
        let left = self.write_node(&Node::Interior {
            keys: keys[..mid].to_vec(),
            children: children[..=mid].to_vec(),
        })?;
        let right = self.write_node(&Node::Interior {
            keys: keys[mid + 1..].to_vec(),
            children: children[mid + 1..].to_vec(),
        })?;
        Ok(Ins::Split { left, sep, right })
    }

    fn delete_rec(&mut self, id: PageId, key: &[u8]) -> io::Result<(PageId, bool)> {
        match self.read_node(id)? {
            Node::Leaf(mut entries) => {
                match entries.binary_search_by(|(k, _)| k.as_slice().cmp(key)) {
                    Ok(i) => {
                        entries.remove(i);
                        self.free_page(id);
                        Ok((self.write_node(&Node::Leaf(entries))?, true))
                    }
                    Err(_) => Ok((id, false)), // unchanged: no CoW, no orphan
                }
            }
            Node::Interior { keys, mut children } => {
                let idx = route(&keys, key);
                let (child, removed) = self.delete_rec(children[idx], key)?;
                if !removed {
                    return Ok((id, false));
                }
                children[idx] = child;
                self.free_page(id);
                Ok((self.write_node(&Node::Interior { keys, children })?, true))
            }
        }
    }

    fn range_rec(
        &mut self,
        id: PageId,
        start: &[u8],
        end: &[u8],
        out: &mut Vec<(Vec<u8>, Vec<u8>)>,
    ) -> io::Result<()> {
        match self.read_node(id)? {
            Node::Leaf(entries) => {
                for (k, v) in entries {
                    if k.as_slice() >= start && k.as_slice() < end {
                        let value = self.load_value(&v)?;
                        out.push((k, value));
                    }
                }
                Ok(())
            }
            Node::Interior { keys, children } => {
                for i in 0..children.len() {
                    // Subtree i covers keys in [lo, hi); skip if disjoint from
                    // the query window.
                    let lo = if i == 0 {
                        None
                    } else {
                        Some(keys[i - 1].as_slice())
                    };
                    let hi = keys.get(i).map(|k| k.as_slice());
                    if hi.is_some_and(|h| h <= start) {
                        continue; // entirely below the window
                    }
                    if lo.is_some_and(|l| l >= end) {
                        break; // this and all later subtrees are above the window
                    }
                    self.range_rec(children[i], start, end, out)?;
                }
                Ok(())
            }
        }
    }

    // ---- page plumbing ----------------------------------------------------

    fn read_node(&mut self, id: PageId) -> io::Result<Node> {
        Ok(Node::decode(&self.pager.read_page(id)?))
    }

    /// Allocate a page (recycling a free one first) and write `node` to it.
    fn write_node(&mut self, node: &Node) -> io::Result<PageId> {
        let id = match self.free.pop() {
            Some(id) => id,
            None => self.pager.allocate()?,
        };
        self.pager.write_page(id, &node.encode())?;
        Ok(id)
    }

    /// Mark a page orphaned by this transaction. It stays referenced by the
    /// committed tree until [`BTree::commit`] folds it into the free list.
    fn free_page(&mut self, id: PageId) {
        self.pending_free.push(id);
    }

    // ---- large-value overflow ---------------------------------------------

    /// Inline cutoff: values up to this stay in the leaf; larger ones spill to
    /// overflow pages. Kept well under a page so any single entry still fits a
    /// leaf — the pre-overflow single-large-value panic is exactly what this
    /// prevents (a one-entry leaf can't be split).
    const INLINE_MAX: usize = 1024;

    /// Overflow page header: `next: u64` (0 ends the chain) + `chunk_len: u32`.
    const OVF_HDR: usize = 12;

    /// Turn a caller value into its on-leaf form, spilling a large value to a
    /// freshly allocated chain of immutable overflow pages and keeping only a
    /// stub on the leaf. Overflow pages live outside the copy-on-write tree; an
    /// overwrite or delete simply orphans the old chain (leaked, never reused —
    /// safe, the same stance the meta free list already takes). Each page is
    /// `next: u64 | chunk_len: u32 | bytes`, with `next == 0` ending the chain.
    fn store_value(&mut self, value: &[u8]) -> io::Result<Val> {
        if value.len() <= Self::INLINE_MAX {
            return Ok(Val::Inline(value.to_vec()));
        }
        let chunk_cap = PAGE_SIZE - Self::OVF_HDR;
        // Write tail-first so each page can name its already-written successor.
        let chunks: Vec<&[u8]> = value.chunks(chunk_cap).collect();
        let mut next: PageId = 0;
        let mut head: PageId = 0;
        for chunk in chunks.into_iter().rev() {
            let pid = self.pager.allocate()?;
            let mut page = [0u8; PAGE_SIZE];
            page[0..8].copy_from_slice(&next.to_le_bytes());
            page[8..12].copy_from_slice(&(chunk.len() as u32).to_le_bytes());
            page[Self::OVF_HDR..Self::OVF_HDR + chunk.len()].copy_from_slice(chunk);
            self.pager.write_page(pid, &page)?;
            next = pid;
            head = pid;
        }
        Ok(Val::Overflow {
            head,
            len: value.len() as u64,
        })
    }

    /// Materialize a leaf value, walking the overflow chain when present.
    fn load_value(&mut self, v: &Val) -> io::Result<Vec<u8>> {
        match v {
            Val::Inline(b) => Ok(b.clone()),
            Val::Overflow { head, len } => {
                let mut out = Vec::with_capacity(*len as usize);
                let mut pid = *head;
                while pid != 0 {
                    let page = self.pager.read_page(pid)?;
                    let next = u64::from_le_bytes(page[0..8].try_into().unwrap());
                    let clen = u32::from_le_bytes(page[8..12].try_into().unwrap()) as usize;
                    out.extend_from_slice(&page[Self::OVF_HDR..Self::OVF_HDR + clen]);
                    pid = next;
                }
                Ok(out)
            }
        }
    }
}

/// Pick the child index to descend for `key` in an interior node: the first
/// `i` with `key < keys[i]`, else the last child. Mirrors [`Node::Interior`].
fn route(keys: &[Vec<u8>], key: &[u8]) -> usize {
    keys.partition_point(|k| k.as_slice() <= key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("akurai-tree-{}-{}.db", name, std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn get_on_empty_tree_returns_none() {
        let path = tmp("empty");
        let mut t = BTree::open(&path).unwrap();
        assert_eq!(t.get(b"missing").unwrap(), None);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn insert_then_get_round_trips() {
        let path = tmp("rt");
        let mut t = BTree::open(&path).unwrap();
        t.insert(b"key", b"value").unwrap();
        assert_eq!(t.get(b"key").unwrap(), Some(b"value".to_vec()));
        assert_eq!(t.get(b"nope").unwrap(), None);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn insert_replaces_existing_value() {
        let path = tmp("replace");
        let mut t = BTree::open(&path).unwrap();
        t.insert(b"k", b"one").unwrap();
        t.insert(b"k", b"two").unwrap();
        assert_eq!(t.get(b"k").unwrap(), Some(b"two".to_vec()));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn delete_removes_only_the_target() {
        let path = tmp("del");
        let mut t = BTree::open(&path).unwrap();
        t.insert(b"a", b"1").unwrap();
        t.insert(b"b", b"2").unwrap();
        assert!(t.delete(b"a").unwrap());
        assert!(!t.delete(b"a").unwrap()); // already gone
        assert_eq!(t.get(b"a").unwrap(), None);
        assert_eq!(t.get(b"b").unwrap(), Some(b"2".to_vec()));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn many_keys_force_splits_and_stay_searchable() {
        let path = tmp("split");
        let mut t = BTree::open(&path).unwrap();
        // Big values make pages overflow fast, forcing many splits.
        let val = vec![b'x'; 200];
        for i in 0..500u32 {
            t.insert(&i.to_be_bytes(), &val).unwrap();
        }
        for i in 0..500u32 {
            assert_eq!(
                t.get(&i.to_be_bytes()).unwrap(),
                Some(val.clone()),
                "key {i}"
            );
        }
        assert_eq!(t.get(&999u32.to_be_bytes()).unwrap(), None);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn range_returns_sorted_window() {
        let path = tmp("range");
        let mut t = BTree::open(&path).unwrap();
        for i in 0..300u32 {
            t.insert(&i.to_be_bytes(), &[i as u8]).unwrap();
        }
        let got = t.range(&10u32.to_be_bytes(), &20u32.to_be_bytes()).unwrap();
        let keys: Vec<u32> = got
            .iter()
            .map(|(k, _)| u32::from_be_bytes(k.as_slice().try_into().unwrap()))
            .collect();
        assert_eq!(keys, (10..20).collect::<Vec<_>>());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn commit_then_reopen_sees_the_data() {
        let path = tmp("durable");
        {
            let mut t = BTree::open(&path).unwrap();
            for i in 0..200u32 {
                t.insert(&i.to_be_bytes(), b"v").unwrap();
            }
            t.commit().unwrap();
        }
        let mut t = BTree::open(&path).unwrap();
        for i in 0..200u32 {
            assert_eq!(t.get(&i.to_be_bytes()).unwrap(), Some(b"v".to_vec()));
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn uncommitted_changes_vanish_on_reopen() {
        let path = tmp("rollback");
        {
            let mut t = BTree::open(&path).unwrap();
            t.insert(b"committed", b"yes").unwrap();
            t.commit().unwrap();
            t.insert(b"dirty", b"no").unwrap(); // never committed
        }
        let mut t = BTree::open(&path).unwrap();
        assert_eq!(t.get(b"committed").unwrap(), Some(b"yes".to_vec()));
        assert_eq!(t.get(b"dirty").unwrap(), None);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn free_list_recycles_pages_across_commits() {
        let path = tmp("recycle");
        let mut t = BTree::open(&path).unwrap();
        t.insert(b"k", &[b'a'; 100]).unwrap();
        t.commit().unwrap();
        let pages_after_first = t.pager.page_count().unwrap();

        // Rewrites of the same key orphan a leaf each time; commits recycle it,
        // so the file must not grow unboundedly.
        for _ in 0..50 {
            t.insert(b"k", &[b'b'; 100]).unwrap();
            t.commit().unwrap();
        }
        let pages_after_many = t.pager.page_count().unwrap();
        assert!(
            pages_after_many <= pages_after_first + 2,
            "expected page reuse, grew {pages_after_first} -> {pages_after_many}"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn large_value_spills_and_round_trips() {
        let path = tmp("overflow");
        let mut t = BTree::open(&path).unwrap();
        // Several pages worth — the exact case that used to panic on encode.
        let big = vec![b'Z'; 20_000];
        t.insert(b"doc", &big).unwrap();
        assert_eq!(t.get(b"doc").unwrap(), Some(big.clone()));
        // Survives commit + reopen (overflow pages are synced before the swap).
        t.commit().unwrap();
        drop(t);
        let mut t = BTree::open(&path).unwrap();
        assert_eq!(t.get(b"doc").unwrap(), Some(big));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn value_straddling_the_inline_cutoff_round_trips() {
        let path = tmp("overflow-cutoff");
        let mut t = BTree::open(&path).unwrap();
        for n in [1024usize, 1025, 4083, 4084, 4085, 8200] {
            let v = vec![b'c'; n];
            t.insert(b"k", &v).unwrap();
            assert_eq!(t.get(b"k").unwrap(), Some(v), "len {n}");
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn large_value_can_be_overwritten_by_small_and_back() {
        let path = tmp("overflow-overwrite");
        let mut t = BTree::open(&path).unwrap();
        let big = vec![b'A'; 10_000];
        t.insert(b"k", &big).unwrap();
        t.insert(b"k", b"tiny").unwrap();
        assert_eq!(t.get(b"k").unwrap(), Some(b"tiny".to_vec()));
        let big2 = vec![b'B'; 9_999];
        t.insert(b"k", &big2).unwrap();
        assert_eq!(t.get(b"k").unwrap(), Some(big2));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn large_and_small_values_coexist_in_range() {
        let path = tmp("overflow-range");
        let mut t = BTree::open(&path).unwrap();
        let big = vec![b'q'; 8_000];
        t.insert(&1u32.to_be_bytes(), b"small").unwrap();
        t.insert(&2u32.to_be_bytes(), &big).unwrap();
        t.insert(&3u32.to_be_bytes(), b"also").unwrap();
        let got = t.range(&0u32.to_be_bytes(), &9u32.to_be_bytes()).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[1].1, big);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn many_large_values_force_overflow_and_stay_searchable() {
        let path = tmp("overflow-many");
        let mut t = BTree::open(&path).unwrap();
        for i in 0..40u32 {
            // Each value spans multiple overflow pages and varies by key.
            let v = vec![i as u8; 6_000 + i as usize];
            t.insert(&i.to_be_bytes(), &v).unwrap();
        }
        for i in 0..40u32 {
            assert_eq!(
                t.get(&i.to_be_bytes()).unwrap(),
                Some(vec![i as u8; 6_000 + i as usize]),
                "key {i}"
            );
        }
        std::fs::remove_file(&path).unwrap();
    }
}
