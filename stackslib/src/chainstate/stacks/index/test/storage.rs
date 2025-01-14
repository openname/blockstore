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

use std::collections::VecDeque;
use std::fs;

use rand::{thread_rng, Rng};

use super::*;
use crate::chainstate::stacks::index::marf::*;
use crate::chainstate::stacks::index::node::*;
use crate::chainstate::stacks::index::*;

fn ptrs_cmp(p1: &[TriePtr], p2: &[TriePtr]) -> bool {
    if p1.len() != p2.len() {
        return false;
    }
    for (ptr1, ptr2) in p1.iter().zip(p2.iter()) {
        if ptr1.chr != ptr2.chr || ptr1.id != ptr2.id {
            return false;
        }
    }
    return true;
}

fn node_cmp(n1: &TrieNodeType, n2: &TrieNodeType) -> bool {
    match (n1, n2) {
        (TrieNodeType::Leaf(ref data1), TrieNodeType::Leaf(ref data2)) => {
            data1.path == data2.path && data1.data == data2.data
        }
        (TrieNodeType::Node4(ref data1), TrieNodeType::Node4(ref data2)) => {
            data1.path == data2.path && ptrs_cmp(&data1.ptrs, &data2.ptrs)
        }
        (TrieNodeType::Node16(ref data1), TrieNodeType::Node16(ref data2)) => {
            data1.path == data2.path && ptrs_cmp(&data1.ptrs, &data2.ptrs)
        }
        (TrieNodeType::Node48(ref data1), TrieNodeType::Node48(ref data2)) => {
            data1.path == data2.path && ptrs_cmp(&data1.ptrs, &data2.ptrs)
        }
        (TrieNodeType::Node256(ref data1), TrieNodeType::Node256(ref data2)) => {
            data1.path == data2.path && ptrs_cmp(&data1.ptrs, &data2.ptrs)
        }
        (_, _) => false,
    }
}

fn trie_print<T: MarfTrieId>(t: &mut TrieRAM<T>) {
    t.print_to_stderr()
}

fn trie_cmp<T: MarfTrieId>(
    storage: &mut TrieStorageTransaction<T>,
    t1: &mut TrieRAM<T>,
    t2: &mut TrieRAM<T>,
) -> bool {
    eprintln!("Begin comparing tries\nTrie 1:");
    trie_print(t1);
    eprintln!("Trie 2");
    trie_print(t2);
    eprintln!("End tries\n");

    let mut t1 = t1.clone();
    let mut t2 = t2.clone();

    t1.test_inner_seal(storage).unwrap();
    t2.test_inner_seal(storage).unwrap();

    let mut frontier_1 = VecDeque::new();
    let mut frontier_2 = VecDeque::new();

    assert!(!t1.data().is_empty());
    assert!(!t2.data().is_empty());

    let (n1_data, n1_hash) = t1.data()[0].clone();
    let (n2_data, n2_hash) = t2.data()[0].clone();

    if let TrieNodeType::Node256(_) = n1_data {
    } else {
        assert!(false)
    }
    if let TrieNodeType::Node256(_) = n2_data {
    } else {
        assert!(false)
    }

    frontier_1.push_back((n1_data, n1_hash));
    frontier_2.push_back((n2_data, n2_hash));

    while !frontier_1.is_empty() && !frontier_2.is_empty() {
        if frontier_1.len() != frontier_2.len() {
            debug!("frontier len mismatch");
            return false;
        }

        let (n1_data, n1_hash) = frontier_1.pop_front().unwrap();
        let (n2_data, n2_hash) = frontier_2.pop_front().unwrap();

        if n1_hash != n2_hash {
            debug!("root hash mismatch: {} != {}", &n1_hash, &n2_hash);
            return false;
        }

        if !node_cmp(&n1_data, &n2_data) {
            debug!("root node mismatch: {:?} != {:?}", &n1_data, &n2_data);
            return false;
        }

        // search children
        for ptr in n1_data.ptrs() {
            if ptr.id != TrieNodeID::Empty as u8 && !is_backptr(ptr.id) {
                let (child_data, child_hash) = t1.read_nodetype(&ptr).unwrap();
                frontier_1.push_back((child_data, child_hash))
            }
        }
        for ptr in n2_data.ptrs() {
            if ptr.id != TrieNodeID::Empty as u8 && !is_backptr(ptr.id) {
                let (child_data, child_hash) = t2.read_nodetype(&ptr).unwrap();
                frontier_2.push_back((child_data, child_hash))
            }
        }
    }

    return true;
}

fn load_store_trie_m_n_same(m: u64, n: u64, same: bool) {
    let test_name = format!(
        "/tmp/load_store_trie_{}_{}_{}",
        m,
        n,
        if same { "same" } else { "unique" }
    );
    if fs::metadata(&test_name).is_ok() {
        fs::remove_file(&test_name).unwrap();
    }

    let marf_opts = MARFOpenOpts::default();
    let confirmed_marf_storage =
        TrieFileStorage::<StacksBlockId>::open(&test_name, marf_opts).unwrap();
    let mut confirmed_marf = MARF::<StacksBlockId>::from_storage(confirmed_marf_storage);

    confirmed_marf
        .begin(&StacksBlockId::sentinel(), &StacksBlockId([0x02; 32]))
        .unwrap();

    // pre-populate
    for i in 0..n {
        let mut path_bytes = [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
            24, 25, 26, 27, 28, 29, 30, 31,
        ];
        path_bytes[24..32].copy_from_slice(&i.to_be_bytes());

        let path = TrieHash::from_bytes(&path_bytes).unwrap();
        let value = TrieLeaf::new(&[], &[i as u8; 40]);
        confirmed_marf.insert_raw(path.clone(), value).unwrap();
    }

    let confirmed_tip = StacksBlockId([0x01; 32]);
    confirmed_marf.commit_to(&confirmed_tip).unwrap();

    let marf_opts = MARFOpenOpts::default();
    let marf_storage =
        TrieFileStorage::<StacksBlockId>::open_unconfirmed(&test_name, marf_opts).unwrap();
    let mut marf = MARF::from_storage(marf_storage);

    let mut last_trie = None;

    let mut all_new_paths = vec![];

    // instantiate unconfirmed m times
    for j in 0..m {
        let unconfirmed_tip = marf.begin_unconfirmed(&confirmed_tip).unwrap();
        let mut new_inserted = vec![];

        if let Some(mut trie) = last_trie.take() {
            let uncommitted_writes = marf
                .borrow_storage_backend()
                .transient_data_mut()
                .uncommitted_writes
                .take();
            if let Some((bhh, orig_loaded_trie)) = uncommitted_writes {
                let mut loaded_trie = orig_loaded_trie.clone();
                marf.borrow_storage_backend()
                    .transient_data_mut()
                    .uncommitted_writes = Some((bhh, orig_loaded_trie));
                assert!(trie_cmp(
                    &mut marf.borrow_storage_transaction(),
                    loaded_trie.trie_ram_mut(),
                    &mut trie
                ));
            }
        }

        // pre-populated keys are present
        for i in 0..n {
            let mut path_bytes = [
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30, 31,
            ];
            path_bytes[24..32].copy_from_slice(&i.to_be_bytes());

            let path = TrieHash::from_bytes(&path_bytes).unwrap();

            // NOTE: may have been overwritten; just check for presence
            assert!(
                MARF::get_path(&mut marf.borrow_storage_backend(), &unconfirmed_tip, &path)
                    .unwrap()
                    .is_some()
            );
        }

        // insert new keys
        for i in 0..n {
            // NOTE: may overwrite prepopulated values
            let mut path_bytes = [
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30, 31,
            ];
            path_bytes[24..32].copy_from_slice(&i.to_be_bytes());
            if !same {
                path_bytes[16..24].copy_from_slice(&j.to_be_bytes());
            }

            let path = TrieHash::from_bytes(&path_bytes).unwrap();
            let value = TrieLeaf::new(&[], &[(i + 128) as u8; 40]);

            new_inserted.push((path.clone(), value.clone()));

            if let Ok(Some(_)) = MARF::get_path(
                &mut confirmed_marf.borrow_storage_backend(),
                &confirmed_tip,
                &path,
            ) {
            } else {
                all_new_paths.push(path.clone());
            }

            marf.insert_raw(path, value).unwrap();
        }

        // verify that all new keys are there, off the unconfirmed tip
        for (path, expected_value) in new_inserted.iter() {
            let value = MARF::get_path(&mut marf.borrow_storage_backend(), &unconfirmed_tip, &path)
                .unwrap()
                .unwrap();
            assert_eq!(expected_value.data, value.data);
        }

        last_trie = Some(
            match marf
                .borrow_storage_backend()
                .transient_data()
                .uncommitted_writes
                .clone()
                .unwrap()
                .1
            {
                UncommittedState::RW(trie) => trie,
                UncommittedState::Sealed(trie, ..) => trie,
            },
        );
        marf.commit().unwrap();
    }

    let unconfirmed_tip = MARF::make_unconfirmed_chain_tip(&confirmed_tip);

    // test rollback
    for path in all_new_paths.iter() {
        eprintln!("path present? {:?}", &path);
        assert!(
            MARF::get_path(&mut marf.borrow_storage_backend(), &unconfirmed_tip, &path)
                .unwrap()
                .is_some()
        );
    }

    marf.drop_unconfirmed();

    for path in all_new_paths.iter() {
        eprintln!("path absent?  {:?}", &path);
        assert!(MARF::get_path(&mut marf.borrow_storage_backend(), &confirmed_tip, &path).is_err());
    }
}

#[test]
fn load_store_trie_4_4_same() {
    load_store_trie_m_n_same(4, 4, true);
}

#[test]
fn load_store_trie_4_4_unique() {
    load_store_trie_m_n_same(4, 4, false);
}

#[test]
fn load_store_trie_4_16_same() {
    load_store_trie_m_n_same(4, 16, true);
}

#[test]
fn load_store_trie_4_16_unique() {
    load_store_trie_m_n_same(4, 16, false);
}

#[test]
fn load_store_trie_4_48_same() {
    load_store_trie_m_n_same(4, 48, true);
}

#[test]
fn load_store_trie_4_48_unique() {
    load_store_trie_m_n_same(4, 48, false);
}

#[test]
fn load_store_trie_4_256_same() {
    load_store_trie_m_n_same(4, 256, true);
}

#[test]
fn load_store_trie_4_256_unique() {
    load_store_trie_m_n_same(4, 256, false);
}
