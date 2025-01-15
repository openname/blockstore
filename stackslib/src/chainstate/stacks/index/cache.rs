// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2022 Stacks Open Internet Foundation
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
use std::hash::{Hash, Hasher};
use std::io::{BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{cmp, env, error, fmt, fs, io, os};

use rusqlite::types::{FromSql, ToSql};
use rusqlite::{
    Connection, Error as SqliteError, ErrorCode as SqliteErrorCode, OpenFlags, OptionalExtension,
    Transaction,
};
use stacks_common::types::chainstate::{
    BlockHeaderHash, TrieHash, BLOCK_HEADER_HASH_ENCODED_SIZE, TRIEHASH_ENCODED_SIZE,
};
use stacks_common::types::sqlite::NO_PARAMS;

use crate::chainstate::stacks::index::bits::{
    get_node_byte_len, get_node_hash, read_block_identifier, read_hash_bytes, read_node_hash_bytes,
    read_nodetype, read_root_hash, write_nodetype_bytes,
};
use crate::chainstate::stacks::index::node::{
    clear_backptr, is_backptr, set_backptr, TrieNode, TrieNode16, TrieNode256, TrieNode4,
    TrieNode48, TrieNodeID, TrieNodeType, TriePtr,
};
use crate::chainstate::stacks::index::{trie_sql, ClarityMarfTrieId, Error, MarfTrieId, TrieLeaf};
use crate::util_lib::db::{
    sql_pragma, sqlite_open, tx_begin_immediate, tx_busy_handler, Error as db_error,
    SQLITE_MMAP_SIZE,
};

/// Fully-qualified address of a Trie node.  Includes both the block's blob rowid and the pointer within the
/// block's blob as to where it is stored.
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct TrieNodeAddr(u32, TriePtr);

/// Cache state for all node caching strategies.
pub struct TrieCacheState<T: MarfTrieId> {
    /// Mapping between trie blob IDs (i.e. rowids) and the MarfTrieId of the trie.  Contents are
    /// never evicted, since the size of this map grows only at the rate of new Stacks blocks.
    block_hash_cache: HashMap<u32, T>,

    /// Mapping between trie blob hashes and their IDs
    block_id_cache: HashMap<T, u32>,

    /// cached nodes
    node_cache: HashMap<TrieNodeAddr, TrieNodeType>,
    /// cached trie root hashes
    hash_cache: HashMap<TrieNodeAddr, TrieHash>,
}

impl<T: MarfTrieId> TrieCacheState<T> {
    pub fn new() -> TrieCacheState<T> {
        TrieCacheState {
            block_hash_cache: HashMap::new(),
            block_id_cache: HashMap::new(),
            node_cache: HashMap::new(),
            hash_cache: HashMap::new(),
        }
    }

    /// Obtain a possibly-cached node and its hash.
    /// Only return data if we have *both* the node and hash
    pub fn load_node_and_hash(
        &self,
        block_id: u32,
        trieptr: &TriePtr,
    ) -> Option<(TrieNodeType, TrieHash)> {
        match (
            self.load_node(block_id, trieptr),
            self.load_node_hash(block_id, trieptr),
        ) {
            (Some(node), Some(hash)) => Some((node, hash)),
            _ => None,
        }
    }

    /// Obtain a possibly-cached node
    pub fn load_node(&self, block_id: u32, trieptr: &TriePtr) -> Option<TrieNodeType> {
        self.node_cache
            .get(&TrieNodeAddr(block_id, trieptr.clone()))
            .cloned()
    }

    /// Obtain a possibly-cached node hash
    pub fn load_node_hash(&self, block_id: u32, trieptr: &TriePtr) -> Option<TrieHash> {
        self.hash_cache
            .get(&TrieNodeAddr(block_id, trieptr.clone()))
            .cloned()
    }

    /// Cache a node and hash
    pub fn store_node_and_hash(
        &mut self,
        block_id: u32,
        trieptr: TriePtr,
        node: TrieNodeType,
        hash: TrieHash,
    ) {
        self.store_node(block_id, trieptr.clone(), node);
        self.store_node_hash(block_id, trieptr, hash)
    }

    /// Cache just a node
    pub fn store_node(&mut self, block_id: u32, trieptr: TriePtr, node: TrieNodeType) {
        self.node_cache
            .insert(TrieNodeAddr(block_id, trieptr), node);
    }

    /// Cache just a node hash
    pub fn store_node_hash(&mut self, block_id: u32, trieptr: TriePtr, hash: TrieHash) {
        self.hash_cache
            .insert(TrieNodeAddr(block_id, trieptr), hash);
    }

    /// Load up a block hash, given its ID
    pub fn load_block_hash(&self, block_id: u32) -> Option<T> {
        self.block_hash_cache.get(&block_id).cloned()
    }

    /// Cache a block hash, given its ID
    pub fn store_block_hash(&mut self, block_id: u32, block_hash: T) {
        assert!(!self.block_hash_cache.contains_key(&block_id));
        self.block_id_cache.insert(block_hash.clone(), block_id);
        self.block_hash_cache.insert(block_id, block_hash);
    }

    /// Get an immutable reference to a block hash, given the ID
    pub fn ref_block_hash(&self, block_id: u32) -> Option<&T> {
        self.block_hash_cache.get(&block_id)
    }

    /// Get the block ID, given its hash
    pub fn load_block_id(&self, block_hash: &T) -> Option<u32> {
        self.block_id_cache.get(block_hash).copied()
    }
}

/// Trie node cache strategies
pub enum TrieCache<T: MarfTrieId> {
    /// Do nothing
    Noop(TrieCacheState<T>),
    /// Cache every node in RAM
    Everything(TrieCacheState<T>),
    /// Cache only TrieNode256's
    Node256(TrieCacheState<T>),
}

impl<T: MarfTrieId> TrieCache<T> {
    /// Instantiate the default strategy.  This can be taken from the `STACKS_MARF_CACHE_STRATEGY`
    /// environ, or failing that, it will use a no-op strategy.
    pub fn default() -> TrieCache<T> {
        if let Ok(strategy) = std::env::var("STACKS_MARF_CACHE_STRATEGY") {
            TrieCache::new(&strategy)
        } else {
            TrieCache::Noop(TrieCacheState::new())
        }
    }

    /// Make a new cache strategy.
    /// `strategy` must be one of "noop", "everything", or "node256".
    /// Any other option causes a runtime panic.
    pub fn new(strategy: &str) -> TrieCache<T> {
        match strategy {
            "noop" => TrieCache::Noop(TrieCacheState::new()),
            "everything" => TrieCache::Everything(TrieCacheState::new()),
            "node256" => TrieCache::Node256(TrieCacheState::new()),
            _ => {
                error!(
                    "Unsupported trie node cache strategy '{}'; falling back to `Noop` strategy",
                    strategy
                );
                TrieCache::Noop(TrieCacheState::new())
            }
        }
    }

    /// Get the inner trie cache state, as an immutable reference
    fn state_ref(&self) -> &TrieCacheState<T> {
        match self {
            TrieCache::Noop(ref state) => state,
            TrieCache::Everything(ref state) => state,
            TrieCache::Node256(ref state) => state,
        }
    }

    /// Get the inner trie cache state, as a mutable reference
    fn state_mut(&mut self) -> &mut TrieCacheState<T> {
        match self {
            TrieCache::Noop(ref mut state) => state,
            TrieCache::Everything(ref mut state) => state,
            TrieCache::Node256(ref mut state) => state,
        }
    }

    /// Load a node from the cache, given its block ID and trie pointer within the block.
    pub fn load_node(&mut self, block_id: u32, trieptr: &TriePtr) -> Option<TrieNodeType> {
        if let TrieCache::Noop(_) = self {
            None
        } else {
            self.state_mut().load_node(block_id, trieptr)
        }
    }

    /// Load both a node and its hash, given its block ID and trie pointer within the block.
    /// Returns None if either the hash or the node are missing -- both must be cached.
    pub fn load_node_and_hash(
        &mut self,
        block_id: u32,
        trieptr: &TriePtr,
    ) -> Option<(TrieNodeType, TrieHash)> {
        if let TrieCache::Noop(_) = self {
            None
        } else {
            self.state_mut().load_node_and_hash(block_id, trieptr)
        }
    }

    /// Load a node's hash, given its node's block ID and trie pointer within the block.
    pub fn load_node_hash(&mut self, block_id: u32, trieptr: &TriePtr) -> Option<TrieHash> {
        if let TrieCache::Noop(_) = self {
            None
        } else {
            self.state_mut().load_node_hash(block_id, trieptr)
        }
    }

    /// Store a node and its hash to the cache.  `trieptr` must NOT be a backpointer
    pub fn store_node_and_hash(
        &mut self,
        block_id: u32,
        trieptr: TriePtr,
        node: TrieNodeType,
        hash: TrieHash,
    ) {
        assert!(!is_backptr(trieptr.id()));
        match self {
            TrieCache::Noop(_) => {}
            TrieCache::Everything(ref mut state) => {
                state.store_node_and_hash(block_id, trieptr, node, hash);
            }
            TrieCache::Node256(ref mut state) => match node {
                TrieNodeType::Node256(data) => {
                    state.store_node_and_hash(block_id, trieptr, TrieNodeType::Node256(data), hash);
                }
                _ => {}
            },
        }
    }

    /// Store a node to the cache.  `trieptr` must NOT be a backpointer
    pub fn store_node(&mut self, block_id: u32, trieptr: TriePtr, node: TrieNodeType) {
        assert!(!is_backptr(trieptr.id()));
        match self {
            TrieCache::Noop(_) => {}
            TrieCache::Everything(ref mut state) => state.store_node(block_id, trieptr, node),
            TrieCache::Node256(ref mut state) => match node {
                TrieNodeType::Node256(data) => {
                    state.store_node(block_id, trieptr, TrieNodeType::Node256(data))
                }
                _ => {}
            },
        }
    }

    /// Store a node's hash to the cache.  `trieptr` must NOT be a backpointer
    pub fn store_node_hash(&mut self, block_id: u32, trieptr: TriePtr, hash: TrieHash) {
        assert!(!is_backptr(trieptr.id()));
        match self {
            TrieCache::Noop(_) => {
                trace!(
                    "Noop node hash cache store for ({},{:?},{})",
                    block_id,
                    &trieptr,
                    &hash
                );
            }
            TrieCache::Everything(ref mut state) => {
                state.store_node_hash(block_id, trieptr, hash);
            }
            TrieCache::Node256(ref mut state) => match trieptr.id {
                x if x == TrieNodeID::Node256 as u8 => {
                    state.store_node_hash(block_id, trieptr, hash);
                }
                _ => {}
            },
        }
    }

    /// Load a block's hash, given its block ID.
    pub fn load_block_hash(&mut self, block_id: u32) -> Option<T> {
        self.state_mut().load_block_hash(block_id)
    }

    /// Store a block's ID and hash to teh cache.
    pub fn store_block_hash(&mut self, block_id: u32, block_hash: T) {
        self.state_mut().store_block_hash(block_id, block_hash)
    }

    /// Get an immutable reference to the block hash, given its ID
    pub fn ref_block_hash(&self, block_id: u32) -> Option<&T> {
        self.state_ref().ref_block_hash(block_id)
    }

    /// Get the block ID, given the block hash
    pub fn load_block_id(&self, block_hash: &T) -> Option<u32> {
        self.state_ref().load_block_id(block_hash)
    }
}

#[cfg(test)]
pub mod test {
    use std::collections::VecDeque;
    use std::fs;
    use std::time::SystemTime;

    use rand::{thread_rng, Rng};
    use sha2::Digest;
    use stacks_common::util::hash::Sha512Trunc256Sum;

    use super::*;
    use crate::chainstate::stacks::index::marf::*;
    use crate::chainstate::stacks::index::node::*;
    use crate::chainstate::stacks::index::storage::*;
    use crate::chainstate::stacks::index::*;

    /// Deterministic random keys to insert
    pub fn make_test_insert_data(
        num_inserts_per_block: u64,
        num_blocks: u64,
    ) -> Vec<Vec<(String, MARFValue)>> {
        let mut data = vec![0u8; 32];
        let mut ret = vec![];

        for blk in 0..num_blocks {
            let mut block_data = vec![];
            test_debug!("Make block {}", blk);
            for val in 0..num_inserts_per_block {
                let path_bytes = Sha512Trunc256Sum::from_data(&data).as_bytes().to_vec();
                data.copy_from_slice(&path_bytes[0..32]);

                let path = to_hex(&path_bytes);

                let value_bytes = Sha512Trunc256Sum::from_data(&data).as_bytes().to_vec();
                data.copy_from_slice(&value_bytes[0..32]);

                let mut value_bytes_slice = [0u8; 40];
                value_bytes_slice[0..32].copy_from_slice(&value_bytes);

                let value = MARFValue(value_bytes_slice);
                block_data.push((path, value));
            }
            ret.push(block_data);
        }
        ret
    }

    fn test_marf_with_cache(
        test_name: &str,
        cache_strategy: &str,
        hash_strategy: TrieHashCalculationMode,
        data: &[Vec<(String, MARFValue)>],
        batch_size: Option<usize>,
    ) -> TrieHash {
        let test_file = if test_name == ":memory:" {
            test_name.to_string()
        } else {
            let test_dir = format!("/tmp/stacks-marf-tests/{}", test_name);
            if fs::metadata(&test_dir).is_ok() {
                fs::remove_dir_all(&test_dir).unwrap();
            }
            fs::create_dir_all(&test_dir).unwrap();

            let test_file = format!(
                "{}/marf-cache-{}-{:?}.sqlite",
                &test_dir, cache_strategy, hash_strategy
            );
            test_file
        };

        let marf_opts = MARFOpenOpts::new(hash_strategy, cache_strategy, true);
        let f = TrieFileStorage::open(&test_file, marf_opts).unwrap();
        let mut marf = MARF::from_storage(f);
        let mut last_block_header = BlockHeaderHash::sentinel();
        let batch_size = batch_size.unwrap_or(0);

        for (i, block_data) in data.iter().enumerate() {
            test_debug!("Write block {}", i);
            let mut block_hash_bytes = [0u8; 32];
            block_hash_bytes[0..8].copy_from_slice(&(i as u64).to_be_bytes());

            let block_header = BlockHeaderHash(block_hash_bytes);
            marf.begin(&last_block_header, &block_header).unwrap();

            if batch_size > 0 {
                for b in (0..block_data.len()).step_by(batch_size) {
                    let batch = &block_data[b..cmp::min(block_data.len(), b + batch_size)];
                    let keys: Vec<_> = batch.iter().map(|(k, _)| k.clone()).collect();
                    let values = batch.iter().map(|(_, v)| v.clone()).collect();
                    marf.insert_batch(&keys, values).unwrap();
                }
            } else {
                for (key, value) in block_data.iter() {
                    let path = TrieHash::from_key(key);
                    let leaf = TrieLeaf::from_value(&[], value.clone());
                    marf.insert_raw(path, leaf).unwrap();
                }
            }

            marf.commit().unwrap();
            last_block_header = block_header;
        }

        let write_bench = marf.borrow_storage_backend().get_benchmarks();
        marf.borrow_storage_backend().reset_benchmarks();
        eprintln!("MARF bench writes: {:#?}", &write_bench);

        debug!("---------");
        debug!("MARF gets");
        debug!("---------");

        let mut total_read_time = 0;
        let mut root_hash = TrieHash([0u8; 32]);
        for (i, block_data) in data.iter().enumerate() {
            test_debug!("Read block {}", i);
            for (key, value) in block_data.iter() {
                let path = TrieHash::from_key(key);
                let marf_leaf = TrieLeaf::from_value(&[], value.clone());

                let read_time = SystemTime::now();
                let leaf = MARF::get_path(
                    &mut marf.borrow_storage_backend(),
                    &last_block_header,
                    &path,
                )
                .unwrap()
                .unwrap();

                let read_time = read_time.elapsed().unwrap().as_nanos();
                total_read_time += read_time;

                assert_eq!(leaf.data.to_vec(), marf_leaf.data.to_vec());
            }
        }

        let read_bench = marf.borrow_storage_backend().get_benchmarks();
        eprintln!(
            "MARF bench reads ({} total): {:#?}",
            total_read_time, &read_bench
        );

        let mut bench = write_bench.clone();
        bench.add(&read_bench);

        eprintln!("MARF bench total: {:#?}", &bench);

        root_hash = marf.get_root_hash_at(&last_block_header).unwrap();
        eprintln!("root hash at {:?}: {:?}", &last_block_header, &root_hash);
        root_hash
    }

    #[test]
    fn test_marf_node_cache_noop() {
        let test_data = make_test_insert_data(128, 128);
        let root_hash = test_marf_with_cache(
            "test_marf_node_cache_noop",
            "noop",
            TrieHashCalculationMode::Immediate,
            &test_data,
            None,
        );
        eprintln!("Final root hash is {}", root_hash);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_noop",
            "noop",
            TrieHashCalculationMode::Immediate,
            &test_data,
            Some(64),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_noop",
            "noop",
            TrieHashCalculationMode::Immediate,
            &test_data,
            Some(128),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_noop",
            "noop",
            TrieHashCalculationMode::Immediate,
            &test_data,
            Some(67),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_noop",
            "noop",
            TrieHashCalculationMode::Immediate,
            &test_data,
            Some(13),
        );
        assert_eq!(root_hash, root_hash_batched);
    }

    #[test]
    fn test_marf_node_cache_noop_deferred() {
        let test_data = make_test_insert_data(128, 128);
        let root_hash = test_marf_with_cache(
            "test_marf_node_cache_noop_deferred",
            "noop",
            TrieHashCalculationMode::Immediate,
            &test_data,
            None,
        );
        eprintln!("Final root hash is {}", root_hash);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_noop_deferred",
            "noop",
            TrieHashCalculationMode::Deferred,
            &test_data,
            None,
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_noop_deferred",
            "noop",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(64),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_noop_deferred",
            "noop",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(128),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_noop_deferred",
            "noop",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(67),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_noop_deferred",
            "noop",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(13),
        );
        assert_eq!(root_hash, root_hash_batched);
    }

    /*
    #[test]
    fn test_marf_node_cache_ram_noop_deferred() {
        let test_data = make_test_insert_data(16384, 32);
        test_marf_with_cache(
            ":memory:",
            "noop",
            TrieHashCalculationMode::Deferred,
            &test_data,
            None,
        );
    }

    #[test]
    fn test_marf_node_cache_big_noop_deferred() {
        let test_data = make_test_insert_data(16384, 32);
        test_marf_with_cache(
            "test_marf_node_cache_big_noop_deferred",
            "noop",
            TrieHashCalculationMode::Deferred,
            &test_data,
            None,
        );
    }
    */

    #[test]
    fn test_marf_node_cache_everything() {
        let test_data = make_test_insert_data(128, 128);
        let root_hash = test_marf_with_cache(
            "test_marf_node_cache_everything",
            "noop",
            TrieHashCalculationMode::Immediate,
            &test_data,
            None,
        );
        eprintln!("Final root hash is {}", root_hash);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_everything",
            "everything",
            TrieHashCalculationMode::Immediate,
            &test_data,
            Some(64),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_everything",
            "everything",
            TrieHashCalculationMode::Immediate,
            &test_data,
            Some(128),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_everything",
            "everything",
            TrieHashCalculationMode::Immediate,
            &test_data,
            Some(67),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_everything",
            "everything",
            TrieHashCalculationMode::Immediate,
            &test_data,
            Some(13),
        );
        assert_eq!(root_hash, root_hash_batched);
    }

    #[test]
    fn test_marf_node_cache_everything_deferred() {
        let test_data = make_test_insert_data(128, 128);
        let root_hash = test_marf_with_cache(
            "test_marf_node_cache_everything_deferred",
            "noop",
            TrieHashCalculationMode::Immediate,
            &test_data,
            None,
        );
        eprintln!("Final root hash is {}", root_hash);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_everything_deferred",
            "everything",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(64),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_everything_deferred",
            "everything",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(128),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_everything_deferred",
            "everything",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(67),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_everything_deferred",
            "everything",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(13),
        );
        assert_eq!(root_hash, root_hash_batched);
    }

    #[test]
    fn test_marf_node_cache_node256() {
        let test_data = make_test_insert_data(128, 128);
        let root_hash = test_marf_with_cache(
            "test_marf_node_cache_node256",
            "noop",
            TrieHashCalculationMode::Immediate,
            &test_data,
            None,
        );
        eprintln!("Final root hash is {}", root_hash);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_node256",
            "node256",
            TrieHashCalculationMode::Immediate,
            &test_data,
            Some(64),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_node256",
            "node256",
            TrieHashCalculationMode::Immediate,
            &test_data,
            Some(128),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_node256",
            "node256",
            TrieHashCalculationMode::Immediate,
            &test_data,
            Some(67),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_node256",
            "node256",
            TrieHashCalculationMode::Immediate,
            &test_data,
            Some(13),
        );
        assert_eq!(root_hash, root_hash_batched);
    }

    #[test]
    fn test_marf_node_cache_node256_deferred() {
        let test_data = make_test_insert_data(128, 128);
        let root_hash = test_marf_with_cache(
            "test_marf_node_cache_node256_deferred",
            "noop",
            TrieHashCalculationMode::Immediate,
            &test_data,
            None,
        );
        eprintln!("Final root hash is {}", root_hash);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_node256_deferred",
            "node256",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(64),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_node256_deferred",
            "node256",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(128),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_node256_deferred",
            "node256",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(67),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_node256_deferred",
            "node256",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(13),
        );
        assert_eq!(root_hash, root_hash_batched);
    }

    #[test]
    fn test_marf_node_cache_node256_deferred_15500() {
        let test_data = make_test_insert_data(15500, 10);
        let root_hash = test_marf_with_cache(
            "test_marf_node_cache_node256_deferred_15500",
            "noop",
            TrieHashCalculationMode::Immediate,
            &test_data,
            None,
        );
        eprintln!("Final root hash is {}", root_hash);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_node256_deferred_15500",
            "node256",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(64),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_node256_deferred_15500",
            "node256",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(128),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_node256_deferred_15500",
            "node256",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(67),
        );
        assert_eq!(root_hash, root_hash_batched);

        let root_hash_batched = test_marf_with_cache(
            "test_marf_node_cache_node256_deferred_15500",
            "node256",
            TrieHashCalculationMode::Deferred,
            &test_data,
            Some(13),
        );
        assert_eq!(root_hash, root_hash_batched);
    }
}
