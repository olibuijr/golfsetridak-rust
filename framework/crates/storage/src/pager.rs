//! The pager: a single file viewed as an array of fixed-size pages.
//!
//! This is the only thing that touches the database file's bytes. It offers
//! four operations — read a page, write a page, allocate a new (zeroed) page at
//! the end, and `sync` to force everything durable. Higher layers never seek;
//! they speak in [`PageId`]s.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Bytes per page. 4 KiB matches the typical OS page and disk block, so a page
/// write maps to one block write where the platform allows it.
pub const PAGE_SIZE: usize = 4096;

/// A page's index in the file (0-based). Page N occupies bytes
/// `[N * PAGE_SIZE, (N+1) * PAGE_SIZE)`.
pub type PageId = u64;

/// A fixed-size page of bytes.
pub type Page = [u8; PAGE_SIZE];

/// Reads and writes fixed-size pages in a single file.
pub struct Pager {
    file: File,
}

impl Pager {
    /// Open the store at `path`, creating it if absent. The file is opened for
    /// reading and writing.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Pager> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        Ok(Pager { file })
    }

    /// Number of whole pages currently in the file. A trailing partial page
    /// (which a clean writer never creates) is not counted.
    pub fn page_count(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len() / PAGE_SIZE as u64)
    }

    /// Append a fresh zeroed page and return its id.
    pub fn allocate(&mut self) -> io::Result<PageId> {
        let id = self.page_count()?;
        self.write_page(id, &[0u8; PAGE_SIZE])?;
        Ok(id)
    }

    /// Read page `id`. Errors if the page is past the end of the file.
    pub fn read_page(&mut self, id: PageId) -> io::Result<Page> {
        if id >= self.page_count()? {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("page {id} is out of range"),
            ));
        }
        let mut buf = [0u8; PAGE_SIZE];
        self.file.seek(SeekFrom::Start(id * PAGE_SIZE as u64))?;
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Write `data` to page `id`. `id` may be the current page count, which
    /// extends the file by one page; it may not skip past the end.
    pub fn write_page(&mut self, id: PageId, data: &Page) -> io::Result<()> {
        if id > self.page_count()? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot write page {id}: would leave a gap"),
            ));
        }
        self.file.seek(SeekFrom::Start(id * PAGE_SIZE as u64))?;
        self.file.write_all(data)
    }

    /// Flush all writes to durable storage (data + metadata). Call this at a
    /// commit point; without it, writes may sit in OS cache and be lost on a
    /// crash — exactly the failure mode that corrupts a database.
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}
