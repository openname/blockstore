// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020 Stacks Open Internet Foundation
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use std::char::from_digit;
use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::{TryFrom, TryInto};
use std::env;
use std::f32::consts::E;
use std::fmt;
use std::fs;
use std::fs::OpenOptions;
use std::hash::{Hash, Hasher};
use std::io;
use std::io::{BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::iter::FromIterator;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::ops::{Deref, DerefMut};
use std::os;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{cmp, error};

use lru::LruCache;
use rusqlite::{
    types::{FromSql, ToSql},
    Connection, Error as SqliteError, ErrorCode as SqliteErrorCode, OpenFlags, OptionalExtension,
    Transaction, NO_PARAMS,
};

use crate::chainstate::stacks::index::bits::{
    get_node_byte_len, get_node_hash, read_block_identifier, read_hash_bytes, read_node_hash_bytes,
    read_nodetype, read_nodetype_at_head, read_nodetype_at_head_nohash, read_root_hash,
    write_nodetype_bytes,
};
use crate::chainstate::stacks::index::node::{
    clear_backptr, is_backptr, set_backptr, TrieNode, TrieNode16, TrieNode256, TrieNode4,
    TrieNode48, TrieNodeID, TrieNodeType, TriePath, TriePtr,
};
use crate::chainstate::stacks::index::storage::NodeHashReader;
use crate::chainstate::stacks::index::storage::TrieStorageConnection;
use crate::chainstate::stacks::index::Error;
use crate::chainstate::stacks::index::TrieLeaf;
use crate::chainstate::stacks::index::{trie_sql, ClarityMarfTrieId, MarfTrieId};

use crate::util_lib::db::sql_pragma;
use crate::util_lib::db::sql_vacuum;
use crate::util_lib::db::sqlite_open;
use crate::util_lib::db::tx_begin_immediate;
use crate::util_lib::db::tx_busy_handler;
use crate::util_lib::db::Error as db_error;
use crate::util_lib::db::SQLITE_MMAP_SIZE;

use stacks_common::types::chainstate::BlockHeaderHash;
use stacks_common::types::chainstate::BLOCK_HEADER_HASH_ENCODED_SIZE;
use stacks_common::types::chainstate::{TrieHash, TRIEHASH_ENCODED_SIZE};

use lz4_flex::{
    compress_prepend_size as lz4_compress_prepend_size, 
    decompress_size_prepended as lz4_decompress_size_prepended,
    block::uncompressed_size as lz4_uncompressed_size
};

/// Mapping between block IDs and trie offsets
pub type TrieIdOffsets = HashMap<u32, TrieIdOffset>;

#[derive(Debug, Clone, Copy)]
pub struct TrieIdOffset {
    pub offset: u64,
    pub length: u64
}

pub const HEADER_INDICATOR: [u8; 3] = [255u8, 255u8, 1u8];

#[derive(Debug, Clone, Copy)]
pub enum TrieBlobCompression {
    None,
    LZ4
}

impl TrieBlobCompression {
    pub fn as_u8(&self) -> u8 {
        match self {
            TrieBlobCompression::None => 0u8,
            TrieBlobCompression::LZ4 => 1u8
        }
    }
}

pub struct BlobCompressionResult {
    pub compressed_bytes: Vec<u8>,
    pub compressed_blob_size: usize,
    pub compression_algorithm: TrieBlobCompression
}

pub struct BlobStorageResult {
    pub offset: u64,
    pub uncompressed_blob_size: usize,
    pub storage_size: usize,
    pub compression: Option<BlobCompressionResult>
}

/// Handle to a flat file containing Trie blobs
pub struct TrieFileDisk {
    fd: fs::File,
    path: String,
    trie_offsets: TrieIdOffsets,
    decompressed_lru: LruCache<u32, Vec<u8>>,
    current_trie: Option<Cursor<Vec<u8>>>,
    current_block_id: Option<u32>,
}

/// Handle to a flat in-memory buffer containing Trie blobs (used for testing)
pub struct TrieFileRAM {
    fd: Cursor<Vec<u8>>,
    readonly: bool,
    trie_offsets: TrieIdOffsets,
}

/// This is flat-file storage for a MARF's tries.  All tries are stored as contiguous byte arrays
/// within a larger byte array.  The variants differ in how those bytes are backed.  The `RAM`
/// variant stores data in RAM in a byte buffer, and the `Disk` variant stores data in a flat file
/// on disk.  This structure is used to support external trie blobs, so that the tries don't need
/// to be stored in sqlite blobs (which incurs a sqlite paging overhead).  This is useful for when
/// the tries are too big to fit into a single page, such as the Stacks chainstate.
pub enum TrieFile {
    RAM(TrieFileRAM),
    Disk(TrieFileDisk),
}

impl TrieFile {
    /// Make a new disk-backed TrieFile
    fn new_disk(path: &str, readonly: bool) -> Result<TrieFile, Error> {
        let fd = OpenOptions::new()
            .read(true)
            .write(!readonly)
            .create(!readonly)
            .open(path)?;

        let lru_cache: LruCache<u32, Vec<u8>> = LruCache::new(NonZeroUsize::new(255).unwrap());

        Ok(TrieFile::Disk(TrieFileDisk {
            fd,
            path: path.to_string(),
            trie_offsets: TrieIdOffsets::new(),
            decompressed_lru: lru_cache,
            current_trie: None,
            current_block_id: None
        }))
    }

    /// Make a new RAM-backed TrieFile
    fn new_ram(readonly: bool) -> TrieFile {
        TrieFile::RAM(TrieFileRAM {
            fd: Cursor::new(vec![]),
            readonly,
            trie_offsets: TrieIdOffsets::new(),
        })
    }

    /// Does the TrieFile exist at the expected path?
    pub fn exists(path: &str) -> Result<bool, Error> {
        if path == ":memory:" {
            Ok(false)
        } else {
            let blob_path = format!("{}.blobs", path);
            match fs::metadata(&blob_path) {
                Ok(_) => Ok(true),
                Err(e) => {
                    if e.kind() == io::ErrorKind::NotFound {
                        Ok(false)
                    } else {
                        return Err(e.into());
                    }
                }
            }
        }
    }

    /// Get a copy of the path to this TrieFile.
    /// If in RAM, then the path will be ":memory:"
    pub fn get_path(&self) -> String {
        match self {
            TrieFile::RAM(_) => ":memory:".to_string(),
            TrieFile::Disk(ref disk) => disk.path.clone(),
        }
    }

    /// Instantiate a TrieFile, given the associated DB path.
    /// If path is ':memory:', then it'll be an in-RAM TrieFile.
    /// Otherwise, it'll be stored as `$db_path.blobs`.
    pub fn from_db_path(path: &str, readonly: bool) -> Result<TrieFile, Error> {
        if path == ":memory:" {
            Ok(TrieFile::new_ram(readonly))
        } else {
            let blob_path = format!("{}.blobs", path);
            TrieFile::new_disk(&blob_path, readonly)
        }
    }

    /// Append a new trie blob to external storage, and add the offset and length to the trie DB.
    /// Return the trie ID
    pub fn store_trie_blob<T: MarfTrieId>(
        &mut self,
        db: &Connection,
        bhh: &T,
        buffer: &[u8],
    ) -> Result<u32, Error> {
        let result = self.append_trie_blob(db, buffer)?;
        test_debug!("Stored trie blob {} to offset {}", bhh, result.offset);
        let block_id = trie_sql::write_external_trie_blob(
            db, 
            bhh, 
            result.offset, 
            result.storage_size as u64, 
            result.compression)?;
        
        match self {
            TrieFile::Disk(disk) => {
                disk.decompressed_lru.put(block_id, buffer.to_vec());
            },
            _ => {}
        }
        
        Ok(block_id)
    }

    /// Read a trie blob in its entirety from the DB
    fn read_trie_blob_from_db(db: &Connection, block_id: u32) -> Result<Vec<u8>, Error> {
        let trie_blob = {
            let mut fd = trie_sql::open_trie_blob_readonly(db, block_id)?;
            let mut trie_blob = vec![];
            fd.read_to_end(&mut trie_blob)?;
            trie_blob
        };
        Ok(trie_blob)
    }

    /// Read a trie blob in its entirety from the blobs file
    #[cfg(test)]
    pub fn read_trie_blob(&mut self, db: &Connection, block_id: u32) -> Result<Vec<u8>, Error> {
        let extern_trie = trie_sql::get_external_trie_offset_length(db, block_id)?;
        self.seek(SeekFrom::Start(extern_trie.offset))?;

        let mut buf = vec![0u8; extern_trie.length as usize];
        self.read_exact(&mut buf)?;

        let buffer = match extern_trie.compression {
            TrieBlobCompression::None => buf,
            TrieBlobCompression::LZ4 => lz4_decompress_size_prepended(&buf).unwrap()
        };

        Ok(buffer)
    }

    /// Vacuum the database and report the size before and after.
    ///
    /// Returns database errors.  Filesystem errors from reporting the file size change are masked.
    fn inner_post_migrate_vacuum(db: &Connection, db_path: &str) -> Result<(), Error> {
        // for fun, report the shrinkage
        let size_before_opt = fs::metadata(db_path)
            .map(|stat| Some(stat.len()))
            .unwrap_or(None);

        info!("Preemptively vacuuming the database file to free up space after copying trie blobs to a separate file");
        sql_vacuum(db)?;

        let size_after_opt = fs::metadata(db_path)
            .map(|stat| Some(stat.len()))
            .unwrap_or(None);

        match (size_before_opt, size_after_opt) {
            (Some(sz_before), Some(sz_after)) => {
                debug!("Shrank DB from {} to {} bytes", sz_before, sz_after);
            }
            _ => {}
        }

        Ok(())
    }

    /// Vacuum the database, and set up and tear down the necessary environment variables to
    /// use same parent directory for scratch space.
    ///
    /// Infallible -- any vacuum errors are masked.
    fn post_migrate_vacuum(db: &Connection, db_path: &str) {
        // set SQLITE_TMPDIR if it isn't set already
        let mut set_sqlite_tmpdir = false;
        let mut old_tmpdir_opt = None;
        if let Some(parent_path) = Path::new(db_path).parent() {
            if let Err(_) = env::var("SQLITE_TMPDIR") {
                debug!(
                    "Sqlite will store temporary migration state in '{}'",
                    parent_path.display()
                );
                env::set_var("SQLITE_TMPDIR", parent_path);
                set_sqlite_tmpdir = true;
            }

            // also set TMPDIR
            old_tmpdir_opt = env::var("TMPDIR").ok();
            env::set_var("TMPDIR", parent_path);
        }

        // don't materialize the error; just warn
        let res = TrieFile::inner_post_migrate_vacuum(db, db_path);
        if let Err(e) = res {
            warn!("Failed to VACUUM the MARF DB post-migration: {:?}", &e);
        }

        if set_sqlite_tmpdir {
            debug!("Unset SQLITE_TMPDIR");
            env::remove_var("SQLITE_TMPDIR");
        }
        if let Some(old_tmpdir) = old_tmpdir_opt {
            debug!("Restore TMPDIR to '{}'", &old_tmpdir);
            env::set_var("TMPDIR", old_tmpdir);
        } else {
            debug!("Unset TMPDIR");
            env::remove_var("TMPDIR");
        }
    }

    /// Recreates the external trie blob file, iterating through all uncompressed trie blobs
    /// and writing the new compressed value to a new file.  After this is done, the trie
    /// file will be replaced with the new compressed file.
    pub fn compress_trie_blobs<T: MarfTrieId>(
        &mut self,
        db: &Connection
    ) -> Result<(), Error>
    {
        if trie_sql::detect_partial_migration_for_schema_v3(db)? {
            panic!("PARTIAL MIGRATION DETECTED! This is an irrecoverable error. You will need to restart your node from genesis.");
        }

        let max_block = trie_sql::count_blocks(db)?;
        info!(
            "Compress {} blocks in external blob storage at {}",
            max_block,
            &self.get_path()
        );

        let check = trie_sql::get_uncompressed_external_trie_blobs(db, 1)?;
        if check.len() == 0
        {
            eprintln!("No uncompressed blobs were found.");
            trie_sql::set_migrated(db)?;
            return Ok(())
        }

        
        let tmp_path = format!("{}.v3", self.get_path());
        eprintln!("Creating new TrieFile on disk: {}", tmp_path);
        let mut v3_file = Self::new_disk(&tmp_path, false)?;
        eprintln!("File created.");

        loop { 
            let results = trie_sql::get_uncompressed_external_trie_blobs(db, 1000)?;
            if results.len() == 0 {
                trie_sql::set_migrated(db)?;
                break;
            }

            let first = results.first().unwrap();
            let last = results.last().unwrap();

            info!("Compressing blocks {} to {} (of {})", first.block_id, last.block_id, max_block);

            for extern_trie in results {
                self.seek(SeekFrom::Start(extern_trie.offset))?;

                let mut buf = vec![0u8; extern_trie.length as usize];
                self.read_exact(&mut buf)?;

                let blob_storage_result = v3_file.append_trie_blob(db, buf.as_slice())?;
                
                trie_sql::update_external_trie_blob_after_compression(db,
                    extern_trie.block_id, 
                    blob_storage_result.offset, 
                    blob_storage_result.storage_size as u64, 
                    blob_storage_result.compression.unwrap().compression_algorithm)?;
            }
        }

        Ok(())
    }

    /// Copy the trie blobs out of a sqlite3 DB into their own file.
    /// NOTE: this is *not* thread-safe.  Do not call while the DB is being used by another thread.
    pub fn export_trie_blobs<T: MarfTrieId>(
        &mut self,
        db: &Connection,
        db_path: &str,
    ) -> Result<(), Error> {
        if trie_sql::detect_partial_migration_for_schema_v2(db)? {
            panic!("PARTIAL MIGRATION DETECTED! This is an irrecoverable error. You will need to restart your node from genesis.");
        }

        let max_block = trie_sql::count_blocks(db)?;
        info!(
            "Migrate {} blocks to external blob storage at {}",
            max_block,
            &self.get_path()
        );

        for block_id in 0..(max_block + 1) {
            match trie_sql::is_unconfirmed_block(db, block_id) {
                Ok(true) => {
                    test_debug!("Skip block_id {} since it's unconfirmed", block_id);
                    continue;
                }
                Err(Error::NotFoundError) => {
                    test_debug!("Skip block_id {} since it's not a block", block_id);
                    continue;
                }
                Ok(false) => {
                    // get the blob
                    let trie_blob = TrieFile::read_trie_blob_from_db(db, block_id)?;

                    // get the block ID
                    let bhh: T = trie_sql::get_block_hash(db, block_id)?;

                    // append the blob, replacing the current trie blob
                    if block_id % 1000 == 0 {
                        info!(
                            "Migrate block {} ({} of {}) to external blob storage",
                            &bhh, block_id, max_block
                        );
                    }

                    // append directly to file, so we can get the true offset
                    self.seek(SeekFrom::End(0))?;
                    let offset = self.stream_position()?;

                    let compression_result = Self::compress_blob(&trie_blob)?;
                    let compressed = &compression_result.compressed_bytes;

                    test_debug!("Write trie of {} (uncompressed) and {} (compressed) bytes at {}", &trie_blob.len(), compressed.len(), offset);

                    self.write_all(&compressed)?;
                    self.flush()?;

                    test_debug!("Stored trie blob {} to offset {}", bhh, offset);
                    trie_sql::update_external_trie_blob(
                        db,
                        &bhh,
                        offset,
                        compressed.len() as u64,
                        Some(compression_result),
                        block_id,
                    )?;
                }
                Err(e) => {
                    test_debug!(
                        "Failed to determine if {} is unconfirmed: {:?}",
                        block_id,
                        &e
                    );
                    return Err(e);
                }
            }
        }

        TrieFile::post_migrate_vacuum(db, db_path);

        debug!("Mark MARF trie migration of '{}' as finished", db_path);
        trie_sql::set_migrated(db).expect("FATAL: failed to mark DB as migrated");
        Ok(())
    }
}

/// NodeHashReader for TrieFile
pub struct TrieFileNodeHashReader<'a> {
    db: &'a Connection,
    file: &'a mut TrieFile,
    block_id: u32,
}

impl<'a> TrieFileNodeHashReader<'a> {
    pub fn new(
        db: &'a Connection,
        file: &'a mut TrieFile,
        block_id: u32,
    ) -> TrieFileNodeHashReader<'a> {
        TrieFileNodeHashReader { db, file, block_id }
    }
}

impl NodeHashReader for TrieFileNodeHashReader<'_> {
    fn read_node_hash_bytes<W: Write>(&mut self, ptr: &TriePtr, w: &mut W) -> Result<(), Error> {
        self.file
            .seek(SeekFrom::Start(ptr.ptr() as u64))?;
            //.seek(SeekFrom::Start(trie_offset + (ptr.ptr() as u64)))?;
        let hash_buff = read_hash_bytes(self.file)?;
        w.write_all(&hash_buff).map_err(|e| e.into())
    }
}

impl TrieFileDisk {
    pub fn get_trie_offset(&mut self, db: &Connection, block_id: u32) -> Result<TrieIdOffset, Error> {
        let cached_offset = self.trie_offsets.get(&block_id);

        match cached_offset {
            Some(offset) => Ok(TrieIdOffset { offset: offset.offset, length: offset.length }),
            None => {
                let extern_trie = trie_sql::get_external_trie_offset_length(db, block_id)?;
                let offset = TrieIdOffset { offset: extern_trie.offset, length: extern_trie.length };
                self.trie_offsets.insert(block_id, offset);
                Ok(offset)
            }
        }
    }

    pub fn load_trie_blob(&mut self, db: &Connection, block_id: u32) -> Result<(), Error> {
        // If the specified block_id is the currently loaded block, simply return.
        if let Some(current_block_id) = self.current_block_id {
            if current_block_id == block_id {
                return Ok(());
            }
        }

        // Check the LRU cache for the specified block.  If found, set the loaded trie
        // to the cached version instead of reading from disk.
        if let Some(cached_trie) = self.decompressed_lru.get(&block_id) {
            self.current_block_id = Some(block_id);
            self.current_trie = Some(Cursor::new(cached_trie.to_vec()));
            return Ok(());
        }

        // We must retrieve the trie from disk.  Retrieve the trie offset+length from the index DB,
        // read the full contents of the trie, decompress it, cache it in the LRU, and set
        // the currently loaded trie.

        let bench_start = SystemTime::now();
        let extern_trie = self.get_trie_offset(db, block_id)?;

        self.seek(SeekFrom::Start(extern_trie.offset))?;
        let mut take_adapter = self.take(extern_trie.length);
        let buf= &mut Vec::<u8>::new();
        take_adapter.read_to_end(buf)?;

        let decompressed = lz4_decompress_size_prepended(buf.as_slice()).unwrap();
        self.decompressed_lru.put(block_id, decompressed.clone());

        self.current_block_id = Some(block_id);
        self.current_trie = Some(Cursor::new(decompressed));

        let bench_elapsed = bench_start.elapsed();
        eprintln!("Loaded trie blob with block id {} in {:?}", &block_id, bench_elapsed);
        
        Ok(())
    }
}

impl TrieFileRAM {
    pub fn get_trie_offset(&mut self, db: &Connection, block_id: u32) -> Result<TrieIdOffset, Error> {
        if let Some(cached) = self.trie_offsets.get(&block_id) {
            Ok(TrieIdOffset { offset: cached.offset, length: cached.length })
        } else {
            let extern_trie = trie_sql::get_external_trie_offset_length(db, block_id)?;
            let offset = TrieIdOffset { offset: extern_trie.offset, length: extern_trie.length };
            self.trie_offsets.insert(block_id, offset);
            Ok(offset)
        }
    }
}

impl TrieFile {
    /// Determine the file offset in the TrieFile where a serialized trie starts.
    /// The offsets are stored in the given DB, and are cached indefinitely once loaded.
    pub fn get_trie_offset(&mut self, db: &Connection, block_id: u32) -> Result<TrieIdOffset, Error> {
        match self {
            TrieFile::Disk(disk) => disk.get_trie_offset(db, block_id),
            TrieFile::RAM(ram) => ram.get_trie_offset(db, block_id),
        }
    }

    /// Obtain a TrieHash for a node, given its block ID and pointer
    pub fn get_node_hash_bytes(
        &mut self,
        db: &Connection,
        block_id: u32,
        ptr: &TriePtr,
    ) -> Result<TrieHash, Error> {
        match self {
            TrieFile::RAM(_) => {
                self.seek_to(db, block_id, ptr)?;
                let hash_buff = read_hash_bytes(self)?;
                Ok(TrieHash(hash_buff))
            },
            TrieFile::Disk(disk) => {
                disk.load_trie_blob(db, block_id)?;
                let blob = disk.current_trie.as_mut().unwrap();
                blob.seek(SeekFrom::Start(ptr.ptr() as u64))?;
                let hash_buff = read_hash_bytes(blob)?;
                Ok(TrieHash(hash_buff))
            }
        }
        
    }

    /// Obtain a TrieNodeType and its associated TrieHash for a node, given its block ID and
    /// pointer
    pub fn read_node_type(
        &mut self,
        db: &Connection,
        block_id: u32,
        ptr: &TriePtr,
    ) -> Result<(TrieNodeType, TrieHash), Error> {
        match self {
            TrieFile::RAM(ram) => {
                let trie = ram.get_trie_offset(db, block_id)?;
                ram.seek(SeekFrom::Start(trie.offset + (ptr.ptr() as u64)))?;
                read_nodetype_at_head(ram, ptr.id())
            },
            TrieFile::Disk(disk) => {
                disk.load_trie_blob(db, block_id)?;
                let blob = disk.current_trie.as_mut().unwrap();
                blob.seek(SeekFrom::Start(ptr.ptr() as u64))?;
                read_nodetype_at_head(blob, ptr.id())
            }
        }
    }

    fn seek_to(&mut self, db: &Connection, block_id: u32, ptr: &TriePtr) -> Result<(), Error> {
        let trie = self.get_trie_offset(db, block_id)?;
        match self {
            TrieFile::RAM(_) => { self.seek(SeekFrom::Start(trie.offset + (ptr.ptr() as u64)))?; },
            TrieFile::Disk(disk) => {
                disk.load_trie_blob(db, block_id)?;
                let cursor = disk.current_trie.as_mut().unwrap();
                cursor.seek(SeekFrom::Start(ptr.ptr() as u64))?; 
            }
        }
        Ok(())
    }

    /// Obtain a TrieNodeType, given its block ID and pointer
    pub fn read_node_type_nohash(
        &mut self,
        db: &Connection,
        block_id: u32,
        ptr: &TriePtr,
    ) -> Result<TrieNodeType, Error> {

        match self {
            TrieFile::Disk(disk) => { 
                disk.load_trie_blob(db, block_id)?;
                let trie = disk.current_trie.as_mut().unwrap();
                trie.seek(SeekFrom::Start(ptr.ptr() as u64))?;
                read_nodetype_at_head_nohash(trie, ptr.id())
            },
            _ => { 
                self.seek_to(db, block_id, ptr)?; 
                read_nodetype_at_head_nohash(self, ptr.id())
            }
        }
    }

    /// Obtain a TrieHash for a node, given the node's block's hash (used only in testing)
    #[cfg(test)]
    pub fn get_node_hash_bytes_by_bhh<T: MarfTrieId>(
        &mut self,
        db: &Connection,
        bhh: &T,
        ptr: &TriePtr,
    ) -> Result<TrieHash, Error> {
        let (offset, _length) = trie_sql::get_external_trie_offset_length_by_bhh(db, bhh)?;
        let block_id = trie_sql::get_block_identifier(db, bhh)?;

        match self {
            TrieFile::Disk(disk) => {
                disk.load_trie_blob(db, block_id)?;
                let blob = disk.current_trie.as_mut().unwrap();
                blob.seek(SeekFrom::Start(ptr.ptr() as u64))?;
                let hash_buff = read_hash_bytes(blob)?;
                Ok(TrieHash(hash_buff))
            },
            TrieFile::RAM(ram) => {
                self.seek(SeekFrom::Start(offset + (ptr.ptr() as u64)))?;
                let hash_buff = read_hash_bytes(self)?;
                Ok(TrieHash(hash_buff))
            }
        }
    }

    /// Get all (root hash, trie hash) pairs for this TrieFile
    #[cfg(test)]
    pub fn read_all_block_hashes_and_roots<T: MarfTrieId>(
        &mut self,
        db: &Connection,
    ) -> Result<Vec<(TrieHash, T)>, Error> {

        let mut s =
            db.prepare("SELECT block_hash, external_offset, external_length FROM marf_data WHERE unconfirmed = 0 ORDER BY block_hash")?;

        let start = TrieStorageConnection::<T>::root_ptr_disk() as u64;

        let rows = s.query_and_then(NO_PARAMS, |row| {
            let block_hash: T = row.get_unwrap("block_hash");
            let offset_i64: i64 = row.get_unwrap("external_offset");
            let length_i64: i64 = row.get_unwrap("external_length");
            let length = length_i64 as u64;
            let offset = offset_i64 as u64;

            //eprintln!("block_hash: {}, offset:{}, start: {}, length: {}",
            //    block_hash, offset, start, length);

            let root_hash = match self {
                TrieFile::RAM(ram) => {
                    ram.seek(SeekFrom::Start(offset + start))?;
                    let hash_buff = read_hash_bytes(ram)?;
                    TrieHash(hash_buff)
                },
                TrieFile::Disk(disk) => {
                    disk.seek(SeekFrom::Start(offset))?;
                    let mut take_adapter = self.take(length);
                    let buf= &mut Vec::<u8>::new();
                    take_adapter.read_to_end(buf)?;
                    //eprintln!("take_adapter length: {}", &buf.len());
                    let decompressed = lz4_decompress_size_prepended(buf.as_slice()).unwrap();
                    //eprintln!("decompressed: {:02X?}", decompressed); 
                    let mut cursor = Cursor::new(decompressed);
                    cursor.seek(SeekFrom::Start(start))?;
                    let hash_buff = read_hash_bytes(&mut cursor)?;
                    //eprintln!("hash_buff: {:02X?}", hash_buff);
                    TrieHash(hash_buff)
                }
            };

            trace!(
                "Root hash for block {} at offset {} is {}",
                &block_hash,
                offset + start,
                &root_hash
            );
            Ok((root_hash, block_hash))
        })?;

        rows.collect()
    }

    /// Compresses a trie blob
    fn compress_blob(buf: &[u8]) -> Result<BlobCompressionResult, Error> {
        // Compress the blob
        let compressed = lz4_compress_prepend_size(buf);
        let compressed_blob_size = compressed.len();

        Ok(BlobCompressionResult {
            compressed_bytes: compressed,
            compressed_blob_size,
            compression_algorithm: TrieBlobCompression::LZ4
        })
    }

    /// Append a serialized and compressed trie to the TrieFile.
    /// Returns the offset at which it was appended.
    pub fn append_trie_blob(&mut self, db: &Connection, buf: &[u8]) -> Result<BlobStorageResult, Error> {
        let offset = trie_sql::get_external_blobs_length(db)?;
        self.seek(SeekFrom::Start(offset))?;

        let result = match self {
            TrieFile::RAM(ram) => {
                ram.fd.write_all(buf)?;
                ram.fd.flush()?;
                BlobStorageResult {
                    offset,
                    uncompressed_blob_size: buf.len(),
                    storage_size: buf.len(),
                    compression: None
                }
            },
            TrieFile::Disk(disk) => {
                let compression_bench = SystemTime::now();
                let compression_result = Self::compress_blob(buf)?;
                let compression_elapsed = compression_bench.elapsed().unwrap();
                let compressed = &compression_result.compressed_bytes;

                eprintln!("Write trie of {} (uncompressed) and {} (compressed) bytes at {}. Compression time {:?}", 
                    buf.len(), 
                    compressed.len(), 
                    offset, 
                    compression_elapsed);

                disk.fd.seek(SeekFrom::Start(offset))?;
                disk.fd.write_all(compressed)?;
                disk.fd.flush()?;
                disk.fd.sync_data()?;

                BlobStorageResult {
                    offset,
                    uncompressed_blob_size: buf.len(),
                    storage_size: compressed.len(),
                    compression: Some(compression_result)
                }
            }
        };

        Ok(result)
    }
}

/// Boilerplate Write implementation for TrieFileDisk.  Plumbs through to the inner fd.
impl Write for TrieFileDisk {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.fd.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.fd.flush()
    }
}

/// Boilerplate Write implementation for TrieFileRAM.  Plumbs through to the inner fd.
impl Write for TrieFileRAM {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.fd.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.fd.flush()
    }
}

/// Boilerplate Write implementation for TrieFile enum.  Plumbs through to the inner struct.
impl Write for TrieFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            TrieFile::RAM(ref mut ram) => ram.write(buf),
            TrieFile::Disk(ref mut disk) => disk.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            TrieFile::RAM(ref mut ram) => ram.flush(),
            TrieFile::Disk(ref mut disk) => disk.flush(),
        }
    }
}

/// Boilerplate Read implementation for TrieFileDisk.  Plumbs through to the inner fd.
impl Read for TrieFileDisk {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        //eprintln!("Read for TrieFileDisk - buf len: {}", &buf.len());
        self.fd.read(buf)
    }
}

/// Boilerplate Read implementation for TrieFileRAM.  Plumbs through to the inner fd.
impl Read for TrieFileRAM {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        //eprintln!("Read for TrieFileRAM - buf len: {}", &buf.len());
        self.fd.read(buf)
    }
}

/// Boilerplate Read implementation for TrieFile enum.  Plumbs through to the inner struct.
impl Read for TrieFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            TrieFile::RAM(ref mut ram) => ram.read(buf),
            TrieFile::Disk(ref mut disk) => disk.read(buf),
        }
    }
}

/// Boilerplate Seek implementation for TrieFileDisk.  Plumbs through to the inner fd
impl Seek for TrieFileDisk {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.fd.seek(pos)
    }
}

/// Boilerplate Seek implementation for TrieFileDisk.  Plumbs through to the inner fd
impl Seek for TrieFileRAM {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.fd.seek(pos)
    }
}

impl Seek for TrieFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            TrieFile::RAM(ref mut ram) => ram.seek(pos),
            TrieFile::Disk(ref mut disk) => disk.seek(pos),
        }
    }
}