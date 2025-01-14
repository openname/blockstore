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
use crate::chainstate::stacks::index::{ClarityMarfTrieId, *};

fn walk_to_insertion_point(
    f: &mut TrieStorageConnection<BlockHeaderHash>,
    cursor: &mut TrieCursor<BlockHeaderHash>,
) -> (TriePtr, TrieNodeType, TrieHash) {
    let (mut node, root_hash) = Trie::read_root(f).unwrap();
    let mut node_hash = TrieHash::from_empty_data();
    let mut node_ptr = f.root_trieptr();

    for _ in 0..cursor.path.len() {
        match Trie::walk_from(f, &node, cursor) {
            Ok(node_data_opt) => match node_data_opt {
                Some((next_nodeptr, next_node, next_node_hash)) => {
                    node = next_node;
                    node_ptr = next_nodeptr;
                    node_hash = next_node_hash;
                    continue;
                }
                None => {
                    panic!("No insertion point found -- reached leaf");
                }
            },
            Err(e) => {
                match e {
                    Error::CursorError(_) => {
                        // don't care about backptrs in this suite of tests
                        return (node_ptr, node, node_hash);
                    }
                    _ => {
                        panic!("Encountered error: {:?}", e);
                    }
                }
            }
        }
    }

    panic!("Encountered a loop in the trie");
}

#[test]
fn trie_cursor_try_attach_leaf() {
    for marf_opts in MARFOpenOpts::all().into_iter() {
        test_debug!("With MARF opts {:?}", &marf_opts);
        for node_id in [
            TrieNodeID::Node4,
            TrieNodeID::Node16,
            TrieNodeID::Node48,
            TrieNodeID::Node256,
        ]
        .iter()
        {
            let mut f_store = TrieFileStorage::new_memory(marf_opts.clone()).unwrap();
            let mut f = f_store.transaction().unwrap();

            let block_header = BlockHeaderHash::from_bytes(&[0u8; 32]).unwrap();
            MARF::format(&mut f, &block_header).unwrap();

            // used to short-circuit block-height lookups, so that we don't
            //   mess up these tests expected trie structures.
            f.test_genesis_block.replace(block_header.clone());

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
            let (nodes, node_ptrs, hashes) =
                make_node_path(&mut f, node_id.to_u8(), &path_segments, [31u8; 40].to_vec());

            let mut ptrs = vec![];

            // append a leaf to each node
            for i in 0..32 {
                let mut path = vec![
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                    22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
                ];
                path[i] = 32;

                let mut c =
                    TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());
                let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

                // end of path -- cursor points to the insertion point.
                // all nodes have space,
                f.open_block(&block_header).unwrap();
                let ptr_opt_res = Trie::test_try_attach_leaf(
                    &mut f,
                    &mut c,
                    &mut TrieLeaf::new(&[], &[i as u8; 40]),
                    &mut node,
                );
                assert!(ptr_opt_res.is_ok());

                let ptr_opt = ptr_opt_res.unwrap();
                assert!(ptr_opt.is_some());

                let ptr = ptr_opt.unwrap();
                ptrs.push(ptr.clone());

                let update_res = Trie::update_root_hash(&mut f, &c);
                assert!(update_res.is_ok());

                // we must be able to query it now
                let leaf_opt_res = MARF::get_path(
                    &mut f,
                    &block_header,
                    &TrieHash::from_bytes(&path[..]).unwrap(),
                );
                assert!(leaf_opt_res.is_ok());

                let leaf_opt = leaf_opt_res.unwrap();
                assert!(leaf_opt.is_some());

                let leaf = leaf_opt.unwrap();
                assert_eq!(leaf, TrieLeaf::new(&path[i + 1..].to_vec(), &[i as u8; 40]));

                // without a MARF commit, merkle tests will fail in deferred mode
                if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                    merkle_test(&mut f, &path, &[i as u8; 40]);
                }
            }

            // look up each leaf we inserted
            for i in 0..32 {
                let mut path = vec![
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                    22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
                ];
                path[i] = 32;

                let leaf_opt_res = MARF::get_path(
                    &mut f,
                    &block_header,
                    &TrieHash::from_bytes(&path[..]).unwrap(),
                );
                assert!(leaf_opt_res.is_ok());

                let leaf_opt = leaf_opt_res.unwrap();
                assert!(leaf_opt.is_some());

                let leaf = leaf_opt.unwrap();
                assert_eq!(leaf, TrieLeaf::new(&path[i + 1..], &[i as u8; 40]));

                // without a MARF commit, merkle tests will fail in deferred mode
                if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                    merkle_test(&mut f, &path, &[i as u8; 40]);
                }
            }

            // each ptr must be a node with two children
            for i in 0..32 {
                let ptr = &ptrs[i];
                let (node, hash) = f.read_nodetype(ptr).unwrap();
                match node {
                    TrieNodeType::Node4(ref data) => assert_eq!(count_children(&data.ptrs), 2),
                    TrieNodeType::Node16(ref data) => assert_eq!(count_children(&data.ptrs), 2),
                    TrieNodeType::Node48(ref data) => assert_eq!(count_children(&data.ptrs), 2),
                    TrieNodeType::Node256(ref data) => {
                        assert_eq!(count_children(&data.ptrs), 2)
                    }
                    _ => assert!(false),
                };
            }

            dump_trie(&mut f);
        }
    }
}

#[test]
fn trie_cursor_promote_leaf_to_node4() {
    for marf_opts in MARFOpenOpts::all().into_iter() {
        let mut f_store = TrieFileStorage::new_memory(marf_opts).unwrap();
        let mut f = f_store.transaction().unwrap();

        let block_header = BlockHeaderHash::from_bytes(&[0u8; 32]).unwrap();
        MARF::format(&mut f, &block_header).unwrap();

        // used to short-circuit block-height lookups, so that we don't
        //   mess up these tests expected trie structures.
        f.test_genesis_block.replace(block_header.clone());

        let (node, root_hash) = Trie::read_root(&mut f).unwrap();

        // add a single leaf
        let mut c = TrieCursor::new(
            &TrieHash::from_bytes(&[
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30, 31,
            ])
            .unwrap(),
            f.root_trieptr(),
        );

        let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

        f.open_block(&block_header).unwrap();
        Trie::test_try_attach_leaf(
            &mut f,
            &mut c,
            &mut TrieLeaf::new(&[], &[128; 40]),
            &mut node,
        )
        .unwrap()
        .unwrap();
        Trie::update_root_hash(&mut f, &c).unwrap();

        assert_eq!(
            MARF::get_path(
                &mut f,
                &block_header,
                &TrieHash::from_bytes(&[
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                    22, 23, 24, 25, 26, 27, 28, 29, 30, 31
                ])
                .unwrap()
            )
            .unwrap()
            .unwrap(),
            TrieLeaf::new(
                &[
                    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                    23, 24, 25, 26, 27, 28, 29, 30, 31
                ],
                &[128; 40]
            )
        );

        // without a MARF commit, merkle tests will fail in deferred mode
        if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
            merkle_test(
                &mut f,
                &[
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                    22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
                ],
                &[128; 40],
            );
        }

        let mut ptrs = vec![];

        // add more leaves -- unzip this path completely
        for i in 1..32 {
            // add a leaf that would go after the prior leaf
            let mut path = vec![
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30, 31,
            ];
            path[i] = 32;

            let mut c =
                TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());

            let (nodeptr, node, node_hash) = walk_to_insertion_point(&mut f, &mut c);
            // end of path -- cursor points to the insertion point
            let mut leaf_data = match node {
                TrieNodeType::Leaf(ref data) => data.clone(),
                _ => panic!("not a leaf"),
            };

            f.open_block(&block_header).unwrap();
            let ptr = Trie::test_promote_leaf_to_node4(
                &mut f,
                &mut c,
                &mut leaf_data,
                &mut TrieLeaf::new(&[], &[(i + 128) as u8; 40]),
            )
            .unwrap();
            ptrs.push(ptr);

            Trie::update_root_hash(&mut f, &c).unwrap();

            // make sure we can query it again
            let leaf_opt_res = MARF::get_path(
                &mut f,
                &block_header,
                &TrieHash::from_bytes(&path[..]).unwrap(),
            );
            assert!(leaf_opt_res.is_ok());

            let leaf_opt = leaf_opt_res.unwrap();
            assert!(leaf_opt.is_some());

            let leaf = leaf_opt.unwrap();
            assert_eq!(leaf, TrieLeaf::new(&path[i + 1..], &[(i + 128) as u8; 40]));

            // without a MARF commit, merkle tests will fail in deferred mode
            if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                merkle_test(&mut f, &path, &[(i + 128) as u8; 40]);
            }
        }

        // look up each leaf we inserted
        for i in 1..31 {
            let mut path = vec![
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30, 31,
            ];
            path[i] = 32;

            let leaf_opt_res = MARF::get_path(
                &mut f,
                &block_header,
                &TrieHash::from_bytes(&path[..]).unwrap(),
            );
            assert!(leaf_opt_res.is_ok());

            let leaf_opt = leaf_opt_res.unwrap();
            assert!(leaf_opt.is_some());

            let leaf = leaf_opt.unwrap();
            assert_eq!(leaf, TrieLeaf::new(&path[i + 1..], &[(i + 128) as u8; 40]));

            // without a MARF commit, merkle tests will fail in deferred mode
            if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                merkle_test(&mut f, &path, &[(i + 128) as u8; 40]);
            }
        }

        // each ptr must be a node with two children
        for i in 0..31 {
            let ptr = &ptrs[i];
            let (node, hash) = f.read_nodetype(ptr).unwrap();
            match node {
                TrieNodeType::Node4(ref data) => assert_eq!(count_children(&data.ptrs), 2),
                TrieNodeType::Node256(ref data) => assert_eq!(count_children(&data.ptrs), 2),
                _ => assert!(false),
            };
        }

        dump_trie(&mut f);
    }
}

#[test]
fn trie_cursor_promote_node4_to_node16() {
    for marf_opts in MARFOpenOpts::all().into_iter() {
        let mut f_store = TrieFileStorage::new_memory(marf_opts).unwrap();
        let mut f = f_store.transaction().unwrap();

        let block_header = BlockHeaderHash::from_bytes(&[0u8; 32]).unwrap();
        MARF::format(&mut f, &block_header).unwrap();
        // used to short-circuit block-height lookups, so that we don't
        //   mess up these tests expected trie structures.
        f.test_genesis_block.replace(block_header.clone());

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
        let (nodes, node_ptrs, hashes) =
            make_node4_path(&mut f, &path_segments, [31u8; 40].to_vec());

        let (node, root_hash) = Trie::read_root(&mut f).unwrap();

        // fill each node4
        for k in 0..31 {
            for j in 0..3 {
                let mut path = [
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                    22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
                ];
                path[k] = j + 32;

                let mut c =
                    TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());
                let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

                f.open_block(&block_header).unwrap();
                Trie::test_try_attach_leaf(
                    &mut f,
                    &mut c,
                    &mut TrieLeaf::new(&[], &[128 + j; 40]),
                    &mut node,
                )
                .unwrap()
                .unwrap();
                Trie::update_root_hash(&mut f, &c).unwrap();

                // should have inserted
                assert_eq!(
                    MARF::get_path(
                        &mut f,
                        &block_header,
                        &TrieHash::from_bytes(&path[..]).unwrap()
                    )
                    .unwrap()
                    .unwrap(),
                    TrieLeaf::new(&path[k + 1..], &[128 + j; 40])
                );

                // without a MARF commit, merkle tests will fail in deferred mode
                if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                    merkle_test(&mut f, &path, &[j + 128; 40]);
                }
            }
        }

        test_debug!("");
        test_debug!("");
        test_debug!("");

        let mut ptrs = vec![];

        // promote each node4 to a node16
        for k in 1..31 {
            let mut path = [
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30, 31,
            ];
            path[k] = 128;

            let mut c =
                TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());

            let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

            f.open_block(&block_header).unwrap();
            let new_ptr = Trie::test_insert_leaf(
                &mut f,
                &mut c,
                &mut TrieLeaf::new(&[], &[192 + k as u8; 40]),
                &mut node,
            )
            .unwrap();
            ptrs.push(new_ptr);

            Trie::update_root_hash(&mut f, &c).unwrap();

            // should have inserted
            assert_eq!(
                MARF::get_path(
                    &mut f,
                    &block_header,
                    &TrieHash::from_bytes(&path[..]).unwrap()
                )
                .unwrap()
                .unwrap(),
                TrieLeaf::new(&path[k + 1..], &[192 + k as u8; 40])
            );

            // without a MARF commit, merkle tests will fail in deferred mode
            if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                merkle_test(&mut f, &path, &[(k + 192) as u8; 40]);
            }
        }

        // each ptr we got should point to a node16 with 5 children
        for ptr in ptrs.iter() {
            let (node, hash) = f.read_nodetype(ptr).unwrap();
            match node {
                TrieNodeType::Node16(ref data) => {
                    assert_eq!(count_children(&data.ptrs), 5);
                }
                _ => {
                    assert!(false);
                }
            }
        }

        dump_trie(&mut f);
    }
}

#[test]
fn trie_cursor_promote_node16_to_node48() {
    for marf_opts in MARFOpenOpts::all().into_iter() {
        let mut f_store = TrieFileStorage::new_memory(marf_opts).unwrap();
        let mut f = f_store.transaction().unwrap();

        let block_header = BlockHeaderHash::from_bytes(&[0u8; 32]).unwrap();
        MARF::format(&mut f, &block_header).unwrap();
        // used to short-circuit block-height lookups, so that we don't
        //   mess up these tests expected trie structures.
        f.test_genesis_block.replace(block_header.clone());

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
        let (nodes, node_ptrs, hashes) =
            make_node4_path(&mut f, &path_segments, [31u8; 40].to_vec());

        let (node, root_hash) = Trie::read_root(&mut f).unwrap();

        // fill each node4
        for k in 0..31 {
            for j in 0..3 {
                let mut path = [
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                    22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
                ];
                path[k] = j + 32;

                let mut c =
                    TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());

                let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

                f.open_block(&block_header).unwrap();
                Trie::test_try_attach_leaf(
                    &mut f,
                    &mut c,
                    &mut TrieLeaf::new(&[], &[128 + j; 40]),
                    &mut node,
                )
                .unwrap()
                .unwrap();

                Trie::update_root_hash(&mut f, &c).unwrap();

                // should have inserted
                assert_eq!(
                    MARF::get_path(
                        &mut f,
                        &block_header,
                        &TrieHash::from_bytes(&path[..]).unwrap()
                    )
                    .unwrap()
                    .unwrap(),
                    TrieLeaf::new(&path[k + 1..], &[128 + j; 40])
                );

                // without a MARF commit, merkle tests will fail in deferred mode
                if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                    merkle_test(&mut f, &path, &[j + 128; 40]);
                }
            }
        }

        test_debug!("");
        test_debug!("promote all node4 to node16");
        test_debug!("");

        let mut ptrs = vec![];

        // promote each node4 to a node16 by inserting one more leaf
        for k in 1..31 {
            let mut path = [
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30, 31,
            ];
            path[k] = 128;

            let mut c =
                TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());

            let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

            f.open_block(&block_header).unwrap();
            let new_ptr = Trie::test_insert_leaf(
                &mut f,
                &mut c,
                &mut TrieLeaf::new(&[], &[192 + k as u8; 40]),
                &mut node,
            )
            .unwrap();
            ptrs.push(new_ptr);

            Trie::update_root_hash(&mut f, &c).unwrap();

            // should have inserted
            assert_eq!(
                MARF::get_path(
                    &mut f,
                    &block_header,
                    &TrieHash::from_bytes(&path[..]).unwrap()
                )
                .unwrap()
                .unwrap(),
                TrieLeaf::new(&path[k + 1..], &[192 + k as u8; 40])
            );

            // without a MARF commit, merkle tests will fail in deferred mode
            if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                merkle_test(&mut f, &path, &[(k + 192) as u8; 40]);
            }
        }

        // each ptr we got should point to a node16 with 5 children
        for ptr in ptrs.iter() {
            let (node, hash) = f.read_nodetype(ptr).unwrap();
            match node {
                TrieNodeType::Node16(ref data) => {
                    assert_eq!(count_children(&data.ptrs), 5);
                }
                _ => {
                    assert!(false);
                }
            }
        }

        // fill each node16 with leaves
        for k in 0..31 {
            for j in 0..11 {
                let mut path = [
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                    22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
                ];
                path[k] = j + 40;

                let mut c =
                    TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());

                let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

                f.open_block(&block_header).unwrap();
                Trie::test_try_attach_leaf(
                    &mut f,
                    &mut c,
                    &mut TrieLeaf::new(&[], &[128 + j; 40]),
                    &mut node,
                )
                .unwrap()
                .unwrap();

                Trie::update_root_hash(&mut f, &c).unwrap();

                // should have inserted
                assert_eq!(
                    MARF::get_path(
                        &mut f,
                        &block_header,
                        &TrieHash::from_bytes(&path[..]).unwrap()
                    )
                    .unwrap()
                    .unwrap(),
                    TrieLeaf::new(&path[k + 1..], &[128 + j; 40])
                );

                // without a MARF commit, merkle tests will fail in deferred mode
                if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                    merkle_test(&mut f, &path, &[j + 128; 40]);
                }
            }
        }

        test_debug!("");
        test_debug!("promote all node16 to node48");
        test_debug!("");

        ptrs.clear();

        // promote each node16 to a node48 by inserting one more leaf
        for k in 1..31 {
            let mut path = [
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30, 31,
            ];
            path[k] = 129;

            let mut c =
                TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());

            let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

            f.open_block(&block_header).unwrap();

            let new_ptr = Trie::test_insert_leaf(
                &mut f,
                &mut c,
                &mut TrieLeaf::new(&[], &[192 + k as u8; 40]),
                &mut node,
            )
            .unwrap();
            ptrs.push(new_ptr);

            Trie::update_root_hash(&mut f, &c).unwrap();

            // should have inserted
            assert_eq!(
                MARF::get_path(
                    &mut f,
                    &block_header,
                    &TrieHash::from_bytes(&path[..]).unwrap()
                )
                .unwrap()
                .unwrap(),
                TrieLeaf::new(&path[k + 1..], &[192 + k as u8; 40])
            );

            // without a MARF commit, merkle tests will fail in deferred mode
            if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                merkle_test(&mut f, &path, &[(k + 192) as u8; 40]);
            }
        }

        // each ptr we got should point to a node48 with 17 children
        for ptr in ptrs.iter() {
            let (node, hash) = f.read_nodetype(ptr).unwrap();
            match node {
                TrieNodeType::Node48(ref data) => {
                    assert_eq!(count_children(&data.ptrs), 17);
                }
                _ => {
                    assert!(false);
                }
            }
        }

        dump_trie(&mut f);
    }
}

#[test]
fn trie_cursor_promote_node48_to_node256() {
    for marf_opts in MARFOpenOpts::all().into_iter() {
        let mut f_store = TrieFileStorage::new_memory(marf_opts).unwrap();
        let mut f = f_store.transaction().unwrap();

        let block_header = BlockHeaderHash::from_bytes(&[0u8; 32]).unwrap();
        MARF::format(&mut f, &block_header).unwrap();
        // used to short-circuit block-height lookups, so that we don't
        //   mess up these tests expected trie structures.
        f.test_genesis_block.replace(block_header.clone());

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
        let (nodes, node_ptrs, hashes) =
            make_node4_path(&mut f, &path_segments, [31u8; 40].to_vec());

        let (node, root_hash) = Trie::read_root(&mut f).unwrap();

        // fill each node4
        for k in 0..31 {
            for j in 0..3 {
                let mut path = [
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                    22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
                ];
                path[k] = j + 32;

                let mut c =
                    TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());

                let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

                f.open_block(&block_header).unwrap();
                Trie::test_try_attach_leaf(
                    &mut f,
                    &mut c,
                    &mut TrieLeaf::new(&[], &[128 + j; 40]),
                    &mut node,
                )
                .unwrap()
                .unwrap();

                Trie::update_root_hash(&mut f, &c).unwrap();

                // should have inserted
                assert_eq!(
                    MARF::get_path(
                        &mut f,
                        &block_header,
                        &TrieHash::from_bytes(&path[..]).unwrap()
                    )
                    .unwrap()
                    .unwrap(),
                    TrieLeaf::new(&path[k + 1..], &[128 + j; 40])
                );

                // without a MARF commit, merkle tests will fail in deferred mode
                if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                    merkle_test(&mut f, &path, &[j + 128; 40]);
                }
            }
        }

        test_debug!("");
        test_debug!("promote all node4 to node16");
        test_debug!("");

        let mut ptrs = vec![];

        // promote each node4 to a node16 by inserting one more leaf
        for k in 1..31 {
            let mut path = [
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30, 31,
            ];
            path[k] = 128;

            let mut c =
                TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());

            let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

            f.open_block(&block_header).unwrap();
            let new_ptr = Trie::test_insert_leaf(
                &mut f,
                &mut c,
                &mut TrieLeaf::new(&[], &[192 + k as u8; 40]),
                &mut node,
            )
            .unwrap();
            ptrs.push(new_ptr);

            Trie::update_root_hash(&mut f, &c).unwrap();

            // should have inserted
            assert_eq!(
                MARF::get_path(
                    &mut f,
                    &block_header,
                    &TrieHash::from_bytes(&path[..]).unwrap()
                )
                .unwrap()
                .unwrap(),
                TrieLeaf::new(&path[k + 1..], &[192 + k as u8; 40])
            );

            // without a MARF commit, merkle tests will fail in deferred mode
            if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                merkle_test(&mut f, &path, &[(k + 192) as u8; 40]);
            }
        }

        // each ptr we got should point to a node16 with 5 children
        for ptr in ptrs.iter() {
            let (node, hash) = f.read_nodetype(ptr).unwrap();
            match node {
                TrieNodeType::Node16(ref data) => {
                    assert_eq!(count_children(&data.ptrs), 5);
                }
                _ => {
                    assert!(false);
                }
            }
        }

        // fill each node16 with leaves
        for k in 0..31 {
            for j in 0..11 {
                let mut path = [
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                    22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
                ];
                path[k] = j + 40;

                let mut c =
                    TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());

                let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

                f.open_block(&block_header).unwrap();
                Trie::test_try_attach_leaf(
                    &mut f,
                    &mut c,
                    &mut TrieLeaf::new(&[], &[128 + j; 40]),
                    &mut node,
                )
                .unwrap()
                .unwrap();
                Trie::update_root_hash(&mut f, &c).unwrap();

                // should have inserted
                assert_eq!(
                    MARF::get_path(
                        &mut f,
                        &block_header,
                        &TrieHash::from_bytes(&path[..]).unwrap()
                    )
                    .unwrap()
                    .unwrap(),
                    TrieLeaf::new(&path[k + 1..], &[128 + j; 40])
                );

                // without a MARF commit, merkle tests will fail in deferred mode
                if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                    merkle_test(&mut f, &path, &[j + 128; 40]);
                }
            }
        }

        test_debug!("");
        test_debug!("promote all node16 to node48");
        test_debug!("");

        ptrs.clear();

        // promote each node16 to a node48 by inserting one more leaf
        for k in 1..31 {
            let mut path = [
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30, 31,
            ];
            path[k] = 129;

            let mut c =
                TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());

            let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

            f.open_block(&block_header).unwrap();
            let new_ptr = Trie::test_insert_leaf(
                &mut f,
                &mut c,
                &mut TrieLeaf::new(&[], &[192 + k as u8; 40]),
                &mut node,
            )
            .unwrap();
            ptrs.push(new_ptr);

            Trie::update_root_hash(&mut f, &c).unwrap();

            // should have inserted
            assert_eq!(
                MARF::get_path(
                    &mut f,
                    &block_header,
                    &TrieHash::from_bytes(&path[..]).unwrap()
                )
                .unwrap()
                .unwrap(),
                TrieLeaf::new(&path[k + 1..], &[192 + k as u8; 40])
            );

            // without a MARF commit, merkle tests will fail in deferred mode
            if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                merkle_test(&mut f, &path, &[(k + 192) as u8; 40]);
            }
        }

        // each ptr we got should point to a node48 with 17 children
        for ptr in ptrs.iter() {
            let (node, hash) = f.read_nodetype(ptr).unwrap();
            match node {
                TrieNodeType::Node48(ref data) => {
                    assert_eq!(count_children(&data.ptrs), 17);
                }
                _ => {
                    assert!(false);
                }
            }
        }

        // fill each node48 with leaves
        for k in 0..31 {
            for j in 0..31 {
                let mut path = [
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                    22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
                ];
                path[k] = j + 90;

                let mut c =
                    TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());

                let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

                f.open_block(&block_header).unwrap();
                Trie::test_try_attach_leaf(
                    &mut f,
                    &mut c,
                    &mut TrieLeaf::new(&[], &[128 + j; 40]),
                    &mut node,
                )
                .unwrap()
                .unwrap();

                Trie::update_root_hash(&mut f, &c).unwrap();

                // should have inserted
                assert_eq!(
                    MARF::get_path(
                        &mut f,
                        &block_header,
                        &TrieHash::from_bytes(&path[..]).unwrap()
                    )
                    .unwrap()
                    .unwrap(),
                    TrieLeaf::new(&path[k + 1..], &[128 + j; 40])
                );

                // without a MARF commit, merkle tests will fail in deferred mode
                if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                    merkle_test(&mut f, &path, &[j + 128; 40]);
                }
            }
        }

        test_debug!("");
        test_debug!("promote all node48 to node256");
        test_debug!("");

        ptrs.clear();

        // promote each node48 to a node256 by inserting one more leaf
        for k in 1..31 {
            let mut path = [
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30, 31,
            ];
            path[k] = 130;

            let mut c =
                TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());

            let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

            f.open_block(&block_header).unwrap();
            let new_ptr = Trie::test_insert_leaf(
                &mut f,
                &mut c,
                &mut TrieLeaf::new(&[], &[192 + k as u8; 40]),
                &mut node,
            )
            .unwrap();
            ptrs.push(new_ptr);

            Trie::update_root_hash(&mut f, &c).unwrap();

            // should have inserted
            assert_eq!(
                MARF::get_path(
                    &mut f,
                    &block_header,
                    &TrieHash::from_bytes(&path[..]).unwrap()
                )
                .unwrap()
                .unwrap(),
                TrieLeaf::new(&path[k + 1..], &[192 + k as u8; 40])
            );

            // without a MARF commit, merkle tests will fail in deferred mode
            if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                merkle_test(&mut f, &path, &[(k + 192) as u8; 40]);
            }
        }

        // each ptr we got should point to a node256 with 49 children
        for ptr in ptrs.iter() {
            let (node, hash) = f.read_nodetype(ptr).unwrap();
            match node {
                TrieNodeType::Node256(ref data) => {
                    assert_eq!(count_children(&data.ptrs), 49);
                }
                _ => {
                    assert!(false);
                }
            }
        }

        dump_trie(&mut f);
    }
}

#[test]
fn trie_cursor_splice_leaf_4() {
    for marf_opts in MARFOpenOpts::all().into_iter() {
        for node_id in [
            TrieNodeID::Node4,
            TrieNodeID::Node16,
            TrieNodeID::Node48,
            TrieNodeID::Node256,
        ]
        .iter()
        {
            let mut f_store = TrieFileStorage::new_memory(marf_opts.clone()).unwrap();
            let mut f = f_store.transaction().unwrap();

            let block_header = BlockHeaderHash::from_bytes(&[0u8; 32]).unwrap();
            MARF::format(&mut f, &block_header).unwrap();

            // used to short-circuit block-height lookups, so that we don't
            //   mess up these tests expected trie structures.
            f.test_genesis_block.replace(block_header.clone());

            let path_segments = vec![
                (vec![0, 1, 2, 3], 4),
                (vec![5, 6, 7, 8], 9),
                (vec![10, 11, 12, 13], 14),
                (vec![15, 16, 17, 18], 19),
                (vec![20, 21, 22, 23], 24),
                (vec![25, 26, 27, 28], 29),
                (vec![30], 31),
            ];

            let (nodes, node_ptrs, hashes) =
                make_node_path(&mut f, node_id.to_u8(), &path_segments, [31u8; 40].to_vec());

            let mut ptrs = vec![];

            // splice in a node in each path segment
            for k in 0..5 {
                let mut path = vec![
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                    22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
                ];
                path[5 * k + 2] = 32;

                let mut c =
                    TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());

                test_debug!("Start splice-insert at {:?}", &c);
                let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

                test_debug!("Splice leaf pattern={} at {:?}", 192 + k, &c);
                f.open_block(&block_header).unwrap();

                eprintln!("Splicing Node @ {}", nodeptr.ptr());
                eprintln!("Splicing Node @ {:x}", c.chr().unwrap());

                let new_ptr = Trie::test_splice_leaf(
                    &mut f,
                    &mut c,
                    &mut TrieLeaf::new(&[], &[192 + k as u8; 40]),
                    &mut node,
                )
                .unwrap();
                ptrs.push(new_ptr);

                Trie::update_root_hash(&mut f, &c).unwrap();

                // should have inserted
                assert_eq!(
                    MARF::get_path(
                        &mut f,
                        &block_header,
                        &TrieHash::from_bytes(&path[..]).unwrap()
                    )
                    .unwrap()
                    .unwrap(),
                    TrieLeaf::new(&path[5 * k + 3..], &[192 + k as u8; 40])
                );

                // without a MARF commit, merkle tests will fail in deferred mode
                if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                    merkle_test(&mut f, &path, &[(k + 192) as u8; 40]);
                }
            }

            dump_trie(&mut f);
        }
    }
}

#[test]
fn trie_cursor_splice_leaf_2() {
    for marf_opts in MARFOpenOpts::all().into_iter() {
        for node_id in [
            TrieNodeID::Node4,
            TrieNodeID::Node16,
            TrieNodeID::Node48,
            TrieNodeID::Node256,
        ]
        .iter()
        {
            let mut f_store = TrieFileStorage::new_memory(marf_opts.clone()).unwrap();
            let mut f = f_store.transaction().unwrap();

            let block_header = BlockHeaderHash::from_bytes(&[0u8; 32]).unwrap();
            MARF::format(&mut f, &block_header).unwrap();

            // used to short-circuit block-height lookups, so that we don't
            //   mess up these tests expected trie structures.
            f.test_genesis_block.replace(block_header.clone());

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

            let (nodes, node_ptrs, hashes) =
                make_node_path(&mut f, node_id.to_u8(), &path_segments, [31u8; 40].to_vec());
            let mut ptrs = vec![];

            // splice in a node in each path segment
            for k in 0..10 {
                let mut path = vec![
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                    22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
                ];
                path[3 * k + 1] = 32;

                let mut c =
                    TrieCursor::new(&TrieHash::from_bytes(&path[..]).unwrap(), f.root_trieptr());

                test_debug!("Start splice-insert at {:?}", &c);
                let (nodeptr, mut node, node_hash) = walk_to_insertion_point(&mut f, &mut c);

                test_debug!("Splice leaf pattern={} at {:?}", 192 + k, &c);
                f.open_block(&block_header).unwrap();
                let new_ptr = Trie::test_splice_leaf(
                    &mut f,
                    &mut c,
                    &mut TrieLeaf::new(&[], &[192 + k as u8; 40]),
                    &mut node,
                )
                .unwrap();
                ptrs.push(new_ptr);

                Trie::update_root_hash(&mut f, &c).unwrap();

                // should have inserted
                assert_eq!(
                    MARF::get_path(
                        &mut f,
                        &block_header,
                        &TrieHash::from_bytes(&path[..]).unwrap()
                    )
                    .unwrap()
                    .unwrap(),
                    TrieLeaf::new(&path[3 * k + 2..], &[192 + k as u8; 40])
                );

                // proofs should still work
                // without a MARF commit, merkle tests will fail in deferred mode
                if f.hash_calculation_mode != TrieHashCalculationMode::Deferred {
                    merkle_test(&mut f, &path, &[(k + 192) as u8; 40]);
                }
            }

            dump_trie(&mut f);
        }
    }
}

fn insert_n_test<F>(filename: &str, merkle_check: bool, count: u32, mut path_gen: F)
where
    F: FnMut(u32) -> [u8; 32],
{
    for marf_opts in MARFOpenOpts::all().into_iter() {
        let f = TrieFileStorage::new_memory(marf_opts).unwrap();

        let block_header = BlockHeaderHash::from_bytes(&[0u8; 32]).unwrap();
        let mut marf = MARF::from_storage(f);
        marf.begin(&BlockHeaderHash::sentinel(), &block_header)
            .unwrap();
        MARF::get_block_height(
            &mut marf.borrow_storage_backend(),
            &block_header,
            &block_header,
        )
        .unwrap()
        .unwrap();

        for i in 0..count {
            eprintln!("{}", i);
            let path = path_gen(i);
            let triepath = TrieHash::from_bytes(&path).unwrap();
            let value = TrieLeaf::new(
                &[],
                &[
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    (i / 256) as u8,
                    (i % 256) as u8,
                ],
            );
            marf.insert_raw(triepath, value).unwrap();

            // without a MARF commit, merkle tests will fail in deferred mode
            if merkle_check
                && marf.borrow_storage_backend().hash_calculation_mode
                    != TrieHashCalculationMode::Deferred
            {
                merkle_test(
                    &mut marf.borrow_storage_backend(),
                    &path,
                    &[
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        (i / 256) as u8,
                        (i % 256) as u8,
                    ],
                );
            }
        }

        for i in 0..count {
            let path = path_gen(i);
            let triepath = TrieHash::from_bytes(&path).unwrap();
            let value =
                MARF::get_path(&mut marf.borrow_storage_backend(), &block_header, &triepath)
                    .unwrap()
                    .unwrap();
            assert_eq!(
                value.data.to_vec(),
                [
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    (i / 256) as u8,
                    (i % 256) as u8
                ]
                .to_vec()
            );
            // without a MARF commit, merkle tests will fail in deferred mode
            if merkle_check
                && marf.borrow_storage_backend().hash_calculation_mode
                    != TrieHashCalculationMode::Deferred
            {
                merkle_test(
                    &mut marf.borrow_storage_backend(),
                    &path,
                    &[
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        (i / 256) as u8,
                        (i % 256) as u8,
                    ],
                );
            }
        }
    }
}

#[test]
fn insert_1024_seq_low() {
    insert_n_test("/tmp/rust_marf_insert_1024_seq_low", true, 1024, |i| {
        [
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
            (i / 256) as u8,
            (i % 256) as u8,
        ]
    })
}

#[test]
fn insert_1024_seq_high() {
    insert_n_test("/tmp/rust_marf_insert_1024_seq_high", true, 1024, |i| {
        [
            (i / 256) as u8,
            (i % 256) as u8,
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
        ]
    })
}

#[test]
fn insert_1024_seq_mid() {
    insert_n_test("/tmp/rust_marf_insert_1024_seq_mid", true, 1024, |i| {
        let i0 = i / 256;
        let i1 = (i % 256) / 32;
        let i2 = (i % 256) % 32;
        let i3 = (i % 256) % 16;
        [
            0, 1, i0 as u8, i1 as u8, i2 as u8, i3 as u8, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
            17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
        ]
    })
}

#[test]
#[ignore]
fn insert_65536_random_deterministic() {
    // deterministic random insert of 65536 keys
    let mut seed = TrieHash::from_data(&[]).as_bytes().to_vec();

    insert_n_test(
        "/tmp/rust_marf_insert_65536_random_deterministic",
        false,
        65536,
        |i| {
            let mut path = [0; 32];
            path.copy_from_slice(
                &TrieHash::from_data(if i == 0 { &[] } else { seed.as_slice() }).as_bytes()[0..32],
            );
            seed = path.to_vec();
            eprintln!("{}", to_hex(&path));
            path
        },
    )
}

#[test]
fn insert_1024_random_deterministic_merkle_proof() {
    // deterministic random insert of 1024 keys
    let mut seed = TrieHash::from_data(&[]).as_bytes().to_vec();

    insert_n_test(
        "/tmp/rust_marf_insert_1024_random_deterministic_merkle_proof",
        true,
        1024,
        |i| {
            let mut path = [0; 32];
            path.copy_from_slice(
                &TrieHash::from_data(if i == 0 { &[] } else { seed.as_slice() }).as_bytes()[0..32],
            );
            seed = path.to_vec();
            eprintln!("{}", to_hex(&path));
            path
        },
    )
}
