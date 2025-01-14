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

use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::ops::DerefMut;
use std::path::PathBuf;
use std::{error, fmt, fs, io};

use rusqlite::{Connection, Transaction};
use sha2::Digest;
use stacks_common::types::chainstate::{BlockHeaderHash, TrieHash, TRIEHASH_ENCODED_SIZE};
use stacks_common::util::hash::Sha512Trunc256Sum;
use stacks_common::util::log;

use crate::chainstate::stacks::index::bits::{get_leaf_hash, get_node_hash, read_root_hash};
use crate::chainstate::stacks::index::node::{
    clear_backptr, is_backptr, set_backptr, CursorError, TrieCursor, TrieNode, TrieNode16,
    TrieNode256, TrieNode4, TrieNode48, TrieNodeID, TrieNodeType, TriePtr, TRIEPTR_SIZE,
};
use crate::chainstate::stacks::index::storage::{
    TrieFileStorage, TrieHashCalculationMode, TrieStorageConnection, TrieStorageTransaction,
};
use crate::chainstate::stacks::index::trie::Trie;
use crate::chainstate::stacks::index::{
    ClarityMarfTrieId, Error, MARFValue, MarfTrieId, TrieLeaf, TrieMerkleProof,
};
use crate::util_lib::db::Error as db_error;

pub const BLOCK_HASH_TO_HEIGHT_MAPPING_KEY: &str = "__MARF_BLOCK_HASH_TO_HEIGHT";
pub const BLOCK_HEIGHT_TO_HASH_MAPPING_KEY: &str = "__MARF_BLOCK_HEIGHT_TO_HASH";
pub const OWN_BLOCK_HEIGHT_KEY: &str = "__MARF_BLOCK_HEIGHT_SELF";

/// Merklized Adaptive-Radix Forest -- a collection of Merklized Adaptive-Radix Tries.
pub struct MARF<T: MarfTrieId> {
    storage: TrieFileStorage<T>,
    open_chain_tip: Option<WriteChainTip<T>>,
}

pub struct MarfTransaction<'a, T: MarfTrieId> {
    storage: TrieStorageTransaction<'a, T>,
    open_chain_tip: &'a mut Option<WriteChainTip<T>>,
}

#[derive(Clone)]
struct WriteChainTip<T> {
    block_hash: T,
    height: u32,
}

/// Options for opening a MARF
#[derive(Clone, Debug)]
pub struct MARFOpenOpts {
    /// Hash calculation mode for calculating a trie root hash
    pub hash_calculation_mode: TrieHashCalculationMode,
    /// Cache strategy to use
    pub cache_strategy: String,
    /// store trie blobs externally from the DB, in a flat file
    pub external_blobs: bool,
    /// unconditionally do a DB migration (used for testing)
    pub force_db_migrate: bool,
}

impl MARFOpenOpts {
    pub fn default() -> MARFOpenOpts {
        MARFOpenOpts {
            hash_calculation_mode: TrieHashCalculationMode::Deferred,
            cache_strategy: "noop".to_string(),
            external_blobs: false,
            force_db_migrate: false,
        }
    }

    pub fn new(
        hash_calculation_mode: TrieHashCalculationMode,
        cache_strategy: &str,
        external_blobs: bool,
    ) -> MARFOpenOpts {
        MARFOpenOpts {
            hash_calculation_mode,
            cache_strategy: cache_strategy.to_string(),
            external_blobs,
            force_db_migrate: false,
        }
    }

    #[cfg(test)]
    pub fn all() -> Vec<MARFOpenOpts> {
        vec![
            MARFOpenOpts::new(TrieHashCalculationMode::Immediate, "noop", false),
            MARFOpenOpts::new(TrieHashCalculationMode::Deferred, "noop", false),
            MARFOpenOpts::new(TrieHashCalculationMode::Immediate, "noop", true),
            MARFOpenOpts::new(TrieHashCalculationMode::Deferred, "noop", true),
            MARFOpenOpts::new(TrieHashCalculationMode::Immediate, "everything", false),
            MARFOpenOpts::new(TrieHashCalculationMode::Deferred, "everything", false),
            MARFOpenOpts::new(TrieHashCalculationMode::Immediate, "everything", true),
            MARFOpenOpts::new(TrieHashCalculationMode::Deferred, "everything", true),
        ]
    }
}

///
/// This trait defines functions that are defined for both
///  MARF structs and MarfTransactions
///
pub trait MarfConnection<T: MarfTrieId> {
    fn with_conn<F, R>(&mut self, exec: F) -> R
    where
        F: FnOnce(&mut TrieStorageConnection<T>) -> R;

    fn sqlite_conn(&self) -> &Connection;

    /// Get and check a value against get_from_hash
    /// (test only)
    #[cfg(test)]
    fn get_and_check_with_hash(&mut self, block_hash: &T, key: &str) {
        let res = self.with_conn(|c| MARF::get_by_key(c, block_hash, key));
        let res_with_hash =
            self.with_conn(|c| MARF::get_by_hash(c, block_hash, &TrieHash::from_key(key)));
        match (res, res_with_hash) {
            (Ok(Some(x)), Ok(Some(y))) => {
                assert_eq!(x, y);
            }
            (Ok(None), Ok(None)) => {}
            (Err(_), Err(_)) => {}
            (x, y) => {
                panic!("Inconsistency: {x:?} != {y:?}");
            }
        }
    }

    #[cfg(not(test))]
    fn get_and_check_with_hash(&mut self, _block_hash: &T, _key: &str) {}

    /// Resolve a key from the MARF to a MARFValue with respect to the given block height.
    fn get(&mut self, block_hash: &T, key: &str) -> Result<Option<MARFValue>, Error> {
        self.get_and_check_with_hash(block_hash, key);
        self.with_conn(|c| MARF::get_by_key(c, block_hash, key))
    }

    /// Resolve a TrieHash from the MARF to a MARFValue with respect to the given block height.
    fn get_from_hash(&mut self, block_hash: &T, th: &TrieHash) -> Result<Option<MARFValue>, Error> {
        self.with_conn(|c| MARF::get_by_hash(c, block_hash, th))
    }

    fn get_with_proof(
        &mut self,
        block_hash: &T,
        key: &str,
    ) -> Result<Option<(MARFValue, TrieMerkleProof<T>)>, Error> {
        self.with_conn(|conn| {
            let marf_value = match MARF::get_by_key(conn, block_hash, key)? {
                None => return Ok(None),
                Some(x) => x,
            };
            let proof = TrieMerkleProof::from_raw_entry(conn, key, &marf_value, block_hash)?;
            Ok(Some((marf_value, proof)))
        })
    }

    fn get_with_proof_from_hash(
        &mut self,
        block_hash: &T,
        hash: &TrieHash,
    ) -> Result<Option<(MARFValue, TrieMerkleProof<T>)>, Error> {
        self.with_conn(|conn| {
            let marf_value = match MARF::get_by_path(conn, block_hash, hash)? {
                None => return Ok(None),
                Some(x) => x,
            };
            let proof = TrieMerkleProof::from_path(conn, hash, &marf_value, block_hash)?;
            Ok(Some((marf_value, proof)))
        })
    }

    fn get_block_at_height(&mut self, height: u32, tip: &T) -> Result<Option<T>, Error> {
        self.with_conn(|c| MARF::get_block_at_height(c, height, tip))
    }

    fn get_block_height(&mut self, ancestor: &T, tip: &T) -> Result<Option<u32>, Error> {
        self.with_conn(|c| MARF::get_block_height(c, ancestor, tip))
    }

    /// Get the root trie hash at a particular block
    fn get_root_hash_at(&mut self, block_hash: &T) -> Result<TrieHash, Error> {
        self.with_conn(|c| c.get_root_hash_at(block_hash))
    }

    /// Check if a block can open successfully, i.e.,
    ///   it's a known block, the storage system isn't issueing IOErrors, _and_ it's in the same fork
    ///   as the current block
    /// The MARF _must_ be open to a valid block for this check to be evaluated.
    fn check_ancestor_block_hash(&mut self, bhh: &T) -> Result<(), Error> {
        self.with_conn(|conn| {
            let cur_block_hash = conn.get_cur_block();
            if cur_block_hash == *bhh {
                // a block is in its own fork
                return Ok(());
            }

            let bhh_height =
                MARF::get_block_height(conn, bhh, &cur_block_hash)?.ok_or_else(|| {
                    Error::NonMatchingForks(bhh.clone().to_bytes(), cur_block_hash.clone().to_bytes())
                })?;

            let actual_block_at_height = MARF::get_block_at_height(conn, bhh_height, &cur_block_hash)?
                .ok_or_else(|| Error::CorruptionError(format!(
                    "ERROR: Could not find block for height {}, but it was returned by MARF::get_block_height()", bhh_height)))?;

            if bhh != &actual_block_at_height {
                test_debug!("non-matching forks: {} != {}", bhh, &actual_block_at_height);
                return Err(Error::NonMatchingForks(
                    bhh.clone().to_bytes(),
                    cur_block_hash.to_bytes(),
                ));
            }

            // test open
            let result = conn.open_block(bhh);

            // restore
            conn.open_block(&cur_block_hash)
                .map_err(|e| Error::RestoreMarfBlockError(Box::new(e)))?;

            result
        })
    }
}

impl<T: MarfTrieId> MarfConnection<T> for MarfTransaction<'_, T> {
    fn with_conn<F, R>(&mut self, exec: F) -> R
    where
        F: FnOnce(&mut TrieStorageConnection<T>) -> R,
    {
        exec(&mut self.storage)
    }
    fn sqlite_conn(&self) -> &Connection {
        self.storage.sqlite_tx()
    }
}

impl<T: MarfTrieId> MarfConnection<T> for MARF<T> {
    fn with_conn<F, R>(&mut self, exec: F) -> R
    where
        F: FnOnce(&mut TrieStorageConnection<T>) -> R,
    {
        let mut conn = self.storage.connection();
        exec(&mut conn)
    }
    fn sqlite_conn(&self) -> &Connection {
        self.storage.sqlite_conn()
    }
}

///
/// MarfTransaction represents a connection to a MARF index,
///   with an open storage transaction. If this struct is
///   dropped without calling commit(), the storage transaction is
///   aborted
///
impl<'a, T: MarfTrieId> MarfTransaction<'a, T> {
    pub fn commit(mut self) -> Result<(), Error> {
        if self.storage.readonly() {
            return Err(Error::ReadOnlyError);
        }
        if let Some(_tip) = self.open_chain_tip.take() {
            self.storage.flush()?;
        }
        self.storage.commit_tx();
        Ok(())
    }

    /// Finish writing the next trie in the MARF, but change the hash of the current Trie's
    /// block hash to something other than what we opened it as.  This persists all changes.
    pub fn commit_to(mut self, real_bhh: &T) -> Result<(), Error> {
        if self.storage.readonly() {
            return Err(Error::ReadOnlyError);
        }
        if self.storage.unconfirmed() {
            return Err(Error::UnconfirmedError);
        }
        if let Some(_tip) = self.open_chain_tip.take() {
            self.storage.flush_to(real_bhh)?;
            self.storage.commit_tx();
        }
        Ok(())
    }

    /// Finish writing the next trie in the MARF -- this is used by miners
    ///   to commit the mined block, but write it to the mined_block table,
    ///   rather than out to the marf_data table (this prevents the
    ///   miner's block from getting stepped on after the sortition).
    pub fn commit_mined(mut self, bhh: &T) -> Result<(), Error> {
        if self.storage.readonly() {
            return Err(Error::ReadOnlyError);
        }
        if self.storage.unconfirmed() {
            return Err(Error::UnconfirmedError);
        }
        if let Some(_tip) = self.open_chain_tip.take() {
            self.storage.flush_mined(bhh)?;
            self.storage.commit_tx();
        }
        Ok(())
    }

    pub fn get_open_chain_tip(&self) -> Option<&T> {
        self.open_chain_tip.as_ref().map(|tip| &tip.block_hash)
    }

    pub fn get_open_chain_tip_height(&self) -> Option<u32> {
        self.open_chain_tip.as_ref().map(|tip| tip.height)
    }

    pub fn get_block_height_of(
        &mut self,
        bhh: &T,
        current_block_hash: &T,
    ) -> Result<Option<u32>, Error> {
        if Some(bhh) == self.get_open_chain_tip() {
            return Ok(self.get_open_chain_tip_height());
        } else {
            MARF::get_block_height_miner_tip(&mut self.storage, bhh, current_block_hash)
        }
    }

    #[cfg(test)]
    fn commit_tx(self) {
        self.storage.commit_tx()
    }

    pub fn sqlite_tx(&self) -> &Transaction<'a> {
        self.storage.sqlite_tx()
    }

    pub fn sqlite_tx_mut(&mut self) -> &mut Transaction<'a> {
        self.storage.sqlite_tx_mut()
    }

    /// Reopen this MARF transaction with readonly storage.
    ///   NOTE: any pending operations in the SQLite transaction _will not_
    ///         have materialized in the reopened view.
    pub fn reopen_readonly(&self) -> Result<MARF<T>, Error> {
        if self.open_chain_tip.is_some() {
            error!(
                "MARF at {} is already in the process of writing",
                &self.storage.db_path
            );
            return Err(Error::InProgressError);
        }

        let ro_storage = self.storage.reopen_readonly()?;
        Ok(MARF {
            storage: ro_storage,
            open_chain_tip: None,
        })
    }

    /// Begin writing the next trie in the MARF, given the block header hash that will contain the
    /// associated block's new state.  Call commit() or commit_to() to persist the changes.
    /// Fails if the block already exists.
    /// Storage will point to new chain tip on success.
    pub fn begin(&mut self, chain_tip: &T, next_chain_tip: &T) -> Result<(), Error> {
        if self.storage.readonly() {
            return Err(Error::ReadOnlyError);
        }
        if self.open_chain_tip.is_some() {
            return Err(Error::InProgressError);
        }
        if self.storage.has_block(next_chain_tip)? {
            error!("Block data already exists: {}", next_chain_tip);
            return Err(Error::ExistsError);
        }

        let block_height = self.inner_get_extension_height(chain_tip, next_chain_tip)?;
        MARF::extend_trie(&mut self.storage, next_chain_tip)?;
        self.inner_setup_extension(chain_tip, next_chain_tip, block_height, true)
    }

    /// Set up the trie extension we're making.
    /// Sets storage pointer to chain_tip.
    /// Returns the height next_chain_tip would be at.
    fn inner_get_extension_height(
        &mut self,
        chain_tip: &T,
        next_chain_tip: &T,
    ) -> Result<u32, Error> {
        // current chain tip must exist if it's not the "sentinel"
        let is_parent_sentinel = chain_tip == &T::sentinel();
        if !is_parent_sentinel {
            debug!("Extending off of existing node {}", chain_tip);
        } else {
            debug!("First-ever block {}", next_chain_tip; "block" => %next_chain_tip);
        }

        self.storage.open_block(chain_tip)?;

        let block_height = if !is_parent_sentinel {
            let height = MARF::get_block_height_miner_tip(&mut self.storage, chain_tip, chain_tip)?
                .ok_or(Error::CorruptionError(format!(
                    "Failed to find block height for `{:?}`",
                    chain_tip
                )))?;
            height
                .checked_add(1)
                .expect("FATAL: block height overflow!")
        } else {
            0
        };

        Ok(block_height)
    }

    /// Set up a new extension.
    /// Opens storage to chain_tip/
    fn inner_setup_extension(
        &mut self,
        chain_tip: &T,
        next_chain_tip: &T,
        block_height: u32,
        new_extension: bool,
    ) -> Result<(), Error> {
        self.storage.open_block(next_chain_tip)?;
        self.open_chain_tip.replace(WriteChainTip {
            block_hash: next_chain_tip.clone(),
            height: block_height,
        });

        if new_extension {
            self.set_block_heights(chain_tip, next_chain_tip, block_height)
                .map_err(|e| {
                    self.open_chain_tip.take();
                    e
                })?;
        }

        debug!("Opened {} to {}", chain_tip, next_chain_tip);
        Ok(())
    }

    pub fn set_block_heights(
        &mut self,
        block_hash: &T,
        next_block_hash: &T,
        height: u32,
    ) -> Result<(), Error> {
        if self.storage.readonly() {
            return Err(Error::ReadOnlyError);
        }
        let mut keys = vec![];
        let mut values = vec![];

        let height_key = format!("{}::{}", BLOCK_HEIGHT_TO_HASH_MAPPING_KEY, height);
        let hash_key = format!("{}::{}", BLOCK_HASH_TO_HEIGHT_MAPPING_KEY, next_block_hash);

        debug!(
            "Set {}::{} = {}",
            BLOCK_HEIGHT_TO_HASH_MAPPING_KEY, height, next_block_hash
        );
        debug!(
            "Set {}::{} = {}",
            BLOCK_HASH_TO_HEIGHT_MAPPING_KEY, next_block_hash, height
        );
        debug!("Set {} = {}", OWN_BLOCK_HEIGHT_KEY, height);

        keys.push(OWN_BLOCK_HEIGHT_KEY.to_string());
        values.push(MARFValue::from(height));

        keys.push(height_key);
        values.push(MARFValue::from(next_block_hash.clone()));

        keys.push(hash_key);
        values.push(MARFValue::from(height));

        if height > 0 {
            let prev_height_key = format!("{}::{}", BLOCK_HEIGHT_TO_HASH_MAPPING_KEY, height - 1);
            let prev_hash_key = format!("{}::{}", BLOCK_HASH_TO_HEIGHT_MAPPING_KEY, block_hash);

            debug!(
                "Set {}::{} = {}",
                BLOCK_HEIGHT_TO_HASH_MAPPING_KEY,
                height - 1,
                block_hash
            );
            debug!(
                "Set {}::{} = {}",
                BLOCK_HASH_TO_HEIGHT_MAPPING_KEY,
                block_hash,
                height - 1
            );

            keys.push(prev_height_key);
            values.push(MARFValue::from(block_hash.clone()));

            keys.push(prev_hash_key);
            values.push(MARFValue::from(height - 1));
        }

        self.insert_batch(&keys, values)?;
        Ok(())
    }

    /// Insert a batch of key/value pairs.  More efficient than inserting them individually, since
    /// the trie root hash will only be calculated once (which is an O(log B) operation).
    pub fn insert_batch(&mut self, keys: &[String], values: Vec<MARFValue>) -> Result<(), Error> {
        if self.storage.readonly() {
            return Err(Error::ReadOnlyError);
        }
        assert_eq!(keys.len(), values.len());

        let block_hash = match self.open_chain_tip {
            None => Err(Error::WriteNotBegunError),
            Some(WriteChainTip { ref block_hash, .. }) => Ok(block_hash.clone()),
        }?;

        if keys.is_empty() {
            return Ok(());
        }

        MARF::inner_insert_batch(&mut self.storage, &block_hash, keys, values)?;
        Ok(())
    }

    /// Begin extending the MARF to an unconfirmed trie.  The resulting trie will have a block hash
    /// equal to MARF::make_unconfirmed_block_hash(chain_tip) to avoid collision
    /// and block hash reuse.
    pub fn begin_unconfirmed(&mut self, chain_tip: &T) -> Result<T, Error> {
        if self.storage.readonly() {
            return Err(Error::ReadOnlyError);
        }
        if self.open_chain_tip.is_some() {
            return Err(Error::InProgressError);
        }
        if !self.storage.unconfirmed() {
            return Err(Error::UnconfirmedError);
        }

        // chain_tip must exist and must be confirmed
        if !self.storage.has_confirmed_block(chain_tip)? {
            error!("No such confirmed block {}", chain_tip);
            return Err(Error::NotFoundError);
        }

        let unconfirmed_tip = MARF::make_unconfirmed_chain_tip(chain_tip);

        let block_height = self.inner_get_extension_height(chain_tip, &unconfirmed_tip)?;

        let created = self.storage.extend_to_unconfirmed_block(&unconfirmed_tip)?;
        if created {
            MARF::root_copy(&mut self.storage, chain_tip)?;
        }

        self.inner_setup_extension(chain_tip, &unconfirmed_tip, block_height, created)?;
        Ok(unconfirmed_tip)
    }

    /// Drop the current trie from the MARF. This rolls back all
    ///   changes in the block, and closes the current chain tip.
    pub fn drop_current(mut self) {
        if !self.storage.readonly() {
            self.storage.drop_extending_trie();
            self.open_chain_tip.take();
            self.storage
                .open_block(&T::sentinel())
                .expect("BUG: should never fail to open the block sentinel");
            self.storage.rollback()
        }
    }

    /// Drop the current trie from the MARF, and roll back all unconfirmed state
    pub fn drop_unconfirmed(mut self) {
        if !self.storage.readonly() && self.storage.unconfirmed() {
            if let Some(tip) = self.open_chain_tip.take() {
                trace!("Dropping unconfirmed trie {}", &tip.block_hash);
                self.storage.drop_unconfirmed_trie(&tip.block_hash);
                self.storage
                    .open_block(&T::sentinel())
                    .expect("BUG: should never fail to open the block sentinel");
                // Dropping unconfirmed state cannot be done with a tx rollback,
                //   because the unconfirmed state may already have been written
                //   to the sqlite table before this transaction began
                self.storage.commit_tx()
            } else {
                trace!("drop_unconfirmed() noop");
            }
        }
    }

    /// Seal the in-RAM MARF state so that no subsequent writes will be permitted.
    /// Returns the new root hash of the MARF.
    /// Runtime-panics if the MARF was already sealed.
    pub fn seal(&mut self) -> Result<TrieHash, Error> {
        if self.storage.readonly() {
            return Err(Error::ReadOnlyError);
        }
        let root_hash = self.storage.seal()?;
        Ok(root_hash)
    }
}

// static methods
impl<T: MarfTrieId> MARF<T> {
    #[cfg(test)]
    pub fn from_storage_opened(storage: TrieFileStorage<T>, opened_to: &T) -> MARF<T> {
        MARF {
            storage,
            open_chain_tip: Some(WriteChainTip {
                block_hash: opened_to.clone(),
                height: 0,
            }),
        }
    }

    #[cfg(test)]
    pub fn begin(&mut self, chain_tip: &T, next_chain_tip: &T) -> Result<(), Error> {
        let mut tx = self.begin_tx()?;
        tx.begin(chain_tip, next_chain_tip)?;
        tx.commit_tx();
        Ok(())
    }

    #[cfg(test)]
    pub fn begin_unconfirmed(&mut self, chain_tip: &T) -> Result<T, Error> {
        let mut tx = self.begin_tx()?;
        let result = tx.begin_unconfirmed(chain_tip)?;
        tx.commit_tx();
        Ok(result)
    }

    #[cfg(test)]
    pub fn seal(&mut self) -> Result<TrieHash, Error> {
        let mut tx = self.begin_tx()?;
        let h = tx.seal()?;
        Ok(h)
    }

    // helper method for walking a node's backpr
    fn walk_backptr(
        storage: &mut TrieStorageConnection<T>,
        start_node: &TrieNodeType,
        chr: u8,
        cursor: &mut TrieCursor<T>,
    ) -> Result<(TrieNodeType, TrieHash, TriePtr, u32), Error> {
        if start_node.is_leaf() {
            panic!("Did not get an intermediate node");
        }

        let ptr_opt = start_node.walk(chr);
        match ptr_opt {
            None => {
                // this node never had a child for this chr
                trace!("Failed to walk to '{}' from {:?}", chr, start_node);
                Err(Error::BackptrNotFoundError)
            }
            Some(ptr) => {
                trace!(
                    "Walk backptrs for {:?} to {:?} from {:?}",
                    cursor,
                    &ptr,
                    &start_node
                );

                // this node had a child for this chr at one point
                let (node, node_hash, node_ptr) = Trie::walk_backptr(storage, &ptr, cursor)?;
                Ok((node, node_hash, node_ptr, ptr.back_block))
            }
        }
    }

    fn node_copy_update_ptrs(ptrs: &mut [TriePtr], child_block_id: u32) {
        for pointer in ptrs.iter_mut() {
            // if the node is empty, do nothing, if it's a back pointer,
            if pointer.id() == TrieNodeID::Empty as u8 || is_backptr(pointer.id()) {
                continue;
            } else {
                // make backptr
                pointer.back_block = child_block_id;
                pointer.id = set_backptr(pointer.id());
            }
        }
    }

    fn node_copy_update(node: &mut TrieNodeType, child_block_id: u32) -> Result<TrieHash, Error> {
        let hash = match node {
            TrieNodeType::Leaf(leaf) => get_leaf_hash(leaf),
            _ => {
                MARF::<T>::node_copy_update_ptrs(node.ptrs_mut(), child_block_id);
                TrieHash::from_data(&[])
            }
        };

        Ok(hash)
    }

    /// Given a node, and the chr of one of its children, go find the last instance of that child in
    /// the MARF and copy it forward.  Update its ptrs to point to its descendents.
    /// s must point to the block hash in which this node lives, to which the child will be copied.
    fn node_child_copy(
        storage: &mut TrieStorageConnection<T>,
        node: &TrieNodeType,
        chr: u8,
        cursor: &mut TrieCursor<T>,
    ) -> Result<(TrieNodeType, TrieHash, TriePtr, T), Error> {
        trace!(
            "Copy to {:?} child {:x} of {:?}",
            storage.get_cur_block(),
            chr,
            node
        );

        let (cur_block_hash, cur_block_id) = storage.get_cur_block_and_id();
        let (mut child_node, _, child_ptr, _) = MARF::walk_backptr(storage, node, chr, cursor)?;
        let child_block_hash = storage.get_cur_block();
        let child_block_identifier = storage.get_cur_block_identifier()?;

        // update child_node with new ptrs and hashes
        storage.open_block_maybe_id(&cur_block_hash, cur_block_id)?;
        let child_hash = MARF::<T>::node_copy_update(&mut child_node, child_block_identifier)
            .map_err(|e| Error::BlockHashMapCorruptionError(Some(Box::new(e))))?;

        // store it in this trie
        storage.open_block_maybe_id(&cur_block_hash, cur_block_id)?;
        let child_disk_ptr = storage.last_ptr()?;
        let child_ptr = TriePtr::new(child_ptr.id(), chr, child_disk_ptr);
        storage.write_nodetype(child_disk_ptr, &child_node, child_hash.clone())?;

        trace!(
            "Copied child 0x{:02x} to {:?}: ptr={:?} child={:?}",
            chr,
            &cur_block_hash,
            &child_ptr,
            &child_node
        );
        Ok((child_node, child_hash, child_ptr, child_block_hash))
    }

    /// Copy the root node from the previous Trie to this Trie, updating its ptrs.
    /// s must point to the target Trie
    fn root_copy(storage: &mut TrieStorageConnection<T>, prev_block_hash: &T) -> Result<(), Error> {
        let (cur_block_hash, cur_block_id) = storage.get_cur_block_and_id();
        storage.open_block(prev_block_hash)?;
        let prev_block_identifier = storage.get_cur_block_identifier().unwrap_or_else(|_| {
            panic!(
                "called open_block on {}, but found no identifier",
                prev_block_hash
            )
        });

        let (mut prev_root, _) = Trie::read_root(storage)?;
        let new_root_hash = MARF::<T>::node_copy_update(&mut prev_root, prev_block_identifier)?;

        storage.open_block_maybe_id(&cur_block_hash, cur_block_id)?;

        let root_ptr = storage.root_ptr();
        storage.write_nodetype(root_ptr, &prev_root, new_root_hash)?;
        Ok(())
    }

    /// create or open a particular Trie.
    /// If the trie doesn't exist, then extend it from the current Trie and create a root node that
    /// has back pointers to its immediate children in the current trie.
    /// On Ok, s will point to new_bhh and will be open for reading.
    /// Returns true/false, based on whether or not the trie will be created (this can return false
    /// if we're resuming work on an unconfirmed trie)
    pub fn extend_trie(storage: &mut TrieStorageTransaction<T>, new_bhh: &T) -> Result<(), Error> {
        if storage.readonly() {
            unreachable!("CORRUPTION: constructed read-only TrieStorageTransaction instance");
        }

        let (cur_bhh, cur_block_id) = storage.get_cur_block_and_id();
        if storage.num_blocks() == 0 || cur_bhh == T::sentinel() {
            // brand new storage
            trace!("Brand new storage -- start with {:?}", new_bhh);
            storage.extend_to_block(new_bhh)?;
            let node = TrieNode256::new(&[]);
            let hash = get_node_hash(&node, &[], storage.deref_mut());
            let root_ptr = storage.root_ptr();
            storage.write_nodetype(root_ptr, &TrieNodeType::Node256(Box::new(node)), hash)?;
            Ok(())
        } else {
            // existing storage
            match storage.open_block(new_bhh) {
                Ok(_) => {
                    trace!("Switch to Trie {:?}", new_bhh);
                    Ok(())
                }
                Err(e) => {
                    match e {
                        Error::NotFoundError => {
                            // bring root forward
                            debug!("Extend {:?} to {:?}", &cur_bhh, new_bhh);
                            storage.open_block_maybe_id(&cur_bhh, cur_block_id)?;
                            storage.extend_to_block(new_bhh)?;
                            MARF::root_copy(storage, &cur_bhh)?;
                            storage.open_block(new_bhh)?;
                            Ok(())
                        }
                        _ => Err(e),
                    }
                }
            }
        }
    }

    /// Walk down this MARF at the given block hash, doing a copy-on-write for intermediate nodes in this block's Trie from any prior Tries.
    /// s must point to the last filled-in Trie -- i.e. block_hash points to the _new_ Trie that is
    /// being filled in.
    fn walk_cow(
        storage: &mut TrieStorageTransaction<T>,
        block_hash: &T,
        path: &TrieHash,
    ) -> Result<TrieCursor<T>, Error> {
        let block_id = storage.get_block_identifier(block_hash);
        MARF::extend_trie(storage, block_hash)?;

        let mut cursor = TrieCursor::new(path, storage.root_trieptr());

        // walk to insertion point
        let mut node = Trie::read_root_nohash(storage)?;
        let mut node_ptr = TriePtr::new(0, 0, 0);

        for _ in 0..(cursor.path.len() + 1) {
            match Trie::walk_from_nohash(storage, &node, &mut cursor) {
                Ok(node_info_opt) => {
                    match node_info_opt {
                        Some((next_node_ptr, next_node)) => {
                            // end of node path.
                            // keep walking.
                            node = next_node;
                            node_ptr = next_node_ptr;
                            continue;
                        }
                        None => {
                            // end of path.  Should have found leaf.
                            if !node.is_leaf()
                                || clear_backptr(node_ptr.id()) != TrieNodeID::Leaf as u8
                            {
                                error!("Out-of-path but encountered a non-leaf");
                                return Err(Error::CorruptionError(
                                    "Non-leaf encountered at end of path".to_string(),
                                ));
                            }

                            trace!(
                                "Out of path in {:?} -- we're done. Node at {:?}",
                                storage.get_cur_block(),
                                &node_ptr
                            );
                            storage.open_block_maybe_id(block_hash, block_id)?;
                            return Ok(cursor);
                        }
                    }
                }
                Err(e) => {
                    match e {
                        Error::CursorError(cursor_error) => {
                            match cursor_error {
                                CursorError::PathDiverged => {
                                    // we're done -- path diverged.  Will need to copy-on-write
                                    // some nodes over.
                                    trace!("Path diverged -- we're done.");
                                    storage.open_block_maybe_id(block_hash, block_id)?;
                                    return Ok(cursor);
                                }
                                CursorError::ChrNotFound => {
                                    // end-of-node-path but no such child -- not even a backptr.
                                    trace!("ChrNotFound encountered at {:?} -- we're done (node not found)", storage.get_cur_block());
                                    storage.open_block_maybe_id(block_hash, block_id)?;
                                    return Ok(cursor);
                                }
                                CursorError::BackptrEncountered(ptr) => {
                                    // at intermediate node whose child is not present in this trie.
                                    // bring the child forward and take the step, if possible.
                                    storage.open_block_maybe_id(block_hash, block_id)?;
                                    let (next_node, _, next_node_ptr, next_node_block_hash) =
                                        MARF::node_child_copy(
                                            storage,
                                            &node,
                                            ptr.chr(),
                                            &mut cursor,
                                        )?;

                                    // finish taking the step
                                    cursor.repair_backptr_finish(
                                        &next_node_ptr,
                                        next_node_block_hash,
                                    );

                                    // keep walking
                                    node = next_node;
                                    node_ptr = next_node_ptr;

                                    storage.open_block_maybe_id(block_hash, block_id)?;
                                }
                            }
                        }
                        _ => {
                            // some other error (e.g. I/O error)
                            return Err(e);
                        }
                    }
                }
            }
        }

        trace!("Trie has a cycle");
        return Err(Error::CorruptionError("Trie has a cycle".to_string()));
    }

    /// Walk down this MARF at the given block hash, resolving backptrs to previous tries.
    /// Return the cursor and the last node visited.
    /// s will point to the block in which the leaf was found, or the last block visited.
    fn walk(
        storage: &mut TrieStorageConnection<T>,
        block_hash: &T,
        path: &TrieHash,
    ) -> Result<(TrieCursor<T>, TrieNodeType), Error> {
        storage.open_block(block_hash)?;

        let mut cursor = TrieCursor::new(path, storage.root_trieptr());

        // walk to insertion point
        let mut node = Trie::read_root_nohash(storage).map_err(|e| {
            test_debug!("Failed to read root of {:?}: {:?}", block_hash, &e);
            e
        })?;

        for _ in 0..(cursor.path.len() + 1) {
            storage.bench_mut().marf_walk_from_start();
            match Trie::walk_from_nohash(storage, &node, &mut cursor) {
                Ok(node_info_opt) => {
                    match node_info_opt {
                        Some((_, next_node)) => {
                            // end-of-node-path, and found a child.
                            // keep walking
                            node = next_node;
                            continue;
                        }
                        None => {
                            // end of path.  Must be at a leaf.
                            if clear_backptr(cursor.ptr().id()) != TrieNodeID::Leaf as u8 {
                                return Err(Error::CorruptionError(
                                    "Non-leaf encountered at end of path".to_string(),
                                ));
                            }

                            trace!("Cursor reached leaf {:?}", &node);
                            storage.bench_mut().marf_walk_from_finish();
                            return Ok((cursor, node));
                        }
                    }
                }
                Err(e) => {
                    match e {
                        Error::CursorError(cursor_error) => {
                            match cursor_error {
                                CursorError::PathDiverged => {
                                    // we're done -- path diverged.  No backptr-walking can help us.
                                    trace!("Path diverged -- we're done.");
                                    storage.bench_mut().marf_walk_from_finish();
                                    return Err(Error::NotFoundError);
                                }
                                CursorError::ChrNotFound => {
                                    // we're done -- end-of-node-path, but no child node.
                                    // Not even a backptr.
                                    trace!("ChrNotFound encountered -- node does not exist");
                                    storage.bench_mut().marf_walk_from_finish();
                                    return Err(Error::NotFoundError);
                                }
                                CursorError::BackptrEncountered(ptr) => {
                                    storage.bench_mut().marf_walk_backptr_start();
                                    // at intermediate node whose child is not present in this trie.
                                    // try to shunt to the prior node that has the child itself.
                                    let (next_node, _, next_node_ptr, _) =
                                        MARF::walk_backptr(storage, &node, ptr.chr(), &mut cursor)?;
                                    storage.bench_mut().marf_walk_backptr_finish();

                                    // finish taking the step
                                    cursor.repair_backptr_finish(
                                        &next_node_ptr,
                                        storage.get_cur_block(),
                                    );

                                    // keep going
                                    node = next_node;
                                    continue;
                                }
                            }
                        }
                        _ => {
                            // some other error (e.g. I/O error)
                            storage.bench_mut().marf_walk_from_finish();
                            return Err(e);
                        }
                    }
                }
            }
        }

        trace!("Trie has a cycle");
        return Err(Error::CorruptionError("Trie has a cycle".to_string()));
    }

    pub fn format(
        storage: &mut TrieStorageTransaction<T>,
        first_block_hash: &T,
    ) -> Result<(), Error> {
        if storage.readonly() {
            unreachable!("CORRUPTION: constructed read-only TrieStorageTransaction instance");
        }

        storage.format()?;
        storage.extend_to_block(first_block_hash)?;
        let node = TrieNode256::new(&[]);
        let hash = get_node_hash(&node, &[], storage.deref_mut());
        let root_ptr = storage.root_ptr();
        let node_type = TrieNodeType::Node256(Box::new(node));
        storage.write_nodetype(root_ptr, &node_type, hash)
    }

    pub fn get_path(
        storage: &mut TrieStorageConnection<T>,
        block_hash: &T,
        path: &TrieHash,
    ) -> Result<Option<TrieLeaf>, Error> {
        trace!("MARF::get_path({:?}) {:?}", block_hash, path);

        // a NotFoundError _here_ means that a block didn't exist
        storage.open_block(block_hash).map_err(|e| {
            test_debug!("Failed to open block {:?}: {:?}", block_hash, &e);
            e
        })?;

        // a NotFoundError _here_ means that the key doesn't exist in this view
        let (cursor, node) = MARF::walk(storage, block_hash, path).map_err(|e| {
            trace!(
                "Failed to look up key {:?} {:?}: {:?}",
                &block_hash,
                path,
                &e
            );
            e
        })?;

        // both of these get caught by get_by_key and turned into Ok(None)
        //   and a lot of downstream code seems to depend on that behavior, but
        //   should these two different cases be differentiable?

        if cursor.block_hashes.len() + 1 != cursor.node_ptrs.len() {
            trace!("cursor.block_hashes = {:?}", &cursor.block_hashes);
            trace!("cursor.node_ptrs = {:?}", cursor.node_ptrs);
            assert!(false);
        }

        assert!(cursor.eop());

        // out of path and reached the end.
        match node {
            TrieNodeType::Leaf(data) => {
                // found!
                return Ok(Some(data));
            }
            _ => {
                // Trie invariant violation -- a full path reached a non-leaf
                return Err(Error::CorruptionError(
                    "Path reached a non-leaf".to_string(),
                ));
            }
        }
    }

    fn do_insert_leaf(
        storage: &mut TrieStorageTransaction<T>,
        block_hash: &T,
        path: &TrieHash,
        leaf_value: &TrieLeaf,
        update_skiplist: bool,
    ) -> Result<(), Error> {
        let mut value = leaf_value.clone();
        let mut cursor = MARF::walk_cow(storage, block_hash, path)?;

        if cursor.block_hashes.len() + 1 != cursor.node_ptrs.len() {
            trace!("c.block_hashes = {:?}", &cursor.block_hashes);
            trace!("c.node_ptrs = {:?}", cursor.node_ptrs);
            assert!(false);
        }

        debug!(
            "MARF Insert in {}: '{}' = '{}' (...{:?})",
            block_hash, path, leaf_value.data, &leaf_value.path
        );

        Trie::add_value(storage, &mut cursor, &mut value)?;

        if update_skiplist {
            Trie::update_root_hash(storage, &cursor)?;
        } else {
            Trie::update_root_node_hash(storage, &cursor)?;
        }
        Ok(())
    }

    pub fn insert_leaf(
        storage: &mut TrieStorageTransaction<T>,
        block_hash: &T,
        path: &TrieHash,
        value: &TrieLeaf,
    ) -> Result<(), Error> {
        if storage.readonly() {
            unreachable!("CORRUPTION: constructed read-only TrieStorageTransaction instance");
        }
        MARF::do_insert_leaf(storage, block_hash, path, value, true)
    }

    // like insert_leaf, but don't update the merkle skiplist
    pub fn insert_leaf_in_batch(
        storage: &mut TrieStorageTransaction<T>,
        block_hash: &T,
        path: &TrieHash,
        value: &TrieLeaf,
    ) -> Result<(), Error> {
        if storage.readonly() {
            unreachable!("CORRUPTION: constructed read-only TrieStorageTransaction instance");
        }

        MARF::do_insert_leaf(storage, block_hash, path, value, false)
    }

    /// Instantiate the MARF from a TrieFileStorage instance
    pub fn from_storage(storage: TrieFileStorage<T>) -> MARF<T> {
        MARF {
            storage,
            open_chain_tip: None,
        }
    }

    /// Instantiate the MARF using a TrieFileStorage instance, from the given path on disk.
    /// This will have the side-effect of instantiating a new fork table from the tries encoded on
    /// disk. Performant code should call this method sparingly.
    pub fn from_path(path: &str, open_opts: MARFOpenOpts) -> Result<MARF<T>, Error> {
        let file_storage = TrieFileStorage::open(path, open_opts)?;
        Ok(MARF::from_storage(file_storage))
    }

    /// Instantiate an unconfirmed MARF using a TrieFileStorage instance, from the given path on disk.
    /// This will have the side-effect of instantiating a new fork table from the tries encoded on
    /// disk. Performant code should call this method sparingly.
    pub fn from_path_unconfirmed(path: &str, open_opts: MARFOpenOpts) -> Result<MARF<T>, Error> {
        let file_storage = TrieFileStorage::open_unconfirmed(path, open_opts)?;
        Ok(MARF::from_storage(file_storage))
    }

    pub fn get_by_path(
        storage: &mut TrieStorageConnection<T>,
        block_hash: &T,
        path: &TrieHash,
    ) -> Result<Option<MARFValue>, Error> {
        let (cur_block_hash, cur_block_id) = storage.get_cur_block_and_id();

        let result = MARF::get_path(storage, block_hash, &path).or_else(|e| match e {
            Error::NotFoundError => Ok(None),
            _ => Err(e),
        });

        // restore
        storage
            .open_block_maybe_id(&cur_block_hash, cur_block_id)
            .map_err(|e| {
                warn!(
                    "Failed to re-open {} {:?}: {:?}",
                    &cur_block_hash, cur_block_id, &e
                );
                warn!("Result of failed path lookup '{}': {:?}", path, &result);
                e
            })?;

        result.map(|option_result| option_result.map(|leaf| leaf.data))
    }

    /// Load up a MARF value by key, given a handle to the storage connection and a tip to work off
    /// of.
    pub fn get_by_key(
        storage: &mut TrieStorageConnection<T>,
        block_hash: &T,
        key: &str,
    ) -> Result<Option<MARFValue>, Error> {
        let (cur_block_hash, cur_block_id) = storage.get_cur_block_and_id();

        let path = TrieHash::from_key(key);

        let result = MARF::get_path(storage, block_hash, &path).or_else(|e| match e {
            Error::NotFoundError => Ok(None),
            _ => Err(e),
        });

        // restore
        storage
            .open_block_maybe_id(&cur_block_hash, cur_block_id)
            .map_err(|e| {
                warn!(
                    "Failed to re-open {} {:?}: {:?}",
                    &cur_block_hash, cur_block_id, &e
                );
                warn!("Result of failed key lookup '{}': {:?}", key, &result);
                e
            })?;

        result.map(|option_result| option_result.map(|leaf| leaf.data))
    }

    /// Load up a MARF value by TrieHash, given a handle to the storage connection and a tip to
    /// work off of.
    pub fn get_by_hash(
        storage: &mut TrieStorageConnection<T>,
        block_hash: &T,
        path: &TrieHash,
    ) -> Result<Option<MARFValue>, Error> {
        let (cur_block_hash, cur_block_id) = storage.get_cur_block_and_id();

        let result = MARF::get_path(storage, block_hash, &path).or_else(|e| match e {
            Error::NotFoundError => Ok(None),
            _ => Err(e),
        });

        // restore
        storage
            .open_block_maybe_id(&cur_block_hash, cur_block_id)
            .map_err(|e| {
                warn!(
                    "Failed to re-open {} {:?}: {:?}",
                    &cur_block_hash, cur_block_id, &e
                );
                warn!("Result of failed hash lookup '{}': {:?}", path, &result);
                e
            })?;

        result.map(|option_result| option_result.map(|leaf| leaf.data))
    }

    pub fn get_block_height_miner_tip(
        storage: &mut TrieStorageConnection<T>,
        block_hash: &T,
        current_block_hash: &T,
    ) -> Result<Option<u32>, Error> {
        let hash_key = format!("{}::{}", BLOCK_HASH_TO_HEIGHT_MAPPING_KEY, block_hash);
        #[cfg(test)]
        {
            // used in testing in order to short-circuit block-height lookups
            //   when the trie struct is tested outside of marf.rs usage
            if storage.test_genesis_block.as_ref() == Some(current_block_hash) {
                return Ok(Some(0));
            }
        }

        let marf_value = if block_hash == current_block_hash {
            MARF::get_by_key(storage, current_block_hash, OWN_BLOCK_HEIGHT_KEY)?
        } else {
            MARF::get_by_key(storage, current_block_hash, &hash_key)?
        };

        Ok(marf_value.map(u32::from))
    }

    pub fn get_block_height(
        storage: &mut TrieStorageConnection<T>,
        block_hash: &T,
        current_block_hash: &T,
    ) -> Result<Option<u32>, Error> {
        MARF::get_block_height_miner_tip(storage, block_hash, current_block_hash)
    }

    pub fn get_block_at_height(
        storage: &mut TrieStorageConnection<T>,
        height: u32,
        current_block_hash: &T,
    ) -> Result<Option<T>, Error> {
        #[cfg(test)]
        {
            // used in testing in order to short-circuit block-height lookups
            //   when the trie struct is tested outside of marf.rs usage
            if height == 0 {
                match storage.test_genesis_block {
                    Some(ref s) => return Ok(Some(s.clone())),
                    _ => {}
                }
            }
        }

        let current_block_height =
            match MARF::get_block_height(storage, current_block_hash, current_block_hash)? {
                Some(x) => x,
                None => {
                    error!(
                        "Could not fetch block height for {}, likely not a known block",
                        current_block_hash
                    );
                    return Ok(None);
                }
            };

        if height == current_block_height {
            return Ok(Some(current_block_hash.clone()));
        }

        let height_key = format!("{}::{}", BLOCK_HEIGHT_TO_HASH_MAPPING_KEY, height);

        MARF::get_by_key(storage, current_block_hash, &height_key)
            .map(|option_result| option_result.map(T::from))
    }

    /// Make an unconfirmed chain tip from an existing chain tip, so that it won't conflict with
    /// the "true" chain tip after the state it represents is later reprocessed and confirmed.
    pub fn make_unconfirmed_chain_tip(chain_tip: &T) -> T {
        let mut bytes = [0u8; 64];
        bytes[0..32].copy_from_slice(chain_tip.as_bytes());
        bytes[32..64].copy_from_slice(chain_tip.as_bytes());

        let h = Sha512Trunc256Sum::from_data(&bytes);
        let mut res_bytes = [0u8; 32];
        res_bytes[0..32].copy_from_slice(h.as_bytes());

        T::from_bytes(res_bytes)
    }

    /// Insert a batch of key/value pairs.  More efficient than inserting them individually, since
    /// the trie root hash will only be calculated once (which is an O(log B) operation).
    fn inner_insert_batch(
        conn: &mut TrieStorageTransaction<T>,
        block_hash: &T,
        keys: &[String],
        values: Vec<MARFValue>,
    ) -> Result<(), Error> {
        assert_eq!(keys.len(), values.len());

        if keys.is_empty() {
            return Ok(());
        }

        let (cur_block_hash, cur_block_id) = conn.get_cur_block_and_id();

        let last = keys.len() - 1;
        let mut progress = 0;
        let eta_enabled = keys.len() > 10_000;
        let mut result = keys[0..last]
            .iter()
            .enumerate()
            .zip(values[0..last].iter())
            .try_for_each(|((index, key), value)| {
                let marf_leaf = TrieLeaf::from_value(&[], value.clone());
                let path = TrieHash::from_key(key);

                if eta_enabled {
                    let updated_progress = 100 * index / last;
                    if updated_progress > progress {
                        progress = updated_progress;
                        info!(
                            "Batching insertions in MARF: {}% ({} out of {})",
                            progress, index, last
                        );
                    }
                }
                MARF::insert_leaf_in_batch(conn, block_hash, &path, &marf_leaf)
            });

        if result.is_ok() {
            // last insert updates the root with the skiplist hash
            let marf_leaf = TrieLeaf::from_value(&[], values[last].clone());
            let path = TrieHash::from_key(&keys[last]);
            result = MARF::insert_leaf(conn, block_hash, &path, &marf_leaf);
        }

        // restore
        conn.open_block_maybe_id(&cur_block_hash, cur_block_id)?;

        result
    }
}

// instance methods
impl<T: MarfTrieId> MARF<T> {
    pub fn begin_tx(&mut self) -> Result<MarfTransaction<'_, T>, Error> {
        let storage = self.storage.transaction()?;
        Ok(MarfTransaction {
            storage,
            open_chain_tip: &mut self.open_chain_tip,
        })
    }

    /// Target the MARF's storage at a given block.
    pub fn open_block(&mut self, block_hash: &T) -> Result<(), Error> {
        self.storage.connection().open_block(block_hash)
    }

    pub fn get_with_proof(
        &mut self,
        block_hash: &T,
        key: &str,
    ) -> Result<Option<(MARFValue, TrieMerkleProof<T>)>, Error> {
        let mut conn = self.storage.connection();
        let marf_value = match MARF::get_by_key(&mut conn, block_hash, key)? {
            None => return Ok(None),
            Some(x) => x,
        };
        let proof = TrieMerkleProof::from_raw_entry(&mut conn, key, &marf_value, block_hash)?;
        Ok(Some((marf_value, proof)))
    }

    pub fn get_with_proof_from_hash(
        &mut self,
        block_hash: &T,
        path: &TrieHash,
    ) -> Result<Option<(MARFValue, TrieMerkleProof<T>)>, Error> {
        let mut conn = self.storage.connection();
        let marf_value = match MARF::get_by_path(&mut conn, block_hash, &path)? {
            None => return Ok(None),
            Some(x) => x,
        };
        let proof = TrieMerkleProof::from_path(&mut conn, &path, &marf_value, block_hash)?;
        Ok(Some((marf_value, proof)))
    }

    pub fn get_bhh_at_height(&mut self, block_hash: &T, height: u32) -> Result<Option<T>, Error> {
        MARF::get_block_at_height(&mut self.storage.connection(), height, block_hash)
    }

    /// Insert a batch of key/value pairs.  More efficient than inserting them individually, since
    /// the trie root hash will only be calculated once (which is an O(log B) operation).
    pub fn insert_batch(&mut self, keys: &[String], values: Vec<MARFValue>) -> Result<(), Error> {
        if self.storage.readonly() {
            return Err(Error::ReadOnlyError);
        }
        assert_eq!(keys.len(), values.len());

        let block_hash = match self.open_chain_tip {
            None => Err(Error::WriteNotBegunError),
            Some(WriteChainTip { ref block_hash, .. }) => Ok(block_hash.clone()),
        }?;

        if keys.is_empty() {
            return Ok(());
        }

        let mut tx = self.storage.transaction()?;
        MARF::inner_insert_batch(&mut tx, &block_hash, keys, values)?;
        tx.commit_tx();
        Ok(())
    }

    pub fn insert(&mut self, key: &str, value: MARFValue) -> Result<(), Error> {
        if self.storage.readonly() {
            return Err(Error::ReadOnlyError);
        }
        let marf_leaf = TrieLeaf::from_value(&[], value);
        let path = TrieHash::from_key(key);
        self.insert_raw(path, marf_leaf)
    }

    /// Insert the given (key, value) pair into the MARF.  Inserting the same key twice silently
    /// overwrites the existing key.  Succeeds if there are no storage errors.
    /// Must be called after a call to .begin() (will fail otherwise)
    pub fn insert_raw(&mut self, path: TrieHash, marf_leaf: TrieLeaf) -> Result<(), Error> {
        if self.storage.readonly() {
            return Err(Error::ReadOnlyError);
        }
        match self.open_chain_tip {
            None => Err(Error::WriteNotBegunError),
            Some(WriteChainTip { ref block_hash, .. }) => {
                let mut tx = self.storage.transaction()?;
                let (cur_block_hash, cur_block_id) = tx.get_cur_block_and_id();

                let result = MARF::insert_leaf(&mut tx, block_hash, &path, &marf_leaf);

                // restore
                tx.open_block_maybe_id(&cur_block_hash, cur_block_id)?;
                tx.commit_tx();

                result
            }
        }
    }

    /// Drop the current trie from the MARF. This rolls back all
    ///   changes in the block, and closes the current chain tip.
    pub fn drop_current(&mut self) {
        if !self.storage.readonly() {
            let mut tx = self
                .storage
                .transaction()
                .expect("BUG: failed to start transaction to drop trie");
            tx.drop_extending_trie();
            self.open_chain_tip.take();
            tx.open_block(&T::sentinel())
                .expect("BUG: should never fail to open the block sentinel");
            tx.commit_tx();
        }
    }

    /// Drop the current trie from the MARF, and roll back all unconfirmed state
    pub fn drop_unconfirmed(&mut self) {
        if !self.storage.readonly() && self.storage.unconfirmed() {
            if let Some(tip) = self.open_chain_tip.take() {
                let mut tx = self
                    .storage
                    .transaction()
                    .expect("BUG: failed to start transaction to drop trie");
                tx.drop_unconfirmed_trie(&tip.block_hash);
                tx.open_block(&T::sentinel())
                    .expect("BUG: should never fail to open the block sentinel");
                tx.commit_tx();
            }
        }
    }

    /// Finish writing the next trie in the MARF.  This persists all changes.
    /// Works for both confirmed and unconfirmed tries
    pub fn commit(&mut self) -> Result<(), Error> {
        if self.storage.readonly() {
            return Err(Error::ReadOnlyError);
        }
        if let Some(_tip) = self.open_chain_tip.take() {
            let mut tx = self.storage.transaction()?;
            tx.flush()?;
            tx.commit_tx();
        }
        Ok(())
    }

    /// Finish writing the next trie in the MARF -- this is used by miners
    ///   to commit the mined block, but write it to the mined_block table,
    ///   rather than out to the marf_data table (this prevents the
    ///   miner's block from getting stepped on after the sortition).
    pub fn commit_mined(&mut self, bhh: &T) -> Result<(), Error> {
        if self.storage.readonly() {
            return Err(Error::ReadOnlyError);
        }
        if self.storage.unconfirmed() {
            return Err(Error::UnconfirmedError);
        }
        if let Some(_tip) = self.open_chain_tip.take() {
            let mut tx = self.storage.transaction()?;
            tx.flush_mined(bhh)?;
            tx.commit_tx();
        }
        Ok(())
    }

    /// Finish writing the next trie in the MARF, but change the hash of the current Trie's
    /// block hash to something other than what we opened it as.  This persists all changes.
    pub fn commit_to(&mut self, real_bhh: &T) -> Result<(), Error> {
        if self.storage.readonly() {
            return Err(Error::ReadOnlyError);
        }
        if self.storage.unconfirmed() {
            return Err(Error::UnconfirmedError);
        }
        if let Some(_tip) = self.open_chain_tip.take() {
            let mut tx = self.storage.transaction()?;
            tx.flush_to(real_bhh)?;
            tx.commit_tx();
        }
        Ok(())
    }

    // Comes from the marf.
    pub fn get_block_height_of(
        &mut self,
        bhh: &T,
        current_block_hash: &T,
    ) -> Result<Option<u32>, Error> {
        if Some(bhh) == self.get_open_chain_tip() {
            return Ok(self.get_open_chain_tip_height());
        } else {
            MARF::get_block_height_miner_tip(
                &mut self.storage.connection(),
                bhh,
                current_block_hash,
            )
        }
    }

    /// Get open chain tip
    pub fn get_open_chain_tip(&self) -> Option<&T> {
        self.open_chain_tip.as_ref().map(|x| &x.block_hash)
    }

    /// Get open chain tip block height
    pub fn get_open_chain_tip_height(&self) -> Option<u32> {
        self.open_chain_tip.as_ref().map(|x| x.height)
    }

    /// Access internal storage
    #[cfg(test)]
    pub fn borrow_storage_backend(&mut self) -> TrieStorageConnection<T> {
        self.storage.connection()
    }

    #[cfg(test)]
    pub fn borrow_storage_transaction(&mut self) -> TrieStorageTransaction<T> {
        self.storage.transaction().unwrap()
    }

    /// Make a raw transaction to the underlying storage
    pub fn storage_tx(&mut self) -> Result<Transaction<'_>, db_error> {
        self.storage.sqlite_tx()
    }

    /// Reopen storage read-only
    pub fn reopen_storage_readonly(&self) -> Result<TrieFileStorage<T>, Error> {
        self.storage.reopen_readonly()
    }

    /// Reopen this MARF with readonly storage.
    ///
    /// Returns Err if:
    ///   1) This class is already in the process of writing.
    ///   2) A new underlying SQLite database connection cannot be established.
    pub fn reopen_readonly(&self) -> Result<MARF<T>, Error> {
        if self.open_chain_tip.is_some() {
            error!(
                "MARF at {} is already in the process of writing",
                &self.storage.db_path
            );
            return Err(Error::InProgressError);
        }

        let ro_storage = self.storage.reopen_readonly()?;
        Ok(MARF {
            storage: ro_storage,
            open_chain_tip: None,
        })
    }

    /// Get the root trie hash at a particular block
    pub fn get_root_hash_at(&mut self, block_hash: &T) -> Result<TrieHash, Error> {
        self.storage.connection().get_root_hash_at(block_hash)
    }

    /// Convert to the inner sqlite connection
    pub fn into_sqlite_conn(self) -> Connection {
        self.storage.into_sqlite_conn()
    }
}
