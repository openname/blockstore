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
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::{error, fmt, io};

use sha2::Digest;
use stacks_common::codec::{read_next, Error as codec_error, StacksMessageCodec};
use stacks_common::types::chainstate::{
    BlockHeaderHash, TrieHash, BLOCK_HEADER_HASH_ENCODED_SIZE, TRIEHASH_ENCODED_SIZE,
};
use stacks_common::util::hash::to_hex;

use crate::chainstate::stacks::index::bits::{
    get_path_byte_len, get_ptrs_byte_len, path_from_bytes, ptrs_from_bytes, write_path_to_bytes,
};
use crate::chainstate::stacks::index::{
    BlockMap, ClarityMarfTrieId, Error, MARFValue, MarfTrieId, TrieHasher, TrieLeaf,
    MARF_VALUE_ENCODED_SIZE,
};

#[derive(Debug, Clone, PartialEq)]
pub enum CursorError {
    PathDiverged,
    BackptrEncountered(TriePtr),
    ChrNotFound,
}

impl fmt::Display for CursorError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            CursorError::PathDiverged => write!(f, "Path diverged"),
            CursorError::BackptrEncountered(_) => write!(f, "Back-pointer encountered"),
            CursorError::ChrNotFound => write!(f, "Node child not found"),
        }
    }
}

impl error::Error for CursorError {
    fn cause(&self) -> Option<&dyn error::Error> {
        None
    }
}

// All numeric values of a Trie node when encoded.
// They are all 7-bit numbers -- the 8th bit is used to indicate whether or not the value
// identifies a back-pointer to be followed.
define_u8_enum!(TrieNodeID {
    Empty = 0,
    Leaf = 1,
    Node4 = 2,
    Node16 = 3,
    Node48 = 4,
    Node256 = 5
});

/// A node ID encodes a back-pointer if its high bit is set
pub fn is_backptr(id: u8) -> bool {
    id & 0x80 != 0
}

/// Set the back-pointer bit
pub fn set_backptr(id: u8) -> u8 {
    id | 0x80
}

/// Clear the back-pointer bit
pub fn clear_backptr(id: u8) -> u8 {
    id & 0x7f
}

// Byte writing operations for pointer lists, paths.

fn write_ptrs_to_bytes<W: Write>(ptrs: &[TriePtr], w: &mut W) -> Result<(), Error> {
    for ptr in ptrs.iter() {
        ptr.write_bytes(w)?;
    }
    Ok(())
}

fn ptrs_consensus_hash<W: Write, M: BlockMap>(
    ptrs: &[TriePtr],
    map: &mut M,
    w: &mut W,
) -> Result<(), Error> {
    for ptr in ptrs.iter() {
        ptr.write_consensus_bytes(map, w)?;
    }
    Ok(())
}

/// All Trie nodes implement the following methods:
pub trait TrieNode {
    /// Node ID for encoding/decoding
    fn id(&self) -> u8;

    /// Is the node devoid of children?
    fn empty() -> Self;

    /// Follow a path character to a child pointer
    fn walk(&self, chr: u8) -> Option<TriePtr>;

    /// Insert a child pointer if the path character slot is not occupied.
    /// Return true if inserted, false if the slot is already filled
    fn insert(&mut self, ptr: &TriePtr) -> bool;

    /// Replace an existing child pointer with a new one.  Returns true if replaced; false if the
    /// child does not exist.
    fn replace(&mut self, ptr: &TriePtr) -> bool;

    /// Read an encoded instance of this node from a byte stream and instantiate it.
    fn from_bytes<R: Read>(r: &mut R) -> Result<Self, Error>
    where
        Self: std::marker::Sized;

    /// Get a reference to the children of this node.
    fn ptrs(&self) -> &[TriePtr];

    /// Get a reference to the children of this node.
    fn path(&self) -> &Vec<u8>;

    /// Construct a TrieNodeType from a TrieNode
    fn as_trie_node_type(&self) -> TrieNodeType;

    /// Encode this node instance into a byte stream and write it to w.
    fn write_bytes<W: Write>(&self, w: &mut W) -> Result<(), Error> {
        w.write_all(&[self.id()])?;
        write_ptrs_to_bytes(self.ptrs(), w)?;
        write_path_to_bytes(self.path().as_slice(), w)
    }

    #[cfg(test)]
    fn to_bytes(&self) -> Vec<u8> {
        let mut r = Vec::new();
        self.write_bytes(&mut r)
            .expect("Failed to write to byte buffer");
        r
    }

    /// Calculate how many bytes this node will take to encode.
    fn byte_len(&self) -> usize {
        get_ptrs_byte_len(self.ptrs()) + get_path_byte_len(self.path())
    }
}

/// Trait for types that can serialize to consensus bytes
/// This is implemented by `TrieNode`s and `ProofTrieNode`s
///  and allows hash calculation routines to be the same for
///  both types.
/// The type `M` is used for any additional data structures required
///   (BlockHashMap for TrieNode and () for ProofTrieNode)
pub trait ConsensusSerializable<M> {
    /// Encode the consensus-relevant bytes of this node and write it to w.
    fn write_consensus_bytes<W: Write>(
        &self,
        additional_data: &mut M,
        w: &mut W,
    ) -> Result<(), Error>;

    #[cfg(test)]
    fn to_consensus_bytes(&self, additional_data: &mut M) -> Vec<u8> {
        let mut r = Vec::new();
        self.write_consensus_bytes(additional_data, &mut r)
            .expect("Failed to write to byte buffer");
        r
    }
}

impl<T: TrieNode, M: BlockMap> ConsensusSerializable<M> for T {
    fn write_consensus_bytes<W: Write>(&self, map: &mut M, w: &mut W) -> Result<(), Error> {
        w.write_all(&[self.id()])?;
        ptrs_consensus_hash(self.ptrs(), map, w)?;
        write_path_to_bytes(self.path().as_slice(), w)
    }
}

/// Child pointer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TriePtr {
    pub id: u8, // ID of the child.  Will have bit 0x80 set if the child is a back-pointer (in which case, back_block will be nonzero)
    pub chr: u8, // Path character at which this child resides
    pub ptr: u32, // Storage-specific pointer to where the child's encoded bytes can be found
    pub back_block: u32, // Pointer back to the block that contains the child, if it's not in this trie
}

pub const TRIEPTR_SIZE: usize = 10; // full size of a TriePtr

pub fn ptrs_fmt(ptrs: &[TriePtr]) -> String {
    let mut strs = vec![];
    for i in 0..ptrs.len() {
        if ptrs[i].id != TrieNodeID::Empty as u8 {
            strs.push(format!(
                "id{}chr{:02x}ptr{}bblk{}",
                ptrs[i].id, ptrs[i].chr, ptrs[i].ptr, ptrs[i].back_block
            ))
        }
    }
    strs.join(",")
}

impl Default for TriePtr {
    #[inline]
    fn default() -> TriePtr {
        TriePtr {
            id: 0,
            chr: 0,
            ptr: 0,
            back_block: 0,
        }
    }
}

impl TriePtr {
    #[inline]
    pub fn new(id: u8, chr: u8, ptr: u32) -> TriePtr {
        TriePtr {
            id,
            chr,
            ptr,
            back_block: 0,
        }
    }

    #[inline]
    pub fn id(&self) -> u8 {
        self.id
    }

    #[inline]
    pub fn chr(&self) -> u8 {
        self.chr
    }

    #[inline]
    pub fn ptr(&self) -> u32 {
        self.ptr
    }

    #[inline]
    pub fn back_block(&self) -> u32 {
        self.back_block
    }

    #[inline]
    pub fn from_backptr(&self) -> TriePtr {
        TriePtr {
            id: clear_backptr(self.id),
            chr: self.chr,
            ptr: self.ptr,
            back_block: 0,
        }
    }

    #[inline]
    pub fn write_bytes<W: Write>(&self, w: &mut W) -> Result<(), Error> {
        w.write_all(&[self.id(), self.chr()])?;
        w.write_all(&self.ptr().to_be_bytes())?;
        w.write_all(&self.back_block().to_be_bytes())?;
        Ok(())
    }

    /// The parts of a child pointer that are relevant for consensus are only its ID, path
    /// character, and referred-to block hash.  The software doesn't care about the details of how/where
    /// nodes are stored.
    pub fn write_consensus_bytes<W: Write, M: BlockMap>(
        &self,
        block_map: &mut M,
        w: &mut W,
    ) -> Result<(), Error> {
        w.write_all(&[self.id(), self.chr()])?;

        if is_backptr(self.id()) {
            w.write_all(
                block_map
                    .get_block_hash_caching(self.back_block())
                    .expect("Block identifier {} refered to an unknown block. Consensus failure.")
                    .as_bytes(),
            )?;
        } else {
            w.write_all(&[0; BLOCK_HEADER_HASH_ENCODED_SIZE])?;
        }
        Ok(())
    }

    #[inline]
    pub fn from_bytes(bytes: &[u8]) -> TriePtr {
        assert!(bytes.len() >= TRIEPTR_SIZE);
        let id = bytes[0];
        let chr = bytes[1];
        let ptr = u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
        let back_block = u32::from_be_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]);

        TriePtr {
            id,
            chr,
            ptr,
            back_block,
        }
    }
}

/// Cursor structure for walking down one or more Tries.  This structure helps other parts of the
/// codebase remember which nodes were visited, which blocks they came from, and which pointers
/// were walked.  In particular, it's useful for figuring out where to insert a new node, and which
/// nodes to visit when updating the root node hash.
#[derive(Debug, Clone, PartialEq)]
pub struct TrieCursor<T: MarfTrieId> {
    pub path: TrieHash,                  // the path to walk
    pub index: usize,                    // index into the path
    pub node_path_index: usize,          // index into the currently-visited node's compressed path
    pub nodes: Vec<TrieNodeType>,        // list of nodes this cursor visits
    pub node_ptrs: Vec<TriePtr>,         // list of ptr branches this cursor has taken
    pub block_hashes: Vec<T>, // list of Tries we've visited.  block_hashes[i] corresponds to node_ptrs[i]
    pub last_error: Option<CursorError>, // last error encountered while walking (used to make sure the client calls the right "recovery" method)
}

impl<T: MarfTrieId> TrieCursor<T> {
    pub fn new(path: &TrieHash, root_ptr: TriePtr) -> TrieCursor<T> {
        TrieCursor {
            path: path.clone(),
            index: 0,
            node_path_index: 0,
            nodes: vec![],
            node_ptrs: vec![root_ptr],
            block_hashes: vec![],
            last_error: None,
        }
    }

    /// what point in the path are we at now?
    /// Will be None only if we haven't taken a step yet.
    pub fn chr(&self) -> Option<u8> {
        if self.index > 0 && self.index <= self.path.len() {
            Some(self.path.as_bytes()[self.index - 1])
        } else {
            None
        }
    }

    /// what offset in the path are we at?
    pub fn tell(&self) -> usize {
        self.index
    }

    /// what is the offset in the node's compressed path?
    pub fn ntell(&self) -> usize {
        self.node_path_index
    }

    /// Are we a the [E]nd [O]f [P]ath?
    pub fn eop(&self) -> bool {
        self.index == self.path.len()
    }

    /// last ptr visited
    pub fn ptr(&self) -> TriePtr {
        // should always be true by construction
        assert!(!self.node_ptrs.is_empty());
        self.node_ptrs[self.node_ptrs.len() - 1].clone()
    }

    /// last node visited.
    /// Will only be None if we haven't taken a step yet.
    pub fn node(&self) -> Option<TrieNodeType> {
        match self.nodes.len() {
            0 => None,
            _ => Some(self.nodes[self.nodes.len() - 1].clone()),
        }
    }

    /// Are we at the [E]nd [O]f a [N]ode's [P]ath?
    pub fn eonp(&self, node: &TrieNodeType) -> bool {
        match node {
            TrieNodeType::Leaf(ref data) => self.node_path_index == data.path.len(),
            TrieNodeType::Node4(ref data) => self.node_path_index == data.path.len(),
            TrieNodeType::Node16(ref data) => self.node_path_index == data.path.len(),
            TrieNodeType::Node48(ref data) => self.node_path_index == data.path.len(),
            TrieNodeType::Node256(ref data) => self.node_path_index == data.path.len(),
        }
    }

    /// Walk to the next node, following its compressed path as far as we can and then walking to
    /// its child pointer.  If we successfully follow the path, then return the pointer we reached.
    /// Otherwise, if we reach the end of the path, return None.  If the path diverges or a node
    /// cannot be found, then return an Err.
    ///
    /// This method does not follow back-pointers, and will return Err if a back-pointer is
    /// reached.  The caller will need to manually call walk() on the last node visited to get the
    /// back-pointer, shunt to the node it points to, and then call walk_backptr_step_backptr() to
    /// record the back-pointer that was followed.  Once the back-pointer has been followed,
    /// caller should call walk_backptr_step_finish().  This is specifically relevant to the MARF,
    /// not to the individual tries.
    pub fn walk(
        &mut self,
        node: &TrieNodeType,
        block_hash: &T,
    ) -> Result<Option<TriePtr>, CursorError> {
        // can only be called if we called the appropriate "repair" method or if there is no error
        assert!(self.last_error.is_none());

        trace!("cursor: walk: node = {:?} block = {:?}", node, block_hash);

        // walk this node
        self.nodes.push((*node).clone());
        self.node_path_index = 0;

        if self.index >= self.path.len() {
            trace!("cursor: out of path");
            return Ok(None);
        }

        let node_path = node.path_bytes();
        let path_bytes = self.path.as_bytes();

        // consume as much of the compressed path as we can
        for i in 0..node_path.len() {
            if node_path[i] != path_bytes[self.index] {
                // diverged
                trace!("cursor: diverged({} != {}): i = {}, self.index = {}, self.node_path_index = {}", to_hex(&node_path), to_hex(path_bytes), i, self.index, self.node_path_index);
                self.last_error = Some(CursorError::PathDiverged);
                return Err(CursorError::PathDiverged);
            }
            self.index += 1;
            self.node_path_index += 1;
        }

        // walked to end of the node's compressed path.
        // Find the pointer to the next node.
        if self.index < self.path.len() {
            let chr = path_bytes[self.index];
            self.index += 1;
            let mut ptr_opt = node.walk(chr);

            let do_walk = match ptr_opt {
                Some(ptr) => {
                    if !is_backptr(ptr.id()) {
                        // not going to follow a back-pointer
                        self.node_ptrs.push(ptr);
                        self.block_hashes.push(block_hash.clone());
                        true
                    } else {
                        // the caller will need to follow the backptr, and call
                        // repair_backptr_step_backptr() for each node visited, and then repair_backptr_finish()
                        // once the final ptr and block_hash are discovered.
                        self.last_error = Some(CursorError::BackptrEncountered(ptr));
                        false
                    }
                }
                None => {
                    self.last_error = Some(CursorError::ChrNotFound);
                    false
                }
            };

            if !do_walk {
                ptr_opt = None;
            }

            if ptr_opt.is_none() {
                assert!(self.last_error.is_some());

                trace!(
                    "cursor: not found: chr = 0x{:02x}, self.index = {}, self.path = {:?}",
                    chr,
                    self.index - 1,
                    &path_bytes
                );
                return Err(self.last_error.clone().unwrap());
            } else {
                return Ok(ptr_opt);
            }
        } else {
            trace!("cursor: now out of path");
            return Ok(None);
        }
    }

    /// Replace the last-visited node and ptr within this trie.  Used when doing a copy-on-write or
    /// promoting a node, so the cursor state accurately reflects the nodes and tries visited.
    #[inline]
    pub fn repair_retarget(&mut self, node: &TrieNodeType, ptr: &TriePtr, hash: &T) {
        // this can only be called if we failed to walk to a node (this method _should not_ be
        // called if we walked to a backptr).
        if Some(CursorError::ChrNotFound) != self.last_error
            && Some(CursorError::PathDiverged) != self.last_error
        {
            eprintln!("{:?}", &self.last_error);
            panic!();
        }

        self.nodes.pop();
        self.node_ptrs.pop();
        self.block_hashes.pop();

        self.nodes.push(node.clone());
        self.node_ptrs.push(ptr.clone());
        self.block_hashes.push(hash.clone());

        self.last_error = None;
    }

    /// Record that a node was walked to by way of a back-pointer.
    /// next_node should be the node walked to.
    /// ptr is the ptr we'll be walking from, off of next_node.
    /// block_hash is the block where next_node came from.
    #[inline]
    pub fn repair_backptr_step_backptr(
        &mut self,
        next_node: &TrieNodeType,
        ptr: &TriePtr,
        block_hash: T,
    ) {
        // this can only be called if we walked to a backptr.
        // If it's anything else, we're in trouble.
        if Some(CursorError::ChrNotFound) == self.last_error
            || Some(CursorError::PathDiverged) == self.last_error
        {
            eprintln!("{:?}", &self.last_error);
            panic!();
        }

        trace!(
            "Cursor: repair_backptr_step_backptr ptr={:?} block_hash={:?} next_node={:?}",
            ptr,
            &block_hash,
            next_node
        );

        let backptr = TriePtr::new(set_backptr(ptr.id()), ptr.chr(), ptr.ptr()); // set_backptr() informs update_root_hash() to skip this node
        self.node_ptrs.push(backptr);
        self.block_hashes.push(block_hash);

        self.nodes.push(next_node.clone());
    }

    /// Record that we landed on a non-backptr from a backptr.
    /// ptr is a non-backptr that refers to the node we landed on.
    #[inline]
    pub fn repair_backptr_finish(&mut self, ptr: &TriePtr, block_hash: T) {
        // this can only be called if we walked to a backptr.
        // If it's anything else, we're in trouble.
        if Some(CursorError::ChrNotFound) == self.last_error
            || Some(CursorError::PathDiverged) == self.last_error
        {
            eprintln!("{:?}", &self.last_error);
            panic!();
        }
        assert!(!is_backptr(ptr.id()));

        trace!(
            "Cursor: repair_backptr_finish ptr={:?} block_hash={:?}",
            &ptr,
            &block_hash
        );

        self.node_ptrs.push(ptr.clone());
        self.block_hashes.push(block_hash);

        self.last_error = None;
    }
}

impl PartialEq for TrieLeaf {
    fn eq(&self, other: &TrieLeaf) -> bool {
        self.path == other.path && self.data.as_bytes() == other.data.as_bytes()
    }
}

impl TrieLeaf {
    pub fn new(path: &[u8], data: &[u8]) -> TrieLeaf {
        assert!(data.len() <= 40);
        let mut bytes = [0u8; 40];
        bytes.copy_from_slice(&data[..]);
        TrieLeaf {
            path: path.to_owned(),
            data: MARFValue(bytes),
        }
    }

    pub fn from_value(path: &[u8], value: MARFValue) -> TrieLeaf {
        TrieLeaf {
            path: path.to_owned(),
            data: value,
        }
    }
}

impl fmt::Debug for TrieLeaf {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "TrieLeaf(path={} data={})",
            &to_hex(&self.path),
            &self.data.to_hex()
        )
    }
}

impl StacksMessageCodec for TrieLeaf {
    fn consensus_serialize<W: Write>(&self, fd: &mut W) -> Result<(), codec_error> {
        self.path.consensus_serialize(fd)?;
        self.data.consensus_serialize(fd)
    }

    fn consensus_deserialize<R: Read>(fd: &mut R) -> Result<TrieLeaf, codec_error> {
        let path = read_next(fd)?;
        let data = read_next(fd)?;

        Ok(TrieLeaf { path, data })
    }
}

/// Trie node with four children
#[derive(Clone, PartialEq)]
pub struct TrieNode4 {
    pub path: Vec<u8>,
    pub ptrs: [TriePtr; 4],
}

impl fmt::Debug for TrieNode4 {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "TrieNode4(path={} ptrs={})",
            &to_hex(&self.path),
            ptrs_fmt(&self.ptrs)
        )
    }
}

impl TrieNode4 {
    pub fn new(path: &[u8]) -> TrieNode4 {
        TrieNode4 {
            path: path.to_owned(),
            ptrs: [TriePtr::default(); 4],
        }
    }
}

/// Trie node with 16 children
#[derive(Clone, PartialEq)]
pub struct TrieNode16 {
    pub path: Vec<u8>,
    pub ptrs: [TriePtr; 16],
}

impl fmt::Debug for TrieNode16 {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "TrieNode16(path={} ptrs={})",
            &to_hex(&self.path),
            ptrs_fmt(&self.ptrs)
        )
    }
}

impl TrieNode16 {
    pub fn new(path: &[u8]) -> TrieNode16 {
        TrieNode16 {
            path: path.to_owned(),
            ptrs: [TriePtr::default(); 16],
        }
    }

    /// Promote a Node4 to a Node16
    pub fn from_node4(node4: &TrieNode4) -> TrieNode16 {
        let mut ptrs = [TriePtr::default(); 16];
        ptrs[..4].copy_from_slice(&node4.ptrs[..4]);
        TrieNode16 {
            path: node4.path.clone(),
            ptrs,
        }
    }
}

/// Trie node with 48 children
#[derive(Clone)]
pub struct TrieNode48 {
    pub path: Vec<u8>,
    indexes: [i8; 256], // indexes[i], if non-negative, is an index into ptrs.
    pub ptrs: [TriePtr; 48],
}

impl fmt::Debug for TrieNode48 {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "TrieNode48(path={} ptrs={})",
            &to_hex(&self.path),
            ptrs_fmt(&self.ptrs)
        )
    }
}

impl PartialEq for TrieNode48 {
    fn eq(&self, other: &TrieNode48) -> bool {
        self.path == other.path && self.ptrs == other.ptrs && self.indexes == other.indexes
    }
}

impl TrieNode48 {
    pub fn new(path: &[u8]) -> TrieNode48 {
        TrieNode48 {
            path: path.to_owned(),
            indexes: [-1; 256],
            ptrs: [TriePtr::default(); 48],
        }
    }

    /// Promote a node16 to a node48
    pub fn from_node16(node16: &TrieNode16) -> TrieNode48 {
        let mut ptrs = [TriePtr::default(); 48];
        let mut indexes = [-1i8; 256];
        for i in 0..16 {
            ptrs[i] = node16.ptrs[i].clone();
            indexes[ptrs[i].chr() as usize] = i as i8;
        }
        TrieNode48 {
            path: node16.path.clone(),
            indexes,
            ptrs,
        }
    }
}

/// Trie node with 256 children
#[derive(Clone)]
pub struct TrieNode256 {
    pub path: Vec<u8>,
    pub ptrs: [TriePtr; 256],
}

impl fmt::Debug for TrieNode256 {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "TrieNode256(path={} ptrs={})",
            &to_hex(&self.path),
            ptrs_fmt(&self.ptrs)
        )
    }
}

impl PartialEq for TrieNode256 {
    fn eq(&self, other: &TrieNode256) -> bool {
        self.path == other.path && self.ptrs == other.ptrs
    }
}

impl TrieNode256 {
    pub fn new(path: &[u8]) -> TrieNode256 {
        TrieNode256 {
            path: path.to_owned(),
            ptrs: [TriePtr::default(); 256],
        }
    }

    pub fn from_node4(node4: &TrieNode4) -> TrieNode256 {
        let mut ptrs = [TriePtr::default(); 256];
        for i in 0..4 {
            let c = node4.ptrs[i].chr();
            ptrs[c as usize] = node4.ptrs[i].clone();
        }
        TrieNode256 {
            path: node4.path.clone(),
            ptrs,
        }
    }

    /// Promote a node48 to a node256
    pub fn from_node48(node48: &TrieNode48) -> TrieNode256 {
        let mut ptrs = [TriePtr::default(); 256];
        for i in 0..48 {
            let c = node48.ptrs[i].chr();
            ptrs[c as usize] = node48.ptrs[i].clone();
        }
        TrieNode256 {
            path: node48.path.clone(),
            ptrs,
        }
    }
}

impl TrieNode for TrieNode4 {
    fn id(&self) -> u8 {
        TrieNodeID::Node4 as u8
    }

    fn empty() -> TrieNode4 {
        TrieNode4 {
            path: vec![],
            ptrs: [TriePtr::default(); 4],
        }
    }

    fn walk(&self, chr: u8) -> Option<TriePtr> {
        for i in 0..4 {
            if self.ptrs[i].id() != TrieNodeID::Empty as u8 && self.ptrs[i].chr() == chr {
                return Some(self.ptrs[i].clone());
            }
        }
        return None;
    }

    fn from_bytes<R: Read>(r: &mut R) -> Result<TrieNode4, Error> {
        let mut ptrs_slice = [TriePtr::default(); 4];
        ptrs_from_bytes(TrieNodeID::Node4 as u8, r, &mut ptrs_slice)?;
        let path = path_from_bytes(r)?;

        Ok(TrieNode4 {
            path,
            ptrs: ptrs_slice,
        })
    }

    fn insert(&mut self, ptr: &TriePtr) -> bool {
        if self.replace(ptr) {
            return true;
        }

        for i in 0..4 {
            if self.ptrs[i].id() == TrieNodeID::Empty as u8 {
                self.ptrs[i] = ptr.clone();
                return true;
            }
        }
        return false;
    }

    fn replace(&mut self, ptr: &TriePtr) -> bool {
        for i in 0..4 {
            if self.ptrs[i].id() != TrieNodeID::Empty as u8 && self.ptrs[i].chr() == ptr.chr() {
                self.ptrs[i] = ptr.clone();
                return true;
            }
        }
        return false;
    }

    fn ptrs(&self) -> &[TriePtr] {
        &self.ptrs
    }

    fn path(&self) -> &Vec<u8> {
        &self.path
    }

    fn as_trie_node_type(&self) -> TrieNodeType {
        TrieNodeType::Node4(self.clone())
    }
}

impl TrieNode for TrieNode16 {
    fn id(&self) -> u8 {
        TrieNodeID::Node16 as u8
    }

    fn empty() -> TrieNode16 {
        TrieNode16 {
            path: vec![],
            ptrs: [TriePtr::default(); 16],
        }
    }

    fn walk(&self, chr: u8) -> Option<TriePtr> {
        for i in 0..16 {
            if self.ptrs[i].id != TrieNodeID::Empty as u8 && self.ptrs[i].chr == chr {
                return Some(self.ptrs[i].clone());
            }
        }
        return None;
    }

    fn from_bytes<R: Read>(r: &mut R) -> Result<TrieNode16, Error> {
        let mut ptrs_slice = [TriePtr::default(); 16];
        ptrs_from_bytes(TrieNodeID::Node16 as u8, r, &mut ptrs_slice)?;

        let path = path_from_bytes(r)?;

        Ok(TrieNode16 {
            path,
            ptrs: ptrs_slice,
        })
    }

    fn insert(&mut self, ptr: &TriePtr) -> bool {
        if self.replace(ptr) {
            return true;
        }

        for i in 0..16 {
            if self.ptrs[i].id() == TrieNodeID::Empty as u8 {
                self.ptrs[i] = ptr.clone();
                return true;
            }
        }
        return false;
    }

    fn replace(&mut self, ptr: &TriePtr) -> bool {
        for i in 0..16 {
            if self.ptrs[i].id() != TrieNodeID::Empty as u8 && self.ptrs[i].chr() == ptr.chr() {
                self.ptrs[i] = ptr.clone();
                return true;
            }
        }
        return false;
    }

    fn ptrs(&self) -> &[TriePtr] {
        &self.ptrs
    }

    fn path(&self) -> &Vec<u8> {
        &self.path
    }

    fn as_trie_node_type(&self) -> TrieNodeType {
        TrieNodeType::Node16(self.clone())
    }
}

impl TrieNode for TrieNode48 {
    fn id(&self) -> u8 {
        TrieNodeID::Node48 as u8
    }

    fn empty() -> TrieNode48 {
        TrieNode48 {
            path: vec![],
            indexes: [-1; 256],
            ptrs: [TriePtr::default(); 48],
        }
    }

    fn walk(&self, chr: u8) -> Option<TriePtr> {
        let idx = self.indexes[chr as usize];
        if idx >= 0 && idx < 48 && self.ptrs[idx as usize].id() != TrieNodeID::Empty as u8 {
            return Some(self.ptrs[idx as usize].clone());
        }
        return None;
    }

    fn write_bytes<W: Write>(&self, w: &mut W) -> Result<(), Error> {
        w.write_all(&[self.id()])?;
        write_ptrs_to_bytes(self.ptrs(), w)?;

        for i in self.indexes.iter() {
            w.write_all(&[*i as u8])?;
        }

        write_path_to_bytes(self.path().as_slice(), w)
    }

    fn byte_len(&self) -> usize {
        get_ptrs_byte_len(&self.ptrs) + 256 + get_path_byte_len(&self.path)
    }

    fn from_bytes<R: Read>(r: &mut R) -> Result<TrieNode48, Error> {
        let mut ptrs_slice = [TriePtr::default(); 48];
        ptrs_from_bytes(TrieNodeID::Node48 as u8, r, &mut ptrs_slice)?;

        let mut indexes = [0u8; 256];
        let l_indexes = r.read(&mut indexes).map_err(Error::IOError)?;

        if l_indexes != 256 {
            return Err(Error::CorruptionError(
                "Node48: Failed to read 256 indexes".to_string(),
            ));
        }

        let path = path_from_bytes(r)?;

        let indexes_i8: Vec<i8> = indexes
            .iter()
            .map(|i| {
                let j = *i as i8;
                j
            })
            .collect();
        let mut indexes_slice = [0i8; 256];
        indexes_slice.copy_from_slice(&indexes_i8[..]);

        // not a for-loop because "for ptr in ptrs_slice.iter()" is actually kinda slow
        let mut i = 0;
        while i < ptrs_slice.len() {
            let ptr = &ptrs_slice[i];
            if !(ptr.id() == TrieNodeID::Empty as u8
                || (indexes_slice[ptr.chr() as usize] >= 0
                    && indexes_slice[ptr.chr() as usize] < 48))
            {
                return Err(Error::CorruptionError(
                    "Node48: corrupt index array: invalid index value".to_string(),
                ));
            }
            i += 1;
        }

        // not a for-loop because "for i in 0..256" is actually kinda slow
        i = 0;
        while i < 256 {
            if !(indexes_slice[i] < 0
                || (indexes_slice[i] >= 0
                    && (indexes_slice[i] as usize) < ptrs_slice.len()
                    && ptrs_slice[indexes_slice[i] as usize].id() != TrieNodeID::Empty as u8))
            {
                return Err(Error::CorruptionError(
                    "Node48: corrupt index array: index points to empty node".to_string(),
                ));
            }
            i += 1;
        }

        Ok(TrieNode48 {
            path,
            indexes: indexes_slice,
            ptrs: ptrs_slice,
        })
    }

    fn insert(&mut self, ptr: &TriePtr) -> bool {
        if self.replace(ptr) {
            return true;
        }

        let c = ptr.chr();
        for i in 0..48 {
            if self.ptrs[i].id() == TrieNodeID::Empty as u8 {
                self.indexes[c as usize] = i as i8;
                self.ptrs[i] = ptr.clone();
                return true;
            }
        }
        return false;
    }

    fn replace(&mut self, ptr: &TriePtr) -> bool {
        let i = self.indexes[ptr.chr() as usize];
        if i >= 0 {
            self.ptrs[i as usize] = ptr.clone();
            return true;
        } else {
            return false;
        }
    }

    fn ptrs(&self) -> &[TriePtr] {
        &self.ptrs
    }

    fn path(&self) -> &Vec<u8> {
        &self.path
    }

    fn as_trie_node_type(&self) -> TrieNodeType {
        TrieNodeType::Node48(Box::new(self.clone()))
    }
}

impl TrieNode for TrieNode256 {
    fn id(&self) -> u8 {
        TrieNodeID::Node256 as u8
    }

    fn empty() -> TrieNode256 {
        TrieNode256 {
            path: vec![],
            ptrs: [TriePtr::default(); 256],
        }
    }

    fn walk(&self, chr: u8) -> Option<TriePtr> {
        if self.ptrs[chr as usize].id() != TrieNodeID::Empty as u8 {
            return Some(self.ptrs[chr as usize].clone());
        }
        return None;
    }

    fn from_bytes<R: Read>(r: &mut R) -> Result<TrieNode256, Error> {
        let mut ptrs_slice = [TriePtr::default(); 256];
        ptrs_from_bytes(TrieNodeID::Node256 as u8, r, &mut ptrs_slice)?;

        let path = path_from_bytes(r)?;

        Ok(TrieNode256 {
            path,
            ptrs: ptrs_slice,
        })
    }

    fn insert(&mut self, ptr: &TriePtr) -> bool {
        if self.replace(ptr) {
            return true;
        }
        let c = ptr.chr() as usize;
        self.ptrs[c] = ptr.clone();
        return true;
    }

    fn replace(&mut self, ptr: &TriePtr) -> bool {
        let c = ptr.chr() as usize;
        if self.ptrs[c].id() != TrieNodeID::Empty as u8 && self.ptrs[c].chr() == ptr.chr() {
            self.ptrs[c] = ptr.clone();
            return true;
        } else {
            return false;
        }
    }

    fn ptrs(&self) -> &[TriePtr] {
        &self.ptrs
    }

    fn path(&self) -> &Vec<u8> {
        &self.path
    }

    fn as_trie_node_type(&self) -> TrieNodeType {
        TrieNodeType::Node256(Box::new(self.clone()))
    }
}

impl TrieNode for TrieLeaf {
    fn id(&self) -> u8 {
        TrieNodeID::Leaf as u8
    }

    fn empty() -> TrieLeaf {
        TrieLeaf::new(&[], &[0u8; 40])
    }

    fn walk(&self, _chr: u8) -> Option<TriePtr> {
        None
    }

    fn write_bytes<W: Write>(&self, w: &mut W) -> Result<(), Error> {
        w.write_all(&[self.id()])?;
        write_path_to_bytes(&self.path, w)?;
        w.write_all(&self.data.0[..])?;
        Ok(())
    }

    fn byte_len(&self) -> usize {
        1 + get_path_byte_len(&self.path) + self.data.len()
    }

    fn from_bytes<R: Read>(r: &mut R) -> Result<TrieLeaf, Error> {
        let mut idbuf = [0u8; 1];
        let l_idbuf = r.read(&mut idbuf).map_err(Error::IOError)?;

        if l_idbuf != 1 {
            return Err(Error::CorruptionError(
                "Leaf: failed to read ID".to_string(),
            ));
        }

        if clear_backptr(idbuf[0]) != TrieNodeID::Leaf as u8 {
            return Err(Error::CorruptionError(format!(
                "Leaf: bad ID {:x}",
                idbuf[0]
            )));
        }

        let path = path_from_bytes(r)?;
        let mut leaf_data = [0u8; MARF_VALUE_ENCODED_SIZE as usize];
        let l_leaf_data = r.read(&mut leaf_data).map_err(Error::IOError)?;

        if l_leaf_data != (MARF_VALUE_ENCODED_SIZE as usize) {
            return Err(Error::CorruptionError(format!(
                "Leaf: read only {} out of {} bytes",
                l_leaf_data, MARF_VALUE_ENCODED_SIZE
            )));
        }

        Ok(TrieLeaf {
            path,
            data: MARFValue(leaf_data),
        })
    }

    fn insert(&mut self, _ptr: &TriePtr) -> bool {
        panic!("can't insert into a leaf");
    }

    fn replace(&mut self, _ptr: &TriePtr) -> bool {
        panic!("can't replace in a leaf");
    }

    fn ptrs(&self) -> &[TriePtr] {
        &[]
    }

    fn path(&self) -> &Vec<u8> {
        &self.path
    }

    fn as_trie_node_type(&self) -> TrieNodeType {
        TrieNodeType::Leaf(self.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrieNodeType {
    Node4(TrieNode4),
    Node16(TrieNode16),
    Node48(Box<TrieNode48>),
    Node256(Box<TrieNode256>),
    Leaf(TrieLeaf),
}

macro_rules! with_node {
    ($self: expr, $pat:pat, $s:expr) => {
        match $self {
            TrieNodeType::Node4($pat) => $s,
            TrieNodeType::Node16($pat) => $s,
            TrieNodeType::Node48($pat) => $s,
            TrieNodeType::Node256($pat) => $s,
            TrieNodeType::Leaf($pat) => $s,
        }
    };
}

impl TrieNodeType {
    pub fn is_leaf(&self) -> bool {
        match self {
            TrieNodeType::Leaf(_) => true,
            _ => false,
        }
    }

    pub fn is_node4(&self) -> bool {
        match self {
            TrieNodeType::Node4(_) => true,
            _ => false,
        }
    }

    pub fn is_node16(&self) -> bool {
        match self {
            TrieNodeType::Node16(_) => true,
            _ => false,
        }
    }

    pub fn is_node48(&self) -> bool {
        match self {
            TrieNodeType::Node48(_) => true,
            _ => false,
        }
    }

    pub fn is_node256(&self) -> bool {
        match self {
            TrieNodeType::Node256(_) => true,
            _ => false,
        }
    }

    pub fn id(&self) -> u8 {
        with_node!(self, ref data, data.id())
    }

    pub fn walk(&self, chr: u8) -> Option<TriePtr> {
        with_node!(self, ref data, data.walk(chr))
    }

    pub fn write_bytes<W: Write>(&self, w: &mut W) -> Result<(), Error> {
        with_node!(self, ref data, data.write_bytes(w))
    }

    pub fn write_consensus_bytes<W: Write, M: BlockMap>(
        &self,
        map: &mut M,
        w: &mut W,
    ) -> Result<(), Error> {
        with_node!(self, ref data, data.write_consensus_bytes(map, w))
    }

    pub fn byte_len(&self) -> usize {
        with_node!(self, ref data, data.byte_len())
    }

    pub fn insert(&mut self, ptr: &TriePtr) -> bool {
        with_node!(self, ref mut data, data.insert(ptr))
    }

    pub fn replace(&mut self, ptr: &TriePtr) -> bool {
        with_node!(self, ref mut data, data.replace(ptr))
    }

    pub fn ptrs(&self) -> &[TriePtr] {
        with_node!(self, ref data, data.ptrs())
    }

    pub fn ptrs_mut(&mut self) -> &mut [TriePtr] {
        match self {
            TrieNodeType::Node4(ref mut data) => &mut data.ptrs,
            TrieNodeType::Node16(ref mut data) => &mut data.ptrs,
            TrieNodeType::Node48(ref mut data) => &mut data.ptrs,
            TrieNodeType::Node256(ref mut data) => &mut data.ptrs,
            TrieNodeType::Leaf(_) => panic!("Leaf has no ptrs"),
        }
    }

    pub fn max_ptrs(&self) -> usize {
        match self {
            TrieNodeType::Node4(_) => 4,
            TrieNodeType::Node16(_) => 16,
            TrieNodeType::Node48(_) => 48,
            TrieNodeType::Node256(_) => 256,
            TrieNodeType::Leaf(_) => 0,
        }
    }

    pub fn path_bytes(&self) -> &Vec<u8> {
        with_node!(self, ref data, &data.path)
    }

    pub fn set_path(&mut self, new_path: Vec<u8>) {
        with_node!(self, ref mut data, data.path = new_path)
    }
}
