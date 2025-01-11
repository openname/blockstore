// Copyright (C) 2023 Stacks Open Internet Foundation
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
use std::{env, thread};

use clarity::boot_util::boot_code_id;
use clarity::vm::types::PrincipalData;
use clarity::vm::{ClarityVersion, Value};
use stacks::burnchains::{Burnchain, PoxConstants};
use stacks::chainstate::burn::db::sortdb::SortitionDB;
use stacks::chainstate::stacks::address::PoxAddress;
use stacks::chainstate::stacks::boot::RawRewardSetEntry;
use stacks::chainstate::stacks::db::StacksChainState;
use stacks::chainstate::stacks::{Error, StacksTransaction, TransactionPayload};
use stacks::clarity_cli::vm_execute as execute;
use stacks::config::InitialBalance;
use stacks::core::{self, EpochList, StacksEpochId};
use stacks_common::address::{AddressHashMode, C32_ADDRESS_VERSION_TESTNET_SINGLESIG};
use stacks_common::consts::STACKS_EPOCH_MAX;
use stacks_common::types::chainstate::{StacksAddress, StacksBlockId, StacksPrivateKey};
use stacks_common::types::Address;
use stacks_common::util::hash::{bytes_to_hex, hex_bytes, Hash160};
use stacks_common::util::secp256k1::Secp256k1PublicKey;
use stacks_common::util::sleep_ms;

use crate::stacks_common::codec::StacksMessageCodec;
use crate::tests::bitcoin_regtest::BitcoinCoreController;
use crate::tests::neon_integrations::{
    get_account, get_chain_info, get_pox_info, neon_integration_test_conf, next_block_and_wait,
    submit_tx, test_observer, wait_for_runloop,
};
use crate::tests::{make_contract_call, to_addr};
use crate::{neon, BitcoinRegtestController, BurnchainController};

#[cfg(test)]
pub fn get_reward_set_entries_at_block(
    state: &mut StacksChainState,
    burnchain: &Burnchain,
    sortdb: &SortitionDB,
    block_id: &StacksBlockId,
    burn_block_height: u64,
) -> Result<Vec<RawRewardSetEntry>, Error> {
    state
        .get_reward_addresses(burnchain, sortdb, burn_block_height, block_id)
        .map(|mut addrs| {
            addrs.sort_by_key(|k| k.reward_address.bytes());
            addrs
        })
}

#[test]
#[ignore]
/// Verify the buggy stacks-increase behavior that was possible in PoX-2 does not crash the
/// node in Epoch 2.4
///
/// Verify that the transition to Epoch 2.4 occurs smoothly even if miners do not mine in the
/// same block as the PoX-3 activation height.
///
/// Verify the PoX-3 payouts get made to the expected recipients.
fn fix_to_pox_contract() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let reward_cycle_len = 10;
    let prepare_phase_len = 3;
    let epoch_2_05 = 215;
    let epoch_2_1 = 230;
    let v1_unlock_height = 231;
    let epoch_2_2 = 255; // two blocks before next prepare phase.
    let epoch_2_3 = 265;
    let epoch_2_4 = 280;
    let pox_3_activation_height = epoch_2_4;

    let stacked = 100_000_000_000 * (core::MICROSTACKS_PER_STACKS as u64);
    let increase_by = 10_000_000 * (core::MICROSTACKS_PER_STACKS as u64);

    let spender_sk = StacksPrivateKey::new();
    let spender_addr: PrincipalData = to_addr(&spender_sk).into();

    let spender_2_sk = StacksPrivateKey::new();
    let spender_2_addr: PrincipalData = to_addr(&spender_2_sk).into();

    let mut initial_balances = vec![];

    initial_balances.push(InitialBalance {
        address: spender_addr.clone(),
        amount: stacked + increase_by + 100_000,
    });

    initial_balances.push(InitialBalance {
        address: spender_2_addr.clone(),
        amount: stacked + 100_000,
    });

    let pox_pubkey_1 = Secp256k1PublicKey::from_hex(
        "02f006a09b59979e2cb8449f58076152af6b124aa29b948a3714b8d5f15aa94ede",
    )
    .unwrap();
    let pox_pubkey_hash_1 = bytes_to_hex(&Hash160::from_node_public_key(&pox_pubkey_1).to_bytes());

    let pox_pubkey_2 = Secp256k1PublicKey::from_hex(
        "03cd91307e16c10428dd0120d0a4d37f14d4e0097b3b2ea1651d7bd0fb109cd44b",
    )
    .unwrap();
    let pox_pubkey_hash_2 = bytes_to_hex(&Hash160::from_node_public_key(&pox_pubkey_2).to_bytes());

    let pox_pubkey_3 = Secp256k1PublicKey::from_hex(
        "0317782e663c77fb02ebf46a3720f41a70f5678ad185974a456d35848e275fe56b",
    )
    .unwrap();
    let pox_pubkey_hash_3 = bytes_to_hex(&Hash160::from_node_public_key(&pox_pubkey_3).to_bytes());

    let (mut conf, _) = neon_integration_test_conf();

    // we'll manually post a forked stream to the node
    conf.node.mine_microblocks = false;
    conf.burnchain.max_rbf = 1000000;
    conf.node.wait_time_for_microblocks = 0;
    conf.node.microblock_frequency = 1_000;
    conf.miner.first_attempt_time_ms = 2_000;
    conf.miner.subsequent_attempt_time_ms = 5_000;
    conf.node.wait_time_for_blocks = 1_000;
    conf.miner.wait_for_block_download = false;

    conf.miner.first_attempt_time_ms = i64::MAX as u64;
    conf.miner.subsequent_attempt_time_ms = i64::MAX as u64;

    test_observer::spawn();
    test_observer::register_any(&mut conf);
    conf.initial_balances.append(&mut initial_balances);

    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = epoch_2_05;
    epochs[StacksEpochId::Epoch2_05].start_height = epoch_2_05;
    epochs[StacksEpochId::Epoch2_05].end_height = epoch_2_1;
    epochs[StacksEpochId::Epoch21].start_height = epoch_2_1;
    epochs[StacksEpochId::Epoch21].end_height = epoch_2_2;
    epochs[StacksEpochId::Epoch22].start_height = epoch_2_2;
    epochs[StacksEpochId::Epoch22].end_height = epoch_2_3;
    epochs[StacksEpochId::Epoch23].start_height = epoch_2_3;
    epochs[StacksEpochId::Epoch23].end_height = epoch_2_4;
    epochs[StacksEpochId::Epoch24].start_height = epoch_2_4;
    epochs[StacksEpochId::Epoch24].end_height = STACKS_EPOCH_MAX;
    epochs.truncate_after(StacksEpochId::Epoch24);
    conf.burnchain.epochs = Some(epochs);

    let mut burnchain_config = Burnchain::regtest(&conf.get_burn_db_path());

    let pox_constants = PoxConstants::new(
        reward_cycle_len,
        prepare_phase_len,
        4 * prepare_phase_len / 5,
        5,
        15,
        u64::MAX - 2,
        u64::MAX - 1,
        v1_unlock_height as u32,
        epoch_2_2 as u32 + 1,
        u32::MAX,
        pox_3_activation_height as u32,
    );
    burnchain_config.pox_constants = pox_constants.clone();

    let mut btcd_controller = BitcoinCoreController::new(conf.clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::with_burnchain(
        conf.clone(),
        None,
        Some(burnchain_config.clone()),
        None,
    );
    let http_origin = format!("http://{}", &conf.node.rpc_bind);

    btc_regtest_controller.bootstrap_chain(201);

    eprintln!("Chain bootstrapped...");

    let mut run_loop = neon::RunLoop::new(conf.clone());
    let runloop_burnchain = burnchain_config.clone();

    let blocks_processed = run_loop.get_blocks_processed_arc();

    let channel = run_loop.get_coordinator_channel().unwrap();

    thread::spawn(move || run_loop.start(Some(runloop_burnchain), 0));

    // give the run loop some time to start up!
    wait_for_runloop(&blocks_processed);

    // first block wakes up the run loop
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // first block will hold our VRF registration
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // second block will be the first mined Stacks block
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // push us to block 205
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // stack right away
    let sort_height = channel.get_sortitions_processed();
    let pox_addr_tuple_1 = execute(
        &format!("{{ hashbytes: 0x{pox_pubkey_hash_1}, version: 0x00 }}"),
        ClarityVersion::Clarity2,
    )
    .unwrap()
    .unwrap();

    let pox_addr_tuple_3 = execute(
        &format!("{{ hashbytes: 0x{pox_pubkey_hash_3}, version: 0x00 }}"),
        ClarityVersion::Clarity2,
    )
    .unwrap()
    .unwrap();

    let tx = make_contract_call(
        &spender_sk,
        0,
        3000,
        conf.burnchain.chain_id,
        &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
        "pox",
        "stack-stx",
        &[
            Value::UInt(stacked.into()),
            pox_addr_tuple_1.clone(),
            Value::UInt(sort_height as u128),
            Value::UInt(12),
        ],
    );

    info!("Submit 2.05 stacking tx to {http_origin:?}");
    submit_tx(&http_origin, &tx);

    // wait until just before epoch 2.1
    loop {
        let tip_info = get_chain_info(&conf);
        if tip_info.burn_block_height >= epoch_2_1 - 2 {
            break;
        }
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    // skip a couple sortitions
    btc_regtest_controller.bootstrap_chain(4);
    sleep_ms(5000);

    let sort_height = channel.get_sortitions_processed();
    assert!(sort_height > epoch_2_1);
    assert!(sort_height > v1_unlock_height);

    // *now* advance to 2.1
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    info!("Test passed processing 2.1");

    let sort_height = channel.get_sortitions_processed();
    let pox_addr_tuple_2 = execute(
        &format!("{{ hashbytes: 0x{pox_pubkey_hash_2}, version: 0x00 }}"),
        ClarityVersion::Clarity2,
    )
    .unwrap()
    .unwrap();
    let tx = make_contract_call(
        &spender_sk,
        1,
        3000,
        conf.burnchain.chain_id,
        &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
        "pox-2",
        "stack-stx",
        &[
            Value::UInt(stacked.into()),
            pox_addr_tuple_2.clone(),
            Value::UInt(sort_height as u128),
            Value::UInt(12),
        ],
    );

    info!("Submit 2.1 stacking tx to {http_origin:?}");
    sleep_ms(5_000);
    submit_tx(&http_origin, &tx);

    // that it can mine _at all_ is a success criterion
    let mut last_block_height = get_chain_info(&conf).burn_block_height;
    for _i in 0..20 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
        let tip_info = get_chain_info(&conf);
        if tip_info.burn_block_height > last_block_height {
            last_block_height = tip_info.burn_block_height;
        } else {
            panic!("FATAL: failed to mine");
        }
    }

    // invoke stack-increase again, in Epoch-2.2, it should
    //  runtime abort
    let aborted_increase_nonce_2_2 = 2;
    let tx = make_contract_call(
        &spender_sk,
        aborted_increase_nonce_2_2,
        3000,
        conf.burnchain.chain_id,
        &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
        "pox-2",
        "stack-increase",
        &[Value::UInt(5000)],
    );

    info!("Submit 2.2 stack-increase tx to {http_origin:?}");
    submit_tx(&http_origin, &tx);

    // transition to epoch 2.3
    loop {
        let tip_info = get_chain_info(&conf);
        if tip_info.burn_block_height > epoch_2_3 {
            break;
        }
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    // invoke stack-increase again, in Epoch-2.3, it should
    //  runtime abort
    let aborted_increase_nonce_2_3 = 3;
    let tx = make_contract_call(
        &spender_sk,
        aborted_increase_nonce_2_3,
        3000,
        conf.burnchain.chain_id,
        &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
        "pox-2",
        "stack-increase",
        &[Value::UInt(5000)],
    );

    info!("Submit 2.3 stack-increase tx to {http_origin:?}");
    submit_tx(&http_origin, &tx);

    // transition to 2 blocks before epoch 2.4
    loop {
        let tip_info = get_chain_info(&conf);
        if tip_info.burn_block_height >= epoch_2_4 - 2 {
            break;
        }
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    // skip a couple sortitions
    btc_regtest_controller.bootstrap_chain(4);
    sleep_ms(5000);

    let sort_height = channel.get_sortitions_processed();
    assert!(sort_height > epoch_2_4);

    // *now* advance to 2.4
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    info!("Test passed processing 2.4");

    // now, try stacking in pox-3
    let sort_height = channel.get_sortitions_processed();
    let tx = make_contract_call(
        &spender_sk,
        4,
        3000,
        conf.burnchain.chain_id,
        &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
        "pox-3",
        "stack-stx",
        &[
            Value::UInt(stacked.into()),
            pox_addr_tuple_2.clone(),
            Value::UInt(sort_height as u128),
            Value::UInt(12),
        ],
    );

    info!("Submit 2.4 stacking tx to {http_origin:?}");
    sleep_ms(5_000);
    submit_tx(&http_origin, &tx);

    let tx = make_contract_call(
        &spender_2_sk,
        0,
        3000,
        conf.burnchain.chain_id,
        &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
        "pox-3",
        "stack-stx",
        &[
            Value::UInt(stacked.into()),
            pox_addr_tuple_3.clone(),
            Value::UInt(sort_height as u128),
            Value::UInt(10),
        ],
    );

    info!("Submit second 2.4 stacking tx to {http_origin:?}");
    submit_tx(&http_origin, &tx);

    // that it can mine _at all_ is a success criterion
    let mut last_block_height = get_chain_info(&conf).burn_block_height;
    for _i in 0..5 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
        let tip_info = get_chain_info(&conf);
        if tip_info.burn_block_height > last_block_height {
            last_block_height = tip_info.burn_block_height;
        } else {
            panic!("FATAL: failed to mine");
        }
    }

    // invoke stack-increase
    let tx = make_contract_call(
        &spender_sk,
        5,
        3000,
        conf.burnchain.chain_id,
        &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
        "pox-3",
        "stack-increase",
        &[Value::UInt(increase_by.into())],
    );

    info!("Submit 2.4 stack-increase tx to {http_origin:?}");
    submit_tx(&http_origin, &tx);

    for _i in 0..19 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
        let tip_info = get_chain_info(&conf);
        if tip_info.burn_block_height > last_block_height {
            last_block_height = tip_info.burn_block_height;
        } else {
            panic!("FATAL: failed to mine");
        }
    }

    let tip_info = get_chain_info(&conf);
    let tip = StacksBlockId::new(&tip_info.stacks_tip_consensus_hash, &tip_info.stacks_tip);

    let (mut chainstate, _) = StacksChainState::open(
        false,
        conf.burnchain.chain_id,
        &conf.get_chainstate_path_str(),
        None,
    )
    .unwrap();
    let sortdb = btc_regtest_controller.sortdb_mut();

    let mut reward_cycle_pox_addrs = HashMap::new();

    info!("Last tip height = {}", tip_info.burn_block_height);

    for height in 211..tip_info.burn_block_height {
        let reward_cycle = pox_constants
            .block_height_to_reward_cycle(burnchain_config.first_block_height, height)
            .unwrap();

        reward_cycle_pox_addrs
            .entry(reward_cycle)
            .or_insert_with(HashMap::new);

        let iconn = sortdb.index_handle_at_block(&chainstate, &tip).unwrap();
        let pox_addrs = chainstate
            .clarity_eval_read_only(
                &iconn,
                &tip,
                &boot_code_id("pox-2", false),
                &format!("(get-burn-block-info? pox-addrs u{height})"),
            )
            .expect_optional()
            .unwrap()
            .unwrap()
            .expect_tuple()
            .unwrap()
            .get_owned("addrs")
            .unwrap()
            .expect_list()
            .unwrap();

        debug!("Test burnchain height {height}");
        if !burnchain_config.is_in_prepare_phase(height) && !pox_addrs.is_empty() {
            assert_eq!(pox_addrs.len(), 2);
            let pox_addr_0 = PoxAddress::try_from_pox_tuple(false, &pox_addrs[0]).unwrap();
            let pox_addr_1 = PoxAddress::try_from_pox_tuple(false, &pox_addrs[1]).unwrap();

            if let Some(pox_slot_count) = reward_cycle_pox_addrs
                .get_mut(&reward_cycle)
                .unwrap()
                .get_mut(&pox_addr_0)
            {
                *pox_slot_count += 1;
            } else {
                reward_cycle_pox_addrs
                    .get_mut(&reward_cycle)
                    .unwrap()
                    .insert(pox_addr_0, 1);
            }

            if let Some(pox_slot_count) = reward_cycle_pox_addrs
                .get_mut(&reward_cycle)
                .unwrap()
                .get_mut(&pox_addr_1)
            {
                *pox_slot_count += 1;
            } else {
                reward_cycle_pox_addrs
                    .get_mut(&reward_cycle)
                    .unwrap()
                    .insert(pox_addr_1, 1);
            }
        }
    }

    let reward_cycle_min = *reward_cycle_pox_addrs.keys().min().unwrap();
    let reward_cycle_max = *reward_cycle_pox_addrs.keys().max().unwrap();

    let pox_addr_1 = PoxAddress::Standard(
        StacksAddress::new(26, Hash160::from_hex(&pox_pubkey_hash_1).unwrap()),
        Some(AddressHashMode::SerializeP2PKH),
    );
    let pox_addr_2 = PoxAddress::Standard(
        StacksAddress::new(26, Hash160::from_hex(&pox_pubkey_hash_2).unwrap()),
        Some(AddressHashMode::SerializeP2PKH),
    );
    let pox_addr_3 = PoxAddress::Standard(
        StacksAddress::new(26, Hash160::from_hex(&pox_pubkey_hash_3).unwrap()),
        Some(AddressHashMode::SerializeP2PKH),
    );
    let burn_pox_addr = PoxAddress::Standard(
        StacksAddress::new(
            26,
            Hash160::from_hex("0000000000000000000000000000000000000000").unwrap(),
        ),
        Some(AddressHashMode::SerializeP2PKH),
    );

    let expected_slots = HashMap::from([
        (
            21u64,
            HashMap::from([(pox_addr_1.clone(), 13u64), (burn_pox_addr.clone(), 1)]),
        ),
        (
            22u64,
            HashMap::from([(pox_addr_1.clone(), 13u64), (burn_pox_addr.clone(), 1)]),
        ),
        (
            23u64,
            HashMap::from([(pox_addr_1.clone(), 13u64), (burn_pox_addr.clone(), 1)]),
        ),
        // cycle 24 is the first 2.1, it should have pox_2 and 1 burn slot
        (
            24,
            HashMap::from([(pox_addr_2.clone(), 13u64), (burn_pox_addr.clone(), 1)]),
        ),
        (
            25,
            HashMap::from([(pox_addr_2.clone(), 13u64), (burn_pox_addr.clone(), 1)]),
        ),
        // Epoch 2.2 has started, so the reward set should be all burns.
        (26, HashMap::from([(burn_pox_addr.clone(), 14)])),
        // Epoch 2.3 has started, so the reward set should be all burns.
        (27, HashMap::from([(burn_pox_addr.clone(), 14)])),
        (28, HashMap::from([(burn_pox_addr.clone(), 14)])),
        // cycle 29 is the first 2.4 cycle, it should have pox_2 and pox_3 with equal
        //  slots (because increase hasn't gone into effect yet)
        (
            29,
            HashMap::from([
                (pox_addr_2.clone(), 6u64),
                (pox_addr_3.clone(), 6),
                (burn_pox_addr.clone(), 2),
            ]),
        ),
        // stack-increase has been invoked, but this should not skew reward set heavily
        // because pox-3 fixes the total-locked bug
        (
            30,
            HashMap::from([
                (pox_addr_2.clone(), 7u64),
                (pox_addr_3.clone(), 6),
                (burn_pox_addr.clone(), 1),
            ]),
        ),
    ]);

    for reward_cycle in reward_cycle_min..(reward_cycle_max + 1) {
        let cycle_counts = &reward_cycle_pox_addrs[&reward_cycle];
        assert_eq!(cycle_counts.len(), expected_slots[&reward_cycle].len(), "The number of expected PoX addresses in reward cycle {reward_cycle} is mismatched with the actual count.");
        for (pox_addr, slots) in cycle_counts.iter() {
            assert_eq!(
                *slots,
                expected_slots[&reward_cycle][pox_addr],
                "The number of expected slots for PoX address {pox_addr} in reward cycle {reward_cycle} is mismatched with the actual count."
            );
            info!("PoX payment received"; "cycle" => reward_cycle, "pox_addr" => %pox_addr, "slots" => slots);
        }
    }

    let mut abort_tested_2_2 = false;
    let mut abort_tested_2_3 = false;
    let blocks = test_observer::get_blocks();
    for block in blocks {
        let transactions = block.get("transactions").unwrap().as_array().unwrap();
        for tx in transactions {
            let raw_tx = tx.get("raw_tx").unwrap().as_str().unwrap();
            if raw_tx == "0x00" {
                continue;
            }
            let tx_bytes = hex_bytes(&raw_tx[2..]).unwrap();
            let parsed =
                StacksTransaction::consensus_deserialize(&mut tx_bytes.as_slice()).unwrap();
            let tx_sender = PrincipalData::from(parsed.auth.origin().address_testnet());
            if tx_sender == spender_addr
                && (parsed.auth.get_origin_nonce() == aborted_increase_nonce_2_2
                    || parsed.auth.get_origin_nonce() == aborted_increase_nonce_2_3)
            {
                let contract_call = match &parsed.payload {
                    TransactionPayload::ContractCall(cc) => cc,
                    _ => panic!("Expected aborted_increase_nonce to be a contract call"),
                };
                assert_eq!(contract_call.contract_name.as_str(), "pox-2");
                assert_eq!(contract_call.function_name.as_str(), "stack-increase");
                let result = Value::try_deserialize_hex_untyped(
                    tx.get("raw_result").unwrap().as_str().unwrap(),
                )
                .unwrap();
                assert_eq!(result.to_string(), "(err none)");
                if parsed.auth.get_origin_nonce() == aborted_increase_nonce_2_2 {
                    abort_tested_2_2 = true;
                } else if parsed.auth.get_origin_nonce() == aborted_increase_nonce_2_3 {
                    abort_tested_2_3 = true;
                } else {
                    panic!("Unexpected nonce for the aborted stack-increase transaction.")
                }
            }
        }
    }

    assert!(
        abort_tested_2_2,
        "The stack-increase transaction must have been aborted in Epoch 2.2, \
            and it must have been tested in the tx receipts"
    );
    assert!(
        abort_tested_2_3,
        "The stack-increase transaction must have been aborted in Epoch 2.3, \
            and it must have been tested in the tx receipts"
    );

    test_observer::clear();
    channel.stop_chains_coordinator();
}

#[test]
#[ignore]
/// Verify that stackers that don't meet the stacking threshold get auto-unlocked in PoX-3.
fn verify_auto_unlock_behavior() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let reward_cycle_len = 10;
    let prepare_phase_len = 3;
    let epoch_2_05 = 215;
    let epoch_2_1 = 230;
    let v1_unlock_height = 231;
    let epoch_2_2 = 255; // two blocks before next prepare phase.
    let epoch_2_3 = 265;
    let epoch_2_4 = 280;
    let pox_3_activation_height = epoch_2_4;

    let first_stacked_init = 200_000_000_000 * (core::MICROSTACKS_PER_STACKS as u64);
    let first_stacked_incr = 40_000_000_000 * (core::MICROSTACKS_PER_STACKS as u64);
    let small_stacked = 17_000_000_000 * (core::MICROSTACKS_PER_STACKS as u64);

    let spender_sk = StacksPrivateKey::new();
    let spender_stx_addr: StacksAddress = to_addr(&spender_sk);
    let spender_addr: PrincipalData = to_addr(&spender_sk).into();

    let spender_2_sk = StacksPrivateKey::new();
    let spender_2_stx_addr: StacksAddress = to_addr(&spender_2_sk);
    let spender_2_addr: PrincipalData = to_addr(&spender_2_sk).into();

    let mut initial_balances = vec![];

    initial_balances.push(InitialBalance {
        address: spender_addr.clone(),
        amount: first_stacked_init + first_stacked_incr + 100_000,
    });

    initial_balances.push(InitialBalance {
        address: spender_2_addr.clone(),
        amount: small_stacked + 100_000,
    });

    let pox_pubkey_1 = Secp256k1PublicKey::from_hex(
        "02f006a09b59979e2cb8449f58076152af6b124aa29b948a3714b8d5f15aa94ede",
    )
    .unwrap();
    let pox_pubkey_hash_1 = bytes_to_hex(&Hash160::from_node_public_key(&pox_pubkey_1).to_bytes());

    let pox_pubkey_2 = Secp256k1PublicKey::from_hex(
        "03cd91307e16c10428dd0120d0a4d37f14d4e0097b3b2ea1651d7bd0fb109cd44b",
    )
    .unwrap();
    let pox_pubkey_hash_2 = bytes_to_hex(&Hash160::from_node_public_key(&pox_pubkey_2).to_bytes());
    let pox_pubkey_2_stx_addr = StacksAddress::from_public_keys(
        C32_ADDRESS_VERSION_TESTNET_SINGLESIG,
        &AddressHashMode::SerializeP2PKH,
        1,
        &vec![pox_pubkey_2],
    )
    .unwrap();

    let pox_pubkey_3 = Secp256k1PublicKey::from_hex(
        "0317782e663c77fb02ebf46a3720f41a70f5678ad185974a456d35848e275fe56b",
    )
    .unwrap();
    let pox_pubkey_hash_3 = bytes_to_hex(&Hash160::from_node_public_key(&pox_pubkey_3).to_bytes());
    let pox_pubkey_3_stx_addr = StacksAddress::from_public_keys(
        C32_ADDRESS_VERSION_TESTNET_SINGLESIG,
        &AddressHashMode::SerializeP2PKH,
        1,
        &vec![pox_pubkey_3],
    )
    .unwrap();

    let (mut conf, _) = neon_integration_test_conf();

    // we'll manually post a forked stream to the node
    conf.node.mine_microblocks = false;
    conf.burnchain.max_rbf = 1000000;
    conf.node.wait_time_for_microblocks = 0;
    conf.node.microblock_frequency = 1_000;
    conf.miner.first_attempt_time_ms = 2_000;
    conf.miner.subsequent_attempt_time_ms = 5_000;
    conf.node.wait_time_for_blocks = 1_000;
    conf.miner.wait_for_block_download = false;

    conf.miner.first_attempt_time_ms = i64::MAX as u64;
    conf.miner.subsequent_attempt_time_ms = i64::MAX as u64;

    test_observer::spawn();
    test_observer::register_any(&mut conf);
    conf.initial_balances.append(&mut initial_balances);

    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = epoch_2_05;
    epochs[StacksEpochId::Epoch2_05].start_height = epoch_2_05;
    epochs[StacksEpochId::Epoch2_05].end_height = epoch_2_1;
    epochs[StacksEpochId::Epoch21].start_height = epoch_2_1;
    epochs[StacksEpochId::Epoch21].end_height = epoch_2_2;
    epochs[StacksEpochId::Epoch22].start_height = epoch_2_2;
    epochs[StacksEpochId::Epoch22].end_height = epoch_2_3;
    epochs[StacksEpochId::Epoch23].start_height = epoch_2_3;
    epochs[StacksEpochId::Epoch23].end_height = epoch_2_4;
    epochs[StacksEpochId::Epoch24].start_height = epoch_2_4;
    epochs[StacksEpochId::Epoch24].end_height = STACKS_EPOCH_MAX;
    epochs.truncate_after(StacksEpochId::Epoch24);
    conf.burnchain.epochs = Some(epochs);

    let mut burnchain_config = Burnchain::regtest(&conf.get_burn_db_path());

    let pox_constants = PoxConstants::new(
        reward_cycle_len,
        prepare_phase_len,
        4 * prepare_phase_len / 5,
        5,
        15,
        u64::MAX - 2,
        u64::MAX - 1,
        v1_unlock_height as u32,
        epoch_2_2 as u32 + 1,
        u32::MAX,
        pox_3_activation_height as u32,
    );
    burnchain_config.pox_constants = pox_constants.clone();

    let first_v3_cycle = burnchain_config
        .block_height_to_reward_cycle(burnchain_config.pox_constants.pox_3_activation_height as u64)
        .unwrap()
        + 1;

    let mut btcd_controller = BitcoinCoreController::new(conf.clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::with_burnchain(
        conf.clone(),
        None,
        Some(burnchain_config.clone()),
        None,
    );
    let http_origin = format!("http://{}", &conf.node.rpc_bind);

    btc_regtest_controller.bootstrap_chain(201);

    eprintln!("Chain bootstrapped...");

    let mut run_loop = neon::RunLoop::new(conf.clone());
    let runloop_burnchain = burnchain_config.clone();

    let blocks_processed = run_loop.get_blocks_processed_arc();

    let channel = run_loop.get_coordinator_channel().unwrap();

    thread::spawn(move || run_loop.start(Some(runloop_burnchain), 0));

    // give the run loop some time to start up!
    wait_for_runloop(&blocks_processed);

    // first block wakes up the run loop
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // first block will hold our VRF registration
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // second block will be the first mined Stacks block
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // push us to block 205
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // stack right away
    let sort_height = channel.get_sortitions_processed();
    let pox_addr_tuple_1 = execute(
        &format!("{{ hashbytes: 0x{pox_pubkey_hash_1}, version: 0x00 }}"),
        ClarityVersion::Clarity2,
    )
    .unwrap()
    .unwrap();

    let pox_addr_tuple_3 = execute(
        &format!("{{ hashbytes: 0x{pox_pubkey_hash_3}, version: 0x00 }}"),
        ClarityVersion::Clarity2,
    )
    .unwrap()
    .unwrap();

    let tx = make_contract_call(
        &spender_sk,
        0,
        3000,
        conf.burnchain.chain_id,
        &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
        "pox",
        "stack-stx",
        &[
            Value::UInt(first_stacked_init.into()),
            pox_addr_tuple_1.clone(),
            Value::UInt(sort_height as u128),
            Value::UInt(12),
        ],
    );

    info!("Submit 2.05 stacking tx to {http_origin:?}");
    submit_tx(&http_origin, &tx);

    // wait until just before epoch 2.1
    loop {
        let tip_info = get_chain_info(&conf);
        if tip_info.burn_block_height >= epoch_2_1 - 2 {
            break;
        }
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    // skip a couple sortitions
    btc_regtest_controller.bootstrap_chain(4);
    sleep_ms(5000);

    let sort_height = channel.get_sortitions_processed();
    assert!(sort_height > epoch_2_1);
    assert!(sort_height > v1_unlock_height);

    // *now* advance to 2.1
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    info!("Test passed processing 2.1");

    let sort_height = channel.get_sortitions_processed();
    let pox_addr_tuple_2 = execute(
        &format!("{{ hashbytes: 0x{pox_pubkey_hash_2}, version: 0x00 }}"),
        ClarityVersion::Clarity2,
    )
    .unwrap()
    .unwrap();
    let tx = make_contract_call(
        &spender_sk,
        1,
        3000,
        conf.burnchain.chain_id,
        &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
        "pox-2",
        "stack-stx",
        &[
            Value::UInt(first_stacked_init.into()),
            pox_addr_tuple_2.clone(),
            Value::UInt(sort_height as u128),
            Value::UInt(12),
        ],
    );

    info!("Submit 2.1 stacking tx to {http_origin:?}");
    sleep_ms(5_000);
    submit_tx(&http_origin, &tx);

    // that it can mine _at all_ is a success criterion
    let mut last_block_height = get_chain_info(&conf).burn_block_height;
    for _i in 0..20 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
        let tip_info = get_chain_info(&conf);
        if tip_info.burn_block_height > last_block_height {
            last_block_height = tip_info.burn_block_height;
        } else {
            panic!("FATAL: failed to mine");
        }
    }

    info!("Successfully transitioned to Epoch 2.2");

    // transition to epoch 2.3
    loop {
        let tip_info = get_chain_info(&conf);
        if tip_info.burn_block_height > epoch_2_3 {
            break;
        }
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

        let pox_info = get_pox_info(&http_origin).unwrap();
        info!(
            "curr height: {}, curr cycle id: {}, pox active: {}",
            tip_info.burn_block_height,
            pox_info.current_cycle.id,
            pox_info.current_cycle.is_pox_active
        );
    }

    info!("Successfully transitioned to Epoch 2.3");

    // transition to 2 blocks before epoch 2.4
    loop {
        let tip_info = get_chain_info(&conf);
        if tip_info.burn_block_height >= epoch_2_4 - 2 {
            break;
        }
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

        let pox_info = get_pox_info(&http_origin).unwrap();
        info!(
            "curr height: {}, curr cycle id: {}, pox active: {}",
            tip_info.burn_block_height,
            pox_info.current_cycle.id,
            pox_info.current_cycle.is_pox_active
        );
    }

    // skip a couple sortitions
    btc_regtest_controller.bootstrap_chain(4);
    sleep_ms(5000);

    let sort_height = channel.get_sortitions_processed();
    assert!(sort_height > epoch_2_4);

    // *now* advance to 2.4
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    info!("Test passed processing 2.4");

    // now, try stacking in pox-3
    let sort_height = channel.get_sortitions_processed();
    let tx = make_contract_call(
        &spender_sk,
        2,
        3000,
        conf.burnchain.chain_id,
        &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
        "pox-3",
        "stack-stx",
        &[
            Value::UInt(first_stacked_init.into()),
            pox_addr_tuple_2.clone(),
            Value::UInt(sort_height as u128),
            Value::UInt(12),
        ],
    );

    info!("Submit 2.4 stacking tx to {http_origin:?}");
    sleep_ms(5_000);
    submit_tx(&http_origin, &tx);

    let tx = make_contract_call(
        &spender_2_sk,
        0,
        3000,
        conf.burnchain.chain_id,
        &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
        "pox-3",
        "stack-stx",
        &[
            Value::UInt(small_stacked.into()),
            pox_addr_tuple_3.clone(),
            Value::UInt(sort_height as u128),
            Value::UInt(10),
        ],
    );

    info!("Submit second 2.4 stacking tx to {http_origin:?}");
    submit_tx(&http_origin, &tx);

    // that it can mine _at all_ is a success criterion
    let mut last_block_height = get_chain_info(&conf).burn_block_height;
    for _i in 0..5 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
        let tip_info = get_chain_info(&conf);
        if tip_info.burn_block_height > last_block_height {
            last_block_height = tip_info.burn_block_height;
        } else {
            panic!("FATAL: failed to mine");
        }
    }

    // Check the locked balance of addr 1.
    let account = get_account(&http_origin, &spender_stx_addr);
    assert_eq!(account.locked, first_stacked_init as u128);

    // Check the locked balance of addr 2.
    let account = get_account(&http_origin, &spender_2_stx_addr);
    assert_eq!(account.locked, small_stacked as u128);

    // Check that the "raw" reward sets for all cycles just contains entries for both addrs
    //  for the next few cycles.
    for _cycle_number in first_v3_cycle..(first_v3_cycle + 6) {
        let (mut chainstate, _) = StacksChainState::open(
            false,
            conf.burnchain.chain_id,
            &conf.get_chainstate_path_str(),
            None,
        )
        .unwrap();
        let sortdb = btc_regtest_controller.sortdb_mut();

        let tip_info = get_chain_info(&conf);
        let tip_block_id =
            StacksBlockId::new(&tip_info.stacks_tip_consensus_hash, &tip_info.stacks_tip);

        let reward_set_entries = get_reward_set_entries_at_block(
            &mut chainstate,
            &burnchain_config,
            sortdb,
            &tip_block_id,
            tip_info.burn_block_height,
        )
        .unwrap();

        assert_eq!(reward_set_entries.len(), 2);
        info!("reward set entries: {reward_set_entries:?}");
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            pox_pubkey_2_stx_addr.bytes.0.to_vec()
        );
        assert_eq!(
            reward_set_entries[0].amount_stacked,
            first_stacked_init as u128
        );
        assert_eq!(
            reward_set_entries[1].reward_address.bytes(),
            pox_pubkey_3_stx_addr.bytes.0.to_vec()
        );
        assert_eq!(reward_set_entries[1].amount_stacked, small_stacked as u128);
    }

    // invoke stack-increase
    let tx = make_contract_call(
        &spender_sk,
        3,
        3000,
        conf.burnchain.chain_id,
        &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
        "pox-3",
        "stack-increase",
        &[Value::UInt(first_stacked_incr.into())],
    );

    info!("Submit 2.4 stack-increase tx to {http_origin:?}");
    submit_tx(&http_origin, &tx);

    for _i in 0..19 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
        let tip_info = get_chain_info(&conf);
        if tip_info.burn_block_height > last_block_height {
            last_block_height = tip_info.burn_block_height;
        } else {
            panic!("FATAL: failed to mine");
        }
    }

    // Check that the locked balance of addr 1 has not changed.
    let account = get_account(&http_origin, &spender_stx_addr);
    assert_eq!(
        account.locked,
        (first_stacked_init + first_stacked_incr) as u128
    );

    // Check that addr 2 has no locked tokens at this height (was auto-unlocked).
    let account = get_account(&http_origin, &spender_2_stx_addr);
    assert_eq!(account.locked, 0);

    let (mut chainstate, _) = StacksChainState::open(
        false,
        conf.burnchain.chain_id,
        &conf.get_chainstate_path_str(),
        None,
    )
    .unwrap();
    let sortdb = btc_regtest_controller.sortdb_mut();

    // Check that the "raw" reward sets for all cycles just contains entries for the first
    //  address at the cycle start, since addr 2 was auto-unlocked.
    for _cycle_number in first_v3_cycle..(first_v3_cycle + 6) {
        let tip_info = get_chain_info(&conf);
        let tip_block_id =
            StacksBlockId::new(&tip_info.stacks_tip_consensus_hash, &tip_info.stacks_tip);

        let reward_set_entries = get_reward_set_entries_at_block(
            &mut chainstate,
            &burnchain_config,
            sortdb,
            &tip_block_id,
            tip_info.burn_block_height,
        )
        .unwrap();

        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            pox_pubkey_2_stx_addr.bytes.0.to_vec()
        );
        assert_eq!(
            reward_set_entries[0].amount_stacked,
            (first_stacked_init + first_stacked_incr) as u128
        );
    }

    let tip_info = get_chain_info(&conf);
    let tip = StacksBlockId::new(&tip_info.stacks_tip_consensus_hash, &tip_info.stacks_tip);

    let mut reward_cycle_pox_addrs = HashMap::new();

    info!("Last tip height = {}", tip_info.burn_block_height);

    for height in 211..tip_info.burn_block_height {
        let reward_cycle = pox_constants
            .block_height_to_reward_cycle(burnchain_config.first_block_height, height)
            .unwrap();

        reward_cycle_pox_addrs
            .entry(reward_cycle)
            .or_insert_with(HashMap::new);

        let iconn = sortdb.index_handle_at_block(&chainstate, &tip).unwrap();
        let pox_addrs = chainstate
            .clarity_eval_read_only(
                &iconn,
                &tip,
                &boot_code_id("pox-2", false),
                &format!("(get-burn-block-info? pox-addrs u{height})"),
            )
            .expect_optional()
            .unwrap()
            .unwrap()
            .expect_tuple()
            .unwrap()
            .get_owned("addrs")
            .unwrap()
            .expect_list()
            .unwrap();

        if !burnchain_config.is_in_prepare_phase(height) && !pox_addrs.is_empty() {
            assert_eq!(pox_addrs.len(), 2);
            let pox_addr_0 = PoxAddress::try_from_pox_tuple(false, &pox_addrs[0]).unwrap();
            let pox_addr_1 = PoxAddress::try_from_pox_tuple(false, &pox_addrs[1]).unwrap();

            if let Some(pox_slot_count) = reward_cycle_pox_addrs
                .get_mut(&reward_cycle)
                .unwrap()
                .get_mut(&pox_addr_0)
            {
                *pox_slot_count += 1;
            } else {
                reward_cycle_pox_addrs
                    .get_mut(&reward_cycle)
                    .unwrap()
                    .insert(pox_addr_0, 1);
            }

            if let Some(pox_slot_count) = reward_cycle_pox_addrs
                .get_mut(&reward_cycle)
                .unwrap()
                .get_mut(&pox_addr_1)
            {
                *pox_slot_count += 1;
            } else {
                reward_cycle_pox_addrs
                    .get_mut(&reward_cycle)
                    .unwrap()
                    .insert(pox_addr_1, 1);
            }
        }
    }

    let reward_cycle_min = *reward_cycle_pox_addrs.keys().min().unwrap();
    let reward_cycle_max = *reward_cycle_pox_addrs.keys().max().unwrap();

    let pox_addr_1 = PoxAddress::Standard(
        StacksAddress::new(26, Hash160::from_hex(&pox_pubkey_hash_1).unwrap()),
        Some(AddressHashMode::SerializeP2PKH),
    );
    let pox_addr_2 = PoxAddress::Standard(
        StacksAddress::new(26, Hash160::from_hex(&pox_pubkey_hash_2).unwrap()),
        Some(AddressHashMode::SerializeP2PKH),
    );
    let pox_addr_3 = PoxAddress::Standard(
        StacksAddress::new(26, Hash160::from_hex(&pox_pubkey_hash_3).unwrap()),
        Some(AddressHashMode::SerializeP2PKH),
    );
    let burn_pox_addr = PoxAddress::Standard(
        StacksAddress::new(
            26,
            Hash160::from_hex("0000000000000000000000000000000000000000").unwrap(),
        ),
        Some(AddressHashMode::SerializeP2PKH),
    );

    let expected_slots = HashMap::from([
        (
            21u64,
            HashMap::from([(pox_addr_1.clone(), 13u64), (burn_pox_addr.clone(), 1)]),
        ),
        (
            22u64,
            HashMap::from([(pox_addr_1.clone(), 13u64), (burn_pox_addr.clone(), 1)]),
        ),
        (
            23u64,
            HashMap::from([(pox_addr_1.clone(), 13u64), (burn_pox_addr.clone(), 1)]),
        ),
        // cycle 24 is the first 2.1, it should have pox_2 and 1 burn slot
        (
            24,
            HashMap::from([(pox_addr_2.clone(), 13u64), (burn_pox_addr.clone(), 1)]),
        ),
        (
            25,
            HashMap::from([(pox_addr_2.clone(), 13u64), (burn_pox_addr.clone(), 1)]),
        ),
        // Epoch 2.2 has started, so the reward set should be all burns.
        (26, HashMap::from([(burn_pox_addr.clone(), 14)])),
        // Epoch 2.3 has started, so the reward set should be all burns.
        (27, HashMap::from([(burn_pox_addr.clone(), 14)])),
        (28, HashMap::from([(burn_pox_addr.clone(), 14)])),
        // cycle 29 is the first 2.4 cycle, it should have pox_2 and pox_3 with equal
        //  slots (because increase hasn't gone into effect yet).
        (
            29,
            HashMap::from([
                (pox_addr_2.clone(), 12u64),
                (pox_addr_3.clone(), 1),
                (burn_pox_addr.clone(), 1),
            ]),
        ),
        // stack-increase has been invoked, which causes spender_addr_2 to be below the stacking
        // minimum, and thus they have zero reward addresses in reward cycle 30.
        (
            30,
            HashMap::from([(pox_addr_2.clone(), 13u64), (burn_pox_addr.clone(), 1)]),
        ),
    ]);

    for reward_cycle in reward_cycle_min..(reward_cycle_max + 1) {
        let cycle_counts = &reward_cycle_pox_addrs[&reward_cycle];
        assert_eq!(cycle_counts.len(), expected_slots[&reward_cycle].len(), "The number of expected PoX addresses in reward cycle {reward_cycle} is mismatched with the actual count.");
        for (pox_addr, slots) in cycle_counts.iter() {
            assert_eq!(
                *slots,
                expected_slots[&reward_cycle][pox_addr],
                "The number of expected slots for PoX address {pox_addr} in reward cycle {reward_cycle} is mismatched with the actual count."
            );
            info!("PoX payment received"; "cycle" => reward_cycle, "pox_addr" => %pox_addr, "slots" => slots);
        }
    }

    test_observer::clear();
    channel.stop_chains_coordinator();
}
