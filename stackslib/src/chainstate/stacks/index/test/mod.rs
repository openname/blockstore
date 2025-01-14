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

#![allow(unused_variables)]
#![allow(unused_assignments)]

use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Seek, SeekFrom};

use stacks_common::types::chainstate::StacksBlockId;
use stacks_common::util::get_epoch_time_ms;
use stacks_common::util::hash::to_hex;

use crate::chainstate::stacks::index::bits::*;
use crate::chainstate::stacks::index::marf::*;
use crate::chainstate::stacks::index::node::*;
use crate::chainstate::stacks::index::proofs::*;
use crate::chainstate::stacks::index::storage::*;
use crate::chainstate::stacks::index::trie::*;
use crate::chainstate::stacks::index::{MARFValue, MarfTrieId, TrieLeaf, TrieMerkleProof};
use crate::chainstate::stacks::{BlockHeaderHash, TrieHash};

pub mod cache;
pub mod file;
pub mod marf;
pub mod node;
pub mod proofs;
pub mod storage;
pub mod trie;

/// Print out a trie to stderr
pub fn dump_trie<T>(s: &mut TrieStorageConnection<T>)
where
    T: MarfTrieId,
{
    test_debug!("\n----- BEGIN TRIE ------");

    fn space(cnt: usize) -> String {
        let mut ret = vec![];
        for _ in 0..cnt {
            ret.push(" ".to_string());
        }
        ret.join("")
    }

    let root_ptr = s.root_ptr();
    let mut frontier: Vec<(TrieNodeType, TrieHash, usize)> = vec![];
    let (root, root_hash) = Trie::read_root(s).unwrap();
    frontier.push((root, root_hash, 0));

    while !frontier.is_empty() {
        let (next, next_hash, depth) = frontier.pop().unwrap();
        let (ptrs, path_len) = match next {
            TrieNodeType::Leaf(ref leaf_data) => {
                test_debug!("{}{} {:?}", &space(depth), next_hash, leaf_data);
                (vec![], leaf_data.path.len())
            }
            TrieNodeType::Node4(ref data) => {
                test_debug!("{}{} {:?}", &space(depth), next_hash, data);
                (data.ptrs.to_vec(), data.path.len())
            }
            TrieNodeType::Node16(ref data) => {
                test_debug!("{}{} {:?}", &space(depth), next_hash, data);
                (data.ptrs.to_vec(), data.path.len())
            }
            TrieNodeType::Node48(ref data) => {
                test_debug!("{}{} {:?}", &space(depth), next_hash, data);
                (data.ptrs.to_vec(), data.path.len())
            }
            TrieNodeType::Node256(ref data) => {
                test_debug!("{}{} {:?}", &space(depth), next_hash, data);
                (data.ptrs.to_vec(), data.path.len())
            }
        };
        for ptr in ptrs.iter() {
            if ptr.id() == TrieNodeID::Empty as u8 {
                continue;
            }
            if !is_backptr(ptr.id()) {
                let (child_node, child_hash) = s.read_nodetype(ptr).unwrap();
                frontier.push((child_node, child_hash, depth + path_len + 1));
            }
        }
    }

    test_debug!("----- END TRIE ------\n");
}

pub fn merkle_test(s: &mut TrieStorageConnection<BlockHeaderHash>, path: &[u8], value: &[u8]) {
    let (_, root_hash) = Trie::read_root(s).unwrap();
    let triepath = TrieHash::from_bytes(&path[..]).unwrap();

    let block_header = BlockHeaderHash([0u8; 32]);
    s.open_block(&block_header).unwrap();

    let mut marf_value = [0u8; 40];
    marf_value.copy_from_slice(&value[0..40]);

    let proof =
        TrieMerkleProof::from_path(s, &triepath, &MARFValue(marf_value.clone()), &block_header)
            .unwrap();
    let empty_root_to_block = HashMap::new();

    assert!(proof.verify(
        &triepath,
        &MARFValue(marf_value.clone()),
        &root_hash,
        &empty_root_to_block
    ));
}

pub fn merkle_test_marf(
    s: &mut TrieStorageConnection<BlockHeaderHash>,
    header: &BlockHeaderHash,
    path: &[u8],
    value: &[u8],
    root_to_block: Option<HashMap<TrieHash, BlockHeaderHash>>,
) -> HashMap<TrieHash, BlockHeaderHash> {
    test_debug!("---------");
    test_debug!(
        "MARF merkle prove: merkle_test_marf({:?}, {:?}, {:?})?",
        header,
        path,
        value
    );
    test_debug!("---------");

    s.open_block(header).unwrap();
    let (_, root_hash) = Trie::read_root(s).unwrap();
    let triepath = TrieHash::from_bytes(&path[..]).unwrap();

    let mut marf_value = [0u8; 40];
    marf_value.copy_from_slice(&value[0..40]);

    let proof = TrieMerkleProof::from_path(s, &triepath, &MARFValue(marf_value), header).unwrap();

    test_debug!("---------");
    test_debug!("MARF merkle verify: {:?}", &proof);
    test_debug!("MARF merkle verify target root hash: {:?}", &root_hash);
    test_debug!("MARF merkle verify source block: {:?}", header);
    test_debug!("---------");

    let root_to_block = root_to_block.unwrap_or_else(|| s.read_root_to_block_table().unwrap());

    assert!(proof.verify(
        &triepath,
        &MARFValue(marf_value),
        &root_hash,
        &root_to_block
    ));

    root_to_block
}

pub fn merkle_test_marf_key_value(
    s: &mut TrieStorageConnection<BlockHeaderHash>,
    header: &BlockHeaderHash,
    key: &String,
    value: &String,
    root_to_block: Option<HashMap<TrieHash, BlockHeaderHash>>,
) -> HashMap<TrieHash, BlockHeaderHash> {
    test_debug!("---------");
    test_debug!(
        "MARF merkle prove: merkle_test_marf({:?}, {:?}, {:?})?",
        header,
        key,
        value
    );
    test_debug!("---------");

    s.open_block(header).unwrap();
    let (_, root_hash) = Trie::read_root(s).unwrap();
    let proof = TrieMerkleProof::from_entry(s, key, value, &header).unwrap();

    test_debug!("---------");
    test_debug!("MARF merkle verify: {:?}", &proof);
    test_debug!("MARF merkle verify target root hash: {:?}", &root_hash);
    test_debug!("MARF merkle verify source block: {:?}", header);
    test_debug!("---------");

    let root_to_block = root_to_block.unwrap_or_else(|| s.read_root_to_block_table().unwrap());
    let triepath = TrieHash::from_key(key);
    let marf_value = MARFValue::from_value(value);

    assert!(proof.verify(&triepath, &marf_value, &root_hash, &root_to_block));

    root_to_block
}

pub fn make_node_path(
    s: &mut TrieStorageConnection<BlockHeaderHash>,
    node_id: u8,
    path_segments: &[(Vec<u8>, u8)],
    leaf_data: Vec<u8>,
) -> (Vec<TrieNodeType>, Vec<TriePtr>, Vec<TrieHash>) {
    // make a fully-fleshed-out path of node's to a leaf
    let root_ptr = s.root_ptr();
    let root = TrieNode256::new(&path_segments[0].0);
    let root_hash = TrieHash::from_data(&[0u8; 32]); // don't care about this in this test
    s.write_node(root_ptr, &root, root_hash.clone()).unwrap();

    let mut parent = TrieNodeType::Node256(Box::new(root));
    let mut parent_ptr = root_ptr;

    let mut nodes = vec![];
    let mut node_ptrs = vec![];
    let mut hashes = vec![];
    let mut seg_id = 0;

    for i in 0..path_segments.len() - 1 {
        let path_segment = &path_segments[i + 1].0;
        let chr = path_segments[i].1;
        let node_ptr = s.last_ptr().unwrap();

        let node = match TrieNodeID::from_u8(node_id).unwrap() {
            TrieNodeID::Node4 => TrieNodeType::Node4(TrieNode4::new(path_segment)),
            TrieNodeID::Node16 => TrieNodeType::Node16(TrieNode16::new(path_segment)),
            TrieNodeID::Node48 => TrieNodeType::Node48(Box::new(TrieNode48::new(path_segment))),
            TrieNodeID::Node256 => TrieNodeType::Node256(Box::new(TrieNode256::new(path_segment))),
            _ => panic!("invalid node ID"),
        };

        s.write_nodetype(
            node_ptr,
            &node,
            TrieHash::from_data(&[(seg_id + 1) as u8; 32]),
        )
        .unwrap();

        // update parent
        match parent {
            TrieNodeType::Node256(ref mut data) => {
                assert!(data.insert(&TriePtr::new(node_id, chr, node_ptr)))
            }
            TrieNodeType::Node48(ref mut data) => {
                assert!(data.insert(&TriePtr::new(node_id, chr, node_ptr)))
            }
            TrieNodeType::Node16(ref mut data) => {
                assert!(data.insert(&TriePtr::new(node_id, chr, node_ptr)))
            }
            TrieNodeType::Node4(ref mut data) => {
                assert!(data.insert(&TriePtr::new(node_id, chr, node_ptr)))
            }
            TrieNodeType::Leaf(_) => panic!("can't insert into leaf"),
        };

        s.write_nodetype(
            parent_ptr,
            &parent,
            TrieHash::from_data(&[seg_id as u8; 32]),
        )
        .unwrap();

        nodes.push(parent.clone());
        node_ptrs.push(TriePtr::new(node_id, chr, node_ptr));
        hashes.push(TrieHash::from_data(&[(seg_id + 1) as u8; 32]));

        parent = node;
        parent_ptr = node_ptr;

        seg_id += 1;
    }

    // add a leaf at the end
    let child = TrieLeaf::new(&path_segments[path_segments.len() - 1].0, &leaf_data);
    let child_chr = path_segments[path_segments.len() - 1].1;
    let child_ptr = s.last_ptr().unwrap();
    s.write_node(
        child_ptr,
        &child,
        TrieHash::from_data(&[(seg_id + 1) as u8; 32]),
    )
    .unwrap();

    // update parent
    match parent {
        TrieNodeType::Node256(ref mut data) => {
            assert!(data.insert(&TriePtr::new(TrieNodeID::Leaf as u8, child_chr, child_ptr)))
        }
        TrieNodeType::Node48(ref mut data) => {
            assert!(data.insert(&TriePtr::new(TrieNodeID::Leaf as u8, child_chr, child_ptr)))
        }
        TrieNodeType::Node16(ref mut data) => {
            assert!(data.insert(&TriePtr::new(TrieNodeID::Leaf as u8, child_chr, child_ptr)))
        }
        TrieNodeType::Node4(ref mut data) => {
            assert!(data.insert(&TriePtr::new(TrieNodeID::Leaf as u8, child_chr, child_ptr)))
        }
        TrieNodeType::Leaf(_) => panic!("can't insert into leaf"),
    };

    s.write_nodetype(
        parent_ptr,
        &parent,
        TrieHash::from_data(&[(seg_id) as u8; 32]),
    )
    .unwrap();

    nodes.push(parent.clone());
    node_ptrs.push(TriePtr::new(TrieNodeID::Leaf as u8, child_chr, child_ptr));
    hashes.push(TrieHash::from_data(&[(seg_id + 1) as u8; 32]));

    (nodes, node_ptrs, hashes)
}

pub fn make_node4_path(
    s: &mut TrieStorageConnection<BlockHeaderHash>,
    path_segments: &[(Vec<u8>, u8)],
    leaf_data: Vec<u8>,
) -> (Vec<TrieNodeType>, Vec<TriePtr>, Vec<TrieHash>) {
    make_node_path(s, TrieNodeID::Node4 as u8, path_segments, leaf_data)
}
