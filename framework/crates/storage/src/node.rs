//! B+tree node encoding: the byte layout of a single page-sized node.
//!
//! Two shapes share one page format, distinguished by a leading tag byte:
//! - **Leaf**: sorted `(key, value)` pairs — all data lives here.
//! - **Interior**: `N` separator keys + `N+1` child page pointers; routing only.
//!
//! Leaf values larger than a page can't live inline (a single entry can't be
//! split across leaves), so they spill to a chain of overflow pages and the
//! leaf keeps only a 16-byte stub. The spill is flagged by the high bit of the
//! entry's on-page `u32` length field — inline values are always far below
//! 4 KiB and never set it, so pages written before overflow support existed
//! decode unchanged (see [`Val`]).
//!
//! Decode trusts its input: pages come from our own writes (and meta pages are
//! checksum-guarded), so we don't defend against arbitrary bytes here.

use crate::pager::{Page, PageId, PAGE_SIZE};

const TAG_LEAF: u8 = 0;
const TAG_INTERIOR: u8 = 1;

/// High bit of a leaf entry's on-page `u32` length field. Set ⇒ the value was
/// spilled to overflow pages and only a [`Val::Overflow`] stub lives on the
/// leaf. Inline values are always far below 4 KiB, so they never set it —
/// pages written before overflow support existed therefore decode unchanged.
const OVERFLOW_FLAG: u32 = 0x8000_0000;

/// A leaf value as it lives on its page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Val {
    /// Stored inline in the leaf.
    Inline(Vec<u8>),
    /// Spilled to a chain of overflow pages; the leaf keeps only `head` (the
    /// first overflow page) and `len` (the value's full byte length). The tree
    /// walks the chain to materialize the value. A `head` of `0` is impossible
    /// (page 0 is a meta slot), so `0` is the chain's null terminator.
    Overflow { head: PageId, len: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// Sorted key/value pairs.
    Leaf(Vec<(Vec<u8>, Val)>),
    /// `children.len() == keys.len() + 1`. To route key `s`: pick `children[i]`
    /// for the first `i` with `s < keys[i]`, else the last child.
    Interior {
        keys: Vec<Vec<u8>>,
        children: Vec<PageId>,
    },
}

impl Node {
    /// Bytes this node needs on a page. Callers check against [`Node::fits`]
    /// before [`Node::encode`], which panics on overflow.
    pub fn encoded_len(&self) -> usize {
        match self {
            Node::Leaf(entries) => {
                1 + 2
                    + entries
                        .iter()
                        .map(|(k, v)| {
                            2 + k.len()
                                + 4
                                + match v {
                                    Val::Inline(b) => b.len(),
                                    // stub: head (u64) + total len (u64)
                                    Val::Overflow { .. } => 16,
                                }
                        })
                        .sum::<usize>()
            }
            Node::Interior { keys, .. } => {
                1 + 2 + 8 + keys.iter().map(|k| 2 + k.len() + 8).sum::<usize>()
            }
        }
    }

    /// Does this node fit in a single page?
    pub fn fits(&self) -> bool {
        self.encoded_len() <= PAGE_SIZE
    }

    /// Encode into a zeroed page. Panics if the node exceeds [`PAGE_SIZE`];
    /// guard with [`Node::fits`] (the tree splits before it gets here).
    pub fn encode(&self) -> Page {
        let mut buf = Vec::with_capacity(self.encoded_len());
        match self {
            Node::Leaf(entries) => {
                buf.push(TAG_LEAF);
                buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());
                for (k, v) in entries {
                    buf.extend_from_slice(&(k.len() as u16).to_le_bytes());
                    buf.extend_from_slice(k);
                    match v {
                        Val::Inline(b) => {
                            buf.extend_from_slice(&(b.len() as u32).to_le_bytes());
                            buf.extend_from_slice(b);
                        }
                        Val::Overflow { head, len } => {
                            buf.extend_from_slice(&OVERFLOW_FLAG.to_le_bytes());
                            buf.extend_from_slice(&head.to_le_bytes());
                            buf.extend_from_slice(&len.to_le_bytes());
                        }
                    }
                }
            }
            Node::Interior { keys, children } => {
                buf.push(TAG_INTERIOR);
                buf.extend_from_slice(&(keys.len() as u16).to_le_bytes());
                buf.extend_from_slice(&children[0].to_le_bytes());
                for (k, child) in keys.iter().zip(children.iter().skip(1)) {
                    buf.extend_from_slice(&(k.len() as u16).to_le_bytes());
                    buf.extend_from_slice(k);
                    buf.extend_from_slice(&child.to_le_bytes());
                }
            }
        }
        let mut page = [0u8; PAGE_SIZE];
        page[..buf.len()].copy_from_slice(&buf);
        page
    }

    /// Decode a node from a page. Assumes the page was produced by [`encode`].
    pub fn decode(page: &Page) -> Node {
        let mut c = Cursor { buf: page, pos: 0 };
        let tag = c.u8();
        let count = c.u16() as usize;
        match tag {
            TAG_LEAF => {
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    let klen = c.u16() as usize;
                    let k = c.bytes(klen);
                    let raw = c.u32();
                    let v = if raw & OVERFLOW_FLAG != 0 {
                        Val::Overflow {
                            head: c.u64(),
                            len: c.u64(),
                        }
                    } else {
                        Val::Inline(c.bytes(raw as usize))
                    };
                    entries.push((k, v));
                }
                Node::Leaf(entries)
            }
            TAG_INTERIOR => {
                let mut children = Vec::with_capacity(count + 1);
                let mut keys = Vec::with_capacity(count);
                children.push(c.u64());
                for _ in 0..count {
                    let klen = c.u16() as usize;
                    keys.push(c.bytes(klen));
                    children.push(c.u64());
                }
                Node::Interior { keys, children }
            }
            other => panic!("invalid node tag {other}"),
        }
    }
}

/// A tiny forward-only reader over a page's bytes.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn u8(&mut self) -> u8 {
        let b = self.buf[self.pos];
        self.pos += 1;
        b
    }
    fn u16(&mut self) -> u16 {
        let v = u16::from_le_bytes(self.buf[self.pos..self.pos + 2].try_into().unwrap());
        self.pos += 2;
        v
    }
    fn u32(&mut self) -> u32 {
        let v = u32::from_le_bytes(self.buf[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        v
    }
    fn u64(&mut self) -> u64 {
        let v = u64::from_le_bytes(self.buf[self.pos..self.pos + 8].try_into().unwrap());
        self.pos += 8;
        v
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        let s = self.buf[self.pos..self.pos + n].to_vec();
        self.pos += n;
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_round_trips() {
        let node = Node::Leaf(vec![
            (b"alpha".to_vec(), Val::Inline(b"1".to_vec())),
            (b"beta".to_vec(), Val::Inline(b"two".to_vec())),
            (b"".to_vec(), Val::Inline(b"".to_vec())), // empty key and value
        ]);
        assert_eq!(Node::decode(&node.encode()), node);
    }

    #[test]
    fn interior_round_trips() {
        let node = Node::Interior {
            keys: vec![b"m".to_vec(), b"t".to_vec()],
            children: vec![10, 20, 30],
        };
        assert_eq!(Node::decode(&node.encode()), node);
    }

    #[test]
    fn empty_leaf_round_trips() {
        let node = Node::Leaf(vec![]);
        assert_eq!(Node::decode(&node.encode()), node);
    }

    #[test]
    fn overflow_stub_round_trips_and_stays_small() {
        let node = Node::Leaf(vec![
            (
                b"big".to_vec(),
                Val::Overflow {
                    head: 42,
                    len: 1_000_000,
                },
            ),
            (b"small".to_vec(), Val::Inline(b"x".to_vec())),
        ]);
        assert_eq!(Node::decode(&node.encode()), node);
        // A stub is tiny regardless of the value's true size, so the leaf fits.
        assert!(node.fits());
    }

    #[test]
    fn encoded_len_is_exact() {
        let node = Node::Leaf(vec![(b"key".to_vec(), Val::Inline(b"value".to_vec()))]);
        // tag(1) + count(2) + klen(2)+3 + vlen(4)+5 = 17
        assert_eq!(node.encoded_len(), 17);
        assert!(node.fits());
    }

    #[test]
    fn oversized_leaf_does_not_fit() {
        let big = vec![0u8; PAGE_SIZE];
        let node = Node::Leaf(vec![(b"k".to_vec(), Val::Inline(big))]);
        assert!(!node.fits());
    }
}
