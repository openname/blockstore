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

use std::{env, thread};

use clarity::vm::types::QualifiedContractIdentifier;
use stacks::chainstate::stacks::StacksPrivateKey;
use stacks::config::{EventKeyType, InitialBalance};
use stacks::libstackerdb::{StackerDBChunkAckData, StackerDBChunkData};
use stacks_common::types::chainstate::StacksAddress;
use stacks_common::util::hash::Sha512Trunc256Sum;
use {reqwest, serde_json};

use super::bitcoin_regtest::BitcoinCoreController;
use crate::burnchains::BurnchainController;
use crate::tests::neon_integrations::{
    neon_integration_test_conf, next_block_and_wait, submit_tx, test_observer, wait_for_runloop,
};
use crate::tests::{make_contract_publish, to_addr};
use crate::{neon, BitcoinRegtestController};

fn post_stackerdb_chunk(
    http_origin: &str,
    stackerdb_contract_id: &QualifiedContractIdentifier,
    data: Vec<u8>,
    signer: &StacksPrivateKey,
    slot_id: u32,
    slot_version: u32,
) -> StackerDBChunkAckData {
    let mut chunk = StackerDBChunkData::new(slot_id, slot_version, data);
    chunk.sign(signer).unwrap();

    let chunk_body = serde_json::to_string(&chunk).unwrap();

    let client = reqwest::blocking::Client::new();
    let path = format!(
        "{http_origin}/v2/stackerdb/{}/{}/chunks",
        &StacksAddress::from(stackerdb_contract_id.issuer.clone()),
        stackerdb_contract_id.name
    );
    let res = client
        .post(&path)
        .header("Content-Type", "application/json")
        .body(chunk_body.as_bytes().to_vec())
        .send()
        .unwrap();
    if res.status().is_success() {
        let ack: StackerDBChunkAckData = res.json().unwrap();
        info!("Got stackerdb ack: {ack:?}");
        ack
    } else {
        eprintln!("StackerDB post error: {}", res.text().unwrap());
        panic!("");
    }
}

fn get_stackerdb_chunk(
    http_origin: &str,
    stackerdb_contract_id: &QualifiedContractIdentifier,
    slot_id: u32,
    slot_version: Option<u32>,
) -> Vec<u8> {
    let path = if let Some(version) = slot_version {
        format!(
            "{http_origin}/v2/stackerdb/{}/{}/{slot_id}/{version}",
            StacksAddress::from(stackerdb_contract_id.issuer.clone()),
            stackerdb_contract_id.name,
        )
    } else {
        format!(
            "{http_origin}/v2/stackerdb/{}/{}/{slot_id}",
            StacksAddress::from(stackerdb_contract_id.issuer.clone()),
            stackerdb_contract_id.name
        )
    };

    let client = reqwest::blocking::Client::new();
    let res = client.get(&path).send().unwrap();

    if res.status().is_success() {
        res.bytes().unwrap().to_vec()
    } else {
        eprintln!("Get chunk error: {}", res.text().unwrap());
        panic!("");
    }
}

#[test]
#[ignore]
fn test_stackerdb_load_store() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let (mut conf, _) = neon_integration_test_conf();
    test_observer::register_any(&mut conf);

    let privks = [
        // ST2DS4MSWSGJ3W9FBC6BVT0Y92S345HY8N3T6AV7R
        StacksPrivateKey::from_hex(
            "9f1f85a512a96a244e4c0d762788500687feb97481639572e3bffbd6860e6ab001",
        )
        .unwrap(),
        // STVN97YYA10MY5F6KQJHKNYJNM24C4A1AT39WRW
        StacksPrivateKey::from_hex(
            "94c319327cc5cd04da7147d32d836eb2e4c44f4db39aa5ede7314a761183d0c701",
        )
        .unwrap(),
    ];

    let stackerdb_contract = "
        ;; stacker DB
        (define-read-only (stackerdb-get-signer-slots)
            (ok (list
                {
                    signer: 'ST2DS4MSWSGJ3W9FBC6BVT0Y92S345HY8N3T6AV7R,
                    num-slots: u3
                }
                {
                    signer: 'STVN97YYA10MY5F6KQJHKNYJNM24C4A1AT39WRW,
                    num-slots: u3
                })))

        (define-read-only (stackerdb-get-config)
            (ok {
                chunk-size: u4096,
                write-freq: u0,
                max-writes: u4096,
                max-neighbors: u32,
                hint-replicas: (list )
            }))
    ";

    conf.initial_balances.append(&mut vec![
        InitialBalance {
            address: to_addr(&privks[0]).into(),
            amount: 10_000_000_000_000,
        },
        InitialBalance {
            address: to_addr(&privks[1]).into(),
            amount: 10_000_000_000_000,
        },
    ]);

    conf.node.stacker_dbs.push(QualifiedContractIdentifier::new(
        to_addr(&privks[0]).into(),
        "hello-world".into(),
    ));
    let contract_id = conf.node.stacker_dbs[0].clone();

    test_observer::spawn();

    let mut btcd_controller = BitcoinCoreController::new(conf.clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::new(conf.clone(), None);

    btc_regtest_controller.bootstrap_chain(201);

    eprintln!("Chain bootstrapped...");

    let mut run_loop = neon::RunLoop::new(conf.clone());
    let blocks_processed = run_loop.get_blocks_processed_arc();

    thread::spawn(move || run_loop.start(None, 0));

    // Give the run loop some time to start up!
    eprintln!("Wait for runloop...");
    wait_for_runloop(&blocks_processed);

    // First block wakes up the run loop.
    eprintln!("Mine first block...");
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // Second block will hold our VRF registration.
    eprintln!("Mine second block...");
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // Third block will be the first mined Stacks block.
    eprintln!("Mine third block...");
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let http_origin = format!("http://{}", &conf.node.rpc_bind);

    eprintln!("Send contract-publish...");
    let tx = make_contract_publish(
        &privks[0],
        0,
        10_000,
        conf.burnchain.chain_id,
        "hello-world",
        stackerdb_contract,
    );
    submit_tx(&http_origin, &tx);

    // mine it
    eprintln!("Mine it...");
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // write some chunks and read them back
    for i in 0..3 {
        let chunk_str = format!("Hello chunks {i}");
        let ack = post_stackerdb_chunk(
            &http_origin,
            &contract_id,
            chunk_str.as_bytes().to_vec(),
            &privks[0],
            0,
            i + 1,
        );
        debug!("ACK: {ack:?}");

        let data = get_stackerdb_chunk(&http_origin, &contract_id, 0, Some(i + 1));
        assert_eq!(data, chunk_str.as_bytes().to_vec());

        let data = get_stackerdb_chunk(&http_origin, &contract_id, 0, None);
        assert_eq!(data, chunk_str.as_bytes().to_vec());
    }
}

#[test]
#[ignore]
fn test_stackerdb_event_observer() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let (mut conf, _) = neon_integration_test_conf();
    test_observer::register(&mut conf, &[EventKeyType::StackerDBChunks]);

    let privks = [
        // ST2DS4MSWSGJ3W9FBC6BVT0Y92S345HY8N3T6AV7R
        StacksPrivateKey::from_hex(
            "9f1f85a512a96a244e4c0d762788500687feb97481639572e3bffbd6860e6ab001",
        )
        .unwrap(),
        // STVN97YYA10MY5F6KQJHKNYJNM24C4A1AT39WRW
        StacksPrivateKey::from_hex(
            "94c319327cc5cd04da7147d32d836eb2e4c44f4db39aa5ede7314a761183d0c701",
        )
        .unwrap(),
    ];

    let stackerdb_contract = "
        ;; stacker DB
        (define-read-only (stackerdb-get-signer-slots)
            (ok (list
                {
                    signer: 'ST2DS4MSWSGJ3W9FBC6BVT0Y92S345HY8N3T6AV7R,
                    num-slots: u3
                }
                {
                    signer: 'STVN97YYA10MY5F6KQJHKNYJNM24C4A1AT39WRW,
                    num-slots: u3
                })))

        (define-read-only (stackerdb-get-config)
            (ok {
                chunk-size: u4096,
                write-freq: u0,
                max-writes: u4096,
                max-neighbors: u32,
                hint-replicas: (list )
            }))
    ";

    conf.initial_balances.append(&mut vec![
        InitialBalance {
            address: to_addr(&privks[0]).into(),
            amount: 10_000_000_000_000,
        },
        InitialBalance {
            address: to_addr(&privks[1]).into(),
            amount: 10_000_000_000_000,
        },
    ]);

    conf.node.stacker_dbs.push(QualifiedContractIdentifier::new(
        to_addr(&privks[0]).into(),
        "hello-world".into(),
    ));
    let contract_id = conf.node.stacker_dbs[0].clone();

    test_observer::spawn();

    let mut btcd_controller = BitcoinCoreController::new(conf.clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::new(conf.clone(), None);

    btc_regtest_controller.bootstrap_chain(201);

    eprintln!("Chain bootstrapped...");

    let mut run_loop = neon::RunLoop::new(conf.clone());
    let blocks_processed = run_loop.get_blocks_processed_arc();

    thread::spawn(move || run_loop.start(None, 0));

    // Give the run loop some time to start up!
    eprintln!("Wait for runloop...");
    wait_for_runloop(&blocks_processed);

    // First block wakes up the run loop.
    eprintln!("Mine first block...");
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // Second block will hold our VRF registration.
    eprintln!("Mine second block...");
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // Third block will be the first mined Stacks block.
    eprintln!("Mine third block...");
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let http_origin = format!("http://{}", &conf.node.rpc_bind);

    eprintln!("Send contract-publish...");
    let tx = make_contract_publish(
        &privks[0],
        0,
        10_000,
        conf.burnchain.chain_id,
        "hello-world",
        stackerdb_contract,
    );
    submit_tx(&http_origin, &tx);

    // mine it
    eprintln!("Mine it...");
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // write some chunks and read them back
    for i in 0..6 {
        let slot_id = i as u32;
        let privk = &privks[i / 3];
        let chunk_str = format!("Hello chunks {i}");
        let ack = post_stackerdb_chunk(
            &http_origin,
            &contract_id,
            chunk_str.as_bytes().to_vec(),
            privk,
            slot_id,
            1,
        );
        debug!("ACK: {ack:?}");

        let data = get_stackerdb_chunk(&http_origin, &contract_id, slot_id, Some(1));
        assert_eq!(data, chunk_str.as_bytes().to_vec());

        let data = get_stackerdb_chunk(&http_origin, &contract_id, slot_id, None);
        assert_eq!(data, chunk_str.as_bytes().to_vec());
    }

    // get events, verifying that they're all for the same contract (i.e. this one)
    let stackerdb_events: Vec<_> = test_observer::get_stackerdb_chunks()
        .into_iter()
        .flat_map(|stackerdb_event| {
            assert_eq!(stackerdb_event.contract_id, contract_id);
            stackerdb_event.modified_slots
        })
        .collect();

    assert_eq!(stackerdb_events.len(), 6);
    for (i, event) in stackerdb_events.iter().enumerate() {
        // reported in order
        assert_eq!(i as u32, event.slot_id);
        assert_eq!(event.slot_version, 1);

        let expected_data = format!("Hello chunks {i}");
        let expected_hash = Sha512Trunc256Sum::from_data(expected_data.as_bytes());

        assert_eq!(event.data, expected_data.as_bytes().to_vec());
        assert_eq!(event.data_hash(), expected_hash);
    }
}
