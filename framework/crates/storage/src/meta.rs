//! The meta page: the durable pointer to the current tree, double-buffered.
//!
//! Pages 0 and 1 are reserved as two meta *slots*. A commit writes the new
//! tree's pages, fsyncs, then writes a fresh meta into the slot for the new
//! transaction id (`txn_id & 1`) and fsyncs again. On open we read both slots,
//! keep the ones whose checksum validates, and pick the highest `txn_id`. A
//! crash mid-commit can only corrupt the slot being written; the other slot
//! still names a fully durable older tree. This is how the B+tree gets crash
//! safety without a write-ahead log.
//!
//! The free list rides inside the meta page, so reclaimed pages are swapped in
//! atomically with the new root. It is capped to whatever fits in one page
//! (~500 entries); overflow is leaked rather than chained, which is safe (never
//! corrupting) and fine at the small/medium scale this engine targets.

use crate::pager::{Page, PageId, PAGE_SIZE};

/// Identifies our format and guards against opening a foreign file.
const MAGIC: &[u8; 8] = b"AKURAIDB";
/// On-disk format version. Bump when the layout changes incompatibly.
const FORMAT: u16 = 1;

/// The two meta slots live at fixed page ids; the tree owns pages `2..`.
pub const META_SLOT_0: PageId = 0;
pub const META_SLOT_1: PageId = 1;
/// First page id the allocator may hand to the tree.
pub const FIRST_TREE_PAGE: PageId = 2;

// Byte offsets within the meta page.
const OFF_MAGIC: usize = 0; // [8]
const OFF_FORMAT: usize = 8; // u16
const OFF_TXN: usize = 12; // u64 (offsets 10..12 reserved for future flags)
const OFF_ROOT: usize = 20; // u64
const OFF_FREE_COUNT: usize = 28; // u16
const OFF_FREE_IDS: usize = 30; // u64 * free_count
const OFF_CHECKSUM: usize = PAGE_SIZE - 4; // u32, last 4 bytes

/// Largest free list that fits in the page after the header and checksum.
pub const FREE_CAP: usize = (OFF_CHECKSUM - OFF_FREE_IDS) / 8;

/// The durable database header: which tree is current and which pages are free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    /// Monotonic commit counter. Its low bit selects the slot it lives in.
    pub txn_id: u64,
    /// Page id of the current B+tree root.
    pub root: PageId,
    /// Pages that are allocated in the file but hold no live data.
    pub free: Vec<PageId>,
}

impl Meta {
    /// The meta slot a meta with this `txn_id` is written to.
    pub fn slot(&self) -> PageId {
        self.txn_id & 1
    }

    /// Serialize into a page, truncating the free list to [`FREE_CAP`].
    pub fn encode(&self) -> Page {
        let mut page = [0u8; PAGE_SIZE];
        page[OFF_MAGIC..OFF_MAGIC + 8].copy_from_slice(MAGIC);
        page[OFF_FORMAT..OFF_FORMAT + 2].copy_from_slice(&FORMAT.to_le_bytes());
        page[OFF_TXN..OFF_TXN + 8].copy_from_slice(&self.txn_id.to_le_bytes());
        page[OFF_ROOT..OFF_ROOT + 8].copy_from_slice(&self.root.to_le_bytes());

        let n = self.free.len().min(FREE_CAP);
        page[OFF_FREE_COUNT..OFF_FREE_COUNT + 2].copy_from_slice(&(n as u16).to_le_bytes());
        for (i, id) in self.free.iter().take(n).enumerate() {
            let at = OFF_FREE_IDS + i * 8;
            page[at..at + 8].copy_from_slice(&id.to_le_bytes());
        }

        let sum = checksum(&page[..OFF_CHECKSUM]);
        page[OFF_CHECKSUM..].copy_from_slice(&sum.to_le_bytes());
        page
    }

    /// Parse a meta page, returning `None` if the magic, format, or checksum
    /// don't validate (an empty slot or a torn write).
    pub fn decode(page: &Page) -> Option<Meta> {
        if &page[OFF_MAGIC..OFF_MAGIC + 8] != MAGIC {
            return None;
        }
        if u16::from_le_bytes(page[OFF_FORMAT..OFF_FORMAT + 2].try_into().unwrap()) != FORMAT {
            return None;
        }
        let stored = u32::from_le_bytes(page[OFF_CHECKSUM..].try_into().unwrap());
        if stored != checksum(&page[..OFF_CHECKSUM]) {
            return None;
        }

        let txn_id = u64::from_le_bytes(page[OFF_TXN..OFF_TXN + 8].try_into().unwrap());
        let root = u64::from_le_bytes(page[OFF_ROOT..OFF_ROOT + 8].try_into().unwrap());
        let n = u16::from_le_bytes(page[OFF_FREE_COUNT..OFF_FREE_COUNT + 2].try_into().unwrap())
            as usize;
        let mut free = Vec::with_capacity(n);
        for i in 0..n {
            let at = OFF_FREE_IDS + i * 8;
            free.push(u64::from_le_bytes(page[at..at + 8].try_into().unwrap()));
        }
        Some(Meta { txn_id, root, free })
    }
}

/// FNV-1a, 32-bit. Small, dependency-free, and strong enough to catch the torn
/// or zeroed meta page a crash leaves behind — this is integrity, not security.
fn checksum(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let meta = Meta {
            txn_id: 42,
            root: 7,
            free: vec![3, 5, 9],
        };
        assert_eq!(Meta::decode(&meta.encode()), Some(meta));
    }

    #[test]
    fn empty_free_list_round_trips() {
        let meta = Meta {
            txn_id: 1,
            root: 2,
            free: vec![],
        };
        assert_eq!(Meta::decode(&meta.encode()), Some(meta));
    }

    #[test]
    fn slot_follows_txn_parity() {
        let even = Meta {
            txn_id: 8,
            root: 2,
            free: vec![],
        };
        let odd = Meta {
            txn_id: 9,
            root: 2,
            free: vec![],
        };
        assert_eq!(even.slot(), META_SLOT_0);
        assert_eq!(odd.slot(), META_SLOT_1);
    }

    #[test]
    fn zeroed_page_is_not_valid_meta() {
        assert_eq!(Meta::decode(&[0u8; PAGE_SIZE]), None);
    }

    #[test]
    fn a_single_bit_flip_fails_the_checksum() {
        let meta = Meta {
            txn_id: 100,
            root: 4,
            free: vec![1, 2],
        };
        let mut page = meta.encode();
        page[OFF_ROOT] ^= 0x01; // corrupt the root pointer
        assert_eq!(Meta::decode(&page), None);
    }

    #[test]
    fn free_list_truncates_at_capacity() {
        let free: Vec<PageId> = (0..(FREE_CAP as u64 + 50)).collect();
        let meta = Meta {
            txn_id: 1,
            root: 2,
            free,
        };
        let decoded = Meta::decode(&meta.encode()).unwrap();
        assert_eq!(decoded.free.len(), FREE_CAP);
        assert_eq!(decoded.free, (0..FREE_CAP as u64).collect::<Vec<_>>());
    }
}
