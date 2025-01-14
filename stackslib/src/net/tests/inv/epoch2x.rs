// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2023 Stacks Open Internet Foundation
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

use std::collections::HashMap;

use stacks_common::deps_common::bitcoin::network::serialize::BitcoinHash;

use crate::burnchains::bitcoin::indexer::BitcoinIndexer;
use crate::burnchains::db::BurnchainHeaderReader;
use crate::burnchains::tests::BURNCHAIN_TEST_BLOCK_TIME;
use crate::burnchains::{Burnchain, BurnchainBlockHeader, BurnchainView, PoxConstants};
use crate::chainstate::burn::db::sortdb::SortitionHandleConn;
use crate::chainstate::coordinator::tests::get_burnchain;
use crate::chainstate::stacks::*;
use crate::net::chat::ConversationP2P;
use crate::net::inv::inv2x::*;
use crate::net::test::*;
use crate::net::{Error as net_error, *};
use crate::util_lib::test::*;

#[test]
fn peerblocksinv_has_ith_block() {
    let peer_inv = PeerBlocksInv::new(vec![0x55, 0x77], vec![0x11, 0x22], vec![0x01], 16, 1, 12345);
    let has_blocks = [
        true, false, true, false, true, false, true, false, true, true, true, false, true, true,
        true, false,
    ];
    let has_microblocks = [
        true, false, false, false, true, false, false, false, false, true, false, false, false,
        true, false, false,
    ];

    assert!(!peer_inv.has_ith_block(12344));
    assert!(!peer_inv.has_ith_block(12345 + 17));

    assert!(!peer_inv.has_ith_microblock_stream(12344));
    assert!(!peer_inv.has_ith_microblock_stream(12345 + 17));

    for i in 0..16 {
        assert_eq!(has_blocks[i], peer_inv.has_ith_block((12345 + i) as u64));
        assert_eq!(
            has_microblocks[i],
            peer_inv.has_ith_microblock_stream((12345 + i) as u64)
        );
    }
}

#[test]
fn peerblocksinv_merge() {
    let peer_inv = PeerBlocksInv::new(
        vec![0x00, 0x00, 0x55, 0x77],
        vec![0x00, 0x00, 0x55, 0x77],
        vec![0x01],
        32,
        1,
        12345,
    );

    // merge below, aligned
    let mut peer_inv_below = peer_inv.clone();
    let (new_blocks, new_microblocks) =
        peer_inv_below.merge_blocks_inv(12345, 16, vec![0x11, 0x22], vec![0x11, 0x22], false);
    assert_eq!(new_blocks, 4);
    assert_eq!(new_microblocks, 4);
    assert_eq!(peer_inv_below.num_sortitions, 32);
    assert_eq!(peer_inv_below.block_inv, vec![0x11, 0x22, 0x55, 0x77]);
    assert_eq!(peer_inv_below.microblocks_inv, vec![0x11, 0x22, 0x55, 0x77]);

    // merge below, overlapping, aligned
    let mut peer_inv_below_overlap = peer_inv.clone();
    let (new_blocks, new_microblocks) = peer_inv_below_overlap.merge_blocks_inv(
        12345 + 8,
        16,
        vec![0x11, 0x22],
        vec![0x11, 0x22],
        false,
    );
    assert_eq!(new_blocks, 4);
    assert_eq!(new_microblocks, 4);
    assert_eq!(peer_inv_below_overlap.num_sortitions, 32);
    assert_eq!(
        peer_inv_below_overlap.block_inv,
        vec![0x00, 0x11, 0x22 | 0x55, 0x77]
    );
    assert_eq!(
        peer_inv_below_overlap.microblocks_inv,
        vec![0x00, 0x11, 0x22 | 0x55, 0x77]
    );

    // merge equal, overlapping, aligned
    let mut peer_inv_equal = peer_inv.clone();
    let (new_blocks, new_microblocks) =
        peer_inv_equal.merge_blocks_inv(12345 + 16, 16, vec![0x11, 0x22], vec![0x11, 0x22], false);
    assert_eq!(new_blocks, 0);
    assert_eq!(new_microblocks, 0);
    assert_eq!(peer_inv_equal.num_sortitions, 32);
    assert_eq!(
        peer_inv_equal.block_inv,
        vec![0x00, 0x00, 0x11 | 0x55, 0x22 | 0x77]
    );
    assert_eq!(
        peer_inv_equal.microblocks_inv,
        vec![0x00, 0x00, 0x11 | 0x55, 0x22 | 0x77]
    );

    // merge above, overlapping, aligned
    let mut peer_inv_above_overlap = peer_inv.clone();
    let (new_blocks, new_microblocks) = peer_inv_above_overlap.merge_blocks_inv(
        12345 + 24,
        16,
        vec![0x11, 0x22],
        vec![0x11, 0x22],
        false,
    );
    assert_eq!(new_blocks, 2);
    assert_eq!(new_microblocks, 2);
    assert_eq!(peer_inv_above_overlap.num_sortitions, 40);
    assert_eq!(
        peer_inv_above_overlap.block_inv,
        vec![0x00, 0x00, 0x55, 0x77 | 0x11, 0x22]
    );
    assert_eq!(
        peer_inv_above_overlap.microblocks_inv,
        vec![0x00, 0x00, 0x55, 0x77 | 0x11, 0x22]
    );

    // merge above, non-overlapping, aligned
    let mut peer_inv_above = peer_inv.clone();
    let (new_blocks, new_microblocks) =
        peer_inv_above.merge_blocks_inv(12345 + 32, 16, vec![0x11, 0x22], vec![0x11, 0x22], false);
    assert_eq!(peer_inv_above.num_sortitions, 48);
    assert_eq!(new_blocks, 4);
    assert_eq!(new_microblocks, 4);
    assert_eq!(
        peer_inv_above.block_inv,
        vec![0x00, 0x00, 0x55, 0x77, 0x11, 0x22]
    );
    assert_eq!(
        peer_inv_above.microblocks_inv,
        vec![0x00, 0x00, 0x55, 0x77, 0x11, 0x22]
    );

    // try merging unaligned
    let mut peer_inv = PeerBlocksInv::new(
        vec![0x00, 0x00, 0x00, 0x00],
        vec![0x00, 0x00, 0x00, 0x00],
        vec![0x01],
        32,
        1,
        12345,
    );
    for i in 0..32 {
        let (new_blocks, new_microblocks) =
            peer_inv.merge_blocks_inv(12345 + i, 1, vec![0x01], vec![0x01], false);
        assert_eq!(new_blocks, 1);
        assert_eq!(new_microblocks, 1);
        assert_eq!(peer_inv.num_sortitions, 32);
        for j in 0..i + 1 {
            assert!(peer_inv.has_ith_block(12345 + j));
            assert!(peer_inv.has_ith_microblock_stream(12345 + j));
        }
        for j in i + 1..32 {
            assert!(!peer_inv.has_ith_block(12345 + j));
            assert!(!peer_inv.has_ith_microblock_stream(12345 + j));
        }
    }

    // try merging unaligned, with multiple blocks
    let mut peer_inv = PeerBlocksInv::new(
        vec![0x00, 0x00, 0x00, 0x00],
        vec![0x00, 0x00, 0x00, 0x00],
        vec![0x01],
        32,
        1,
        12345,
    );
    for i in 0..16 {
        let (new_blocks, new_microblocks) = peer_inv.merge_blocks_inv(
            12345 + i,
            32,
            vec![0x01, 0x00, 0x01, 0x00],
            vec![0x01, 0x00, 0x01, 0x00],
            false,
        );
        assert_eq!(new_blocks, 2);
        assert_eq!(new_microblocks, 2);
        assert_eq!(peer_inv.num_sortitions, 32 + i);
        for j in 0..i + 1 {
            assert!(peer_inv.has_ith_block(12345 + j));
            assert!(peer_inv.has_ith_block(12345 + j + 16));

            assert!(peer_inv.has_ith_microblock_stream(12345 + j));
            assert!(peer_inv.has_ith_microblock_stream(12345 + j + 16));
        }
        for j in i + 1..16 {
            assert!(!peer_inv.has_ith_block(12345 + j));
            assert!(!peer_inv.has_ith_block(12345 + j + 16));

            assert!(!peer_inv.has_ith_microblock_stream(12345 + j));
            assert!(!peer_inv.has_ith_microblock_stream(12345 + j + 16));
        }
    }

    // merge 0's grows the bitvec
    let mut peer_inv = PeerBlocksInv::new(
        vec![0x00, 0x00, 0x00, 0x00],
        vec![0x00, 0x00, 0x00, 0x00],
        vec![0x01],
        32,
        1,
        12345,
    );
    let (new_blocks, new_microblocks) =
        peer_inv.merge_blocks_inv(12345 + 24, 16, vec![0x00, 0x00], vec![0x00, 0x00], false);
    assert_eq!(new_blocks, 0);
    assert_eq!(new_microblocks, 0);
    assert_eq!(peer_inv.num_sortitions, 40);
    assert_eq!(peer_inv.block_inv, vec![0x00, 0x00, 0x00, 0x00, 0x00]);
    assert_eq!(peer_inv.microblocks_inv, vec![0x00, 0x00, 0x00, 0x00, 0x00]);
}

#[test]
fn peerblocksinv_merge_clear_bits() {
    let peer_inv = PeerBlocksInv::new(
        vec![0x00, 0x00, 0x55, 0x77],
        vec![0x00, 0x00, 0x55, 0x77],
        vec![0x01],
        32,
        1,
        12345,
    );

    // merge below, aligned
    let mut peer_inv_below = peer_inv.clone();
    let (new_blocks, new_microblocks) =
        peer_inv_below.merge_blocks_inv(12345, 16, vec![0x11, 0x22], vec![0x11, 0x22], true);
    assert_eq!(new_blocks, 4);
    assert_eq!(new_microblocks, 4);
    assert_eq!(peer_inv_below.num_sortitions, 32);
    assert_eq!(peer_inv_below.block_inv, vec![0x11, 0x22, 0x55, 0x77]);
    assert_eq!(peer_inv_below.microblocks_inv, vec![0x11, 0x22, 0x55, 0x77]);

    // merge below, overlapping, aligned
    let mut peer_inv_below_overlap = peer_inv.clone();
    let (new_blocks, new_microblocks) = peer_inv_below_overlap.merge_blocks_inv(
        12345 + 8,
        16,
        vec![0x11, 0x22],
        vec![0x11, 0x22],
        true,
    );
    assert_eq!(new_blocks, 4);
    assert_eq!(new_microblocks, 4);
    assert_eq!(peer_inv_below_overlap.num_sortitions, 32);
    assert_eq!(
        peer_inv_below_overlap.block_inv,
        vec![0x00, 0x11, 0x22, 0x77]
    );
    assert_eq!(
        peer_inv_below_overlap.microblocks_inv,
        vec![0x00, 0x11, 0x22, 0x77]
    );

    // merge equal, overlapping, aligned
    let mut peer_inv_equal = peer_inv.clone();
    let (new_blocks, new_microblocks) =
        peer_inv_equal.merge_blocks_inv(12345 + 16, 16, vec![0x11, 0x22], vec![0x11, 0x22], true);
    assert_eq!(new_blocks, 0);
    assert_eq!(new_microblocks, 0);
    assert_eq!(peer_inv_equal.num_sortitions, 32);
    assert_eq!(peer_inv_equal.block_inv, vec![0x00, 0x00, 0x11, 0x22]);
    assert_eq!(peer_inv_equal.microblocks_inv, vec![0x00, 0x00, 0x11, 0x22]);

    // merge above, overlapping, aligned
    let mut peer_inv_above_overlap = peer_inv.clone();
    let (new_blocks, new_microblocks) = peer_inv_above_overlap.merge_blocks_inv(
        12345 + 24,
        16,
        vec![0x11, 0x22],
        vec![0x11, 0x22],
        true,
    );
    assert_eq!(new_blocks, 2);
    assert_eq!(new_microblocks, 2);
    assert_eq!(peer_inv_above_overlap.num_sortitions, 40);
    assert_eq!(
        peer_inv_above_overlap.block_inv,
        vec![0x00, 0x00, 0x55, 0x11, 0x22]
    );
    assert_eq!(
        peer_inv_above_overlap.microblocks_inv,
        vec![0x00, 0x00, 0x55, 0x11, 0x22]
    );

    // merge above, non-overlapping, aligned
    let mut peer_inv_above = peer_inv.clone();
    let (new_blocks, new_microblocks) =
        peer_inv_above.merge_blocks_inv(12345 + 32, 16, vec![0x11, 0x22], vec![0x11, 0x22], true);
    assert_eq!(peer_inv_above.num_sortitions, 48);
    assert_eq!(new_blocks, 4);
    assert_eq!(new_microblocks, 4);
    assert_eq!(
        peer_inv_above.block_inv,
        vec![0x00, 0x00, 0x55, 0x77, 0x11, 0x22]
    );
    assert_eq!(
        peer_inv_above.microblocks_inv,
        vec![0x00, 0x00, 0x55, 0x77, 0x11, 0x22]
    );

    // try merging unaligned
    let mut peer_inv = PeerBlocksInv::new(
        vec![0x00, 0x00, 0x00, 0x00],
        vec![0x00, 0x00, 0x00, 0x00],
        vec![0x01],
        32,
        1,
        12345,
    );
    for i in 0..32 {
        let (new_blocks, new_microblocks) =
            peer_inv.merge_blocks_inv(12345 + i, 1, vec![0x01], vec![0x01], true);
        assert_eq!(new_blocks, 1);
        assert_eq!(new_microblocks, 1);
        assert_eq!(peer_inv.num_sortitions, 32);
        for j in 0..i + 1 {
            assert!(peer_inv.has_ith_block(12345 + j));
            assert!(peer_inv.has_ith_microblock_stream(12345 + j));
        }
        for j in i + 1..32 {
            assert!(!peer_inv.has_ith_block(12345 + j));
            assert!(!peer_inv.has_ith_microblock_stream(12345 + j));
        }
    }

    // try merging unaligned, with multiple blocks
    let mut peer_inv = PeerBlocksInv::new(
        vec![0x00, 0x00, 0x00, 0x00],
        vec![0x00, 0x00, 0x00, 0x00],
        vec![0x01],
        32,
        1,
        12345,
    );
    for i in 0..16 {
        let (new_blocks, new_microblocks) = peer_inv.merge_blocks_inv(
            12345 + i,
            32,
            vec![0x01, 0x00, 0x01, 0x00],
            vec![0x01, 0x00, 0x01, 0x00],
            true,
        );
        assert_eq!(new_blocks, 2);
        assert_eq!(new_microblocks, 2);
        assert_eq!(peer_inv.num_sortitions, 32 + i);
        for j in 0..i {
            assert!(peer_inv.has_ith_block(12345 + j));
            assert!(!peer_inv.has_ith_block(12345 + j + 16));

            assert!(peer_inv.has_ith_microblock_stream(12345 + j));
            assert!(!peer_inv.has_ith_microblock_stream(12345 + j + 16));
        }

        assert!(peer_inv.has_ith_block(12345 + i));
        assert!(peer_inv.has_ith_block(12345 + i + 16));

        assert!(peer_inv.has_ith_microblock_stream(12345 + i));
        assert!(peer_inv.has_ith_microblock_stream(12345 + i + 16));

        for j in i + 1..16 {
            assert!(!peer_inv.has_ith_block(12345 + j));
            assert!(!peer_inv.has_ith_block(12345 + j + 16));

            assert!(!peer_inv.has_ith_microblock_stream(12345 + j));
            assert!(!peer_inv.has_ith_microblock_stream(12345 + j + 16));
        }
    }

    // merge 0's grows the bitvec
    let mut peer_inv = PeerBlocksInv::new(
        vec![0x00, 0x00, 0x00, 0x00],
        vec![0x00, 0x00, 0x00, 0x00],
        vec![0x01],
        32,
        1,
        12345,
    );
    let (new_blocks, new_microblocks) =
        peer_inv.merge_blocks_inv(12345 + 24, 16, vec![0x00, 0x00], vec![0x00, 0x00], true);
    assert_eq!(new_blocks, 0);
    assert_eq!(new_microblocks, 0);
    assert_eq!(peer_inv.num_sortitions, 40);
    assert_eq!(peer_inv.block_inv, vec![0x00, 0x00, 0x00, 0x00, 0x00]);
    assert_eq!(peer_inv.microblocks_inv, vec![0x00, 0x00, 0x00, 0x00, 0x00]);
}

#[test]
fn test_inv_set_block_microblock_bits() {
    let mut peer_inv = PeerBlocksInv::new(vec![0x01], vec![0x01], vec![0x01], 1, 1, 12345);

    assert!(peer_inv.set_block_bit(12345 + 1));
    assert_eq!(peer_inv.block_inv, vec![0x03]);
    assert_eq!(peer_inv.num_sortitions, 2);
    assert!(!peer_inv.set_block_bit(12345 + 1));
    assert_eq!(peer_inv.block_inv, vec![0x03]);
    assert_eq!(peer_inv.num_sortitions, 2);

    assert!(peer_inv.set_microblocks_bit(12345 + 1));
    assert_eq!(peer_inv.microblocks_inv, vec![0x03]);
    assert_eq!(peer_inv.num_sortitions, 2);
    assert!(!peer_inv.set_microblocks_bit(12345 + 1));
    assert_eq!(peer_inv.microblocks_inv, vec![0x03]);
    assert_eq!(peer_inv.num_sortitions, 2);

    assert!(peer_inv.set_block_bit(12345 + 1 + 16));
    assert_eq!(peer_inv.block_inv, vec![0x03, 0x00, 0x02]);
    assert_eq!(peer_inv.microblocks_inv, vec![0x03, 0x00, 0x00]);
    assert_eq!(peer_inv.num_sortitions, 18);
    assert!(!peer_inv.set_block_bit(12345 + 1 + 16));
    assert_eq!(peer_inv.block_inv, vec![0x03, 0x00, 0x02]);
    assert_eq!(peer_inv.microblocks_inv, vec![0x03, 0x00, 0x00]);
    assert_eq!(peer_inv.num_sortitions, 18);

    assert!(peer_inv.set_microblocks_bit(12345 + 1 + 32));
    assert_eq!(peer_inv.block_inv, vec![0x03, 0x00, 0x02, 0x00, 0x00]);
    assert_eq!(peer_inv.microblocks_inv, vec![0x03, 0x00, 0x00, 0x00, 0x02]);
    assert_eq!(peer_inv.num_sortitions, 34);
    assert!(!peer_inv.set_microblocks_bit(12345 + 1 + 32));
    assert_eq!(peer_inv.block_inv, vec![0x03, 0x00, 0x02, 0x00, 0x00]);
    assert_eq!(peer_inv.microblocks_inv, vec![0x03, 0x00, 0x00, 0x00, 0x02]);
    assert_eq!(peer_inv.num_sortitions, 34);
}

#[test]
fn test_inv_merge_pox_inv() {
    let mut burnchain = Burnchain::regtest("unused");
    burnchain.pox_constants = PoxConstants::test_20_no_sunset();
    let mut peer_inv = PeerBlocksInv::new(vec![0x01], vec![0x01], vec![0x01], 1, 1, 0);
    for i in 0..32 {
        let bit_flipped = peer_inv
            .merge_pox_inv(&burnchain, i + 1, 1, vec![0x01], false)
            .unwrap();
        assert_eq!(bit_flipped, i + 1);
        assert_eq!(peer_inv.num_reward_cycles, i + 2);
    }

    assert_eq!(peer_inv.pox_inv, vec![0xff, 0xff, 0xff, 0xff, 0x01]);
    assert_eq!(peer_inv.num_reward_cycles, 33);
}

#[test]
fn test_inv_truncate_pox_inv() {
    let mut burnchain = Burnchain::regtest("unused");
    burnchain.pox_constants = PoxConstants::test_20_no_sunset();
    let mut peer_inv = PeerBlocksInv::new(vec![0x01], vec![0x01], vec![0x01], 1, 1, 0);
    for i in 0..5 {
        let bit_flipped_opt = peer_inv.merge_pox_inv(&burnchain, i + 1, 1, vec![0x00], false);
        assert!(bit_flipped_opt.is_none());
        assert_eq!(peer_inv.num_reward_cycles, i + 2);
    }

    assert_eq!(peer_inv.pox_inv, vec![0x01]); // 0000 0001
    assert_eq!(peer_inv.num_reward_cycles, 6);

    for i in 0..(6 * burnchain.pox_constants.reward_cycle_length) {
        peer_inv.set_block_bit(i as u64);
        peer_inv.set_microblocks_bit(i as u64);
    }

    // 30 bits set, since the reward cycle is 5 blocks long
    assert_eq!(peer_inv.block_inv, vec![0xff, 0xff, 0xff, 0x3f]);
    assert_eq!(peer_inv.microblocks_inv, vec![0xff, 0xff, 0xff, 0x3f]);
    assert_eq!(
        peer_inv.num_sortitions,
        (6 * burnchain.pox_constants.reward_cycle_length) as u64
    );

    // PoX bit 3 flipped
    let bit_flipped = peer_inv
        .merge_pox_inv(&burnchain, 3, 1, vec![0x01], false)
        .unwrap();
    assert_eq!(bit_flipped, 3);

    assert_eq!(peer_inv.pox_inv, vec![0x9]); // 0000 1001
    assert_eq!(peer_inv.num_reward_cycles, 6);

    // truncate happened -- only reward cycles 0, 1, and 2 remain (3 * 5 = 15 bits)
    // BUT: reward cycles start on the _first_ block, so the first bit doesn't count!
    // The expected bit vector (grouped by reward cycle) is actually 1 11111 11111 11111.
    assert_eq!(peer_inv.block_inv, vec![0xff, 0xff, 0x00, 0x00]);
    assert_eq!(peer_inv.microblocks_inv, vec![0xff, 0xff, 0x00, 0x00]);
    assert_eq!(
        peer_inv.num_sortitions,
        (3 * burnchain.pox_constants.reward_cycle_length + 1) as u64
    );
}

#[test]
fn test_sync_inv_set_blocks_microblocks_available() {
    let mut peer_1_config = TestPeerConfig::new(function_name!(), 0, 0);
    let mut peer_2_config = TestPeerConfig::new(function_name!(), 0, 0);

    let mut peer_1 = TestPeer::new(peer_1_config.clone());
    let mut peer_2 = TestPeer::new(peer_2_config.clone());

    let peer_1_test_path = TestPeer::make_test_path(&peer_1.config);
    let peer_2_test_path = TestPeer::make_test_path(&peer_2.config);

    assert!(peer_1_test_path != peer_2_test_path);

    for (test_path, burnchain) in [
        (peer_1_test_path, &mut peer_1.config.burnchain),
        (peer_2_test_path, &mut peer_2.config.burnchain),
    ]
    .iter_mut()
    {
        let working_dir = get_burnchain(&test_path, None).working_dir;

        // pre-populate headers
        let mut indexer = BitcoinIndexer::new_unit_test(&working_dir);
        let now = BURNCHAIN_TEST_BLOCK_TIME;

        for header_height in 1..6 {
            let parent_hdr = indexer
                .read_burnchain_header(header_height - 1)
                .unwrap()
                .unwrap();

            let block_header_hash = BurnchainHeaderHash::from_bitcoin_hash(
                &BitcoinIndexer::mock_bitcoin_header(&parent_hdr.block_hash, now as u32)
                    .bitcoin_hash(),
            );

            let block_header = BurnchainBlockHeader {
                block_height: header_height,
                block_hash: block_header_hash.clone(),
                parent_block_hash: parent_hdr.block_hash.clone(),
                num_txs: 0,
                timestamp: now,
            };

            test_debug!(
                "Pre-populate block header for {}-{} ({})",
                &block_header.block_hash,
                &block_header.parent_block_hash,
                block_header.block_height
            );
            indexer.raw_store_header(block_header.clone()).unwrap();
        }

        let hdr = indexer
            .read_burnchain_header(burnchain.first_block_height)
            .unwrap()
            .unwrap();
        burnchain.first_block_hash = hdr.block_hash;
    }

    peer_1_config.burnchain.first_block_height = 5;
    peer_2_config.burnchain.first_block_height = 5;
    peer_1.config.burnchain.first_block_height = 5;
    peer_2.config.burnchain.first_block_height = 5;

    assert_eq!(
        peer_1_config.burnchain.first_block_hash,
        peer_2_config.burnchain.first_block_hash
    );

    let burnchain = peer_1_config.burnchain.clone();

    let num_blocks = 5;
    let first_stacks_block_height = {
        let sn = SortitionDB::get_canonical_burn_chain_tip(&peer_1.sortdb.as_ref().unwrap().conn())
            .unwrap();
        sn.block_height
    };

    for i in 0..num_blocks {
        let (burn_ops, stacks_block, microblocks) = peer_2.make_default_tenure();

        peer_1.next_burnchain_block(burn_ops.clone());
        peer_2.next_burnchain_block(burn_ops.clone());
        peer_2.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
    }

    let (tip, num_burn_blocks) = {
        let sn = SortitionDB::get_canonical_burn_chain_tip(peer_1.sortdb.as_ref().unwrap().conn())
            .unwrap();
        let num_burn_blocks = sn.block_height - peer_1.config.burnchain.first_block_height;
        (sn, num_burn_blocks)
    };

    let nk = peer_1.to_neighbor().addr;

    let sortdb = peer_1.sortdb.take().unwrap();
    peer_1.network.init_inv_sync_epoch2x(&sortdb);
    match peer_1.network.inv_state {
        Some(ref mut inv) => {
            inv.add_peer(nk.clone(), true);
        }
        None => {
            panic!("No inv state");
        }
    };
    peer_1.sortdb = Some(sortdb);

    for i in 0..num_blocks {
        let sortdb = peer_1.sortdb.take().unwrap();
        let sn = {
            let ic = sortdb.index_conn();
            let sn = SortitionDB::get_ancestor_snapshot(
                &ic,
                i + 1 + first_stacks_block_height,
                &tip.sortition_id,
            )
            .unwrap()
            .unwrap();
            eprintln!("{:?}", &sn);
            sn
        };
        peer_1.sortdb = Some(sortdb);
    }

    for i in 0..num_blocks {
        let sortdb = peer_1.sortdb.take().unwrap();
        match peer_1.network.inv_state {
            Some(ref mut inv) => {
                assert!(!inv
                    .block_stats
                    .get(&nk)
                    .unwrap()
                    .inv
                    .has_ith_block(i + first_stacks_block_height + 1));
                assert!(!inv
                    .block_stats
                    .get(&nk)
                    .unwrap()
                    .inv
                    .has_ith_microblock_stream(i + first_stacks_block_height + 1));

                let sn = {
                    let ic = sortdb.index_conn();
                    let sn = SortitionDB::get_ancestor_snapshot(
                        &ic,
                        i + first_stacks_block_height + 1,
                        &tip.sortition_id,
                    )
                    .unwrap()
                    .unwrap();
                    eprintln!("{:?}", &sn);
                    sn
                };

                // non-existent consensus has
                let sh =
                    inv.set_block_available(&burnchain, &nk, &sortdb, &ConsensusHash([0xfe; 20]));
                assert_eq!(Err(net_error::NotFoundError), sh);
                assert!(!inv
                    .block_stats
                    .get(&nk)
                    .unwrap()
                    .inv
                    .has_ith_block(i + first_stacks_block_height + 1));
                assert!(!inv
                    .block_stats
                    .get(&nk)
                    .unwrap()
                    .inv
                    .has_ith_microblock_stream(i + first_stacks_block_height + 1));

                // existing consensus hash (mock num_reward_cycles)
                inv.block_stats.get_mut(&nk).unwrap().inv.num_reward_cycles = 10;
                let sh = inv
                    .set_block_available(&burnchain, &nk, &sortdb, &sn.consensus_hash)
                    .unwrap();

                assert_eq!(
                    Some(i + first_stacks_block_height - sortdb.first_block_height + 1),
                    sh
                );
                assert!(inv
                    .block_stats
                    .get(&nk)
                    .unwrap()
                    .inv
                    .has_ith_block(i + first_stacks_block_height + 1));

                // idempotent
                let sh = inv
                    .set_microblocks_available(&burnchain, &nk, &sortdb, &sn.consensus_hash)
                    .unwrap();

                assert_eq!(
                    Some(i + first_stacks_block_height - sortdb.first_block_height + 1),
                    sh
                );
                assert!(inv
                    .block_stats
                    .get(&nk)
                    .unwrap()
                    .inv
                    .has_ith_microblock_stream(i + first_stacks_block_height + 1));

                assert!(inv
                    .set_block_available(&burnchain, &nk, &sortdb, &sn.consensus_hash)
                    .unwrap()
                    .is_none());
                assert!(inv
                    .set_microblocks_available(&burnchain, &nk, &sortdb, &sn.consensus_hash)
                    .unwrap()
                    .is_none());

                // existing consensus hash, but too far ahead (mock)
                inv.block_stats.get_mut(&nk).unwrap().inv.num_reward_cycles = 0;
                let sh = inv.set_block_available(&burnchain, &nk, &sortdb, &sn.consensus_hash);
                assert_eq!(Err(net_error::NotFoundError), sh);

                let sh =
                    inv.set_microblocks_available(&burnchain, &nk, &sortdb, &sn.consensus_hash);
                assert_eq!(Err(net_error::NotFoundError), sh);
            }
            None => {
                panic!("No inv state");
            }
        }
        peer_1.sortdb = Some(sortdb);
    }
}

#[test]
fn test_sync_inv_make_inv_messages() {
    let peer_1_config = TestPeerConfig::new(function_name!(), 0, 0);

    let indexer = BitcoinIndexer::new_unit_test(&peer_1_config.burnchain.working_dir);
    let reward_cycle_length = peer_1_config.burnchain.pox_constants.reward_cycle_length;
    let num_blocks = peer_1_config.burnchain.pox_constants.reward_cycle_length * 2;

    assert_eq!(reward_cycle_length, 5);

    let mut peer_1 = TestPeer::new(peer_1_config);

    let first_stacks_block_height = {
        let sn = SortitionDB::get_canonical_burn_chain_tip(&peer_1.sortdb.as_ref().unwrap().conn())
            .unwrap();
        sn.block_height
    };

    for i in 0..num_blocks {
        let (burn_ops, stacks_block, microblocks) = peer_1.make_default_tenure();

        peer_1.next_burnchain_block(burn_ops.clone());
        peer_1.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
    }

    let (tip, num_burn_blocks) = {
        let sn = SortitionDB::get_canonical_burn_chain_tip(peer_1.sortdb.as_ref().unwrap().conn())
            .unwrap();
        let num_burn_blocks = sn.block_height - peer_1.config.burnchain.first_block_height;
        (sn, num_burn_blocks)
    };

    peer_1
        .with_network_state(|sortdb, chainstate, network, _relayer, _mempool| {
            network.refresh_local_peer().unwrap();
            network
                .refresh_burnchain_view(&indexer, sortdb, chainstate, false)
                .unwrap();
            network.refresh_sortition_view(sortdb).unwrap();
            Ok(())
        })
        .unwrap();

    // simulate a getpoxinv / poxinv for one reward cycle
    let getpoxinv_request = peer_1
        .with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let height = network.burnchain.reward_cycle_to_block_height(1);
            let sn = {
                let ic = sortdb.index_conn();
                let sn = SortitionDB::get_ancestor_snapshot(&ic, height, &tip.sortition_id)
                    .unwrap()
                    .unwrap();
                sn
            };
            let getpoxinv = GetPoxInv {
                consensus_hash: sn.consensus_hash,
                num_cycles: 1,
            };
            Ok(getpoxinv)
        })
        .unwrap();

    test_debug!("\n\nSend {:?}\n\n", &getpoxinv_request);

    let reply = peer_1
        .with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getpoxinv_response(network, sortdb, &getpoxinv_request)
        })
        .unwrap();

    test_debug!("\n\nReply {:?}\n\n", &reply);

    match reply {
        StacksMessageType::PoxInv(poxinv) => {
            assert_eq!(poxinv.bitlen, 1);
            assert_eq!(poxinv.pox_bitvec, vec![0x01]);
        }
        x => {
            error!("Did not get PoxInv, but got {:?}", &x);
            assert!(false);
        }
    }

    // simulate a getpoxinv / poxinv for several reward cycles, including more than we have
    // (10, but only have 7)
    let getpoxinv_request = peer_1
        .with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let height = network.burnchain.reward_cycle_to_block_height(1);
            let sn = {
                let ic = sortdb.index_conn();
                let sn = SortitionDB::get_ancestor_snapshot(&ic, height, &tip.sortition_id)
                    .unwrap()
                    .unwrap();
                sn
            };
            let getpoxinv = GetPoxInv {
                consensus_hash: sn.consensus_hash,
                num_cycles: 10,
            };
            Ok(getpoxinv)
        })
        .unwrap();

    test_debug!("\n\nSend {:?}\n\n", &getpoxinv_request);

    let reply = peer_1
        .with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getpoxinv_response(network, sortdb, &getpoxinv_request)
        })
        .unwrap();

    test_debug!("\n\nReply {:?}\n\n", &reply);

    match reply {
        StacksMessageType::PoxInv(poxinv) => {
            assert_eq!(poxinv.bitlen, 7); // 2 reward cycles we generated, plus 5 reward cycles when booted up (1 reward cycle = 5 blocks).  1st one is free
            assert_eq!(poxinv.pox_bitvec, vec![0x7f]);
        }
        x => {
            error!("Did not get PoxInv, but got {:?}", &x);
            assert!(false);
        }
    }

    // ask for a PoX vector off of an unknown consensus hash
    let getpoxinv_request = peer_1
        .with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let getpoxinv = GetPoxInv {
                consensus_hash: ConsensusHash([0xaa; 20]),
                num_cycles: 10,
            };
            Ok(getpoxinv)
        })
        .unwrap();

    test_debug!("\n\nSend {:?}\n\n", &getpoxinv_request);

    let reply = peer_1
        .with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getpoxinv_response(network, sortdb, &getpoxinv_request)
        })
        .unwrap();

    test_debug!("\n\nReply {:?}\n\n", &reply);

    match reply {
        StacksMessageType::Nack(nack_data) => {
            assert_eq!(nack_data.error_code, NackErrorCodes::InvalidPoxFork);
        }
        x => {
            error!("Did not get PoxInv, but got {:?}", &x);
            assert!(false);
        }
    }

    // ask for a getblocksinv, aligned on a reward cycle.
    let getblocksinv_request = peer_1
        .with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let height = network.burnchain.reward_cycle_to_block_height(
                network
                    .burnchain
                    .block_height_to_reward_cycle(first_stacks_block_height)
                    .unwrap(),
            );
            let sn = {
                let ic = sortdb.index_conn();
                let sn = SortitionDB::get_ancestor_snapshot(&ic, height, &tip.sortition_id)
                    .unwrap()
                    .unwrap();
                sn
            };
            let getblocksinv = GetBlocksInv {
                consensus_hash: sn.consensus_hash,
                num_blocks: reward_cycle_length as u16,
            };
            Ok(getblocksinv)
        })
        .unwrap();

    test_debug!("\n\nSend {:?}\n\n", &getblocksinv_request);

    let reply = peer_1
        .with_network_state(|sortdb, chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getblocksinv_response(
                network,
                sortdb,
                chainstate,
                &getblocksinv_request,
            )
        })
        .unwrap();

    test_debug!("\n\nReply {:?}\n\n", &reply);

    match reply {
        StacksMessageType::BlocksInv(blocksinv) => {
            assert_eq!(blocksinv.bitlen, reward_cycle_length as u16);
            assert_eq!(blocksinv.block_bitvec, vec![0x1f]);
            assert_eq!(blocksinv.microblocks_bitvec, vec![0x1e]);
        }
        x => {
            error!("Did not get BlocksInv, but got {:?}", &x);
            assert!(false);
        }
    };

    // ask for a getblocksinv, right at the first Stacks block height
    let getblocksinv_request = peer_1
        .with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let height = network.burnchain.reward_cycle_to_block_height(
                network
                    .burnchain
                    .block_height_to_reward_cycle(first_stacks_block_height)
                    .unwrap(),
            );
            test_debug!("Ask for inv at height {}", height);
            let sn = {
                let ic = sortdb.index_conn();
                let sn = SortitionDB::get_ancestor_snapshot(&ic, height, &tip.sortition_id)
                    .unwrap()
                    .unwrap();
                sn
            };
            let getblocksinv = GetBlocksInv {
                consensus_hash: sn.consensus_hash,
                num_blocks: reward_cycle_length as u16,
            };
            Ok(getblocksinv)
        })
        .unwrap();

    test_debug!("\n\nSend {:?}\n\n", &getblocksinv_request);

    let reply = peer_1
        .with_network_state(|sortdb, chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getblocksinv_response(
                network,
                sortdb,
                chainstate,
                &getblocksinv_request,
            )
        })
        .unwrap();

    test_debug!("\n\nReply {:?}\n\n", &reply);

    match reply {
        StacksMessageType::BlocksInv(blocksinv) => {
            assert_eq!(blocksinv.bitlen, reward_cycle_length as u16);
            assert_eq!(blocksinv.block_bitvec, vec![0x1f]);
            assert_eq!(blocksinv.microblocks_bitvec, vec![0x1e]);
        }
        x => {
            error!("Did not get Nack, but got {:?}", &x);
            assert!(false);
        }
    };

    // ask for a getblocksinv, prior to the first Stacks block height
    let getblocksinv_request = peer_1
        .with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let height = network.burnchain.reward_cycle_to_block_height(
                network
                    .burnchain
                    .block_height_to_reward_cycle(first_stacks_block_height)
                    .unwrap()
                    - 1,
            );
            test_debug!("Ask for inv at height {}", height);
            let sn = {
                let ic = sortdb.index_conn();
                let sn = SortitionDB::get_ancestor_snapshot(&ic, height, &tip.sortition_id)
                    .unwrap()
                    .unwrap();
                sn
            };
            let getblocksinv = GetBlocksInv {
                consensus_hash: sn.consensus_hash,
                num_blocks: reward_cycle_length as u16,
            };
            Ok(getblocksinv)
        })
        .unwrap();

    test_debug!("\n\nSend {:?}\n\n", &getblocksinv_request);

    let reply = peer_1
        .with_network_state(|sortdb, chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getblocksinv_response(
                network,
                sortdb,
                chainstate,
                &getblocksinv_request,
            )
        })
        .unwrap();

    test_debug!("\n\nReply {:?}\n\n", &reply);

    match reply {
        StacksMessageType::BlocksInv(blocksinv) => {
            assert_eq!(blocksinv.bitlen, reward_cycle_length as u16);
            assert_eq!(blocksinv.block_bitvec, vec![0x0]);
            assert_eq!(blocksinv.microblocks_bitvec, vec![0x0]);
        }
        x => {
            error!("Did not get BlocksInv, but got {:?}", &x);
            assert!(false);
        }
    };

    // ask for a getblocksinv, unaligned to a reward cycle
    let getblocksinv_request = peer_1
        .with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let height = network.burnchain.reward_cycle_to_block_height(
                network
                    .burnchain
                    .block_height_to_reward_cycle(first_stacks_block_height)
                    .unwrap(),
            ) + 1;
            let sn = {
                let ic = sortdb.index_conn();
                let sn = SortitionDB::get_ancestor_snapshot(&ic, height, &tip.sortition_id)
                    .unwrap()
                    .unwrap();
                sn
            };
            let getblocksinv = GetBlocksInv {
                consensus_hash: sn.consensus_hash,
                num_blocks: reward_cycle_length as u16,
            };
            Ok(getblocksinv)
        })
        .unwrap();

    test_debug!("\n\nSend {:?}\n\n", &getblocksinv_request);

    let reply = peer_1
        .with_network_state(|sortdb, chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getblocksinv_response(
                network,
                sortdb,
                chainstate,
                &getblocksinv_request,
            )
        })
        .unwrap();

    test_debug!("\n\nReply {:?}\n\n", &reply);

    match reply {
        StacksMessageType::Nack(nack_data) => {
            assert_eq!(nack_data.error_code, NackErrorCodes::InvalidPoxFork);
        }
        x => {
            error!("Did not get Nack, but got {:?}", &x);
            assert!(false);
        }
    };

    // ask for a getblocksinv, for an unknown consensus hash
    let getblocksinv_request = peer_1
        .with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let getblocksinv = GetBlocksInv {
                consensus_hash: ConsensusHash([0xaa; 20]),
                num_blocks: reward_cycle_length as u16,
            };
            Ok(getblocksinv)
        })
        .unwrap();

    test_debug!("\n\nSend {:?}\n\n", &getblocksinv_request);

    let reply = peer_1
        .with_network_state(|sortdb, chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getblocksinv_response(
                network,
                sortdb,
                chainstate,
                &getblocksinv_request,
            )
        })
        .unwrap();

    test_debug!("\n\nReply {:?}\n\n", &reply);

    match reply {
        StacksMessageType::Nack(nack_data) => {
            assert_eq!(nack_data.error_code, NackErrorCodes::NoSuchBurnchainBlock);
        }
        x => {
            error!("Did not get Nack, but got {:?}", &x);
            assert!(false);
        }
    };
}

#[test]
fn test_sync_inv_diagnose_nack() {
    let peer_config = TestPeerConfig::new(function_name!(), 0, 0);
    let neighbor = peer_config.to_neighbor();
    let neighbor_key = neighbor.addr.clone();
    let nack_no_block = NackData {
        error_code: NackErrorCodes::NoSuchBurnchainBlock,
    };

    let mut burnchain_view = BurnchainView {
        burn_block_height: 12346,
        burn_block_hash: BurnchainHeaderHash([0x11; 32]),
        burn_stable_block_height: 12340,
        burn_stable_block_hash: BurnchainHeaderHash([0x22; 32]),
        last_burn_block_hashes: HashMap::new(),
        rc_consensus_hash: ConsensusHash([0x33; 20]),
    };

    burnchain_view.make_test_data();
    let ch_12345 = burnchain_view
        .last_burn_block_hashes
        .get(&12345)
        .unwrap()
        .clone();
    let ch_12340 = burnchain_view
        .last_burn_block_hashes
        .get(&12340)
        .unwrap()
        .clone();
    let ch_12341 = burnchain_view
        .last_burn_block_hashes
        .get(&12341)
        .unwrap()
        .clone();
    let ch_12339 = burnchain_view
        .last_burn_block_hashes
        .get(&12339)
        .unwrap()
        .clone();
    let ch_12334 = burnchain_view
        .last_burn_block_hashes
        .get(&12334)
        .unwrap()
        .clone();

    // should be stable; but got nacked (so this would be inappropriate)
    assert_eq!(
        NodeStatus::Diverged,
        NeighborBlockStats::diagnose_nack(
            &neighbor_key,
            nack_no_block.clone(),
            &burnchain_view,
            12346,
            12340,
            &BurnchainHeaderHash([0x11; 32]),
            &BurnchainHeaderHash([0x22; 32]),
            false
        )
    );

    assert_eq!(
        NodeStatus::Diverged,
        NeighborBlockStats::diagnose_nack(
            &neighbor_key,
            nack_no_block.clone(),
            &burnchain_view,
            12346,
            12340,
            &BurnchainHeaderHash([0x11; 32]),
            &BurnchainHeaderHash([0x22; 32]),
            true
        )
    );

    // should be stale
    assert_eq!(
        NodeStatus::Stale,
        NeighborBlockStats::diagnose_nack(
            &neighbor_key,
            nack_no_block.clone(),
            &burnchain_view,
            12345,
            12339,
            &ch_12345.clone(),
            &ch_12339.clone(),
            false
        )
    );

    // should be diverged -- different stable burn block hash
    assert_eq!(
        NodeStatus::Diverged,
        NeighborBlockStats::diagnose_nack(
            &neighbor_key,
            nack_no_block.clone(),
            &burnchain_view,
            12346,
            12340,
            &BurnchainHeaderHash([0x12; 32]),
            &BurnchainHeaderHash([0x23; 32]),
            false
        )
    );
}

#[test]
fn test_inv_sync_start_reward_cycle() {
    let mut peer_1_config = TestPeerConfig::new(function_name!(), 0, 0);
    peer_1_config.connection_opts.inv_reward_cycles = 0;

    let mut peer_1 = TestPeer::new(peer_1_config);

    let num_blocks = GETPOXINV_MAX_BITLEN * 2;
    for i in 0..num_blocks {
        let (burn_ops, stacks_block, microblocks) = peer_1.make_default_tenure();
        peer_1.next_burnchain_block(burn_ops.clone());
        peer_1.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
    }

    let _ = peer_1.step();

    let block_scan_start = peer_1
        .network
        .get_block_scan_start(peer_1.sortdb.as_ref().unwrap());
    assert_eq!(block_scan_start, 7);

    peer_1.network.connection_opts.inv_reward_cycles = 1;

    let block_scan_start = peer_1
        .network
        .get_block_scan_start(peer_1.sortdb.as_ref().unwrap());
    assert_eq!(block_scan_start, 7);

    peer_1.network.connection_opts.inv_reward_cycles = 2;

    let block_scan_start = peer_1
        .network
        .get_block_scan_start(peer_1.sortdb.as_ref().unwrap());
    assert_eq!(block_scan_start, 6);

    peer_1.network.connection_opts.inv_reward_cycles = 3;

    let block_scan_start = peer_1
        .network
        .get_block_scan_start(peer_1.sortdb.as_ref().unwrap());
    assert_eq!(block_scan_start, 5);

    peer_1.network.connection_opts.inv_reward_cycles = 300;

    let block_scan_start = peer_1
        .network
        .get_block_scan_start(peer_1.sortdb.as_ref().unwrap());
    assert_eq!(block_scan_start, 0);
}

#[test]
fn test_inv_sync_check_peer_epoch2x_synced() {
    let mut peer_1_config = TestPeerConfig::new(function_name!(), 0, 0);
    peer_1_config.connection_opts.inv_reward_cycles = 0;

    let mut peer_1 = TestPeer::new(peer_1_config);

    let num_blocks = GETPOXINV_MAX_BITLEN * 2;
    for i in 0..num_blocks {
        let (burn_ops, stacks_block, microblocks) = peer_1.make_default_tenure();
        peer_1.next_burnchain_block(burn_ops.clone());
        peer_1.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
    }

    let _ = peer_1.step();
    let tip_rc = peer_1
        .network
        .burnchain
        .block_height_to_reward_cycle(peer_1.network.burnchain_tip.block_height)
        .unwrap();
    assert!(tip_rc > 0);

    let pox_rc = peer_1.network.pox_id.num_inventory_reward_cycles() as u64;

    assert!(peer_1.network.check_peer_epoch2x_synced(true, tip_rc));
    assert!(peer_1.network.check_peer_epoch2x_synced(true, tip_rc + 1));
    assert!(!peer_1.network.check_peer_epoch2x_synced(true, tip_rc - 1));

    assert!(peer_1.network.check_peer_epoch2x_synced(false, pox_rc));
    assert!(peer_1.network.check_peer_epoch2x_synced(false, pox_rc + 1));
    assert!(!peer_1.network.check_peer_epoch2x_synced(false, pox_rc - 1));
}

#[test]
#[ignore]
fn test_sync_inv_2_peers_plain() {
    with_timeout(600, || {
        let mut peer_1_config = TestPeerConfig::new(function_name!(), 0, 0);
        let mut peer_2_config = TestPeerConfig::new(function_name!(), 0, 0);

        peer_1_config.connection_opts.inv_reward_cycles = 10;
        peer_2_config.connection_opts.inv_reward_cycles = 10;

        let mut peer_1 = TestPeer::new(peer_1_config);
        let mut peer_2 = TestPeer::new(peer_2_config);

        peer_1.add_neighbor(&mut peer_2.to_neighbor(), None, true);
        peer_2.add_neighbor(&mut peer_1.to_neighbor(), None, true);

        let num_blocks = GETPOXINV_MAX_BITLEN * 2;
        let first_stacks_block_height = {
            let sn =
                SortitionDB::get_canonical_burn_chain_tip(&peer_1.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            sn.block_height + 1
        };

        for i in 0..num_blocks {
            let (burn_ops, stacks_block, microblocks) = peer_2.make_default_tenure();

            peer_1.next_burnchain_block(burn_ops.clone());
            peer_2.next_burnchain_block(burn_ops.clone());

            peer_1.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
            peer_2.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
        }

        let num_burn_blocks = {
            let sn =
                SortitionDB::get_canonical_burn_chain_tip(peer_1.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            sn.block_height + 1
        };

        let mut round = 0;
        let mut inv_1_count = 0;
        let mut inv_2_count = 0;

        while inv_1_count < num_blocks || inv_2_count < num_blocks {
            let _ = peer_1.step();
            let _ = peer_2.step();

            inv_1_count = match peer_1.network.inv_state {
                Some(ref inv) => {
                    info!("Peer 1 stats: {:?}", &inv.block_stats);
                    inv.get_inv_num_blocks(&peer_2.to_neighbor().addr)
                }
                None => 0,
            };

            inv_2_count = match peer_2.network.inv_state {
                Some(ref inv) => {
                    info!("Peer 2 stats: {:?}", &inv.block_stats);
                    inv.get_inv_num_blocks(&peer_1.to_neighbor().addr)
                }
                None => 0,
            };

            // nothing should break
            match peer_1.network.inv_state {
                Some(ref inv) => {
                    assert_eq!(inv.get_broken_peers().len(), 0);
                    assert_eq!(inv.get_dead_peers().len(), 0);
                    assert_eq!(inv.get_diverged_peers().len(), 0);
                }
                None => {}
            }

            match peer_2.network.inv_state {
                Some(ref inv) => {
                    assert_eq!(inv.get_broken_peers().len(), 0);
                    assert_eq!(inv.get_dead_peers().len(), 0);
                    assert_eq!(inv.get_diverged_peers().len(), 0);
                }
                None => {}
            }

            round += 1;

            info!("Peer 1: {}, Peer 2: {}", inv_1_count, inv_2_count);
        }

        info!("Completed walk round {} step(s)", round);

        peer_1.dump_frontier();
        peer_2.dump_frontier();

        info!(
            "Peer 1 stats: {:?}",
            &peer_1.network.inv_state.as_ref().unwrap().block_stats
        );
        info!(
            "Peer 2 stats: {:?}",
            &peer_2.network.inv_state.as_ref().unwrap().block_stats
        );

        let peer_1_inv = peer_2
            .network
            .inv_state
            .as_ref()
            .unwrap()
            .block_stats
            .get(&peer_1.to_neighbor().addr)
            .unwrap()
            .inv
            .clone();
        let peer_2_inv = peer_1
            .network
            .inv_state
            .as_ref()
            .unwrap()
            .block_stats
            .get(&peer_2.to_neighbor().addr)
            .unwrap()
            .inv
            .clone();

        info!("Peer 1 inv: {:?}", &peer_1_inv);
        info!("Peer 2 inv: {:?}", &peer_2_inv);

        info!("peer 1's view of peer 2: {:?}", &peer_2_inv);

        assert_eq!(peer_2_inv.num_sortitions, num_burn_blocks);

        // peer 1 should have learned that peer 2 has all the blocks
        for i in 0..num_blocks {
            assert!(
                peer_2_inv.has_ith_block(i + first_stacks_block_height),
                "Missing block {} (+ {})",
                i,
                first_stacks_block_height
            );
        }

        // peer 1 should have learned that peer 2 has all the microblock streams
        for i in 1..(num_blocks - 1) {
            assert!(
                peer_2_inv.has_ith_microblock_stream(i + first_stacks_block_height),
                "Missing microblock {} (+ {})",
                i,
                first_stacks_block_height
            );
        }

        let peer_1_inv = peer_2
            .network
            .inv_state
            .as_ref()
            .unwrap()
            .block_stats
            .get(&peer_1.to_neighbor().addr)
            .unwrap()
            .inv
            .clone();
        test_debug!("peer 2's view of peer 1: {:?}", &peer_1_inv);

        assert_eq!(peer_1_inv.num_sortitions, num_burn_blocks);

        // peer 2 should have learned that peer 1 has all the blocks as well
        for i in 0..num_blocks {
            assert!(
                peer_1_inv.has_ith_block(i + first_stacks_block_height),
                "Missing block {} (+ {})",
                i,
                first_stacks_block_height
            );
        }
    })
}

#[test]
#[ignore]
fn test_sync_inv_2_peers_stale() {
    with_timeout(600, || {
        let mut peer_1_config = TestPeerConfig::new(function_name!(), 0, 0);
        let mut peer_2_config = TestPeerConfig::new(function_name!(), 0, 0);

        peer_1_config.connection_opts.inv_reward_cycles = 10;
        peer_2_config.connection_opts.inv_reward_cycles = 10;

        let mut peer_1 = TestPeer::new(peer_1_config);
        let mut peer_2 = TestPeer::new(peer_2_config);

        peer_1.add_neighbor(&mut peer_2.to_neighbor(), None, true);
        peer_2.add_neighbor(&mut peer_1.to_neighbor(), None, true);

        let num_blocks = GETPOXINV_MAX_BITLEN * 2;
        let first_stacks_block_height = {
            let sn =
                SortitionDB::get_canonical_burn_chain_tip(&peer_1.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            sn.block_height + 1
        };

        for i in 0..num_blocks {
            let (burn_ops, stacks_block, microblocks) = peer_2.make_default_tenure();

            peer_2.next_burnchain_block(burn_ops.clone());
            peer_2.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
        }

        let mut round = 0;
        let mut inv_1_count = 0;
        let mut inv_2_count = 0;

        let mut peer_1_check = false;
        let mut peer_2_check = false;

        while !peer_1_check || !peer_2_check {
            let _ = peer_1.step();
            let _ = peer_2.step();

            inv_1_count = match peer_1.network.inv_state {
                Some(ref inv) => inv.get_inv_sortitions(&peer_2.to_neighbor().addr),
                None => 0,
            };

            inv_2_count = match peer_2.network.inv_state {
                Some(ref inv) => inv.get_inv_sortitions(&peer_1.to_neighbor().addr),
                None => 0,
            };

            match peer_1.network.inv_state {
                Some(ref inv) => {
                    info!("Peer 1 stats: {:?}", &inv.block_stats);
                    assert_eq!(inv.get_broken_peers().len(), 0);
                    assert_eq!(inv.get_dead_peers().len(), 0);
                    assert_eq!(inv.get_diverged_peers().len(), 0);

                    if let Some(ref peer_2_inv) = inv.block_stats.get(&peer_2.to_neighbor().addr) {
                        if peer_2_inv.inv.num_sortitions
                            == first_stacks_block_height
                                - peer_1.config.burnchain.first_block_height
                        {
                            for i in 0..first_stacks_block_height {
                                assert!(!peer_2_inv.inv.has_ith_block(i));
                                assert!(!peer_2_inv.inv.has_ith_microblock_stream(i));
                            }
                            peer_2_check = true;
                        }
                    }
                }
                None => {}
            }

            match peer_2.network.inv_state {
                Some(ref inv) => {
                    info!("Peer 2 stats: {:?}", &inv.block_stats);
                    assert_eq!(inv.get_broken_peers().len(), 0);
                    assert_eq!(inv.get_dead_peers().len(), 0);
                    assert_eq!(inv.get_diverged_peers().len(), 0);

                    if let Some(ref peer_1_inv) = inv.block_stats.get(&peer_1.to_neighbor().addr) {
                        if peer_1_inv.inv.num_sortitions
                            == first_stacks_block_height
                                - peer_1.config.burnchain.first_block_height
                        {
                            peer_1_check = true;
                        }
                    }
                }
                None => {}
            }

            round += 1;

            test_debug!("\n\npeer_1_check = {}, peer_2_check = {}, inv_1_count = {}, inv_2_count = {}, first_stacks_block_height = {}\n\n", peer_1_check, peer_2_check, inv_1_count, inv_2_count, first_stacks_block_height);
        }

        info!("Completed walk round {} step(s)", round);

        peer_1.dump_frontier();
        peer_2.dump_frontier();
    })
}

#[test]
#[ignore]
fn test_sync_inv_2_peers_unstable() {
    with_timeout(600, || {
        let mut peer_1_config = TestPeerConfig::new(function_name!(), 0, 0);
        let mut peer_2_config = TestPeerConfig::new(function_name!(), 0, 0);

        peer_1_config.connection_opts.inv_reward_cycles = 10;
        peer_2_config.connection_opts.inv_reward_cycles = 10;

        let stable_confs = peer_1_config.burnchain.stable_confirmations as u64;

        let mut peer_1 = TestPeer::new(peer_1_config);
        let mut peer_2 = TestPeer::new(peer_2_config);

        peer_1.add_neighbor(&mut peer_2.to_neighbor(), None, true);
        peer_2.add_neighbor(&mut peer_1.to_neighbor(), None, true);

        let num_blocks = GETPOXINV_MAX_BITLEN * 2;

        let first_stacks_block_height = {
            let sn =
                SortitionDB::get_canonical_burn_chain_tip(&peer_1.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            sn.block_height + 1
        };

        // only peer 2 makes progress after the point of stability.
        for i in 0..num_blocks {
            let (mut burn_ops, stacks_block, microblocks) = peer_2.make_default_tenure();

            let (_, burn_header_hash, consensus_hash) =
                peer_2.next_burnchain_block(burn_ops.clone());
            peer_2.process_stacks_epoch_at_tip(&stacks_block, &microblocks);

            TestPeer::set_ops_burn_header_hash(&mut burn_ops, &burn_header_hash);

            // NOTE: the nodes only differ by one block -- they agree on the same PoX vector
            if i + 1 < num_blocks {
                peer_1.next_burnchain_block_raw(burn_ops.clone());
                peer_1.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
            } else {
                // peer 1 diverges
                test_debug!("Peer 1 diverges at {}", i + first_stacks_block_height);
                peer_1.next_burnchain_block_diverge(vec![burn_ops[0].clone()]);
            }
        }

        // tips must differ
        {
            let sn1 =
                SortitionDB::get_canonical_burn_chain_tip(peer_1.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            let sn2 =
                SortitionDB::get_canonical_burn_chain_tip(peer_2.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            assert_ne!(sn1.burn_header_hash, sn2.burn_header_hash);
        }

        let num_stable_blocks = num_blocks - stable_confs;

        let num_burn_blocks = {
            let sn =
                SortitionDB::get_canonical_burn_chain_tip(peer_1.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            sn.block_height + 1
        };

        let mut round = 0;
        let mut inv_1_count = 0;
        let mut inv_2_count = 0;

        let mut peer_1_pox_cycle_start = false;
        let mut peer_1_block_cycle_start = false;
        let mut peer_2_pox_cycle_start = false;
        let mut peer_2_block_cycle_start = false;

        let mut peer_1_pox_cycle = false;
        let mut peer_1_block_cycle = false;
        let mut peer_2_pox_cycle = false;
        let mut peer_2_block_cycle = false;

        while inv_1_count < num_stable_blocks || inv_2_count < num_stable_blocks {
            let _ = peer_1.step();
            let _ = peer_2.step();

            inv_1_count = match peer_1.network.inv_state {
                Some(ref inv) => inv.get_inv_num_blocks(&peer_2.to_neighbor().addr),
                None => 0,
            };

            inv_2_count = match peer_2.network.inv_state {
                Some(ref inv) => inv.get_inv_num_blocks(&peer_1.to_neighbor().addr),
                None => 0,
            };

            match peer_1.network.inv_state {
                Some(ref inv) => {
                    info!("Peer 1 stats: {:?}", &inv.block_stats);
                    assert_eq!(inv.get_broken_peers().len(), 0);
                    assert_eq!(inv.get_dead_peers().len(), 0);
                    assert_eq!(inv.get_diverged_peers().len(), 0);

                    if let Some(stats) = inv.get_stats(&peer_2.to_neighbor().addr) {
                        if stats.target_pox_reward_cycle > 0 {
                            peer_1_pox_cycle_start = true;
                        }
                        if stats.target_block_reward_cycle > 0 {
                            peer_1_block_cycle_start = true;
                        }
                        if stats.target_pox_reward_cycle == 0 && peer_1_pox_cycle_start {
                            peer_1_pox_cycle = true;
                        }
                        if stats.target_block_reward_cycle == 0 && peer_1_block_cycle_start {
                            peer_1_block_cycle = true;
                        }
                    }
                }
                None => {}
            }

            match peer_2.network.inv_state {
                Some(ref inv) => {
                    info!("Peer 2 stats: {:?}", &inv.block_stats);
                    assert_eq!(inv.get_broken_peers().len(), 0);
                    assert_eq!(inv.get_dead_peers().len(), 0);
                    assert_eq!(inv.get_diverged_peers().len(), 0);

                    if let Some(stats) = inv.get_stats(&peer_1.to_neighbor().addr) {
                        if stats.target_pox_reward_cycle > 0 {
                            peer_2_pox_cycle_start = true;
                        }
                        if stats.target_block_reward_cycle > 0 {
                            peer_2_block_cycle_start = true;
                        }
                        if stats.target_pox_reward_cycle == 0 && peer_2_pox_cycle_start {
                            peer_2_pox_cycle = true;
                        }
                        if stats.target_block_reward_cycle == 0 && peer_2_block_cycle_start {
                            peer_2_block_cycle = true;
                        }
                    }
                }
                None => {}
            }

            round += 1;

            test_debug!(
                "\n\ninv_1_count = {}, inv_2_count = {}, num_stable_blocks = {}\n\n",
                inv_1_count,
                inv_2_count,
                num_stable_blocks
            );
        }

        info!("Completed walk round {} step(s)", round);

        peer_1.dump_frontier();
        peer_2.dump_frontier();

        let peer_2_inv = peer_1
            .network
            .inv_state
            .as_ref()
            .unwrap()
            .block_stats
            .get(&peer_2.to_neighbor().addr)
            .unwrap()
            .inv
            .clone();
        test_debug!("peer 1's view of peer 2: {:?}", &peer_2_inv);

        let peer_1_inv = peer_2
            .network
            .inv_state
            .as_ref()
            .unwrap()
            .block_stats
            .get(&peer_1.to_neighbor().addr)
            .unwrap()
            .inv
            .clone();
        test_debug!("peer 2's view of peer 1: {:?}", &peer_1_inv);

        assert_eq!(peer_2_inv.num_sortitions, num_burn_blocks - stable_confs);
        assert_eq!(peer_1_inv.num_sortitions, num_burn_blocks - stable_confs);

        // only 8 reward cycles -- we couldn't agree on the 9th
        assert_eq!(peer_1_inv.pox_inv, vec![255]);
        assert_eq!(peer_2_inv.pox_inv, vec![255]);

        // peer 1 should have learned that peer 2 has all the blocks, up to the point of
        // instability
        for i in 0..(num_blocks - stable_confs) {
            assert!(peer_2_inv.has_ith_block(i + first_stacks_block_height));
            if i > 0 {
                assert!(peer_2_inv.has_ith_microblock_stream(i + first_stacks_block_height));
            } else {
                assert!(!peer_2_inv.has_ith_microblock_stream(i + first_stacks_block_height));
            }
        }

        for i in 0..(num_blocks - stable_confs) {
            assert!(peer_1_inv.has_ith_block(i + first_stacks_block_height));
        }

        assert!(!peer_2_inv.has_ith_block(num_blocks - stable_confs));
        assert!(!peer_2_inv.has_ith_microblock_stream(num_blocks - stable_confs));
    })
}

#[test]
#[ignore]
fn test_sync_inv_2_peers_different_pox_vectors() {
    with_timeout(600, || {
        let mut peer_1_config = TestPeerConfig::new(function_name!(), 0, 0);
        let mut peer_2_config = TestPeerConfig::new(function_name!(), 0, 0);

        peer_1_config.connection_opts.inv_reward_cycles = 10;
        peer_2_config.connection_opts.inv_reward_cycles = 10;

        let reward_cycle_length = peer_1_config.burnchain.pox_constants.reward_cycle_length as u64;
        assert_eq!(reward_cycle_length, 5);

        let mut peer_1 = TestPeer::new(peer_1_config);
        let mut peer_2 = TestPeer::new(peer_2_config);

        peer_1.add_neighbor(&mut peer_2.to_neighbor(), None, true);
        peer_2.add_neighbor(&mut peer_1.to_neighbor(), None, true);

        let num_blocks = GETPOXINV_MAX_BITLEN * 3;

        let first_stacks_block_height = {
            let sn =
                SortitionDB::get_canonical_burn_chain_tip(&peer_1.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            sn.block_height + 1
        };

        // only peer 2 makes progress after the point of stability.
        for i in 0..num_blocks {
            let (mut burn_ops, stacks_block, microblocks) = peer_2.make_default_tenure();

            let (_, burn_header_hash, consensus_hash) =
                peer_2.next_burnchain_block(burn_ops.clone());
            peer_2.process_stacks_epoch_at_tip(&stacks_block, &microblocks);

            TestPeer::set_ops_burn_header_hash(&mut burn_ops, &burn_header_hash);

            peer_1.next_burnchain_block_raw(burn_ops.clone());
            if i < num_blocks - reward_cycle_length * 2 {
                peer_1.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
            }
        }

        let peer_1_pox_id = {
            let tip_sort_id =
                SortitionDB::get_canonical_sortition_tip(peer_1.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            let ic = peer_1.sortdb.as_ref().unwrap().index_conn();
            let sortdb_reader = SortitionHandleConn::open_reader(&ic, &tip_sort_id).unwrap();
            sortdb_reader.get_pox_id().unwrap()
        };

        let peer_2_pox_id = {
            let tip_sort_id =
                SortitionDB::get_canonical_sortition_tip(peer_2.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            let ic = peer_2.sortdb.as_ref().unwrap().index_conn();
            let sortdb_reader = SortitionHandleConn::open_reader(&ic, &tip_sort_id).unwrap();
            sortdb_reader.get_pox_id().unwrap()
        };

        // peers must have different PoX bit vectors -- peer 1 didn't see the last reward cycle
        assert_eq!(
            peer_1_pox_id,
            PoxId::from_bools(vec![
                true, true, true, true, true, true, true, true, true, true, false
            ])
        );
        assert_eq!(
            peer_2_pox_id,
            PoxId::from_bools(vec![
                true, true, true, true, true, true, true, true, true, true, true
            ])
        );

        let num_burn_blocks = {
            let sn =
                SortitionDB::get_canonical_burn_chain_tip(peer_1.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            sn.block_height + 1
        };

        let mut round = 0;
        let mut inv_1_count = 0;
        let mut inv_2_count = 0;
        let mut peer_1_sorts = 0;
        let mut peer_2_sorts = 0;

        while inv_1_count < reward_cycle_length * 4
            || inv_2_count < num_blocks - reward_cycle_length * 2
            || peer_1_sorts < reward_cycle_length * 9 + 1
            || peer_2_sorts < reward_cycle_length * 9 + 1
        {
            let _ = peer_1.step();
            let _ = peer_2.step();

            // peer 1 should see that peer 2 has all blocks for reward cycles 5 through 9
            match peer_1.network.inv_state {
                Some(ref inv) => {
                    inv_1_count = inv.get_inv_num_blocks(&peer_2.to_neighbor().addr);
                    peer_1_sorts = inv.get_inv_sortitions(&peer_2.to_neighbor().addr);
                }
                None => {}
            };

            // peer 2 should see that peer 1 has all blocks up to where we stopped feeding them to
            // it
            match peer_2.network.inv_state {
                Some(ref inv) => {
                    inv_2_count = inv.get_inv_num_blocks(&peer_1.to_neighbor().addr);
                    peer_2_sorts = inv.get_inv_sortitions(&peer_1.to_neighbor().addr);
                }
                None => {}
            };

            match peer_1.network.inv_state {
                Some(ref inv) => {
                    info!("Peer 1 stats: {:?}", &inv.block_stats);
                    assert_eq!(inv.get_broken_peers().len(), 0);
                    assert_eq!(inv.get_dead_peers().len(), 0);
                    assert_eq!(inv.get_diverged_peers().len(), 0);
                }
                None => {}
            }

            match peer_2.network.inv_state {
                Some(ref inv) => {
                    info!("Peer 2 stats: {:?}", &inv.block_stats);
                    assert_eq!(inv.get_broken_peers().len(), 0);
                    assert_eq!(inv.get_dead_peers().len(), 0);
                    assert_eq!(inv.get_diverged_peers().len(), 0);
                }
                None => {}
            }

            round += 1;

            test_debug!(
                "\n\ninv_1_count = {} <? {}, inv_2_count = {} <? {}, peer_1_sorts = {} <? {}, peer_2_sorts = {} <? {}",
                inv_1_count,
                reward_cycle_length * 4,
                inv_2_count,
                num_blocks - reward_cycle_length * 2,
                peer_1_sorts,
                reward_cycle_length * 9 + 1,
                peer_2_sorts,
                reward_cycle_length * 9 + 1
            );
        }

        info!("Completed walk round {} step(s)", round);

        peer_1.dump_frontier();
        peer_2.dump_frontier();

        let peer_1_pox_id = {
            let tip_sort_id =
                SortitionDB::get_canonical_sortition_tip(peer_1.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            let ic = peer_1.sortdb.as_ref().unwrap().index_conn();
            let sortdb_reader = SortitionHandleConn::open_reader(&ic, &tip_sort_id).unwrap();
            sortdb_reader.get_pox_id().unwrap()
        };

        let peer_2_pox_id = {
            let tip_sort_id =
                SortitionDB::get_canonical_sortition_tip(peer_2.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            let ic = peer_2.sortdb.as_ref().unwrap().index_conn();
            let sortdb_reader = SortitionHandleConn::open_reader(&ic, &tip_sort_id).unwrap();
            sortdb_reader.get_pox_id().unwrap()
        };

        let peer_2_inv = peer_1
            .network
            .inv_state
            .as_ref()
            .unwrap()
            .block_stats
            .get(&peer_2.to_neighbor().addr)
            .unwrap()
            .inv
            .clone();
        test_debug!("peer 1's view of peer 2: {:?}", &peer_2_inv);
        test_debug!("peer 1's PoX bit vector is {:?}", &peer_1_pox_id);

        let peer_1_inv = peer_2
            .network
            .inv_state
            .as_ref()
            .unwrap()
            .block_stats
            .get(&peer_1.to_neighbor().addr)
            .unwrap()
            .inv
            .clone();
        test_debug!("peer 2's view of peer 1: {:?}", &peer_1_inv);
        test_debug!("peer 2's PoX bit vector is {:?}", &peer_2_pox_id);

        // nodes only learn about the prefix of their PoX bit vectors that they agree on
        assert_eq!(peer_2_inv.num_sortitions, reward_cycle_length * 9 + 1);
        assert_eq!(peer_1_inv.num_sortitions, reward_cycle_length * 9 + 1);

        // only 9 reward cycles -- we couldn't agree on the 10th
        assert_eq!(peer_1_inv.pox_inv, vec![255, 1]);
        assert_eq!(peer_2_inv.pox_inv, vec![255, 1]);

        // peer 1 should have learned that peer 2 has all the blocks, up to the point of
        // PoX instability between the two
        for i in 0..(reward_cycle_length * 4) {
            assert!(peer_2_inv.has_ith_block(i + first_stacks_block_height));
            if i > 0 {
                assert!(peer_2_inv.has_ith_microblock_stream(i + first_stacks_block_height));
            } else {
                assert!(!peer_2_inv.has_ith_microblock_stream(i + first_stacks_block_height));
            }
        }

        // peer 2 should have learned about all of peer 1's blocks
        for i in 0..(num_blocks - 2 * reward_cycle_length) {
            assert!(peer_1_inv.has_ith_block(i + first_stacks_block_height));
            if i > 0 && i != num_blocks - 2 * reward_cycle_length - 1 {
                // peer 1 doesn't have the final microblock stream, since no anchor block confirmed it
                assert!(peer_1_inv.has_ith_microblock_stream(i + first_stacks_block_height));
            }
        }

        assert!(!peer_1_inv.has_ith_block(reward_cycle_length * 4));
        assert!(!peer_1_inv.has_ith_microblock_stream(reward_cycle_length * 4));

        assert!(!peer_2_inv.has_ith_block(num_blocks - 2 * reward_cycle_length));
        assert!(!peer_2_inv.has_ith_microblock_stream(num_blocks - 2 * reward_cycle_length));
    })
}
