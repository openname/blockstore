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

/// This module defines the methods for reading and inserting into a Trie
use std::fmt;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::{error, io};

use sha2::Digest;
use stacks_common::types::chainstate::{
    BlockHeaderHash, TrieHash, BLOCK_HEADER_HASH_ENCODED_SIZE, TRIEHASH_ENCODED_SIZE,
};
use stacks_common::util::hash::to_hex;
use stacks_common::util::log;
use stacks_common::util::macros::is_trace;

use crate::chainstate::stacks::index::bits::{
    get_leaf_hash, get_node_hash, get_nodetype_hash_bytes,
};
use crate::chainstate::stacks::index::marf::MARF;
use crate::chainstate::stacks::index::node::{
    clear_backptr, is_backptr, set_backptr, CursorError, TrieCursor, TrieNode, TrieNode16,
    TrieNode256, TrieNode4, TrieNode48, TrieNodeID, TrieNodeType, TriePtr,
};
use crate::chainstate::stacks::index::storage::{
    TrieFileStorage, TrieHashCalculationMode, TrieStorageConnection,
};
use crate::chainstate::stacks::index::{Error, MarfTrieId, TrieHasher, TrieLeaf};

/// We don't actually instantiate a Trie, but we still need to pass a type parameter for the
/// storage implementation.
pub struct Trie {}

/// Fetch children hashes and compute the node's hash
fn get_nodetype_hash<T: MarfTrieId>(
    storage: &mut TrieStorageConnection<T>,
    node: &TrieNodeType,
) -> Result<TrieHash, Error> {
    if storage.hash_calculation_mode == TrieHashCalculationMode::Deferred {
        trace!(
            "get_nodetype_hash (deferred): hash {:?} = {:?} + ::children::",
            &TrieHash([0u8; 32]),
            node
        );
        return Ok(TrieHash([0u8; 32]));
    }

    let mut hasher = TrieHasher::new();

    node.write_consensus_bytes(storage, &mut hasher)
        .expect("IO Failure pushing to hasher.");

    storage.write_children_hashes(node, &mut hasher)?;

    let mut res = [0u8; 32];
    res.copy_from_slice(hasher.finalize().as_slice());

    let ret = TrieHash(res);

    trace!(
        "get_nodetype_hash: hash {:?} = {:?} + ::children::",
        &ret,
        node
    );
    Ok(ret)
}

impl Trie {
    /// Read the root node.  First try to read it as a back-pointer (since all root nodes except for
    /// the root node in the very first trie will be back-pointers), and if that fails due to a
    /// node ID mismatch (i.e. CorruptionError), then try to read it as a non-backpointer.
    fn read_root_maybe_hash<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        read_hash: bool,
    ) -> Result<(TrieNodeType, Option<TrieHash>), Error> {
        let ptr = TriePtr::new(
            set_backptr(TrieNodeID::Node256 as u8),
            0,
            storage.root_ptr(),
        );
        let res = if read_hash {
            storage
                .read_nodetype(&ptr)
                .map(|(node, hash)| (node, Some(hash)))
        } else {
            storage.read_nodetype_nohash(&ptr).map(|node| (node, None))
        };

        match res {
            Err(Error::CorruptionError(_)) => {
                let non_backptr_ptr = storage.root_trieptr();
                if read_hash {
                    storage
                        .read_nodetype(&non_backptr_ptr)
                        .map(|(node, hash)| (node, Some(hash)))
                } else {
                    storage
                        .read_nodetype_nohash(&non_backptr_ptr)
                        .map(|node| (node, None))
                }
            }
            Err(e) => Err(e),
            Ok(data) => Ok(data),
        }
    }

    pub fn read_root<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
    ) -> Result<(TrieNodeType, TrieHash), Error> {
        Trie::read_root_maybe_hash(storage, true).map(|(node, hash_opt)| {
            (
                node,
                hash_opt.expect("FATAL: expected some node hash but got none"),
            )
        })
    }

    pub fn read_root_nohash<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
    ) -> Result<TrieNodeType, Error> {
        Trie::read_root_maybe_hash(storage, false).map(|(node, _)| node)
    }

    /// Walk from the given node to the next node on the path, advancing the cursor.
    /// Return the TriePtr followed, the _next_ node to walk, and the hash of the _current_ node.
    /// Returns None if we either didn't find the node, or we're out of path, or we're at a leaf.
    /// NOTE: This only works if we're walking a Trie, not a MARF.  Returns Ok(None) if a
    /// back-pointer is found.
    fn walk_from_maybe_hash<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        node: &TrieNodeType,
        cursor: &mut TrieCursor<T>,
        read_hash: bool,
    ) -> Result<Option<(TriePtr, TrieNodeType, TrieHash)>, Error> {
        match cursor.walk(node, &storage.get_cur_block()) {
            Ok(ptr_opt) => {
                match ptr_opt {
                    None => {
                        // end of path
                        Ok(None)
                    }
                    Some(ptr) => {
                        // end of node path
                        trace!("Walked to {:?}", &ptr);
                        let (node, hash) = if read_hash {
                            storage.read_nodetype(&ptr)?
                        } else {
                            storage
                                .read_nodetype_nohash(&ptr)
                                .map(|node| (node, TrieHash([0u8; TRIEHASH_ENCODED_SIZE])))?
                        };

                        Ok(Some((ptr, node, hash)))
                    }
                }
            }
            Err(e) => Err(Error::CursorError(e)),
        }
    }

    pub fn walk_from<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        node: &TrieNodeType,
        cursor: &mut TrieCursor<T>,
    ) -> Result<Option<(TriePtr, TrieNodeType, TrieHash)>, Error> {
        Trie::walk_from_maybe_hash(storage, node, cursor, true)
    }

    pub fn walk_from_nohash<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        node: &TrieNodeType,
        cursor: &mut TrieCursor<T>,
    ) -> Result<Option<(TriePtr, TrieNodeType)>, Error> {
        Trie::walk_from_maybe_hash(storage, node, cursor, false)
            .map(|x| x.map(|(trieptr, trienode, _)| (trieptr, trienode)))
    }

    /// Follow a back-pointer back to a trie node in a previous trie.
    ///
    /// If the ptr is a back-pointer, then shunt to the block that contains the target node, read
    /// it, and update the cursor to record that we followed the back-pointer.
    ///
    /// If the ptr is not a back-pointer, read the node from this trie.
    /// s must point to this trie's block, not the block pointed at by the ptr.
    ///
    /// Either way, return the node, its hash, and the ptr to the node in the block in which it was
    /// found (it will _not_ be a back-pointer).
    pub fn walk_backptr<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        ptr: &TriePtr,
        cursor: &mut TrieCursor<T>,
    ) -> Result<(TrieNodeType, TrieHash, TriePtr), Error> {
        if !is_backptr(ptr.id()) {
            // child is in this block
            if ptr.id() == (TrieNodeID::Empty as u8) {
                // shouldn't happen
                return Err(Error::CorruptionError("ptr is empty".to_string()));
            }
            let (node, node_hash) = storage.read_nodetype(ptr)?;
            return Ok((node, node_hash, ptr.clone()));
        } else {
            storage.bench_mut().marf_find_backptr_node_start();
            // ptr is a backptr -- find the block
            let back_block_hash = storage
                .get_block_from_local_id(ptr.back_block())
                .map_err(|e| {
                    test_debug!("Failed to get block from local ID {}", ptr.back_block());
                    e
                })?
                .clone();

            storage
                .open_block_known_id(&back_block_hash, ptr.back_block())
                .map_err(|e| {
                    test_debug!(
                        "Failed to open block {} with id {}: {:?}",
                        &back_block_hash,
                        ptr.back_block(),
                        &e
                    );
                    e
                })?;

            let backptr = ptr.from_backptr();
            storage.bench_mut().marf_find_backptr_node_finish();

            let (node, node_hash) = storage.read_nodetype(&backptr)?;
            cursor.repair_backptr_step_backptr(&node, &backptr, storage.get_cur_block());
            Ok((node, node_hash, backptr))
        }
    }

    /// Read a node's children's hashes as a vector of TrieHashes.
    /// This only works for intermediate nodes and leafs (the latter of which have no children).
    ///
    /// See: TrieStorageConnection::write_children_hashes for more information on the hash contents.
    pub fn get_children_hashes<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        node: &TrieNodeType,
    ) -> Result<Vec<TrieHash>, Error> {
        let mut buffer = Vec::with_capacity(node.ptrs().len() * TRIEHASH_ENCODED_SIZE);
        storage.write_children_hashes(node, &mut buffer)?;
        assert_eq!(buffer.len() % TRIEHASH_ENCODED_SIZE, 0);

        let trie_hashes: Vec<_> = buffer
            .chunks_exact(TRIEHASH_ENCODED_SIZE)
            .map(|x| {
                TrieHash::from_bytes(x).expect("Failed to re-encode TrieHash from byte buffer")
            })
            .collect();

        Ok(trie_hashes)
    }

    /// Given an existing leaf, replace it with the new leaf.
    /// c must point to the node to replace.
    fn replace_leaf<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        cursor: &mut TrieCursor<T>,
        value: &mut TrieLeaf,
    ) -> Result<TriePtr, Error> {
        let (cur_leaf, _) = storage.read_nodetype(&cursor.ptr())?;
        if !cur_leaf.is_leaf() {
            return Err(Error::CorruptionError(format!(
                "Not a leaf: {:?}",
                &cursor.ptr()
            )));
        }

        value.path.clone_from(cur_leaf.path_bytes());

        let leaf_hash = get_leaf_hash(value);

        let leaf_ptr = cursor.ptr();
        storage.write_node(leaf_ptr.ptr(), value, leaf_hash)?;

        trace!("replace_leaf: wrote {:?} at {:?}", &value, &cursor.ptr());
        Ok(cursor.ptr())
    }

    /// Append a leaf to the trie, and return the TriePtr to it.
    /// Do lazy expansion -- have the leaf store the trailing path to it.
    /// Return the TriePtr to the newly-written leaf
    fn append_leaf<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        cursor: &mut TrieCursor<T>,
        value: &mut TrieLeaf,
    ) -> Result<TriePtr, Error> {
        assert!(cursor.chr().is_some());

        let ptr = storage.last_ptr()?;
        let chr = cursor.chr().unwrap();

        value.path = cursor.path.as_bytes()[cursor.index..].to_vec();

        let leaf_hash = get_leaf_hash(value);
        let leaf_ptr = TriePtr::new(TrieNodeID::Leaf as u8, chr, ptr);
        storage.write_node(ptr, value, leaf_hash)?;

        trace!("append_leaf: append {:?} at {:?}", value, &leaf_ptr);
        Ok(leaf_ptr)
    }

    /// Given a leaf and a cursor that is _not_ EOP, and a new leaf, create a node4 with the two
    /// leaves as its children and return its pointer.
    ///
    /// f must point to the start of cur_leaf.
    ///
    /// before:
    ///
    /// leaf[path=aabbccddeeff00112233]=123456
    ///
    /// insert (aabbccddeeff99887766, 98765)
    ///
    /// after:
    ///                          [00]leaf[path=112233]=123456
    ///                         /
    /// node4[path=aabbccddeeff]
    ///                         \
    ///                          [99]leaf[887766]=98765
    ///
    fn promote_leaf_to_node4<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        cursor: &mut TrieCursor<T>,
        cur_leaf_data: &mut TrieLeaf,
        new_leaf_data: &mut TrieLeaf,
    ) -> Result<TriePtr, Error> {
        // can only work if we're not at the end of the path, and the current node has a path
        assert!(!cursor.eop());
        assert!(!cur_leaf_data.path.is_empty());

        // switch from lazy expansion to path compression --
        // * the current and new leaves will have unique suffixes
        // * the node4 will have their shared prefix
        let cur_leaf_ptr = cursor.ptr();

        let node4_path = cur_leaf_data.path[0..(cursor.ntell())].to_vec();
        let node4_chr = cur_leaf_ptr.chr();

        let cur_leaf_chr = cur_leaf_data.path[cursor.ntell()];
        let cur_leaf_path = cur_leaf_data.path[(if cursor.ntell() >= cur_leaf_data.path.len() {
            cursor.ntell()
        } else {
            cursor.ntell() + 1
        })..]
            .to_vec();

        // update current leaf (path changed) and save it
        let cur_leaf_disk_ptr = cur_leaf_ptr.ptr();
        let cur_leaf_new_ptr =
            TriePtr::new(TrieNodeID::Leaf as u8, cur_leaf_chr, cur_leaf_disk_ptr);

        assert!(cur_leaf_path.len() <= cur_leaf_data.path.len());
        let _sav_cur_leaf_data = cur_leaf_data.clone();
        cur_leaf_data.path = cur_leaf_path;
        let cur_leaf_hash = get_leaf_hash(cur_leaf_data);

        // NOTE: this is safe since the current leaf's byte representation has gotten shorter
        storage.write_node(cur_leaf_ptr.ptr(), cur_leaf_data, cur_leaf_hash.clone())?;

        // append the new leaf and the end of the file.
        let new_leaf_disk_ptr = storage.last_ptr()?;
        let new_leaf_chr = cursor.path[cursor.tell()]; // NOTE: this is safe because !cursor.eop()
        let new_leaf_path = cursor.path[(if cursor.tell() + 1 <= cursor.path.len() {
            cursor.tell() + 1
        } else {
            cursor.path.len()
        })..]
            .to_vec();
        new_leaf_data.path = new_leaf_path;
        let new_leaf_hash = get_leaf_hash(new_leaf_data);

        // put new leaf at the end of this Trie
        let new_leaf_ptr = TriePtr::new(TrieNodeID::Leaf as u8, new_leaf_chr, new_leaf_disk_ptr);

        storage.write_node(new_leaf_disk_ptr, new_leaf_data, new_leaf_hash.clone())?;

        // append the Node4 that points to both of them, and put it after the new leaf
        let mut node4_data = TrieNode4::new(&node4_path);

        assert!(node4_data.insert(&cur_leaf_new_ptr));
        assert!(node4_data.insert(&new_leaf_ptr));

        let node4_hash = get_node_hash(
            &node4_data,
            &[
                cur_leaf_hash,
                new_leaf_hash,
                TrieHash::from_data(&[]),
                TrieHash::from_data(&[]),
            ],
            storage,
        );

        let node4 = TrieNodeType::Node4(node4_data);

        // append the node4 to the end of the trie
        let node4_disk_ptr = storage.last_ptr()?;

        let ret = TriePtr::new(TrieNodeID::Node4 as u8, node4_chr, node4_disk_ptr);
        storage.write_nodetype(node4_disk_ptr, &node4, node4_hash)?;

        // update cursor to point to this node4 as the last-node-visited, and set the newly-created
        // ptr as the last ptr traversed (so the cursor still points to this leaf, but accurately
        // reflects the path taken to it).
        cursor.repair_retarget(&node4, &ret, &storage.get_cur_block());

        trace!(
            "Promoted {:?} to {:?}, {:?}, {:?}, new ptr = {:?}",
            _sav_cur_leaf_data,
            cur_leaf_data,
            &node4,
            new_leaf_data,
            &ret
        );
        Ok(ret)
    }

    #[cfg(test)]
    pub fn test_promote_leaf_to_node4<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        cursor: &mut TrieCursor<T>,
        cur_leaf_data: &mut TrieLeaf,
        new_leaf_data: &mut TrieLeaf,
    ) -> Result<TriePtr, Error> {
        Trie::promote_leaf_to_node4(storage, cursor, cur_leaf_data, new_leaf_data)
    }

    fn node_has_space(chr: u8, children: &[TriePtr]) -> bool {
        let mut i = (children.len() - 1) as i64;
        while i >= 0 {
            if children[i as usize].id() == (TrieNodeID::Empty as u8)
                || children[i as usize].chr() == chr
            {
                return true;
            }
            i -= 1;
        }
        return false;
    }

    /// Try to insert a leaf node into the given node, if there's space to do so and if the leaf
    /// belongs as a child of this node.
    /// If so, then save the leaf and its hash, update the node's ptrs and hash, and return the
    /// node's ptr and the node's new hash so we can update the trie.
    /// Return None if there's no space, or if the leaf doesn't share its full path prefix with the
    /// given node.
    ///
    /// ```text
    /// before:
    ///                          [00]nodeY[path=112233] ...
    ///                         /
    /// nodeX[path=aabbccddeeff]
    ///
    /// insert (aabbccddeeff99887766, 123456)
    ///
    ///                          [00]nodeY[path=112233] ...
    ///                         /
    /// nodeX[path=aabbccddeeff]
    ///                         \
    ///                          [99]leaf[path=887766]=123456
    /// ```
    fn try_attach_leaf<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        cursor: &mut TrieCursor<T>,
        leaf: &mut TrieLeaf,
        node: &mut TrieNodeType,
    ) -> Result<Option<TriePtr>, Error> {
        // can only do this if we're at the end of the node's path
        if !cursor.eonp(node) {
            // nope
            return Ok(None);
        }
        assert!(cursor.chr().is_some());
        assert!(!node.is_leaf());

        let has_space = Trie::node_has_space(cursor.chr().unwrap(), node.ptrs());
        if !has_space {
            // nope!
            return Ok(None);
        }

        // write leaf and update parent
        let leaf_ptr = Trie::append_leaf(storage, cursor, leaf)?;
        let inserted = node.insert(&leaf_ptr);

        assert!(inserted);

        let new_node_hash = get_nodetype_hash(storage, node)?;

        storage.write_nodetype(cursor.ptr().ptr(), node, new_node_hash)?;

        Ok(Some(cursor.ptr()))
    }

    #[cfg(test)]
    pub fn test_try_attach_leaf<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        cursor: &mut TrieCursor<T>,
        leaf: &mut TrieLeaf,
        node: &mut TrieNodeType,
    ) -> Result<Option<TriePtr>, Error> {
        Trie::try_attach_leaf(storage, cursor, leaf, node)
    }

    /// Given a node and a leaf, attach the leaf.  Promote the intermediate node if necessary.
    /// Does the same thing as try_attach_leaf, but the node might get expanaded.  In this case, the
    /// new node will be appended and the old node will be leaked in the storage implementation
    /// (leakage isn't a concern in practice, because the "leak" will happen inside the TrieRAM
    /// storage implementation, which will be garbage-collected and dumped to disk once we finish
    /// all the block's inserts and call the TrieRAM's containing TrieStorageConnection instance's
    /// flush() method).
    fn insert_leaf<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        cursor: &mut TrieCursor<T>,
        leaf: &mut TrieLeaf,
        node: &mut TrieNodeType,
    ) -> Result<TriePtr, Error> {
        // can only do this if we're at the end of the node's path
        assert!(cursor.eonp(node));

        if let Some(ptr) = Trie::try_attach_leaf(storage, cursor, leaf, node)? {
            return Ok(ptr);
        }

        // not enough space -- need to promote node
        let mut new_node = match node {
            TrieNodeType::Leaf(_) => panic!("Cannot insert into a leaf"),
            TrieNodeType::Node256(_) => panic!("Somehow could not insert into a Node256"),
            TrieNodeType::Node4(ref data) => TrieNodeType::Node16(TrieNode16::from_node4(data)),
            TrieNodeType::Node16(ref data) => {
                TrieNodeType::Node48(Box::new(TrieNode48::from_node16(data)))
            }
            TrieNodeType::Node48(ref data) => {
                TrieNodeType::Node256(Box::new(TrieNode256::from_node48(data.as_ref())))
            }
        };

        let node_ptr = cursor.ptr();
        let leaf_ptr = Trie::append_leaf(storage, cursor, leaf)?;
        let inserted = new_node.insert(&leaf_ptr);
        assert!(inserted);

        let new_node_hash = get_nodetype_hash(storage, &new_node)?;

        // append this leaf to the Trie
        let new_node_disk_ptr = storage.last_ptr()?;

        let ret = TriePtr::new(new_node.id(), node_ptr.chr(), new_node_disk_ptr);
        storage.write_nodetype(new_node_disk_ptr, &new_node, new_node_hash)?;

        // update the cursor so its path of nodes and ptrs accurately reflects that we would have
        // visited this leaf on its path.
        cursor.repair_retarget(&new_node, &ret, &storage.get_cur_block());
        Ok(ret)
    }

    #[cfg(test)]
    pub fn test_insert_leaf<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        cursor: &mut TrieCursor<T>,
        leaf: &mut TrieLeaf,
        node: &mut TrieNodeType,
    ) -> Result<TriePtr, Error> {
        Trie::insert_leaf(storage, cursor, leaf, node)
    }

    /// Given a node and a leaf to insert, break apart the node's compressed path into the shared
    /// prefix and the node- and leaf-specific segments, and add a Node4 at the break with the
    /// leaf.  Updates the given node and leaf, and returns the node4's ptr and hash.
    ///
    /// ```text
    /// before:
    ///                                        [00]nodeY[path=112233]...
    ///                                       /
    /// (parent)----[aa]nodeX[path=bbccddeeff]
    ///                                       \
    ///                                        [99]nodeZ[path=887766]...
    ///
    /// insert (aabbccffccbbaa, 123456)
    ///
    /// after:
    ///
    ///                                  [ff]leaf[path=ccbbaa]=123456
    ///                                 /
    /// (parent)----[aa]node4[path=bbcc]---[dd]nodeX[path=eeff]---[00]nodeY[path=112233]...
    ///                                                        \
    ///                                                         [99]nodeZ[path=887766]...
    ///
    /// ```
    /// (if nodeX was the root, then there is no parent, and the resulting node will be a node256
    /// instead of a node4).
    ///
    fn splice_leaf<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        cursor: &mut TrieCursor<T>,
        leaf: &mut TrieLeaf,
        node: &mut TrieNodeType,
    ) -> Result<TriePtr, Error> {
        assert!(!cursor.eop());
        assert!(!cursor.eonp(node));
        assert!(cursor.chr().is_some());
        assert!(!node.is_leaf());

        let node_path = node.path_bytes().clone();
        let shared_path_prefix = node_path[0..cursor.ntell()].to_vec();
        let leaf_path = cursor.path[cursor.tell() + 1..].to_vec();
        let new_cur_node_path = node_path[cursor.ntell() + 1..].to_vec();
        let new_cur_node_chr = node_path[cursor.ntell()]; // chr for node-X post-update

        // store leaf
        leaf.path = leaf_path;
        let leaf_chr = cursor.path[cursor.tell()];
        let leaf_disk_ptr = storage.last_ptr()?;
        let leaf_hash = get_leaf_hash(leaf);
        let leaf_ptr = TriePtr::new(TrieNodeID::Leaf as u8, leaf_chr, leaf_disk_ptr);
        storage.write_node(leaf_disk_ptr, leaf, leaf_hash.clone())?;

        // update current node (node-X) and make a new path and ptr for it
        let cur_node_cur_ptr = cursor.ptr();
        let new_cur_node_disk_ptr = storage.last_ptr()?;
        let new_cur_node_ptr = TriePtr::new(
            cur_node_cur_ptr.id(),
            new_cur_node_chr,
            new_cur_node_disk_ptr,
        );

        node.set_path(new_cur_node_path);

        let new_cur_node_hash = get_nodetype_hash(storage, &node)?;

        let mut new_node4 = TrieNode4::new(&shared_path_prefix);
        new_node4.insert(&leaf_ptr);
        new_node4.insert(&new_cur_node_ptr);

        let new_node_hash = get_node_hash(
            &new_node4,
            &[
                leaf_hash,
                new_cur_node_hash,
                TrieHash::from_data(&[]),
                TrieHash::from_data(&[]),
            ],
            storage,
        );

        let (new_node_id, new_node) = if cursor.node_ptrs.len() == 1 {
            // we just split the compressed path in the root node,
            // so make sure the root node _stays_ as a node256.
            // Note that the hash we write here doesn't matter -- it'll get overwritten in the
            // subsequent call to update_root_hash()
            (
                TrieNodeID::Node256,
                TrieNode256::from_node4(&new_node4).as_trie_node_type(),
            )
        } else {
            (TrieNodeID::Node4, TrieNodeType::Node4(new_node4))
        };

        // store node4 where node-X used to be
        storage.write_nodetype(cur_node_cur_ptr.ptr(), &new_node, new_node_hash)?;

        // store node-X at the end
        storage.write_nodetype(new_cur_node_disk_ptr, node, new_cur_node_hash)?;

        let ret = TriePtr::new(
            new_node_id as u8,
            cur_node_cur_ptr.chr(),
            cur_node_cur_ptr.ptr(),
        );
        cursor.repair_retarget(&new_node, &ret, &storage.get_cur_block());

        trace!("splice_leaf: node-X' at {:?}", &ret);
        Ok(ret)
    }

    #[cfg(test)]
    pub fn test_splice_leaf<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        cursor: &mut TrieCursor<T>,
        leaf: &mut TrieLeaf,
        node: &mut TrieNodeType,
    ) -> Result<TriePtr, Error> {
        Trie::splice_leaf(storage, cursor, leaf, node)
    }

    /// Add a new value to the Trie at the location pointed at by the cursor.
    /// Returns a ptr to be inserted into the last node visited by the cursor.
    pub fn add_value<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        cursor: &mut TrieCursor<T>,
        value: &mut TrieLeaf,
    ) -> Result<TriePtr, Error> {
        let mut node = match cursor.node() {
            Some(n) => n,
            None => panic!("Cursor is uninitialized"),
        };

        if cursor.eop() {
            match node {
                TrieNodeType::Leaf(_) => Trie::replace_leaf(storage, cursor, value),
                _ => Trie::insert_leaf(storage, cursor, value, &mut node),
            }
        } else {
            // didn't reach the end of the path, so we're on an intermediate node
            // or we're somewhere in the path of a leaf.
            // Either tack the leaf on (possibly promoting the node), or splice the leaf in.
            if cursor.eonp(&node) {
                trace!(
                    "eop = {}, eonp = {}, c = {:?}, node = {:?}",
                    cursor.eop(),
                    cursor.eonp(&node),
                    cursor,
                    &node
                );
                Trie::insert_leaf(storage, cursor, value, &mut node)
            } else {
                match node {
                    TrieNodeType::Leaf(ref mut data) => {
                        Trie::promote_leaf_to_node4(storage, cursor, data, value)
                    }
                    _ => Trie::splice_leaf(storage, cursor, value, &mut node),
                }
            }
        }
    }

    /// Perform the reads, lookups, etc. for computing the ancestor byte vector.
    /// This method _does not_ restore the previously open block on failure, the caller will do that.
    fn inner_get_trie_ancestor_hashes_bytes<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
    ) -> Result<Vec<TrieHash>, Error> {
        let cur_block_header = storage.get_cur_block();
        // definitely enough space for the foreseeable future
        //    ancestor depth _cannot_ exceed 32 -- 2^32 > max size of u32
        //    (which is how we are identifying blocks).
        let mut hash_buf = Vec::with_capacity(33);

        // here is where some mind-bending things begin to happen.
        //   we want to find the block at a given _height_. but how to do so?
        //   use the data stored already in the MARF.
        let cur_block_height =
            MARF::get_block_height_miner_tip(storage, &cur_block_header, &cur_block_header)
                .map_err(|e| match e {
                    Error::NotFoundError => Error::CorruptionError(format!(
                        "Could not obtain block height for block {}: not found",
                        &cur_block_header
                    )),
                    x => x,
                })?
                .ok_or_else(|| {
                    Error::CorruptionError(format!(
                        "Could not obtain block height for block {}: got None",
                        &cur_block_header
                    ))
                })?;

        let mut log_depth = 0;
        while log_depth < 32 && (1u32 << log_depth) <= cur_block_height {
            let prev_block_header = MARF::get_block_at_height(
                storage,
                cur_block_height - (1u32 << log_depth),
                &cur_block_header,
            )?
            .ok_or_else(|| {
                Error::CorruptionError(format!(
                    "Could not obtain block hash at block height {}",
                    cur_block_height - (1u32 << log_depth)
                ))
            })?;

            storage.open_block(&prev_block_header)?;

            let root_ptr = storage.root_trieptr();
            let ancestor_hash = storage.read_node_hash_bytes(&root_ptr)?;

            trace!(
                "Include root hash {} from block {} in ancestor #{}",
                ancestor_hash,
                prev_block_header,
                1u32 << log_depth
            );

            hash_buf.push(ancestor_hash);

            log_depth += 1;
        }

        Ok(hash_buf)
    }

    /// Calculate the byte vector of the ancestor root hashes of this trie.
    /// `storage` must point to the block that contains the trie's root.
    pub fn get_trie_ancestor_hashes_bytes<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
    ) -> Result<Vec<TrieHash>, Error> {
        let (cur_block_header, cur_block_id) = storage.get_cur_block_and_id();
        if let Some(cached_ancestor_hashes_bytes) =
            storage.check_cached_ancestor_hashes_bytes(&cur_block_header)
        {
            Ok(cached_ancestor_hashes_bytes)
        } else {
            let result = Trie::inner_get_trie_ancestor_hashes_bytes(storage);
            if let Ok(ref result) = result {
                storage.set_cached_ancestor_hashes_bytes(&cur_block_header, result.clone());
            }

            // restore
            storage.open_block_maybe_id(&cur_block_header, cur_block_id)?;
            result
        }
    }

    /// Calculate the bytes of the ancestor root hashes of this trie, plus the current trie's root.
    /// Return the resulting sequence of hashes a a single byte buffer.
    pub fn get_trie_root_ancestor_hashes_bytes<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        children_root_hash: &TrieHash,
    ) -> Result<Vec<TrieHash>, Error> {
        trace!(
            "Calculate Trie hash from root node digest {:?}",
            children_root_hash
        );
        let mut ancestor_bytes = Trie::get_trie_ancestor_hashes_bytes(storage)?;
        ancestor_bytes.insert(0, children_root_hash.clone());

        trace!(
            "Trie ancestor bytes for root hash calculation: {:?}",
            &ancestor_bytes
        );

        Ok(ancestor_bytes)
    }

    /// Calculate the root hash of the trie (i.e. the hash for the root node) by including both the
    /// digest of this Trie, as well as a geometric sequence of prior Trie root hashes as far back
    /// as we can go.
    pub fn get_trie_root_hash<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        children_root_hash: &TrieHash,
    ) -> Result<TrieHash, Error> {
        let hashes = Trie::get_trie_root_ancestor_hashes_bytes(storage, children_root_hash)?;
        if hashes.len() == 1 {
            Ok(hashes[0])
        } else {
            Ok(TrieHash::from_data_array(hashes.as_slice()))
        }
    }

    /// Unwind a TrieCursor to update the Merkle root of the trie.
    /// The root hashes of each trie form a Merkle skip-list -- the hash of Trie i is calculated
    /// from the hash of its children, plus the hash Tries i-1, i-2, i-4, i-8, ..., i-2**j, ...
    /// This is required for Merkle proofs to work (specifically, the shunt proofs).
    fn recalculate_root_hash<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        cursor: &TrieCursor<T>,
        update_skiplist: bool,
    ) -> Result<(), Error> {
        assert!(!cursor.node_ptrs.is_empty());

        let mut ptrs = cursor.node_ptrs.clone();
        trace!("update_root_hash: ptrs = {:?}", &ptrs);
        let mut child_ptr = ptrs.pop().unwrap();

        if ptrs.is_empty() {
            // root node was already updated by trie operations, but it will have the wrong hash.
            // we need to "fix" the root node so it mixes in its ancestor hashes.
            trace!("Fix up root node so it mixes in its ancestor hashes");
            let (node, _cur_hash) = storage.read_nodetype(&child_ptr)?;
            if !node.is_node256() {
                return Err(Error::CorruptionError(
                    "Only ptr was not a node256".to_string(),
                ));
            }

            if child_ptr != storage.root_trieptr() {
                return Err(Error::CorruptionError(
                    "Only ptr is not the root".to_string(),
                ));
            }

            let my_hash = get_nodetype_hash(storage, &node)?;

            let h = if update_skiplist {
                trace!("Update root skiplist");
                Trie::get_trie_root_hash(storage, &my_hash)?
            } else {
                trace!("Not updating root skiplist");
                my_hash
            };

            // for debug purposes
            if cfg!(test) && is_trace() {
                let node_hash = my_hash.clone();
                let _ = Trie::get_trie_root_ancestor_hashes_bytes(storage, &node_hash)
                    .map(|_hs| {
                        storage.clear_cached_ancestor_hashes_bytes();
                        trace!("update_root_hash: Updated {:?} with {:?} from {} to {} + {:?} = {} (fixed root)", &node, &child_ptr, &_cur_hash, &node_hash, &_hs[1..].to_vec(), &h);
                    });
            }

            debug!(
                "Next root hash is {} (update_skiplist={})",
                h, update_skiplist
            );

            storage.write_nodetype(child_ptr.ptr(), &node, h)?;
        } else {
            while let Some(ptr) = ptrs.pop() {
                if is_backptr(ptr.id()) {
                    // this node was not altered, but instead queued to the cursor as part of walking a
                    // backptr skiplist.  Do nothing.
                    continue;
                }

                let (mut node, _cur_hash) = storage.read_nodetype(&ptr)?;
                assert!(!node.is_leaf());

                // this child_ptr _must_ be in the node.
                let updated = node.replace(&child_ptr);
                if !updated {
                    trace!(
                        "FAILED TO UPDATE {:?} WITH {:?}: {:?}",
                        &node,
                        &child_ptr,
                        cursor
                    );
                    assert!(updated);
                }

                let content_hash = get_nodetype_hash(storage, &node)?;

                // flush the current node to storage --
                //  necessary because computing ancestor hashes requires that the trie's pointers
                //  all be intact, since it does ancestor lookups!
                // however, since we're going to update the hash in the next write anyways, just write an empty buff
                storage.write_nodetype(ptr.ptr(), &node, TrieHash([0; 32]))?;

                let h = if !node.is_node256() {
                    trace!(
                        "update_root_hash: Updated {:?} with {:?} from {:?} to {:?}",
                        node,
                        &child_ptr,
                        &_cur_hash,
                        &content_hash
                    );
                    content_hash.clone()
                } else {
                    let root_ptr = storage.root_trieptr();
                    let node_hash = if ptr == root_ptr {
                        let h = if update_skiplist {
                            Trie::get_trie_root_hash(storage, &content_hash)?
                        } else {
                            content_hash.clone()
                        };

                        if cfg!(test) && is_trace() {
                            let _ = Trie::get_trie_root_ancestor_hashes_bytes(storage, &content_hash)
                                        .map(|_hs| {
                                            storage.clear_cached_ancestor_hashes_bytes();
                                            trace!("update_root_hash: Updated {:?} with {:?} from {:?} to {:?} + {:?} = {:?}", &node, &child_ptr, &_cur_hash, &content_hash, &_hs[1..].to_vec(), &h);
                                        });
                        }

                        debug!(
                            "Next root hash is {} (update_skiplist={})",
                            h, update_skiplist
                        );
                        h
                    } else {
                        trace!(
                            "update_root_hash: Updated {:?} with {:?} from {:?} to {:?}",
                            &node,
                            &child_ptr,
                            &_cur_hash,
                            &content_hash
                        );
                        content_hash
                    };
                    node_hash
                };

                storage.write_nodetype(ptr.ptr(), &node, h)?;

                child_ptr = ptr;
                child_ptr.id = clear_backptr(child_ptr.id);
            }
        }
        // must be at the root
        assert_eq!(child_ptr, storage.root_trieptr());
        Ok(())
    }

    pub fn update_root_hash<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        cursor: &TrieCursor<T>,
    ) -> Result<(), Error> {
        Trie::recalculate_root_hash(
            storage,
            cursor,
            storage.hash_calculation_mode != TrieHashCalculationMode::Deferred,
        )
    }

    pub fn update_root_node_hash<T: MarfTrieId>(
        storage: &mut TrieStorageConnection<T>,
        cursor: &TrieCursor<T>,
    ) -> Result<(), Error> {
        Trie::recalculate_root_hash(storage, cursor, false)
    }
}
