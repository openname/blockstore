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

use std::fs;

use clarity::vm::types::QualifiedContractIdentifier;
use clarity::vm::ContractName;
use libstackerdb::SlotMetadata;
use rand::prelude::*;
use rand::seq::SliceRandom;
use rand::{thread_rng, Rng, RngCore};
use stacks_common::address::{
    AddressHashMode, C32_ADDRESS_VERSION_MAINNET_MULTISIG, C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
};
use stacks_common::types::chainstate::{
    BlockHeaderHash, ConsensusHash, StacksAddress, StacksPrivateKey, StacksPublicKey,
};
use stacks_common::util::hash::{Hash160, Sha512Trunc256Sum};
use stacks_common::util::secp256k1::{MessageSignature, Secp256k1PrivateKey};

use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::stacks::db::StacksChainState;
use crate::net::p2p::PeerNetwork;
use crate::net::relay::Relayer;
use crate::net::stackerdb::db::SlotValidation;
use crate::net::stackerdb::{StackerDBConfig, StackerDBs};
use crate::net::test::{TestPeer, TestPeerConfig};
use crate::net::{Error as net_error, NetworkResult, StackerDBChunkData};
use crate::util_lib::test::with_timeout;

const BASE_PORT: u16 = 33000;

// Minimum chunk size for FROST is 97 + T * 33, where T = 3000
const CHUNK_SIZE: u64 = 2 * (97 + 3000 * 33);

// Number of neighbors to test with
const NUM_NEIGHBORS: usize = 8;

/// Some testable configurations for stacker DB configs
impl StackerDBConfig {
    #[cfg(test)]
    pub fn template() -> StackerDBConfig {
        StackerDBConfig {
            chunk_size: CHUNK_SIZE,
            write_freq: 0,
            max_writes: u32::MAX,
            hint_replicas: vec![],
            max_neighbors: NUM_NEIGHBORS,
            signers: vec![], // to be filled in
        }
    }
}

/// Add a stacker DB to a node's config.
/// Return its index into the list of configured stacker DBs (can be used as `idx` in the call to
/// `setup_stackerdb()`
fn add_stackerdb(config: &mut TestPeerConfig, stackerdb_config: Option<StackerDBConfig>) -> usize {
    let name = ContractName::try_from(format!("db-{}", config.stacker_dbs.len())).unwrap();
    let addr = StacksAddress {
        version: C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
        bytes: Hash160::from_data(&config.stacker_dbs.len().to_be_bytes()),
    };

    let stackerdb_config = stackerdb_config.unwrap_or(StackerDBConfig::noop());

    config
        .stacker_dbs
        .push(QualifiedContractIdentifier::new(addr.into(), name));
    config.stacker_db_configs.push(Some(stackerdb_config));

    config.stacker_dbs.len() - 1
}

/// Set up a stacker DB and optionally fill it with random data.
/// `idx` refers to the `idx`th stacker DB in the node config struct.
fn setup_stackerdb(peer: &mut TestPeer, idx: usize, fill: bool, num_slots: usize) {
    let contract_id = &peer.config.stacker_dbs[idx];
    let rc_consensus_hash = &peer.network.get_chain_view().rc_consensus_hash;

    let mut noop = StackerDBConfig::noop();
    let stackerdb_config = peer.config.stacker_db_configs[idx]
        .as_mut()
        .unwrap_or(&mut noop);
    let chunk_size = stackerdb_config.chunk_size;

    let mut k: u64 = 0;
    let mut pks = vec![];
    let mut slots = vec![];
    for i in 0..num_slots {
        // deterministically generate private keys
        let pk = loop {
            let h = Sha512Trunc256Sum::from_data(&k.to_be_bytes());
            k += 1;

            if let Ok(pk) = Secp256k1PrivateKey::from_slice(&h.0) {
                break pk;
            }
        };
        let pubk = StacksPublicKey::from_private(&pk);
        let addr = StacksAddress {
            version: C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
            bytes: Hash160::from_node_public_key(&pubk),
        };

        pks.push(pk);
        slots.push((addr, 1u32));
    }

    stackerdb_config.signers = slots.clone();
    let tx = peer
        .network
        .stackerdbs
        .tx_begin(stackerdb_config.clone())
        .unwrap();

    tx.reconfigure_stackerdb(contract_id, &slots).unwrap();

    if fill {
        for i in 0..num_slots {
            // deterministically generate chunk data
            let mut inner_data = vec![0x00; chunk_size as usize];
            thread_rng().fill(&mut inner_data[..]);

            let mut chunk_data = StackerDBChunkData::new(i as u32, 1, inner_data);
            chunk_data.sign(&pks[i]).unwrap();

            let chunk_md = chunk_data.get_slot_metadata();
            tx.try_replace_chunk(contract_id, &chunk_md, &chunk_data.data)
                .unwrap();
        }
    }

    tx.commit().unwrap();

    // load the new slot data into the sync state machine
    peer.network
        .stacker_db_syncs
        .as_mut()
        .unwrap()
        .get_mut(contract_id)
        .unwrap()
        .reset(None, stackerdb_config);
}

/// Load up the entire stacker DB, including its metadata
fn load_stackerdb(peer: &TestPeer, idx: usize) -> Vec<(SlotMetadata, Vec<u8>)> {
    let num_slots = peer.config.stacker_db_configs[idx]
        .as_ref()
        .unwrap_or(&StackerDBConfig::noop())
        .num_slots();
    let rc_consensus_hash = &peer.network.get_chain_view().rc_consensus_hash;
    let mut ret = vec![];
    for i in 0..num_slots {
        let chunk_metadata = peer
            .network
            .stackerdbs
            .get_slot_metadata(&peer.config.stacker_dbs[idx], i)
            .unwrap()
            .unwrap();
        let chunk = peer
            .network
            .stackerdbs
            .get_latest_chunk(&peer.config.stacker_dbs[idx], i)
            .unwrap()
            .unwrap_or(vec![]);
        ret.push((chunk_metadata, chunk));
    }
    ret
}

fn check_sync_results(network_sync: &NetworkResult) {
    for res in network_sync.stacker_db_sync_results.iter() {
        assert!(
            res.num_connections <= res.num_attempted_connections,
            "{} < {}",
            res.num_connections,
            res.num_attempted_connections
        );
    }
}

fn test_reconnect(network: &mut PeerNetwork) {
    let mut stacker_db_syncs = network
        .stacker_db_syncs
        .take()
        .expect("FATAL: did not replace stacker dbs");

    for (_sc, stacker_db_sync) in stacker_db_syncs.iter_mut() {
        match stacker_db_sync.connect_begin(network) {
            Ok(x) => {}
            Err(net_error::PeerNotConnected) => {}
            Err(net_error::NoSuchNeighbor) => {}
            Err(e) => {
                panic!("Failed to connect_begin: {:?}", &e);
            }
        }
    }

    network.stacker_db_syncs = Some(stacker_db_syncs);
}

#[test]
fn test_stackerdb_replica_2_neighbors_1_chunk() {
    with_timeout(600, || {
        std::env::set_var("STACKS_TEST_DISABLE_EDGE_TRIGGER_TEST", "1");
        let mut peer_1_config = TestPeerConfig::from_port(BASE_PORT);
        let mut peer_2_config = TestPeerConfig::from_port(BASE_PORT + 2);

        peer_1_config.allowed = -1;
        peer_2_config.allowed = -1;

        // short-lived walks...
        peer_1_config.connection_opts.walk_max_duration = 10;
        peer_2_config.connection_opts.walk_max_duration = 10;

        // peer 1 crawls peer 2, and peer 2 crawls peer 1
        peer_1_config.add_neighbor(&peer_2_config.to_neighbor());
        peer_2_config.add_neighbor(&peer_1_config.to_neighbor());

        // set up stacker DBs for both peers
        let idx_1 = add_stackerdb(&mut peer_1_config, Some(StackerDBConfig::template()));
        let idx_2 = add_stackerdb(&mut peer_2_config, Some(StackerDBConfig::template()));

        let mut peer_1 = TestPeer::new(peer_1_config);
        let mut peer_2 = TestPeer::new(peer_2_config);

        // peer 1 gets the DB
        setup_stackerdb(&mut peer_1, idx_1, true, 1);
        setup_stackerdb(&mut peer_2, idx_2, false, 1);

        // verify that peer 1 got the data
        let peer_1_db_chunks = load_stackerdb(&peer_1, idx_1);
        assert_eq!(peer_1_db_chunks.len(), 1);
        assert_eq!(peer_1_db_chunks[0].0.slot_id, 0);
        assert_eq!(peer_1_db_chunks[0].0.slot_version, 1);
        assert!(!peer_1_db_chunks[0].1.is_empty());

        // verify that peer 2 did NOT get the data
        let peer_2_db_chunks = load_stackerdb(&peer_2, idx_2);
        assert_eq!(peer_2_db_chunks.len(), 1);
        assert_eq!(peer_2_db_chunks[0].0.slot_id, 0);
        assert_eq!(peer_2_db_chunks[0].0.slot_version, 0);
        assert!(peer_2_db_chunks[0].1.is_empty());

        let peer_1_db_configs = peer_1.config.get_stacker_db_configs();
        let peer_2_db_configs = peer_2.config.get_stacker_db_configs();

        let mut i = 0;
        loop {
            // run peer network state-machines
            peer_1.network.stacker_db_configs = peer_1_db_configs.clone();
            peer_2.network.stacker_db_configs = peer_2_db_configs.clone();

            let res_1 = peer_1.step_with_ibd(false);
            let res_2 = peer_2.step_with_ibd(false);

            // test that re-connects are limited to 1 per host
            test_reconnect(&mut peer_1.network);
            test_reconnect(&mut peer_2.network);

            if let Ok(res) = res_1 {
                check_sync_results(&res);
                peer_1
                    .relayer
                    .process_stacker_db_chunks(
                        &peer_1.network.get_chain_view().rc_consensus_hash,
                        &peer_1_db_configs,
                        res.stacker_db_sync_results,
                        None,
                    )
                    .unwrap();
                peer_1
                    .relayer
                    .process_pushed_stacker_db_chunks(
                        &peer_1.network.get_chain_view().rc_consensus_hash,
                        &peer_1_db_configs,
                        res.pushed_stackerdb_chunks,
                        None,
                    )
                    .unwrap();
            }

            if let Ok(res) = res_2 {
                check_sync_results(&res);
                peer_2
                    .relayer
                    .process_stacker_db_chunks(
                        &peer_2.network.get_chain_view().rc_consensus_hash,
                        &peer_2_db_configs,
                        res.stacker_db_sync_results,
                        None,
                    )
                    .unwrap();
                peer_2
                    .relayer
                    .process_pushed_stacker_db_chunks(
                        &peer_2.network.get_chain_view().rc_consensus_hash,
                        &peer_2_db_configs,
                        res.pushed_stackerdb_chunks,
                        None,
                    )
                    .unwrap();
            }

            let db1 = load_stackerdb(&peer_1, idx_1);
            let db2 = load_stackerdb(&peer_2, idx_2);

            if db1 == db2 {
                break;
            }
            i += 1;
        }

        debug!("Completed stacker DB sync in {} step(s)", i);
    })
}

#[test]
fn test_stackerdb_replica_2_neighbors_1_chunk_stale_view() {
    with_timeout(600, || {
        std::env::set_var("STACKS_TEST_DISABLE_EDGE_TRIGGER_TEST", "1");
        let mut peer_1_config = TestPeerConfig::from_port(BASE_PORT + 4);
        let mut peer_2_config = TestPeerConfig::from_port(BASE_PORT + 8);

        peer_1_config.allowed = -1;
        peer_2_config.allowed = -1;

        // short-lived walks...
        peer_1_config.connection_opts.walk_max_duration = 10;
        peer_2_config.connection_opts.walk_max_duration = 10;

        // peer 1 crawls peer 2, and peer 2 crawls peer 1
        peer_1_config.add_neighbor(&peer_2_config.to_neighbor());
        peer_2_config.add_neighbor(&peer_1_config.to_neighbor());

        // set up stacker DBs for both peers
        let idx_1 = add_stackerdb(&mut peer_1_config, Some(StackerDBConfig::template()));
        let idx_2 = add_stackerdb(&mut peer_2_config, Some(StackerDBConfig::template()));

        let mut peer_1 = TestPeer::new(peer_1_config);
        let mut peer_2 = TestPeer::new(peer_2_config);

        // peer 1 gets the DB
        setup_stackerdb(&mut peer_1, idx_1, true, 1);
        setup_stackerdb(&mut peer_2, idx_2, false, 1);

        // verify that peer 1 got the data
        let peer_1_db_chunks = load_stackerdb(&peer_1, idx_1);
        assert_eq!(peer_1_db_chunks.len(), 1);
        assert_eq!(peer_1_db_chunks[0].0.slot_id, 0);
        assert_eq!(peer_1_db_chunks[0].0.slot_version, 1);
        assert!(!peer_1_db_chunks[0].1.is_empty());

        // verify that peer 2 did NOT get the data
        let peer_2_db_chunks = load_stackerdb(&peer_2, idx_2);
        assert_eq!(peer_2_db_chunks.len(), 1);
        assert_eq!(peer_2_db_chunks[0].0.slot_id, 0);
        assert_eq!(peer_2_db_chunks[0].0.slot_version, 0);
        assert!(peer_2_db_chunks[0].1.is_empty());

        let peer_1_db_configs = peer_1.config.get_stacker_db_configs();
        let peer_2_db_configs = peer_2.config.get_stacker_db_configs();

        // force peer 2 to have a stale view
        let (old_tip_ch, old_tip_bh) = {
            let sortdb = peer_1.sortdb();
            let (tip_bh, tip_ch) =
                SortitionDB::get_canonical_stacks_chain_tip_hash(sortdb.conn()).unwrap();
            SortitionDB::set_canonical_stacks_chain_tip(
                sortdb.conn(),
                &ConsensusHash([0x22; 20]),
                &BlockHeaderHash([0x33; 32]),
                45,
            )
            .unwrap();
            (tip_bh, tip_ch)
        };

        let mut i = 0;
        let mut peer_1_stale = false;
        let mut peer_2_stale = false;
        loop {
            // run peer network state-machines
            peer_1.network.stacker_db_configs = peer_1_db_configs.clone();
            peer_2.network.stacker_db_configs = peer_2_db_configs.clone();

            let res_1 = peer_1.step_with_ibd(false);
            let res_2 = peer_2.step_with_ibd(false);

            if let Ok(res) = res_1 {
                check_sync_results(&res);
                for sync_res in res.stacker_db_sync_results.iter() {
                    assert_eq!(sync_res.chunks_to_store.len(), 0);
                    if !sync_res.stale.is_empty() {
                        peer_1_stale = true;
                    }
                }

                peer_1
                    .relayer
                    .process_stacker_db_chunks(
                        &peer_1.network.get_chain_view().rc_consensus_hash,
                        &peer_1_db_configs,
                        res.stacker_db_sync_results,
                        None,
                    )
                    .unwrap();
                peer_1
                    .relayer
                    .process_pushed_stacker_db_chunks(
                        &peer_1.network.get_chain_view().rc_consensus_hash,
                        &peer_1_db_configs,
                        res.pushed_stackerdb_chunks,
                        None,
                    )
                    .unwrap();
            }

            if let Ok(res) = res_2 {
                check_sync_results(&res);
                for sync_res in res.stacker_db_sync_results.iter() {
                    assert_eq!(sync_res.chunks_to_store.len(), 0);
                    if !sync_res.stale.is_empty() {
                        peer_2_stale = true;
                    }
                }
                peer_2
                    .relayer
                    .process_stacker_db_chunks(
                        &peer_2.network.get_chain_view().rc_consensus_hash,
                        &peer_2_db_configs,
                        res.stacker_db_sync_results,
                        None,
                    )
                    .unwrap();
                peer_2
                    .relayer
                    .process_pushed_stacker_db_chunks(
                        &peer_2.network.get_chain_view().rc_consensus_hash,
                        &peer_2_db_configs,
                        res.pushed_stackerdb_chunks,
                        None,
                    )
                    .unwrap();
            }

            if peer_1_stale && peer_2_stale {
                break;
            }

            i += 1;
        }

        debug!("Completed stacker DB stale detection in {} step(s)", i);

        // fix and re-run
        {
            let sortdb = peer_1.sortdb();
            SortitionDB::set_canonical_stacks_chain_tip(sortdb.conn(), &old_tip_ch, &old_tip_bh, 0)
                .unwrap();

            // force chain view refresh
            peer_1.network.num_state_machine_passes = 0;
        }

        let mut i = 0;
        loop {
            // run peer network state-machines
            peer_1.network.stacker_db_configs = peer_1_db_configs.clone();
            peer_2.network.stacker_db_configs = peer_2_db_configs.clone();

            let res_1 = peer_1.step_with_ibd(false);
            let res_2 = peer_2.step_with_ibd(false);

            if let Ok(res) = res_1 {
                check_sync_results(&res);
                peer_1
                    .relayer
                    .process_stacker_db_chunks(
                        &peer_1.network.get_chain_view().rc_consensus_hash,
                        &peer_1_db_configs,
                        res.stacker_db_sync_results,
                        None,
                    )
                    .unwrap();
                peer_1
                    .relayer
                    .process_pushed_stacker_db_chunks(
                        &peer_1.network.get_chain_view().rc_consensus_hash,
                        &peer_1_db_configs,
                        res.pushed_stackerdb_chunks,
                        None,
                    )
                    .unwrap();
            }

            if let Ok(res) = res_2 {
                check_sync_results(&res);
                peer_2
                    .relayer
                    .process_stacker_db_chunks(
                        &peer_2.network.get_chain_view().rc_consensus_hash,
                        &peer_2_db_configs,
                        res.stacker_db_sync_results,
                        None,
                    )
                    .unwrap();
                peer_2
                    .relayer
                    .process_pushed_stacker_db_chunks(
                        &peer_2.network.get_chain_view().rc_consensus_hash,
                        &peer_2_db_configs,
                        res.pushed_stackerdb_chunks,
                        None,
                    )
                    .unwrap();
            }

            let db1 = load_stackerdb(&peer_1, idx_1);
            let db2 = load_stackerdb(&peer_2, idx_2);

            if db1 == db2 {
                break;
            }
            i += 1;
        }

        debug!("Completed stacker DB sync in {} step(s)", i);
    })
}

#[test]
#[ignore]
fn test_stackerdb_replica_2_neighbors_10_chunks() {
    inner_test_stackerdb_replica_2_neighbors_10_chunks(false, BASE_PORT + 10);
}

#[test]
#[ignore]
fn test_stackerdb_replica_2_neighbors_10_push_chunks() {
    inner_test_stackerdb_replica_2_neighbors_10_chunks(true, BASE_PORT + 30);
}

fn inner_test_stackerdb_replica_2_neighbors_10_chunks(push_only: bool, base_port: u16) {
    with_timeout(600, move || {
        std::env::set_var("STACKS_TEST_DISABLE_EDGE_TRIGGER_TEST", "1");
        let mut peer_1_config = TestPeerConfig::from_port(base_port);
        let mut peer_2_config = TestPeerConfig::from_port(base_port + 2);

        peer_1_config.allowed = -1;
        peer_2_config.allowed = -1;

        // short-lived walks...
        peer_1_config.connection_opts.walk_max_duration = 10;
        peer_2_config.connection_opts.walk_max_duration = 10;

        if push_only {
            peer_1_config.connection_opts.disable_stackerdb_get_chunks = true;
            peer_2_config.connection_opts.disable_stackerdb_get_chunks = true;
        }

        // peer 1 crawls peer 2, and peer 2 crawls peer 1
        peer_1_config.add_neighbor(&peer_2_config.to_neighbor());
        peer_2_config.add_neighbor(&peer_1_config.to_neighbor());

        // set up stacker DBs for both peers
        let idx_1 = add_stackerdb(&mut peer_1_config, Some(StackerDBConfig::template()));
        let idx_2 = add_stackerdb(&mut peer_2_config, Some(StackerDBConfig::template()));

        let mut peer_1 = TestPeer::new(peer_1_config);
        let mut peer_2 = TestPeer::new(peer_2_config);

        // peer 1 gets the DB
        setup_stackerdb(&mut peer_1, idx_1, true, 10);
        setup_stackerdb(&mut peer_2, idx_2, false, 10);

        // verify that peer 1 got the data
        let peer_1_db_chunks = load_stackerdb(&peer_1, idx_1);
        assert_eq!(peer_1_db_chunks.len(), 10);
        for i in 0..10 {
            assert_eq!(peer_1_db_chunks[i].0.slot_id, i as u32);
            assert_eq!(peer_1_db_chunks[i].0.slot_version, 1);
            assert!(!peer_1_db_chunks[i].1.is_empty());
        }

        // verify that peer 2 did NOT get the data
        let peer_2_db_chunks = load_stackerdb(&peer_2, idx_2);
        assert_eq!(peer_2_db_chunks.len(), 10);
        for i in 0..10 {
            assert_eq!(peer_2_db_chunks[i].0.slot_id, i as u32);
            assert_eq!(peer_2_db_chunks[i].0.slot_version, 0);
            assert!(peer_2_db_chunks[i].1.is_empty());
        }

        let peer_1_db_configs = peer_1.config.get_stacker_db_configs();
        let peer_2_db_configs = peer_2.config.get_stacker_db_configs();

        let mut i = 0;
        loop {
            // run peer network state-machines
            peer_1.network.stacker_db_configs = peer_1_db_configs.clone();
            peer_2.network.stacker_db_configs = peer_2_db_configs.clone();

            let res_1 = peer_1.step_with_ibd(false);
            let res_2 = peer_2.step_with_ibd(false);

            if let Ok(res) = res_1 {
                check_sync_results(&res);
                peer_1
                    .relayer
                    .process_stacker_db_chunks(
                        &peer_1.network.get_chain_view().rc_consensus_hash,
                        &peer_1_db_configs,
                        res.stacker_db_sync_results,
                        None,
                    )
                    .unwrap();
                peer_1
                    .relayer
                    .process_pushed_stacker_db_chunks(
                        &peer_1.network.get_chain_view().rc_consensus_hash,
                        &peer_1_db_configs,
                        res.pushed_stackerdb_chunks,
                        None,
                    )
                    .unwrap();
            }

            if let Ok(res) = res_2 {
                check_sync_results(&res);
                peer_2
                    .relayer
                    .process_stacker_db_chunks(
                        &peer_2.network.get_chain_view().rc_consensus_hash,
                        &peer_2_db_configs,
                        res.stacker_db_sync_results,
                        None,
                    )
                    .unwrap();
                peer_2
                    .relayer
                    .process_pushed_stacker_db_chunks(
                        &peer_2.network.get_chain_view().rc_consensus_hash,
                        &peer_2_db_configs,
                        res.pushed_stackerdb_chunks,
                        None,
                    )
                    .unwrap();
            }

            let db1 = load_stackerdb(&peer_1, idx_1);
            let db2 = load_stackerdb(&peer_2, idx_2);

            if db1 == db2 {
                break;
            }
            i += 1;

            debug!("StackerDB sync step {}", i);
        }

        debug!("Completed stacker DB sync in {} step(s)", i);
    })
}

/// Verify that the relayer will push stackerdb chunks.
/// Replica A has the data.
/// Replica B receives the data via StackerDB sync
/// Replica C receives the data from B's relayer pushes
#[test]
fn test_stackerdb_push_relayer() {
    with_timeout(600, move || {
        std::env::set_var("STACKS_TEST_DISABLE_EDGE_TRIGGER_TEST", "1");
        let mut peer_1_config = TestPeerConfig::from_port(BASE_PORT + 100);
        let mut peer_2_config = TestPeerConfig::from_port(BASE_PORT + 102);
        let mut peer_3_config = TestPeerConfig::from_port(BASE_PORT + 104);

        peer_1_config.allowed = -1;
        peer_2_config.allowed = -1;
        peer_3_config.allowed = -1;

        // short-lived walks...
        peer_1_config.connection_opts.walk_max_duration = 10;
        peer_2_config.connection_opts.walk_max_duration = 10;
        peer_3_config.connection_opts.walk_max_duration = 10;

        peer_3_config.connection_opts.disable_stackerdb_sync = true;

        // peer 1 crawls peer 2, and peer 2 crawls peer 1 and peer 3, and peer 3 crawls peer 2
        peer_1_config.add_neighbor(&peer_2_config.to_neighbor());
        peer_2_config.add_neighbor(&peer_1_config.to_neighbor());
        peer_2_config.add_neighbor(&peer_3_config.to_neighbor());
        peer_3_config.add_neighbor(&peer_2_config.to_neighbor());

        // set up stacker DBs for both peers
        let idx_1 = add_stackerdb(&mut peer_1_config, Some(StackerDBConfig::template()));
        let idx_2 = add_stackerdb(&mut peer_2_config, Some(StackerDBConfig::template()));
        let idx_3 = add_stackerdb(&mut peer_3_config, Some(StackerDBConfig::template()));

        let mut peer_1 = TestPeer::new(peer_1_config);
        let mut peer_2 = TestPeer::new(peer_2_config);
        let mut peer_3 = TestPeer::new(peer_3_config);

        // peer 1 gets the DB
        setup_stackerdb(&mut peer_1, idx_1, true, 10);
        setup_stackerdb(&mut peer_2, idx_2, false, 10);
        setup_stackerdb(&mut peer_3, idx_2, false, 10);

        // verify that peer 1 got the data
        let peer_1_db_chunks = load_stackerdb(&peer_1, idx_1);
        assert_eq!(peer_1_db_chunks.len(), 10);
        for i in 0..10 {
            assert_eq!(peer_1_db_chunks[i].0.slot_id, i as u32);
            assert_eq!(peer_1_db_chunks[i].0.slot_version, 1);
            assert!(!peer_1_db_chunks[i].1.is_empty());
        }

        // verify that peer 2 and 3 did NOT get the data
        let peer_2_db_chunks = load_stackerdb(&peer_2, idx_2);
        assert_eq!(peer_2_db_chunks.len(), 10);
        for i in 0..10 {
            assert_eq!(peer_2_db_chunks[i].0.slot_id, i as u32);
            assert_eq!(peer_2_db_chunks[i].0.slot_version, 0);
            assert!(peer_2_db_chunks[i].1.is_empty());
        }

        let peer_3_db_chunks = load_stackerdb(&peer_3, idx_2);
        assert_eq!(peer_3_db_chunks.len(), 10);
        for i in 0..10 {
            assert_eq!(peer_3_db_chunks[i].0.slot_id, i as u32);
            assert_eq!(peer_3_db_chunks[i].0.slot_version, 0);
            assert!(peer_3_db_chunks[i].1.is_empty());
        }

        let peer_1_db_configs = peer_1.config.get_stacker_db_configs();
        let peer_2_db_configs = peer_2.config.get_stacker_db_configs();
        let peer_3_db_configs = peer_3.config.get_stacker_db_configs();

        let mut i = 0;
        loop {
            // run peer network state-machines
            peer_1.network.stacker_db_configs = peer_1_db_configs.clone();
            peer_2.network.stacker_db_configs = peer_2_db_configs.clone();
            peer_3.network.stacker_db_configs = peer_3_db_configs.clone();

            let res_1 = peer_1.step_with_ibd(false);
            let res_2 = peer_2.step_with_ibd(false);
            let res_3 = peer_3.step_with_ibd(false);

            if let Ok(res) = res_1 {
                check_sync_results(&res);
                peer_1
                    .relayer
                    .process_stacker_db_chunks(
                        &peer_1.network.get_chain_view().rc_consensus_hash,
                        &peer_1_db_configs,
                        res.stacker_db_sync_results,
                        None,
                    )
                    .unwrap();
                peer_1
                    .relayer
                    .process_pushed_stacker_db_chunks(
                        &peer_1.network.get_chain_view().rc_consensus_hash,
                        &peer_1_db_configs,
                        res.pushed_stackerdb_chunks,
                        None,
                    )
                    .unwrap();
            }

            if let Ok(res) = res_2 {
                check_sync_results(&res);
                peer_2
                    .relayer
                    .process_stacker_db_chunks(
                        &peer_2.network.get_chain_view().rc_consensus_hash,
                        &peer_2_db_configs,
                        res.stacker_db_sync_results,
                        None,
                    )
                    .unwrap();
                peer_2
                    .relayer
                    .process_pushed_stacker_db_chunks(
                        &peer_2.network.get_chain_view().rc_consensus_hash,
                        &peer_2_db_configs,
                        res.pushed_stackerdb_chunks,
                        None,
                    )
                    .unwrap();
            }

            if let Ok(res) = res_3 {
                check_sync_results(&res);
                peer_3
                    .relayer
                    .process_stacker_db_chunks(
                        &peer_3.network.get_chain_view().rc_consensus_hash,
                        &peer_3_db_configs,
                        res.stacker_db_sync_results,
                        None,
                    )
                    .unwrap();
                peer_3
                    .relayer
                    .process_pushed_stacker_db_chunks(
                        &peer_3.network.get_chain_view().rc_consensus_hash,
                        &peer_3_db_configs,
                        res.pushed_stackerdb_chunks,
                        None,
                    )
                    .unwrap();
            }

            let db1 = load_stackerdb(&peer_1, idx_1);
            let db2 = load_stackerdb(&peer_2, idx_2);
            let db3 = load_stackerdb(&peer_3, idx_3);

            if db1 == db2 && db2 == db3 {
                break;
            }
            i += 1;

            debug!("StackerDB sync step {}", i);
        }

        debug!("Completed stacker DB sync in {} step(s)", i);
    })
}

/// Verify that the relayer will push stackerdb chunks, AND, those chunks will get buffered if the
/// recipient has not yet processed the sortition.
/// Replica A has the data.
/// Replica B receives the data via StackerDB sync
/// Replica C receives the data from B's relayer pushes, but is not yet at the Stacks tip that A
/// and B are on.
/// Replica C processes them all when the Stacks tip advances
#[test]
fn test_stackerdb_push_relayer_late_chunks() {
    with_timeout(600, move || {
        std::env::set_var("STACKS_TEST_DISABLE_EDGE_TRIGGER_TEST", "1");
        let mut peer_1_config = TestPeerConfig::from_port(BASE_PORT + 106);
        let mut peer_2_config = TestPeerConfig::from_port(BASE_PORT + 108);
        let mut peer_3_config = TestPeerConfig::from_port(BASE_PORT + 110);

        peer_1_config.allowed = -1;
        peer_2_config.allowed = -1;
        peer_3_config.allowed = -1;

        // short-lived walks...
        peer_1_config.connection_opts.walk_max_duration = 10;
        peer_2_config.connection_opts.walk_max_duration = 10;
        peer_3_config.connection_opts.walk_max_duration = 10;

        peer_3_config.connection_opts.disable_stackerdb_sync = true;

        // peer 1 crawls peer 2, and peer 2 crawls peer 1 and peer 3, and peer 3 crawls peer 2
        peer_1_config.add_neighbor(&peer_2_config.to_neighbor());
        peer_2_config.add_neighbor(&peer_1_config.to_neighbor());
        peer_2_config.add_neighbor(&peer_3_config.to_neighbor());
        peer_3_config.add_neighbor(&peer_2_config.to_neighbor());

        // set up stacker DBs for all peers
        let idx_1 = add_stackerdb(&mut peer_1_config, Some(StackerDBConfig::template()));
        let idx_2 = add_stackerdb(&mut peer_2_config, Some(StackerDBConfig::template()));
        let idx_3 = add_stackerdb(&mut peer_3_config, Some(StackerDBConfig::template()));

        let mut peer_1 = TestPeer::new(peer_1_config);
        let mut peer_2 = TestPeer::new(peer_2_config);
        let mut peer_3 = TestPeer::new(peer_3_config);

        // advance peers 1 and 2, but not 3
        let mut peer_1_nonce = 0;
        let mut peer_2_nonce = 0;
        let mut peer_3_nonce = 0;
        peer_1.tenure_with_txs(&vec![], &mut peer_1_nonce);
        peer_2.tenure_with_txs(&vec![], &mut peer_2_nonce);

        // sanity check -- peer 1 and 2 are at the same tip, but not 3
        let sn1 = SortitionDB::get_canonical_burn_chain_tip(peer_1.sortdb().conn()).unwrap();
        let sn2 = SortitionDB::get_canonical_burn_chain_tip(peer_2.sortdb().conn()).unwrap();
        let sn3 = SortitionDB::get_canonical_burn_chain_tip(peer_3.sortdb().conn()).unwrap();
        assert_eq!(sn1.consensus_hash, sn2.consensus_hash);
        assert_eq!(sn1.block_height, sn2.block_height);

        assert_ne!(sn1.consensus_hash, sn3.consensus_hash);
        assert_ne!(sn2.consensus_hash, sn3.consensus_hash);
        assert!(sn3.block_height < sn1.block_height);
        assert!(sn3.block_height < sn2.block_height);

        let st1 = SortitionDB::get_canonical_stacks_chain_tip_hash(peer_1.sortdb().conn()).unwrap();
        let st2 = SortitionDB::get_canonical_stacks_chain_tip_hash(peer_2.sortdb().conn()).unwrap();
        let st3 = SortitionDB::get_canonical_stacks_chain_tip_hash(peer_3.sortdb().conn()).unwrap();

        assert_eq!(st1, st2);
        assert_ne!(st1, st3);
        assert_ne!(st2, st3);

        // peer 1 gets the DB
        setup_stackerdb(&mut peer_1, idx_1, true, 10);
        setup_stackerdb(&mut peer_2, idx_2, false, 10);
        setup_stackerdb(&mut peer_3, idx_2, false, 10);

        // verify that peer 1 got the data
        let peer_1_db_chunks = load_stackerdb(&peer_1, idx_1);
        assert_eq!(peer_1_db_chunks.len(), 10);
        for i in 0..10 {
            assert_eq!(peer_1_db_chunks[i].0.slot_id, i as u32);
            assert_eq!(peer_1_db_chunks[i].0.slot_version, 1);
            assert!(!peer_1_db_chunks[i].1.is_empty());
        }

        // verify that peer 2 and 3 did NOT get the data
        let peer_2_db_chunks = load_stackerdb(&peer_2, idx_2);
        assert_eq!(peer_2_db_chunks.len(), 10);
        for i in 0..10 {
            assert_eq!(peer_2_db_chunks[i].0.slot_id, i as u32);
            assert_eq!(peer_2_db_chunks[i].0.slot_version, 0);
            assert!(peer_2_db_chunks[i].1.is_empty());
        }

        let peer_3_db_chunks = load_stackerdb(&peer_3, idx_2);
        assert_eq!(peer_3_db_chunks.len(), 10);
        for i in 0..10 {
            assert_eq!(peer_3_db_chunks[i].0.slot_id, i as u32);
            assert_eq!(peer_3_db_chunks[i].0.slot_version, 0);
            assert!(peer_3_db_chunks[i].1.is_empty());
        }

        let peer_1_db_configs = peer_1.config.get_stacker_db_configs();
        let peer_2_db_configs = peer_2.config.get_stacker_db_configs();
        let peer_3_db_configs = peer_3.config.get_stacker_db_configs();

        let mut i = 0;
        let mut advanced_tenure = false;
        loop {
            // run peer network state-machines
            peer_1.network.stacker_db_configs = peer_1_db_configs.clone();
            peer_2.network.stacker_db_configs = peer_2_db_configs.clone();
            peer_3.network.stacker_db_configs = peer_3_db_configs.clone();

            let res_1 = peer_1.step_with_ibd(false);
            let res_2 = peer_2.step_with_ibd(false);
            let res_3 = peer_3.step_with_ibd(false);

            if let Ok(res) = res_1 {
                check_sync_results(&res);
                peer_1
                    .relayer
                    .process_stacker_db_chunks(
                        &peer_1.network.get_chain_view().rc_consensus_hash,
                        &peer_1_db_configs,
                        res.stacker_db_sync_results,
                        None,
                    )
                    .unwrap();
                peer_1
                    .relayer
                    .process_pushed_stacker_db_chunks(
                        &peer_1.network.get_chain_view().rc_consensus_hash,
                        &peer_1_db_configs,
                        res.pushed_stackerdb_chunks,
                        None,
                    )
                    .unwrap();
            }

            if let Ok(res) = res_2 {
                check_sync_results(&res);
                peer_2
                    .relayer
                    .process_stacker_db_chunks(
                        &peer_2.network.get_chain_view().rc_consensus_hash,
                        &peer_2_db_configs,
                        res.stacker_db_sync_results,
                        None,
                    )
                    .unwrap();
                peer_2
                    .relayer
                    .process_pushed_stacker_db_chunks(
                        &peer_2.network.get_chain_view().rc_consensus_hash,
                        &peer_2_db_configs,
                        res.pushed_stackerdb_chunks,
                        None,
                    )
                    .unwrap();
            }

            if let Ok(res) = res_3 {
                check_sync_results(&res);
                peer_3
                    .relayer
                    .process_stacker_db_chunks(
                        &peer_3.network.get_chain_view().rc_consensus_hash,
                        &peer_3_db_configs,
                        res.stacker_db_sync_results,
                        None,
                    )
                    .unwrap();
                peer_3
                    .relayer
                    .process_pushed_stacker_db_chunks(
                        &peer_3.network.get_chain_view().rc_consensus_hash,
                        &peer_3_db_configs,
                        res.pushed_stackerdb_chunks,
                        None,
                    )
                    .unwrap();
            }

            let db1 = load_stackerdb(&peer_1, idx_1);
            let db2 = load_stackerdb(&peer_2, idx_2);
            let db3 = load_stackerdb(&peer_3, idx_3);

            if db1 == db2 && db2 == db3 {
                break;
            }
            i += 1;

            debug!("StackerDB sync step {}", i);

            let num_pending = peer_3
                .network
                .pending_stacks_messages
                .iter()
                .fold(0, |acc, (_, msgs)| acc + msgs.len());
            debug!("peer_3.network.pending_stacks_messages: {}", num_pending);

            if num_pending >= 10 && !advanced_tenure {
                debug!("======= Advancing peer 3 tenure ========");
                peer_3.tenure_with_txs(&vec![], &mut peer_3_nonce);
                advanced_tenure = true;
            }
        }

        debug!("Completed stacker DB sync in {} step(s)", i);
    })
}

#[test]
#[ignore]
fn test_stackerdb_10_replicas_10_neighbors_line_10_chunks() {
    inner_test_stackerdb_10_replicas_10_neighbors_line_10_chunks(false, BASE_PORT + 50);
}

#[test]
#[ignore]
fn test_stackerdb_10_replicas_10_neighbors_line_push_10_chunks() {
    inner_test_stackerdb_10_replicas_10_neighbors_line_10_chunks(true, BASE_PORT + 70);
}

fn inner_test_stackerdb_10_replicas_10_neighbors_line_10_chunks(push_only: bool, base_port: u16) {
    with_timeout(600, move || {
        std::env::set_var("STACKS_TEST_DISABLE_EDGE_TRIGGER_TEST", "1");
        let num_peers: usize = 10;
        let mut peer_configs = vec![];
        let mut peer_db_idxs = vec![];
        let mut peers = vec![];
        let mut peer_db_configs = vec![];

        for i in 0..num_peers {
            let mut peer_config = TestPeerConfig::from_port(base_port + (2 * i as u16));

            peer_config.allowed = -1;

            if push_only {
                peer_config.connection_opts.disable_stackerdb_get_chunks = true;
            }

            // run up against pruner limits
            peer_config.connection_opts.disable_network_prune = false;
            peer_config.connection_opts.num_neighbors = 5;
            peer_config.connection_opts.num_clients = 5;
            peer_config.connection_opts.soft_num_neighbors = 5;
            peer_config.connection_opts.soft_num_clients = 5;
            peer_config.connection_opts.max_neighbors_per_host = 5;
            peer_config.connection_opts.max_clients_per_host = 5;
            peer_config.connection_opts.soft_max_neighbors_per_host = 5;
            peer_config.connection_opts.soft_max_neighbors_per_org = 5;
            peer_config.connection_opts.soft_max_clients_per_host = 5;
            peer_config.connection_opts.max_neighbors_of_neighbor = 5;

            // short-lived walks...
            peer_config.connection_opts.walk_max_duration = 10;
            let idx = add_stackerdb(&mut peer_config, Some(StackerDBConfig::template()));

            peer_configs.push(peer_config);
            peer_db_idxs.push(idx);
        }

        // line topology: neighbor N connects to neighbors N-1 and N+1
        for i in 1..(num_peers - 1) {
            let n1 = peer_configs[i - 1].to_neighbor();
            let n2 = peer_configs[i + 1].to_neighbor();
            peer_configs[i].add_neighbor(&n1);
            peer_configs[i].add_neighbor(&n2);
        }

        for (i, peer_config) in peer_configs.into_iter().enumerate() {
            let mut peer = TestPeer::new(peer_config);

            if i == 0 {
                // peer 0 -- at one end of the line -- gets the initial DB
                setup_stackerdb(&mut peer, peer_db_idxs[i], true, 10);

                // verify instantiation
                let peer_db_chunks = load_stackerdb(&peer, peer_db_idxs[i]);
                assert_eq!(peer_db_chunks.len(), 10);
                for j in 0..10 {
                    assert_eq!(peer_db_chunks[j].0.slot_id, j as u32);
                    assert_eq!(peer_db_chunks[j].0.slot_version, 1);
                    assert!(!peer_db_chunks[j].1.is_empty());
                }
            } else {
                // everyone else gets nothing
                setup_stackerdb(&mut peer, peer_db_idxs[i], false, 10);

                // verify instantiation
                let peer_db_chunks = load_stackerdb(&peer, peer_db_idxs[i]);
                assert_eq!(peer_db_chunks.len(), 10);
                for j in 0..10 {
                    assert_eq!(peer_db_chunks[j].0.slot_id, j as u32);
                    assert_eq!(peer_db_chunks[j].0.slot_version, 0);
                    assert!(peer_db_chunks[j].1.is_empty());
                }
            }

            peers.push(peer);
        }

        for (i, peer) in peers.iter().enumerate() {
            let peer_db_config = peer.config.get_stacker_db_configs();
            peer_db_configs.push(peer_db_config);
        }
        let mut step_count = 0;
        loop {
            // run peer network state-machines
            for i in 0..num_peers {
                peers[i].network.stacker_db_configs = peer_db_configs[i].clone();
                let res = peers[i].step_with_ibd(false);

                // force this to run
                peers[i].network.prune_connections();

                if let Ok(res) = res {
                    check_sync_results(&res);
                    let rc_ch = peers[i].network.get_chain_view().rc_consensus_hash.clone();
                    peers[i]
                        .relayer
                        .process_stacker_db_chunks(
                            &rc_ch,
                            &peer_db_configs[i],
                            res.stacker_db_sync_results,
                            None,
                        )
                        .unwrap();
                    peers[i]
                        .relayer
                        .process_pushed_stacker_db_chunks(
                            &rc_ch,
                            &peer_db_configs[i],
                            res.pushed_stackerdb_chunks,
                            None,
                        )
                        .unwrap();
                }
            }

            let mut db_state = None;
            let mut different = false;
            for i in 0..num_peers {
                let db = load_stackerdb(&peers[i], peer_db_idxs[i]);
                if let Some(cmp_db) = db_state.as_ref() {
                    if &db != cmp_db {
                        different = true;
                        break;
                    }
                } else {
                    db_state = Some(db);
                }
            }

            if !different {
                break;
            }
            step_count += 1;
        }

        debug!("Completed stacker DB sync in {} step(s)", step_count);
    })
}
