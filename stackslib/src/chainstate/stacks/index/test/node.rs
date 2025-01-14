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

use std::io::Cursor;

use super::*;
use crate::chainstate::stacks::index::bits::*;
use crate::chainstate::stacks::index::marf::*;
use crate::chainstate::stacks::index::node::*;
use crate::chainstate::stacks::index::proofs::*;
use crate::chainstate::stacks::index::storage::*;
use crate::chainstate::stacks::index::test::*;
use crate::chainstate::stacks::index::trie::*;
use crate::chainstate::stacks::index::*;

#[test]
fn trieptr_to_bytes() {
    let mut t = TriePtr::new(0x11, 0x22, 0x33445566);
    t.back_block = 0x778899aa;

    let t_bytes = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa];

    let mut buf = Vec::new();
    t.write_bytes(&mut buf).unwrap();
    assert_eq!(buf, t_bytes);
    assert_eq!(TriePtr::from_bytes(&t_bytes[..]), t);
}

#[test]
fn trie_node4_to_bytes() {
    let mut node4 = TrieNode4::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
    ]);
    for i in 0..3 {
        assert!(node4.insert(&TriePtr::new(
            TrieNodeID::Node16 as u8,
            (i + 1) as u8,
            (i + 2) as u32
        )));
    }
    let node4_bytes = vec![
        // node ID
        TrieNodeID::Node4 as u8,
        // ptrs (4)
        TrieNodeID::Node16 as u8,
        0x01,
        0x00,
        0x00,
        0x00,
        0x2,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node16 as u8,
        0x02,
        0x00,
        0x00,
        0x00,
        0x3,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node16 as u8,
        0x03,
        0x00,
        0x00,
        0x00,
        0x4,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Empty as u8,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        // path length
        0x14,
        // path
        0x00,
        0x01,
        0x02,
        0x03,
        0x04,
        0x05,
        0x06,
        0x07,
        0x08,
        0x09,
        0x0a,
        0x0b,
        0x0c,
        0x0d,
        0x0e,
        0x0f,
        0x10,
        0x11,
        0x12,
        0x13,
    ];
    let mut node4_stream = Cursor::new(node4_bytes.clone());
    let buf = node4.to_bytes();
    assert_eq!(buf, node4_bytes);
    assert_eq!(node4.byte_len(), node4_bytes.len());
    assert_eq!(TrieNode4::from_bytes(&mut node4_stream).unwrap(), node4);
}

#[test]
fn trie_node4_to_consensus_bytes() {
    let mut node4 = TrieNode4::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
    ]);
    for i in 0..3 {
        assert!(node4.insert(&TriePtr::new(
            TrieNodeID::Node16 as u8,
            (i + 1) as u8,
            (i + 2) as u32
        )));
    }
    let node4_bytes = vec![
        // node ID
        TrieNodeID::Node4 as u8,
        // ptrs (4): ID, chr, block-header-hash
        TrieNodeID::Node16 as u8,
        0x01,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node16 as u8,
        0x02,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node16 as u8,
        0x03,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Empty as u8,
        0x00,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        // path length
        0x14,
        // path
        0x00,
        0x01,
        0x02,
        0x03,
        0x04,
        0x05,
        0x06,
        0x07,
        0x08,
        0x09,
        0x0a,
        0x0b,
        0x0c,
        0x0d,
        0x0e,
        0x0f,
        0x10,
        0x11,
        0x12,
        0x13,
    ];

    let buf = node4.to_consensus_bytes(&mut ());
    assert_eq!(to_hex(buf.as_slice()), to_hex(node4_bytes.as_slice()));
}

#[test]
fn trie_node16_to_bytes() {
    let mut node16 = TrieNode16::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
    ]);
    for i in 0..15 {
        assert!(node16.insert(&TriePtr::new(
            TrieNodeID::Node48 as u8,
            (i + 1) as u8,
            (i + 2) as u32
        )));
    }
    let node16_bytes = vec![
        // node ID
        TrieNodeID::Node16 as u8,
        // ptrs (16)
        TrieNodeID::Node48 as u8,
        0x01,
        0x00,
        0x00,
        0x00,
        0x02,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node48 as u8,
        0x02,
        0x00,
        0x00,
        0x00,
        0x03,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node48 as u8,
        0x03,
        0x00,
        0x00,
        0x00,
        0x04,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node48 as u8,
        0x04,
        0x00,
        0x00,
        0x00,
        0x05,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node48 as u8,
        0x05,
        0x00,
        0x00,
        0x00,
        0x06,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node48 as u8,
        0x06,
        0x00,
        0x00,
        0x00,
        0x07,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node48 as u8,
        0x07,
        0x00,
        0x00,
        0x00,
        0x08,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node48 as u8,
        0x08,
        0x00,
        0x00,
        0x00,
        0x09,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node48 as u8,
        0x09,
        0x00,
        0x00,
        0x00,
        0x0a,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node48 as u8,
        0x0a,
        0x00,
        0x00,
        0x00,
        0x0b,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node48 as u8,
        0x0b,
        0x00,
        0x00,
        0x00,
        0x0c,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node48 as u8,
        0x0c,
        0x00,
        0x00,
        0x00,
        0x0d,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node48 as u8,
        0x0d,
        0x00,
        0x00,
        0x00,
        0x0e,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node48 as u8,
        0x0e,
        0x00,
        0x00,
        0x00,
        0x0f,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node48 as u8,
        0x0f,
        0x00,
        0x00,
        0x00,
        0x10,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Empty as u8,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        // path length
        0x14,
        // path
        0x00,
        0x01,
        0x02,
        0x03,
        0x04,
        0x05,
        0x06,
        0x07,
        0x08,
        0x09,
        0x0a,
        0x0b,
        0x0c,
        0x0d,
        0x0e,
        0x0f,
        0x10,
        0x11,
        0x12,
        0x13,
    ];
    let mut node16_stream = Cursor::new(node16_bytes.clone());
    let buf = node16.to_bytes();
    assert_eq!(buf, node16_bytes);
    assert_eq!(node16.byte_len(), node16_bytes.len());
    assert_eq!(TrieNode16::from_bytes(&mut node16_stream).unwrap(), node16);
}

#[test]
fn trie_node16_to_consensus_bytes() {
    let mut node16 = TrieNode16::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
    ]);
    for i in 0..15 {
        assert!(node16.insert(&TriePtr::new(
            TrieNodeID::Node48 as u8,
            (i + 1) as u8,
            (i + 2) as u32
        )));
    }
    let node16_bytes = vec![
        // node ID
        TrieNodeID::Node16 as u8,
        TrieNodeID::Node48 as u8,
        0x01,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node48 as u8,
        0x02,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node48 as u8,
        0x03,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node48 as u8,
        0x04,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node48 as u8,
        0x05,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node48 as u8,
        0x06,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node48 as u8,
        0x07,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node48 as u8,
        0x08,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node48 as u8,
        0x09,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node48 as u8,
        0x0a,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node48 as u8,
        0x0b,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node48 as u8,
        0x0c,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node48 as u8,
        0x0d,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node48 as u8,
        0x0e,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node48 as u8,
        0x0f,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Empty as u8,
        0x00,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        // path length
        0x14,
        // path
        0x00,
        0x01,
        0x02,
        0x03,
        0x04,
        0x05,
        0x06,
        0x07,
        0x08,
        0x09,
        0x0a,
        0x0b,
        0x0c,
        0x0d,
        0x0e,
        0x0f,
        0x10,
        0x11,
        0x12,
        0x13,
    ];
    let buf = node16.to_consensus_bytes(&mut ());
    assert_eq!(to_hex(buf.as_slice()), to_hex(node16_bytes.as_slice()));
}

#[test]
fn trie_node48_to_bytes() {
    let mut node48 = TrieNode48::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
    ]);
    for i in 0..47 {
        assert!(node48.insert(&TriePtr::new(
            TrieNodeID::Node256 as u8,
            (i + 1) as u8,
            (i + 2) as u32
        )));
    }

    let node48_bytes = vec![
        // node ID
        TrieNodeID::Node48 as u8,
        // ptrs (48)
        TrieNodeID::Node256 as u8,
        0x01,
        0x00,
        0x00,
        0x00,
        0x02,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x02,
        0x00,
        0x00,
        0x00,
        0x03,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x03,
        0x00,
        0x00,
        0x00,
        0x04,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x04,
        0x00,
        0x00,
        0x00,
        0x05,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x05,
        0x00,
        0x00,
        0x00,
        0x06,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x06,
        0x00,
        0x00,
        0x00,
        0x07,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x07,
        0x00,
        0x00,
        0x00,
        0x08,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x08,
        0x00,
        0x00,
        0x00,
        0x09,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x09,
        0x00,
        0x00,
        0x00,
        0x0a,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x0a,
        0x00,
        0x00,
        0x00,
        0x0b,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x0b,
        0x00,
        0x00,
        0x00,
        0x0c,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x0c,
        0x00,
        0x00,
        0x00,
        0x0d,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x0d,
        0x00,
        0x00,
        0x00,
        0x0e,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x0e,
        0x00,
        0x00,
        0x00,
        0x0f,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x0f,
        0x00,
        0x00,
        0x00,
        0x10,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x10,
        0x00,
        0x00,
        0x00,
        0x11,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x11,
        0x00,
        0x00,
        0x00,
        0x12,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x12,
        0x00,
        0x00,
        0x00,
        0x13,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x13,
        0x00,
        0x00,
        0x00,
        0x14,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x14,
        0x00,
        0x00,
        0x00,
        0x15,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x15,
        0x00,
        0x00,
        0x00,
        0x16,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x16,
        0x00,
        0x00,
        0x00,
        0x17,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x17,
        0x00,
        0x00,
        0x00,
        0x18,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x18,
        0x00,
        0x00,
        0x00,
        0x19,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x19,
        0x00,
        0x00,
        0x00,
        0x1a,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x1a,
        0x00,
        0x00,
        0x00,
        0x1b,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x1b,
        0x00,
        0x00,
        0x00,
        0x1c,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x1c,
        0x00,
        0x00,
        0x00,
        0x1d,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x1d,
        0x00,
        0x00,
        0x00,
        0x1e,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x1e,
        0x00,
        0x00,
        0x00,
        0x1f,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x1f,
        0x00,
        0x00,
        0x00,
        0x20,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x20,
        0x00,
        0x00,
        0x00,
        0x21,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x21,
        0x00,
        0x00,
        0x00,
        0x22,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x22,
        0x00,
        0x00,
        0x00,
        0x23,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x23,
        0x00,
        0x00,
        0x00,
        0x24,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x24,
        0x00,
        0x00,
        0x00,
        0x25,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x25,
        0x00,
        0x00,
        0x00,
        0x26,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x26,
        0x00,
        0x00,
        0x00,
        0x27,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x27,
        0x00,
        0x00,
        0x00,
        0x28,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x28,
        0x00,
        0x00,
        0x00,
        0x29,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x29,
        0x00,
        0x00,
        0x00,
        0x2a,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x2a,
        0x00,
        0x00,
        0x00,
        0x2b,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x2b,
        0x00,
        0x00,
        0x00,
        0x2c,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x2c,
        0x00,
        0x00,
        0x00,
        0x2d,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x2d,
        0x00,
        0x00,
        0x00,
        0x2e,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x2e,
        0x00,
        0x00,
        0x00,
        0x2f,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Node256 as u8,
        0x2f,
        0x00,
        0x00,
        0x00,
        0x30,
        0x00,
        0x00,
        0x00,
        0x00,
        TrieNodeID::Empty as u8,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        // indexes (256)
        255,
        0,
        1,
        2,
        3,
        4,
        5,
        6,
        7,
        8,
        9,
        10,
        11,
        12,
        13,
        14,
        15,
        16,
        17,
        18,
        19,
        20,
        21,
        22,
        23,
        24,
        25,
        26,
        27,
        28,
        29,
        30,
        31,
        32,
        33,
        34,
        35,
        36,
        37,
        38,
        39,
        40,
        41,
        42,
        43,
        44,
        45,
        46,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        255,
        // path len
        0x14,
        // path
        0x00,
        0x01,
        0x02,
        0x03,
        0x04,
        0x05,
        0x06,
        0x07,
        0x08,
        0x09,
        0x0a,
        0x0b,
        0x0c,
        0x0d,
        0x0e,
        0x0f,
        0x10,
        0x11,
        0x12,
        0x13,
    ];
    let mut node48_stream = Cursor::new(node48_bytes.clone());

    let buf = node48.to_bytes();
    assert_eq!(buf, node48_bytes);
    assert_eq!(node48.byte_len(), node48_bytes.len());
    assert_eq!(TrieNode48::from_bytes(&mut node48_stream).unwrap(), node48);
}

#[test]
fn trie_node48_to_consensus_bytes() {
    let mut node48 = TrieNode48::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
    ]);
    for i in 0..47 {
        assert!(node48.insert(&TriePtr::new(
            TrieNodeID::Node256 as u8,
            (i + 1) as u8,
            (i + 2) as u32
        )));
    }
    let node48_bytes = vec![
        // node ID
        TrieNodeID::Node48 as u8,
        // ptrs (48)
        TrieNodeID::Node256 as u8,
        0x01,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x02,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x03,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x04,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x05,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x06,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x07,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x08,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x09,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x0a,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x0b,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x0c,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x0d,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x0e,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x0f,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x10,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x11,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x12,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x13,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x14,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x15,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x16,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x17,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x18,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x19,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x1a,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x1b,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x1c,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x1d,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x1e,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x1f,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x20,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x21,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x22,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x23,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x24,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x25,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x26,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x27,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x28,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x29,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x2a,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x2b,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x2c,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x2d,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x2e,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Node256 as u8,
        0x2f,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        TrieNodeID::Empty as u8,
        0x00,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        // path len
        0x14,
        // path
        0x00,
        0x01,
        0x02,
        0x03,
        0x04,
        0x05,
        0x06,
        0x07,
        0x08,
        0x09,
        0x0a,
        0x0b,
        0x0c,
        0x0d,
        0x0e,
        0x0f,
        0x10,
        0x11,
        0x12,
        0x13,
    ];
    let buf = node48.to_consensus_bytes(&mut ());
    assert_eq!(buf, node48_bytes);
}

#[test]
fn trie_node256_to_bytes() {
    let mut node256 = TrieNode256::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
    ]);
    for i in 0..255 {
        assert!(node256.insert(&TriePtr::new(
            TrieNodeID::Node256 as u8,
            i as u8,
            (i + 2) % 256
        )));
    }

    let mut node256_bytes = vec![
        // node ID
        TrieNodeID::Node256 as u8,
    ];
    // ptrs (256)
    for i in 0..255 {
        node256_bytes.append(&mut vec![
            TrieNodeID::Node256 as u8,
            i as u8,
            0,
            0,
            0,
            (((i + 2) % 256) as u8),
            0,
            0,
            0,
            0,
        ]);
    }
    // last ptr is empty
    node256_bytes.append(&mut vec![
        TrieNodeID::Empty as u8,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    // path
    node256_bytes.append(&mut vec![
        // path len
        0x14, // path
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13,
    ]);

    let mut node256_stream = Cursor::new(node256_bytes.clone());

    let buf = node256.to_bytes();
    assert_eq!(buf, node256_bytes);
    assert_eq!(node256.byte_len(), node256_bytes.len());
    assert_eq!(
        TrieNode256::from_bytes(&mut node256_stream).unwrap(),
        node256
    );
}

#[test]
fn trie_node256_to_consensus_bytes() {
    let mut node256 = TrieNode256::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
    ]);
    for i in 0..255 {
        assert!(node256.insert(&TriePtr::new(
            TrieNodeID::Node256 as u8,
            i as u8,
            (i + 2) % 256
        )));
    }

    let mut node256_bytes = vec![
        // node ID
        TrieNodeID::Node256 as u8,
    ];
    // ptrs (256)

    let pointer_back_block_bytes = [0; 32];
    for i in 0..255 {
        node256_bytes.append(&mut vec![
            TrieNodeID::Node256 as u8,
            i as u8,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
    }
    // last ptr is empty
    node256_bytes.append(&mut vec![
        TrieNodeID::Empty as u8,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);

    // path
    node256_bytes.append(&mut vec![
        // path len
        0x14, // path
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13,
    ]);

    let buf = node256.to_consensus_bytes(&mut ());
    assert_eq!(buf, node256_bytes);
}

#[test]
fn trie_leaf_to_bytes() {
    let leaf = TrieLeaf::new(
        &[
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
        ],
        &[
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
            24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39,
        ],
    );
    let leaf_bytes = vec![
        // node ID
        TrieNodeID::Leaf as u8,
        // path len
        0x14,
        // path
        0,
        1,
        2,
        3,
        4,
        5,
        6,
        7,
        8,
        9,
        10,
        11,
        12,
        13,
        14,
        15,
        16,
        17,
        18,
        19,
        // data
        0,
        1,
        2,
        3,
        4,
        5,
        6,
        7,
        8,
        9,
        10,
        11,
        12,
        13,
        14,
        15,
        16,
        17,
        18,
        19,
        20,
        21,
        22,
        23,
        24,
        25,
        26,
        27,
        28,
        29,
        30,
        31,
        32,
        33,
        34,
        35,
        36,
        37,
        38,
        39,
    ];

    let buf = leaf.to_bytes();

    assert_eq!(buf, leaf_bytes);
    assert_eq!(leaf.byte_len(), buf.len());
}

#[test]
fn read_write_node4() {
    let mut node4 = TrieNode4::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
    ]);
    for i in 0..3 {
        assert!(node4.insert(&TriePtr::new(
            TrieNodeID::Node16 as u8,
            (i + 1) as u8,
            (i + 2) as u32
        )));
    }
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let hash = TrieHash::from_data(&[0u8; 32]);
    let wres = trie_io.write_nodetype(0, &TrieNodeType::Node4(node4.clone()), hash.clone());
    assert!(wres.is_ok());

    let rres = trie_io.read_nodetype(&TriePtr::new(TrieNodeID::Node4 as u8, 0, 0));

    assert!(rres.is_ok());
    assert_eq!(rres.unwrap(), (TrieNodeType::Node4(node4.clone()), hash));
}

#[test]
fn read_write_node16() {
    let mut node16 = TrieNode16::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
    ]);
    for i in 0..16 {
        assert!(node16.insert(&TriePtr::new(
            TrieNodeID::Node48 as u8,
            (i + 1) as u8,
            (i + 2) as u32
        )));
    }

    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let hash = TrieHash::from_data(&[0u8; 32]);
    let wres = trie_io.write_nodetype(0, &TrieNodeType::Node16(node16.clone()), hash.clone());
    assert!(wres.is_ok());

    let rres = trie_io.read_nodetype(&TriePtr::new(TrieNodeID::Node16 as u8, 0, 0));

    assert!(rres.is_ok());
    assert_eq!(rres.unwrap(), (TrieNodeType::Node16(node16.clone()), hash));
}

#[test]
fn read_write_node48() {
    let mut node48 = TrieNode48::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
    ]);
    for i in 0..48 {
        assert!(node48.insert(&TriePtr::new(
            TrieNodeID::Node256 as u8,
            (i + 1) as u8,
            (i + 2) as u32
        )));
    }

    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let hash = TrieHash::from_data(&[0u8; 32]);
    let wres = trie_io.write_nodetype(0, &node48.as_trie_node_type(), hash.clone());
    assert!(wres.is_ok());

    let rres = trie_io.read_nodetype(&TriePtr::new(TrieNodeID::Node48 as u8, 0, 0));

    assert!(rres.is_ok());
    assert_eq!(rres.unwrap(), (node48.as_trie_node_type(), hash));
}

#[test]
fn read_write_node256() {
    let mut node256 = TrieNode256::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
    ]);
    for i in 0..256 {
        assert!(node256.insert(&TriePtr::new(
            TrieNodeID::Node256 as u8,
            (i + 1) as u8,
            (i + 2) as u32
        )));
    }

    let hash = TrieHash::from_data(&[0u8; 32]);
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let wres = trie_io.write_nodetype(0, &node256.as_trie_node_type(), hash.clone());
    assert!(wres.is_ok());

    let root_ptr = trie_io.root_ptr();
    let rres = trie_io.read_nodetype(&TriePtr::new(TrieNodeID::Node256 as u8, 0, root_ptr));

    assert!(rres.is_ok());
    assert_eq!(rres.unwrap(), (node256.as_trie_node_type(), hash));
}

#[test]
fn read_write_leaf() {
    let leaf = TrieLeaf::new(
        &[
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
        ],
        &[
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
            24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39,
        ],
    );

    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let hash = TrieHash::from_data(&[0u8; 32]);
    let wres = trie_io.write_nodetype(0, &TrieNodeType::Leaf(leaf.clone()), hash.clone());
    assert!(wres.is_ok());

    let rres = trie_io.read_nodetype(&TriePtr::new(TrieNodeID::Leaf as u8, 0, 0));

    assert!(rres.is_ok());
    assert_eq!(rres.unwrap(), (TrieNodeType::Leaf(leaf.clone()), hash));
}

#[test]
fn read_write_node4_hashes() {
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let mut node4 = TrieNode4::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
    ]);
    let hash = TrieHash::from_data(&[0u8; 32]);

    let mut child_hashes = vec![];
    for i in 0..3 {
        let child = TrieLeaf::new(
            &[
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, i as u8,
            ],
            &[i as u8; 40],
        );
        let child_hash = get_leaf_hash(&child);

        child_hashes.push(child_hash.clone());

        let ptr = trie_io.last_ptr().unwrap();
        trie_io.write_node(ptr, &child, child_hash).unwrap();
        assert!(node4.insert(&TriePtr::new(TrieNodeID::Leaf as u8, i as u8, ptr)));
    }

    // no final child
    child_hashes.push(TrieHash::from_data(&[]));

    let node4_ptr = trie_io.last_ptr().unwrap();
    let node4_hash = get_node_hash(&node4, &child_hashes, &mut trie_io);
    trie_io.write_node(node4_ptr, &node4, node4_hash).unwrap();

    let read_child_hashes =
        Trie::get_children_hashes(&mut trie_io, &TrieNodeType::Node4(node4)).unwrap();

    assert_eq!(read_child_hashes, child_hashes);
}

#[test]
fn read_write_node16_hashes() {
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let mut node16 = TrieNode16::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
    ]);
    let hash = TrieHash::from_data(&[0u8; 32]);

    let mut child_hashes = vec![];
    for i in 0..15 {
        let child = TrieLeaf::new(
            &[
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, i as u8,
            ],
            &[i as u8; 40],
        );
        let child_hash = get_leaf_hash(&child);

        child_hashes.push(child_hash.clone());

        let ptr = trie_io.last_ptr().unwrap();
        trie_io.write_node(ptr, &child, child_hash).unwrap();
        assert!(node16.insert(&TriePtr::new(TrieNodeID::Leaf as u8, i as u8, ptr)));
    }

    // no final child
    child_hashes.push(TrieHash::from_data(&[]));

    let node16_ptr = trie_io.last_ptr().unwrap();
    let node16_hash = get_node_hash(&node16, &child_hashes, &mut trie_io);
    trie_io
        .write_node(node16_ptr, &node16, node16_hash)
        .unwrap();

    let read_child_hashes =
        Trie::get_children_hashes(&mut trie_io, &TrieNodeType::Node16(node16)).unwrap();

    assert_eq!(read_child_hashes, child_hashes);
}

#[test]
fn read_write_node48_hashes() {
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let mut node48 = TrieNode48::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
    ]);
    let hash = TrieHash::from_data(&[0u8; 32]);

    let mut child_hashes = vec![];
    for i in 0..47 {
        let child = TrieLeaf::new(
            &[
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, i as u8,
            ],
            &[i as u8; 40],
        );
        let child_hash = get_leaf_hash(&child);

        child_hashes.push(child_hash.clone());

        let ptr = trie_io.last_ptr().unwrap();
        trie_io.write_node(ptr, &child, child_hash).unwrap();
        assert!(node48.insert(&TriePtr::new(TrieNodeID::Leaf as u8, i as u8, ptr)));
    }

    // no final child
    child_hashes.push(TrieHash::from_data(&[]));

    let node48_ptr = trie_io.last_ptr().unwrap();
    let node48_hash = get_node_hash(&node48, &child_hashes, &mut trie_io);
    trie_io
        .write_node(node48_ptr, &node48, node48_hash)
        .unwrap();

    let read_child_hashes =
        Trie::get_children_hashes(&mut trie_io, &TrieNodeType::Node48(Box::new(node48))).unwrap();

    assert_eq!(read_child_hashes, child_hashes);
}

#[test]
fn read_write_node256_hashes() {
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let mut node256 = TrieNode256::new(&[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
    ]);
    let hash = TrieHash::from_data(&[0u8; 32]);

    let mut child_hashes = vec![];
    for i in 0..255 {
        let child = TrieLeaf::new(
            &[
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, i as u8,
            ],
            &[i as u8; 40],
        );
        let child_hash = get_leaf_hash(&child);

        child_hashes.push(child_hash.clone());

        let ptr = trie_io.last_ptr().unwrap();
        trie_io.write_node(ptr, &child, child_hash).unwrap();
        assert!(node256.insert(&TriePtr::new(TrieNodeID::Leaf as u8, i as u8, ptr)));
    }

    // no final child
    child_hashes.push(TrieHash::from_data(&[]));

    let node256_ptr = trie_io.last_ptr().unwrap();
    let node256_hash = get_node_hash(&node256, &child_hashes, &mut trie_io);
    trie_io
        .write_node(node256_ptr, &node256, node256_hash)
        .unwrap();

    let read_child_hashes =
        Trie::get_children_hashes(&mut trie_io, &TrieNodeType::Node256(Box::new(node256))).unwrap();

    assert_eq!(read_child_hashes, child_hashes);
}

#[test]
fn trie_cursor_walk_full() {
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let path_segments = vec![
        (vec![], 0),
        (vec![], 1),
        (vec![], 2),
        (vec![], 3),
        (vec![], 4),
        (vec![], 5),
        (vec![], 6),
        (vec![], 7),
        (vec![], 8),
        (vec![], 9),
        (vec![], 10),
        (vec![], 11),
        (vec![], 12),
        (vec![], 13),
        (vec![], 14),
        (vec![], 15),
        (vec![], 16),
        (vec![], 17),
        (vec![], 18),
        (vec![], 19),
        (vec![], 20),
        (vec![], 21),
        (vec![], 22),
        (vec![], 23),
        (vec![], 24),
        (vec![], 25),
        (vec![], 26),
        (vec![], 27),
        (vec![], 28),
        (vec![], 29),
        (vec![], 30),
        (vec![], 31),
    ];
    let path = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    let (nodes, node_ptrs, hashes) =
        make_node4_path(&mut trie_io, &path_segments, [31u8; 40].to_vec());

    assert_eq!(nodes.len(), 32);
    assert_eq!(node_ptrs.len(), 32);
    assert_eq!(hashes.len(), 32);

    assert_eq!(node_ptrs[node_ptrs.len() - 1].chr, 31);
    assert_eq!(node_ptrs[node_ptrs.len() - 1].id, TrieNodeID::Leaf as u8);

    // walk down the trie
    let mut c = TrieCursor::new(
        &TrieHash::from_bytes(&path).unwrap(),
        trie_io.root_trieptr(),
    );
    let mut walk_point = nodes[0].clone();

    for i in 0..31 {
        let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
        assert!(res.is_ok());

        let fields_opt = res.unwrap();
        assert!(fields_opt.is_some());

        let (ptr, node, hash) = fields_opt.unwrap();
        assert_eq!(ptr, node_ptrs[i]);
        assert_eq!(hash, hashes[i]);
        assert_eq!(node, nodes[i + 1]);

        assert_eq!(c.node().unwrap(), nodes[i]);
        assert_eq!(c.ptr(), node_ptrs[i]);
        assert_eq!(c.chr().unwrap(), path[i]);
        assert_eq!(c.tell(), i + 1);
        assert_eq!(c.ntell(), 0);
        assert!(c.eonp(&c.node().unwrap()));

        walk_point = node;
    }

    // walk to the leaf
    let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
    assert!(res.is_ok());

    let fields_opt = res.unwrap();
    assert!(fields_opt.is_some());

    let (ptr, node, hash) = fields_opt.unwrap();
    assert_eq!(ptr, node_ptrs[31]);
    assert_eq!(
        node,
        TrieNodeType::Leaf(TrieLeaf::new(&[], &[31u8; 40].to_vec()))
    );
    assert_eq!(hash, hashes[31]);

    // cursor's last-visited node points at the penultimate node (the last node4),
    // but its ptr() is the pointer to the leaf.
    assert_eq!(c.node().unwrap(), nodes[31]);
    assert_eq!(c.ptr(), node_ptrs[31]);
    assert_eq!(c.chr(), Some(path[path.len() - 1]));
    assert_eq!(c.tell(), 32);
    assert!(c.eop());
    assert!(c.eonp(&c.node().unwrap()));

    dump_trie(&mut trie_io);
}

#[test]
fn trie_cursor_walk_1() {
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let path_segments = vec![
        (vec![0], 1),
        (vec![2], 3),
        (vec![4], 5),
        (vec![6], 7),
        (vec![8], 9),
        (vec![10], 11),
        (vec![12], 13),
        (vec![14], 15),
        (vec![16], 17),
        (vec![18], 19),
        (vec![20], 21),
        (vec![22], 23),
        (vec![24], 25),
        (vec![26], 27),
        (vec![28], 29),
        (vec![30], 31),
    ];
    let path = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    let (nodes, node_ptrs, hashes) =
        make_node4_path(&mut trie_io, &path_segments, [31u8; 40].to_vec());

    assert_eq!(nodes.len(), 16);
    assert_eq!(node_ptrs.len(), 16);
    assert_eq!(hashes.len(), 16);

    assert_eq!(node_ptrs[node_ptrs.len() - 1].chr, 31);
    assert_eq!(node_ptrs[node_ptrs.len() - 1].id, TrieNodeID::Leaf as u8);

    // walk down the trie
    let mut c = TrieCursor::new(
        &TrieHash::from_bytes(&path).unwrap(),
        trie_io.root_trieptr(),
    );
    let mut walk_point = nodes[0].clone();

    for i in 0..15 {
        let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
        assert!(res.is_ok());

        let fields_opt = res.unwrap();
        assert!(fields_opt.is_some());

        let (ptr, node, hash) = fields_opt.unwrap();
        assert_eq!(ptr, node_ptrs[i]);
        assert_eq!(hash, hashes[i]);
        assert_eq!(node, nodes[i + 1]);

        assert_eq!(c.node().unwrap(), nodes[i]);
        assert_eq!(c.ptr(), node_ptrs[i]);
        assert_eq!(c.chr().unwrap(), path[2 * (i + 1) - 1]);
        assert_eq!(c.tell(), 2 * (i + 1));
        assert_eq!(c.ntell(), 1);
        assert!(c.eonp(&c.node().unwrap()));

        walk_point = node;
    }

    // walk to the leaf
    let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
    assert!(res.is_ok());

    let fields_opt = res.unwrap();
    assert!(fields_opt.is_some());

    let (ptr, node, hash) = fields_opt.unwrap();
    assert_eq!(ptr, node_ptrs[15]);
    assert_eq!(node, TrieNodeType::Leaf(TrieLeaf::new(&[30], &[31u8; 40])));
    assert_eq!(hash, hashes[15]);

    // cursor's last-visited node points at the penultimate node (the last node4),
    // but its ptr() is the pointer to the leaf.
    assert_eq!(c.node().unwrap(), nodes[15]);
    assert_eq!(c.ptr(), node_ptrs[15]);
    assert_eq!(c.chr(), Some(path[path.len() - 1]));
    assert_eq!(c.tell(), 32);
    assert!(c.eop());
    assert!(c.eonp(&c.node().unwrap()));

    dump_trie(&mut trie_io);
}

#[test]
fn trie_cursor_walk_2() {
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let path_segments = vec![
        (vec![0, 1], 2),
        (vec![3, 4], 5),
        (vec![6, 7], 8),
        (vec![9, 10], 11),
        (vec![12, 13], 14),
        (vec![15, 16], 17),
        (vec![18, 19], 20),
        (vec![21, 22], 23),
        (vec![24, 25], 26),
        (vec![27, 28], 29),
        (vec![30], 31),
    ];
    let path = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    let (nodes, node_ptrs, hashes) =
        make_node4_path(&mut trie_io, &path_segments, [31u8; 40].to_vec());

    assert_eq!(nodes.len(), 11);
    assert_eq!(node_ptrs.len(), 11);
    assert_eq!(hashes.len(), 11);

    assert_eq!(node_ptrs[node_ptrs.len() - 1].chr, 31);
    assert_eq!(node_ptrs[node_ptrs.len() - 1].id, TrieNodeID::Leaf as u8);

    // walk down the trie
    let mut c = TrieCursor::new(
        &TrieHash::from_bytes(&path).unwrap(),
        trie_io.root_trieptr(),
    );
    let mut walk_point = nodes[0].clone();

    for i in 0..10 {
        let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
        assert!(res.is_ok());

        let fields_opt = res.unwrap();
        assert!(fields_opt.is_some());

        let (ptr, node, hash) = fields_opt.unwrap();
        assert_eq!(ptr, node_ptrs[i]);
        assert_eq!(hash, hashes[i]);
        assert_eq!(node, nodes[i + 1]);

        assert_eq!(c.node().unwrap(), nodes[i]);
        assert_eq!(c.ptr(), node_ptrs[i]);
        assert_eq!(c.chr().unwrap(), path[3 * (i + 1) - 1]);
        assert_eq!(c.tell(), 3 * (i + 1));
        assert_eq!(c.ntell(), 2);
        assert!(c.eonp(&c.node().unwrap()));

        walk_point = node;
    }

    // walk to the leaf
    let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
    assert!(res.is_ok());

    let fields_opt = res.unwrap();
    assert!(fields_opt.is_some());

    let (ptr, node, hash) = fields_opt.unwrap();
    assert_eq!(ptr, node_ptrs[10]);
    assert_eq!(node, TrieNodeType::Leaf(TrieLeaf::new(&[30], &[31u8; 40])));
    assert_eq!(hash, hashes[10]);

    // cursor's last-visited node points at the penultimate node (the last node4),
    // but its ptr() is the pointer to the leaf.
    assert_eq!(c.node().unwrap(), nodes[10]);
    assert_eq!(c.ptr(), node_ptrs[10]);
    assert_eq!(c.chr(), Some(path[path.len() - 1]));
    assert_eq!(c.tell(), 32);
    assert!(c.eop());
    assert!(c.eonp(&c.node().unwrap()));

    dump_trie(&mut trie_io);
}

#[test]
fn trie_cursor_walk_3() {
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let path_segments = vec![
        (vec![0, 1, 2], 3),
        (vec![4, 5, 6], 7),
        (vec![8, 9, 10], 11),
        (vec![12, 13, 14], 15),
        (vec![16, 17, 18], 19),
        (vec![20, 21, 22], 23),
        (vec![24, 25, 26], 27),
        (vec![28, 29, 30], 31),
    ];
    let path = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    let (nodes, node_ptrs, hashes) =
        make_node4_path(&mut trie_io, &path_segments, [31u8; 40].to_vec());

    assert_eq!(nodes.len(), 8);
    assert_eq!(node_ptrs.len(), 8);
    assert_eq!(hashes.len(), 8);

    assert_eq!(node_ptrs[node_ptrs.len() - 1].chr, 31);
    assert_eq!(node_ptrs[node_ptrs.len() - 1].id, TrieNodeID::Leaf as u8);

    // walk down the trie
    let mut c = TrieCursor::new(
        &TrieHash::from_bytes(&path).unwrap(),
        trie_io.root_trieptr(),
    );
    let mut walk_point = nodes[0].clone();

    for i in 0..7 {
        let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
        assert!(res.is_ok());

        let fields_opt = res.unwrap();
        assert!(fields_opt.is_some());

        let (ptr, node, hash) = fields_opt.unwrap();
        assert_eq!(ptr, node_ptrs[i]);
        assert_eq!(hash, hashes[i]);
        assert_eq!(node, nodes[i + 1]);

        assert_eq!(c.node().unwrap(), nodes[i]);
        assert_eq!(c.ptr(), node_ptrs[i]);
        assert_eq!(c.chr().unwrap(), path[4 * (i + 1) - 1]);
        assert_eq!(c.tell(), 4 * (i + 1));
        assert_eq!(c.ntell(), 3);
        assert!(c.eonp(&c.node().unwrap()));

        walk_point = node;
    }

    // walk to the leaf
    let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
    assert!(res.is_ok());

    let fields_opt = res.unwrap();
    assert!(fields_opt.is_some());

    let (ptr, node, hash) = fields_opt.unwrap();
    assert_eq!(ptr, node_ptrs[7]);
    assert_eq!(
        node,
        TrieNodeType::Leaf(TrieLeaf::new(&[28, 29, 30], &[31u8; 40]))
    );
    assert_eq!(hash, hashes[7]);

    // cursor's last-visited node points at the penultimate node (the last node4),
    // but its ptr() is the pointer to the leaf.
    assert_eq!(c.node().unwrap(), nodes[7]);
    assert_eq!(c.ptr(), node_ptrs[7]);
    assert_eq!(c.chr(), Some(path[path.len() - 1]));
    assert_eq!(c.tell(), 32);
    assert!(c.eop());
    assert!(c.eonp(&c.node().unwrap()));

    dump_trie(&mut trie_io);
}

#[test]
fn trie_cursor_walk_4() {
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let path_segments = vec![
        (vec![0, 1, 2, 3], 4),
        (vec![5, 6, 7, 8], 9),
        (vec![10, 11, 12, 13], 14),
        (vec![15, 16, 17, 18], 19),
        (vec![20, 21, 22, 23], 24),
        (vec![25, 26, 27, 28], 29),
        (vec![30], 31),
    ];
    let path = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    let (nodes, node_ptrs, hashes) =
        make_node4_path(&mut trie_io, &path_segments, [31u8; 40].to_vec());

    assert_eq!(nodes.len(), 7);
    assert_eq!(node_ptrs.len(), 7);
    assert_eq!(hashes.len(), 7);

    assert_eq!(node_ptrs[node_ptrs.len() - 1].chr, 31);
    assert_eq!(node_ptrs[node_ptrs.len() - 1].id, TrieNodeID::Leaf as u8);

    // walk down the trie
    let mut c = TrieCursor::new(
        &TrieHash::from_bytes(&path).unwrap(),
        trie_io.root_trieptr(),
    );
    let mut walk_point = nodes[0].clone();

    for i in 0..6 {
        let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
        assert!(res.is_ok());

        let fields_opt = res.unwrap();
        assert!(fields_opt.is_some());

        let (ptr, node, hash) = fields_opt.unwrap();
        assert_eq!(ptr, node_ptrs[i]);
        assert_eq!(hash, hashes[i]);
        assert_eq!(node, nodes[i + 1]);

        assert_eq!(c.node().unwrap(), nodes[i]);
        assert_eq!(c.ptr(), node_ptrs[i]);
        assert_eq!(c.chr().unwrap(), path[5 * (i + 1) - 1]);
        assert_eq!(c.tell(), 5 * (i + 1));
        assert_eq!(c.ntell(), 4);
        assert!(c.eonp(&c.node().unwrap()));

        walk_point = node;
    }

    // walk to the leaf
    let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
    assert!(res.is_ok());

    let fields_opt = res.unwrap();
    assert!(fields_opt.is_some());

    let (ptr, node, hash) = fields_opt.unwrap();
    assert_eq!(ptr, node_ptrs[6]);
    assert_eq!(node, TrieNodeType::Leaf(TrieLeaf::new(&[30], &[31u8; 40])));
    assert_eq!(hash, hashes[6]);

    // cursor's last-visited node points at the penultimate node (the last node4),
    // but its ptr() is the pointer to the leaf.
    assert_eq!(c.node().unwrap(), nodes[6]);
    assert_eq!(c.ptr(), node_ptrs[6]);
    assert_eq!(c.chr(), Some(path[path.len() - 1]));
    assert_eq!(c.tell(), 32);
    assert!(c.eop());
    assert!(c.eonp(&c.node().unwrap()));

    dump_trie(&mut trie_io);
}

#[test]
fn trie_cursor_walk_5() {
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let path_segments = vec![
        (vec![0, 1, 2, 3, 4], 5),
        (vec![6, 7, 8, 9, 10], 11),
        (vec![12, 13, 14, 15, 16], 17),
        (vec![18, 19, 20, 21, 22], 23),
        (vec![24, 25, 26, 27, 28], 29),
        (vec![30], 31),
    ];
    let path = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    let (nodes, node_ptrs, hashes) =
        make_node4_path(&mut trie_io, &path_segments, [31u8; 40].to_vec());

    assert_eq!(nodes.len(), 6);
    assert_eq!(node_ptrs.len(), 6);
    assert_eq!(hashes.len(), 6);

    assert_eq!(node_ptrs[node_ptrs.len() - 1].chr, 31);
    assert_eq!(node_ptrs[node_ptrs.len() - 1].id, TrieNodeID::Leaf as u8);

    // walk down the trie
    let mut c = TrieCursor::new(
        &TrieHash::from_bytes(&path).unwrap(),
        trie_io.root_trieptr(),
    );
    let mut walk_point = nodes[0].clone();

    for i in 0..5 {
        let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
        assert!(res.is_ok());

        let fields_opt = res.unwrap();
        assert!(fields_opt.is_some());

        let (ptr, node, hash) = fields_opt.unwrap();
        assert_eq!(ptr, node_ptrs[i]);
        assert_eq!(hash, hashes[i]);
        assert_eq!(node, nodes[i + 1]);

        assert_eq!(c.node().unwrap(), nodes[i]);
        assert_eq!(c.ptr(), node_ptrs[i]);
        assert_eq!(c.chr().unwrap(), path[6 * (i + 1) - 1]);
        assert_eq!(c.tell(), 6 * (i + 1));
        assert_eq!(c.ntell(), 5);
        assert!(c.eonp(&c.node().unwrap()));

        walk_point = node;
    }

    // walk to the leaf
    let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
    assert!(res.is_ok());

    let fields_opt = res.unwrap();
    assert!(fields_opt.is_some());

    let (ptr, node, hash) = fields_opt.unwrap();
    assert_eq!(ptr, node_ptrs[5]);
    assert_eq!(node, TrieNodeType::Leaf(TrieLeaf::new(&[30], &[31u8; 40])));
    assert_eq!(hash, hashes[5]);

    // cursor's last-visited node points at the penultimate node (the last node4),
    // but its ptr() is the pointer to the leaf.
    assert_eq!(c.node().unwrap(), nodes[5]);
    assert_eq!(c.ptr(), node_ptrs[5]);
    assert_eq!(c.chr(), Some(path[path.len() - 1]));
    assert_eq!(c.tell(), 32);
    assert!(c.eop());
    assert!(c.eonp(&c.node().unwrap()));

    dump_trie(&mut trie_io);
}

#[test]
fn trie_cursor_walk_6() {
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let path_segments = vec![
        (vec![0, 1, 2, 3, 4, 5], 6),
        (vec![7, 8, 9, 10, 11, 12], 13),
        (vec![14, 15, 16, 17, 18, 19], 20),
        (vec![21, 22, 23, 24, 25, 26], 27),
        (vec![28, 29, 30], 31),
    ];
    let path = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    let (nodes, node_ptrs, hashes) =
        make_node4_path(&mut trie_io, &path_segments, [31u8; 40].to_vec());

    assert_eq!(nodes.len(), 5);
    assert_eq!(node_ptrs.len(), 5);
    assert_eq!(hashes.len(), 5);

    assert_eq!(node_ptrs[node_ptrs.len() - 1].chr, 31);
    assert_eq!(node_ptrs[node_ptrs.len() - 1].id, TrieNodeID::Leaf as u8);

    // walk down the trie
    let mut c = TrieCursor::new(
        &TrieHash::from_bytes(&path).unwrap(),
        trie_io.root_trieptr(),
    );
    let mut walk_point = nodes[0].clone();

    for i in 0..4 {
        let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
        assert!(res.is_ok());

        let fields_opt = res.unwrap();
        assert!(fields_opt.is_some());

        let (ptr, node, hash) = fields_opt.unwrap();
        assert_eq!(ptr, node_ptrs[i]);
        assert_eq!(hash, hashes[i]);
        assert_eq!(node, nodes[i + 1]);

        assert_eq!(c.node().unwrap(), nodes[i]);
        assert_eq!(c.ptr(), node_ptrs[i]);
        assert_eq!(c.chr().unwrap(), path[7 * (i + 1) - 1]);
        assert_eq!(c.tell(), 7 * (i + 1));
        assert_eq!(c.ntell(), 6);
        assert!(c.eonp(&c.node().unwrap()));

        walk_point = node;
    }

    // walk to the leaf
    let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
    assert!(res.is_ok());

    let fields_opt = res.unwrap();
    assert!(fields_opt.is_some());

    let (ptr, node, hash) = fields_opt.unwrap();
    assert_eq!(ptr, node_ptrs[4]);
    assert_eq!(
        node,
        TrieNodeType::Leaf(TrieLeaf::new(&[28, 29, 30], &[31u8; 40]))
    );
    assert_eq!(hash, hashes[4]);

    // cursor's last-visited node points at the penultimate node (the last node4),
    // but its ptr() is the pointer to the leaf.
    assert_eq!(c.node().unwrap(), nodes[4]);
    assert_eq!(c.ptr(), node_ptrs[4]);
    assert_eq!(c.chr(), Some(path[path.len() - 1]));
    assert_eq!(c.tell(), 32);
    assert!(c.eop());
    assert!(c.eonp(&c.node().unwrap()));

    dump_trie(&mut trie_io);
}

#[test]
fn trie_cursor_walk_10() {
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let path_segments = vec![
        (vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9], 10),
        (vec![11, 12, 13, 14, 15, 16, 17, 18, 19, 20], 21),
        (vec![22, 23, 24, 25, 26, 27, 28, 29, 30], 31),
    ];
    let path = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    let (nodes, node_ptrs, hashes) =
        make_node4_path(&mut trie_io, &path_segments, [31u8; 40].to_vec());

    assert_eq!(nodes.len(), 3);
    assert_eq!(node_ptrs.len(), 3);
    assert_eq!(hashes.len(), 3);

    assert_eq!(node_ptrs[node_ptrs.len() - 1].chr, 31);
    assert_eq!(node_ptrs[node_ptrs.len() - 1].id, TrieNodeID::Leaf as u8);

    // walk down the trie
    let mut c = TrieCursor::new(
        &TrieHash::from_bytes(&path).unwrap(),
        trie_io.root_trieptr(),
    );
    let mut walk_point = nodes[0].clone();

    for i in 0..2 {
        let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
        assert!(res.is_ok());

        let fields_opt = res.unwrap();
        assert!(fields_opt.is_some());

        let (ptr, node, hash) = fields_opt.unwrap();
        assert_eq!(ptr, node_ptrs[i]);
        assert_eq!(hash, hashes[i]);
        assert_eq!(node, nodes[i + 1]);

        assert_eq!(c.node().unwrap(), nodes[i]);
        assert_eq!(c.ptr(), node_ptrs[i]);
        assert_eq!(c.chr().unwrap(), path[11 * (i + 1) - 1]);
        assert_eq!(c.tell(), 11 * (i + 1));
        assert_eq!(c.ntell(), 10);
        assert!(c.eonp(&c.node().unwrap()));

        walk_point = node;
    }

    // walk to the leaf
    let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
    assert!(res.is_ok());

    let fields_opt = res.unwrap();
    assert!(fields_opt.is_some());

    let (ptr, node, hash) = fields_opt.unwrap();
    assert_eq!(ptr, node_ptrs[2]);
    assert_eq!(
        node,
        TrieNodeType::Leaf(TrieLeaf::new(
            &[22, 23, 24, 25, 26, 27, 28, 29, 30],
            &[31u8; 40]
        ))
    );
    assert_eq!(hash, hashes[2]);

    // cursor's last-visited node points at the penultimate node (the last node4),
    // but its ptr() is the pointer to the leaf.
    assert_eq!(c.node().unwrap(), nodes[2]);
    assert_eq!(c.ptr(), node_ptrs[2]);
    assert_eq!(c.chr(), Some(path[path.len() - 1]));
    assert_eq!(c.tell(), 32);
    assert!(c.eop());
    assert!(c.eonp(&c.node().unwrap()));

    dump_trie(&mut trie_io);
}

#[test]
fn trie_cursor_walk_20() {
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();
    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let path_segments = vec![
        (
            vec![
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
            ],
            20,
        ),
        (vec![21, 22, 23, 24, 25, 26, 27, 28, 29, 30], 31),
    ];
    let path = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    let (nodes, node_ptrs, hashes) =
        make_node4_path(&mut trie_io, &path_segments, [31u8; 40].to_vec());

    assert_eq!(nodes.len(), 2);
    assert_eq!(node_ptrs.len(), 2);
    assert_eq!(hashes.len(), 2);

    assert_eq!(node_ptrs[node_ptrs.len() - 1].chr, 31);
    assert_eq!(node_ptrs[node_ptrs.len() - 1].id, TrieNodeID::Leaf as u8);

    // walk down the trie
    let mut c = TrieCursor::new(
        &TrieHash::from_bytes(&path).unwrap(),
        trie_io.root_trieptr(),
    );
    let mut walk_point = nodes[0].clone();

    for i in 0..1 {
        let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
        assert!(res.is_ok());

        let fields_opt = res.unwrap();
        assert!(fields_opt.is_some());

        let (ptr, node, hash) = fields_opt.unwrap();
        assert_eq!(ptr, node_ptrs[i]);
        assert_eq!(hash, hashes[i]);
        assert_eq!(node, nodes[i + 1]);

        assert_eq!(c.node().unwrap(), nodes[i]);
        assert_eq!(c.ptr(), node_ptrs[i]);
        assert_eq!(c.chr().unwrap(), path[21 * (i + 1) - 1]);
        assert_eq!(c.tell(), 21 * (i + 1));
        assert_eq!(c.ntell(), 20);
        assert!(c.eonp(&c.node().unwrap()));

        walk_point = node;
    }

    // walk to the leaf
    let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
    assert!(res.is_ok());

    let fields_opt = res.unwrap();
    assert!(fields_opt.is_some());

    let (ptr, node, hash) = fields_opt.unwrap();
    assert_eq!(ptr, node_ptrs[1]);
    assert_eq!(
        node,
        TrieNodeType::Leaf(TrieLeaf::new(
            &[21, 22, 23, 24, 25, 26, 27, 28, 29, 30],
            &[31u8; 40]
        ))
    );
    assert_eq!(hash, hashes[1]);

    // cursor's last-visited node points at the penultimate node (the last node4),
    // but its ptr() is the pointer to the leaf.
    assert_eq!(c.node().unwrap(), nodes[1]);
    assert_eq!(c.ptr(), node_ptrs[1]);
    assert_eq!(c.chr(), Some(path[path.len() - 1]));
    assert_eq!(c.tell(), 32);
    assert!(c.eop());
    assert!(c.eonp(&c.node().unwrap()));

    dump_trie(&mut trie_io);
}

#[test]
fn trie_cursor_walk_32() {
    let marf_opts = MARFOpenOpts::default();
    let mut trie_io_store = TrieFileStorage::new_memory(marf_opts).unwrap();
    let mut trie_io = trie_io_store.transaction().unwrap();

    trie_io
        .extend_to_block(&BlockHeaderHash([0u8; 32]))
        .unwrap();

    let path_segments = vec![(
        vec![
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
            24, 25, 26, 27, 28, 29, 30,
        ],
        31,
    )];
    let path = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    let (nodes, node_ptrs, hashes) =
        make_node4_path(&mut trie_io, &path_segments, [31u8; 40].to_vec());

    assert_eq!(nodes.len(), 1);
    assert_eq!(node_ptrs.len(), 1);
    assert_eq!(hashes.len(), 1);

    assert_eq!(node_ptrs[node_ptrs.len() - 1].chr, 31);
    assert_eq!(node_ptrs[node_ptrs.len() - 1].id, TrieNodeID::Leaf as u8);

    // walk down the trie
    let mut c = TrieCursor::new(
        &TrieHash::from_bytes(&path).unwrap(),
        trie_io.root_trieptr(),
    );
    let walk_point = nodes[0].clone();

    // walk to the leaf
    let res = Trie::walk_from(&mut trie_io, &walk_point, &mut c);
    assert!(res.is_ok());

    let fields_opt = res.unwrap();
    assert!(fields_opt.is_some());

    let (ptr, node, hash) = fields_opt.unwrap();
    assert_eq!(ptr, node_ptrs[0]);
    assert_eq!(
        node,
        TrieNodeType::Leaf(TrieLeaf::new(
            &[
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30
            ],
            &[31u8; 40]
        ))
    );
    assert_eq!(hash, hashes[0]);

    // cursor's last-visited node points at the penultimate node (the last node4),
    // but its ptr() is the pointer to the leaf.
    assert_eq!(c.node().unwrap(), nodes[0]);
    assert_eq!(c.ptr(), node_ptrs[0]);
    assert_eq!(c.chr(), Some(path[path.len() - 1]));
    assert_eq!(c.tell(), 32);
    assert!(c.eop());
    assert!(c.eonp(&c.node().unwrap()));

    dump_trie(&mut trie_io);
}
