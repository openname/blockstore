use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::{env, thread};

use clarity::vm::costs::ExecutionCost;
use clarity::vm::types::PrincipalData;
use clarity::vm::ContractName;
use stacks::burnchains::{Burnchain, Txid};
use stacks::chainstate::burn::operations::leader_block_commit::BURN_BLOCK_MINED_AT_MODULUS;
use stacks::chainstate::burn::operations::{BlockstackOperationType, LeaderBlockCommitOp};
use stacks::chainstate::stacks::address::PoxAddress;
use stacks::chainstate::stacks::db::StacksChainState;
use stacks::chainstate::stacks::{
    StacksBlockHeader, StacksPrivateKey, StacksTransaction, TransactionPayload,
};
use stacks::config::{EventKeyType, InitialBalance};
use stacks::core::{
    self, EpochList, StacksEpoch, StacksEpochId, PEER_VERSION_EPOCH_1_0, PEER_VERSION_EPOCH_2_0,
    PEER_VERSION_EPOCH_2_05, PEER_VERSION_EPOCH_2_1,
};
use stacks_common::codec::StacksMessageCodec;
use stacks_common::types::chainstate::{BlockHeaderHash, BurnchainHeaderHash, VRFSeed};
use stacks_common::util::hash::hex_bytes;
use stacks_common::util::sleep_ms;

use crate::tests::bitcoin_regtest::BitcoinCoreController;
use crate::tests::neon_integrations::*;
use crate::tests::{
    make_contract_call, make_contract_call_mblock_only, make_contract_publish,
    make_contract_publish_microblock_only, run_until_burnchain_height, select_transactions_where,
    to_addr,
};
use crate::{neon, BitcoinRegtestController, BurnchainController, Keychain};

#[test]
#[ignore]
// Test that the miner code path and the follower code path end up with the exact same calculation of a
//  a block's execution cost, budget expended, and total allotted budget.
// This test will broadcast transactions from a single user account, where *even* nonce transactions
//  are marked anchored block only and *odd* nonce transactions are marked microblock only. This will ensure
//  that each anchored block (after the first) will confirm a 1-transaction microblock stream and contain 1
//  transaction.
fn test_exact_block_costs() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let spender_sk = StacksPrivateKey::new();
    let spender_addr = PrincipalData::from(to_addr(&spender_sk));
    let spender_addr_c32 = to_addr(&spender_sk);

    let epoch_205_transition_height = 210;
    let transactions_to_broadcast = 25;

    let (mut conf, _miner_account) = neon_integration_test_conf();
    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = epoch_205_transition_height;
    epochs[StacksEpochId::Epoch2_05].start_height = epoch_205_transition_height;

    conf.burnchain.epochs = Some(epochs);
    conf.node.mine_microblocks = true;
    conf.node.wait_time_for_microblocks = 10_000;
    conf.node.microblock_frequency = 500;

    conf.initial_balances.push(InitialBalance {
        address: spender_addr.clone(),
        amount: 200_000_000,
    });

    let contract_name = "test-contract";
    let contract_content = "
      (define-data-var db2 (list 500 int) (list 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20))
      (define-public (db-get2)
        (begin (var-get db2)
               (ok 1)))
    ";

    let contract_publish_tx = make_contract_publish(
        &spender_sk,
        0,
        210_000,
        conf.burnchain.chain_id,
        contract_name,
        contract_content,
    );

    // make txs that alternate between
    let txs: Vec<_> = (1..transactions_to_broadcast + 1)
        .map(|nonce| {
            if nonce % 2 == 0 {
                make_contract_call(
                    &spender_sk,
                    nonce,
                    200_000,
                    conf.burnchain.chain_id,
                    &spender_addr_c32,
                    contract_name,
                    "db-get2",
                    &[],
                )
            } else {
                make_contract_call_mblock_only(
                    &spender_sk,
                    nonce,
                    200_000,
                    conf.burnchain.chain_id,
                    &spender_addr_c32,
                    contract_name,
                    "db-get2",
                    &[],
                )
            }
        })
        .collect();

    test_observer::spawn();
    test_observer::register(
        &mut conf,
        &[EventKeyType::AnyEvent, EventKeyType::MinedBlocks],
    );

    let mut btcd_controller = BitcoinCoreController::new(conf.clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::new(conf.clone(), None);
    let http_origin = format!("http://{}", &conf.node.rpc_bind);

    btc_regtest_controller.bootstrap_chain(201);

    eprintln!("Chain bootstrapped...");

    let mut run_loop = neon::RunLoop::new(conf.clone());
    let blocks_processed = run_loop.get_blocks_processed_arc();

    let channel = run_loop.get_coordinator_channel().unwrap();

    thread::spawn(move || run_loop.start(None, 0));

    // give the run loop some time to start up!
    wait_for_runloop(&blocks_processed);

    // first block wakes up the run loop
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // first block will hold our VRF registration
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // second block will be the first mined Stacks block
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let tip_info = get_chain_info(&conf);
    let current_burn_height = tip_info.burn_block_height as u32;
    assert_eq!(current_burn_height, 204);

    // broadcast the contract
    submit_tx(&http_origin, &contract_publish_tx);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let tip_info = get_chain_info(&conf);
    let current_burn_height = tip_info.burn_block_height as u32;
    assert_eq!(current_burn_height, 205);

    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let tip_info = get_chain_info(&conf);
    let current_burn_height = tip_info.burn_block_height as u32;
    assert_eq!(current_burn_height, 206);

    // broadcast the rest of our transactions
    for tx in txs.iter() {
        submit_tx(&http_origin, tx);
    }

    // produce 10 more blocks
    for _i in 0..10 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    let tip_info = get_chain_info(&conf);
    let current_burn_height = tip_info.burn_block_height as u32;
    assert_eq!(current_burn_height, 216);

    let blocks = test_observer::get_blocks();
    let mined_blocks = test_observer::get_mined_blocks();
    let mut mined_blocks_map = HashMap::new();
    for mined_block in mined_blocks.into_iter() {
        mined_blocks_map.insert(mined_block.target_burn_height, mined_block);
    }

    let mut processed_txs_before_205 = false;
    let mut processed_txs_after_205 = false;

    for block in blocks {
        let burn_height = block.get("burn_block_height").unwrap().as_i64().unwrap();
        if burn_height == 0 {
            // no data for genesis block
            continue;
        }
        let transactions = block.get("transactions").unwrap().as_array().unwrap();
        let anchor_cost = block
            .get("anchored_cost")
            .unwrap()
            .get("runtime")
            .unwrap()
            .as_i64()
            .unwrap();
        let mblock_confirm_cost = block
            .get("confirmed_microblocks_cost")
            .unwrap()
            .get("runtime")
            .unwrap()
            .as_i64()
            .unwrap();

        let mined_event = mined_blocks_map.get(&(burn_height as u64)).unwrap();

        let mined_anchor_cost = mined_event.anchored_cost.runtime;

        let mined_mblock_confirmed_cost = mined_event.confirmed_microblocks_cost.runtime;

        info!(
            "Processed block";
            "burn_height" => burn_height,
            "confirmed_tx_count" => transactions.len(),
            "anchor_cost" => anchor_cost,
            "mined_anchor_cost" => mined_anchor_cost,
            "mblock_cost" => mblock_confirm_cost,
            "mined_mblock_cost" => mined_mblock_confirmed_cost,
        );

        let dbget_txs: Vec<_> = transactions
            .iter()
            .filter_map(|tx| {
                let raw_tx = tx.get("raw_tx").unwrap().as_str().unwrap();
                let tx_bytes = hex_bytes(&raw_tx[2..]).unwrap();
                let parsed = StacksTransaction::consensus_deserialize(&mut &tx_bytes[..]).unwrap();
                if let TransactionPayload::ContractCall(ref cc) = &parsed.payload {
                    if cc.function_name.as_str() == "db-get2" {
                        Some(parsed)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        if burn_height as u64 <= epoch_205_transition_height {
            if dbget_txs.len() >= 2 {
                processed_txs_before_205 = true;
            }
        } else if dbget_txs.len() >= 2 {
            processed_txs_after_205 = true;
        }

        assert_eq!(mined_anchor_cost, anchor_cost as u64);
        assert_eq!(mined_mblock_confirmed_cost, mblock_confirm_cost as u64);
    }

    // check that we processed at least 2 user transactions in a block pre-2.05 and post-2.05
    assert!(processed_txs_before_205);
    assert!(processed_txs_after_205);

    test_observer::clear();
    channel.stop_chains_coordinator();
}

#[test]
#[ignore]
// Test dynamic db method costs by invoking a db fetch operation
//  with the same data schema, but with varying db size. Check
//  that when the epoch boundary crosses, the cost difference between
//  the operations changes.
fn test_dynamic_db_method_costs() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let spender_sk = StacksPrivateKey::new();
    let spender_addr = PrincipalData::from(to_addr(&spender_sk));
    let spender_addr_c32 = to_addr(&spender_sk);
    let contract_name = "test-contract";

    let epoch_205_transition_height = 210;

    let contract_content = "
      (define-data-var db1 (list 500 int) (list 1 2 3 4 5))
      (define-data-var db2 (list 500 int) (list 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20))
      (define-public (db-get1)
        (begin (var-get db1)
               (ok 1)))
      (define-public (db-get2)
        (begin (var-get db2)
               (ok 1)))
    ";

    let (mut conf, _miner_account) = neon_integration_test_conf();
    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = epoch_205_transition_height;
    epochs[StacksEpochId::Epoch2_05].start_height = epoch_205_transition_height;

    conf.burnchain.epochs = Some(epochs);

    conf.initial_balances.push(InitialBalance {
        address: spender_addr.clone(),
        amount: 200_000_000,
    });

    let contract_publish_tx = make_contract_publish(
        &spender_sk,
        0,
        210_000,
        conf.burnchain.chain_id,
        contract_name,
        contract_content,
    );

    let chain_id = conf.burnchain.chain_id;
    let make_db_get1_call = |nonce| {
        make_contract_call(
            &spender_sk,
            nonce,
            200_000,
            chain_id,
            &spender_addr_c32,
            contract_name,
            "db-get1",
            &[],
        )
    };

    let make_db_get2_call = |nonce| {
        make_contract_call(
            &spender_sk,
            nonce,
            200_000,
            chain_id,
            &spender_addr_c32,
            contract_name,
            "db-get2",
            &[],
        )
    };

    test_observer::spawn();
    test_observer::register_any(&mut conf);

    let mut btcd_controller = BitcoinCoreController::new(conf.clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::new(conf.clone(), None);
    let http_origin = format!("http://{}", &conf.node.rpc_bind);

    btc_regtest_controller.bootstrap_chain(201);

    eprintln!("Chain bootstrapped...");

    let mut run_loop = neon::RunLoop::new(conf.clone());
    let blocks_processed = run_loop.get_blocks_processed_arc();

    let channel = run_loop.get_coordinator_channel().unwrap();

    thread::spawn(move || run_loop.start(None, 0));

    // give the run loop some time to start up!
    wait_for_runloop(&blocks_processed);

    // first block wakes up the run loop
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // first block will hold our VRF registration
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // second block will be the first mined Stacks block
    //  include the testing contract publish tx
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let tip_info = get_chain_info(&conf);
    let current_burn_height = tip_info.burn_block_height as u32;
    assert_eq!(current_burn_height, 204);
    submit_tx(&http_origin, &contract_publish_tx);

    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let tip_info = get_chain_info(&conf);
    let current_burn_height = tip_info.burn_block_height as u32;
    assert_eq!(current_burn_height, 205);

    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let tip_info = get_chain_info(&conf);
    let current_burn_height = tip_info.burn_block_height as u32;
    assert_eq!(current_burn_height, 206);

    // broadcast 10 contract calls and produce 10 more blocks
    //  there's an off-by-one behavior in `next_block_and_wait`, where the miner
    //  has already assembled the next block when it is called, so the tx broadcasted
    //  when current_burn_height = `n` will be included in a block elected at burn height
    //  `n + 2`
    for i in 0..10 {
        submit_tx(&http_origin, &make_db_get1_call(1 + (2 * i)));
        submit_tx(&http_origin, &make_db_get2_call(2 + (2 * i)));
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    let tip_info = get_chain_info(&conf);
    let current_burn_height = tip_info.burn_block_height as u32;
    assert_eq!(current_burn_height, 216);

    let blocks = test_observer::get_blocks();

    let mut tested_heights = vec![];

    for block in blocks {
        let burn_height = block.get("burn_block_height").unwrap().as_i64().unwrap();
        let transactions = block.get("transactions").unwrap().as_array().unwrap();

        let mut db1_cost = None;
        let mut db2_cost = None;
        for tx in transactions.iter() {
            let raw_tx = tx.get("raw_tx").unwrap().as_str().unwrap();
            if raw_tx == "0x00" {
                continue;
            }
            let tx_bytes = hex_bytes(&raw_tx[2..]).unwrap();
            let parsed = StacksTransaction::consensus_deserialize(&mut &tx_bytes[..]).unwrap();

            if let TransactionPayload::ContractCall(ref cc) = parsed.payload {
                assert_eq!(
                    cc.contract_name.as_str(),
                    contract_name,
                    "All contract calls should be to the test contract"
                );

                let function_name = cc.function_name.as_str();
                assert!(function_name == "db-get1" || function_name == "db-get2");

                let runtime_cost = tx
                    .get("execution_cost")
                    .unwrap()
                    .get("runtime")
                    .unwrap()
                    .as_i64()
                    .unwrap();
                eprintln!(
                    "Burn height = {burn_height}, runtime_cost = {runtime_cost}, function_name = {function_name}"
                );

                if function_name == "db-get1" {
                    db1_cost = Some(runtime_cost);
                } else if function_name == "db-get2" {
                    db2_cost = Some(runtime_cost);
                }
            }
        }

        if let Some(db1_cost) = db1_cost {
            tested_heights.push(burn_height as u64);
            let db2_cost = db2_cost.expect("`db-get1` was called in block without `db-get2`");
            if burn_height <= epoch_205_transition_height as i64 {
                assert_eq!(
                    db1_cost, db2_cost,
                    "In Epoch 2.0, the cost of `db-get1` and `db-get2` should be equal"
                );
            } else {
                assert!(
                    db1_cost < db2_cost,
                    "In Epoch 2.05, the cost of `db-get1` should be less than `db-get2`"
                );
            };
        }
    }

    // make sure that the test covered the blocks before, at, and after the epoch transition.
    assert!(tested_heights.contains(&(epoch_205_transition_height - 1)));
    assert!(tested_heights.contains(&epoch_205_transition_height));
    assert!(tested_heights.contains(&(epoch_205_transition_height + 1)));

    test_observer::clear();
    channel.stop_chains_coordinator();
}

#[test]
#[ignore]
fn transition_empty_blocks() {
    // very simple test to verify that the miner will keep making valid (empty) blocks after the
    // transition.  Really tests that the block-commits are well-formed before and after the epoch
    // transition.
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let epoch_2_05 = 210;

    let (mut conf, miner_account) = neon_integration_test_conf();

    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = epoch_2_05;
    epochs[StacksEpochId::Epoch2_05].start_height = epoch_2_05;

    conf.burnchain.epochs = Some(epochs);

    let keychain = Keychain::default(conf.node.seed.clone());

    let mut btcd_controller = BitcoinCoreController::new(conf.clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::new(conf.clone(), None);
    let http_origin = format!("http://{}", &conf.node.rpc_bind);

    btc_regtest_controller.bootstrap_chain(epoch_2_05 - 5);

    eprintln!("Chain bootstrapped...");

    let mut run_loop = neon::RunLoop::new(conf.clone());
    let blocks_processed = run_loop.get_blocks_processed_arc();

    let channel = run_loop.get_coordinator_channel().unwrap();

    thread::spawn(move || run_loop.start(None, 0));

    // give the run loop some time to start up!
    wait_for_runloop(&blocks_processed);

    // first block wakes up the run loop
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // first block will hold our VRF registration
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let tip_info = get_chain_info(&conf);
    let key_block_ptr = tip_info.burn_block_height as u32;
    let key_vtxindex = 1; // nothing else here but the coinbase

    // second block will be the first mined Stacks block
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let burnchain = Burnchain::regtest(&conf.get_burn_db_path());
    let mut bitcoin_controller = BitcoinRegtestController::new_dummy(conf.clone());

    // these should all succeed across the epoch boundary
    for _i in 0..5 {
        // also, make *huge* block-commits with invalid marker bytes once we reach the new
        // epoch, and verify that it fails.
        let tip_info = get_chain_info(&conf);

        // this block is the epoch transition?
        let (chainstate, _) = StacksChainState::open(
            false,
            conf.burnchain.chain_id,
            &conf.get_chainstate_path_str(),
            None,
        )
        .unwrap();
        let res = StacksChainState::block_crosses_epoch_boundary(
            chainstate.db(),
            &tip_info.stacks_tip_consensus_hash,
            &tip_info.stacks_tip,
        )
        .unwrap();
        debug!(
            "Epoch transition at {} ({}/{}) height {}: {res}",
            &StacksBlockHeader::make_index_block_hash(
                &tip_info.stacks_tip_consensus_hash,
                &tip_info.stacks_tip
            ),
            &tip_info.stacks_tip_consensus_hash,
            &tip_info.stacks_tip,
            tip_info.burn_block_height
        );

        if tip_info.burn_block_height == epoch_2_05 {
            assert!(res);
        } else {
            assert!(!res);
        }

        if tip_info.burn_block_height + 1 >= epoch_2_05 {
            let burn_fee_cap = 100000000; // 1 BTC
            let sunset_burn = burnchain.expected_sunset_burn(
                tip_info.burn_block_height + 1,
                burn_fee_cap,
                StacksEpochId::Epoch2_05,
            );
            let rest_commit = burn_fee_cap - sunset_burn;

            let commit_outs = if tip_info.burn_block_height + 1 < burnchain.pox_constants.sunset_end
                && !burnchain.is_in_prepare_phase(tip_info.burn_block_height + 1)
            {
                vec![
                    PoxAddress::standard_burn_address(conf.is_mainnet()),
                    PoxAddress::standard_burn_address(conf.is_mainnet()),
                ]
            } else {
                vec![PoxAddress::standard_burn_address(conf.is_mainnet())]
            };

            // let's commit
            let burn_parent_modulus =
                (tip_info.burn_block_height % BURN_BLOCK_MINED_AT_MODULUS) as u8;
            let op = BlockstackOperationType::LeaderBlockCommit(LeaderBlockCommitOp {
                treatment: vec![],
                sunset_burn,
                block_header_hash: BlockHeaderHash([0xff; 32]),
                burn_fee: rest_commit,
                input: (Txid([0; 32]), 0),
                apparent_sender: keychain.get_burnchain_signer(),
                key_block_ptr,
                key_vtxindex,
                memo: vec![0], // bad epoch marker
                new_seed: VRFSeed([0x11; 32]),
                parent_block_ptr: 0,
                parent_vtxindex: 0,
                // to be filled in
                vtxindex: 0,
                txid: Txid([0u8; 32]),
                block_height: 0,
                burn_header_hash: BurnchainHeaderHash::zero(),
                burn_parent_modulus,
                commit_outs,
            });
            let mut op_signer = keychain.generate_op_signer();
            let res = bitcoin_controller.submit_operation(
                StacksEpochId::Epoch2_05,
                op,
                &mut op_signer,
                1,
            );
            assert!(res.is_ok(), "Failed to submit block-commit");
        }

        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    let account = get_account(&http_origin, &miner_account);
    assert_eq!(account.nonce, 6);

    channel.stop_chains_coordinator();
}

/// This test checks that the block limit is changed at Stacks 2.05. We lower the allowance, and
/// check that we can 1) afford the function call before the target height and
/// 2) cannot afford the function call after the target height.
#[test]
#[ignore]
fn test_cost_limit_switch_version205() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    // This contract contains `increment-many`, which does many MARF reads.
    let giant_contract = r#"
;; define counter variable
(define-data-var counter int 0)

;; increment method
(define-public (increment)
  (begin
    (var-set counter (+ (var-get counter) 1))
    (ok (var-get counter))))

  (define-public (increment-many)
    (begin
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (unwrap! (increment) (err u1))
      (ok (var-get counter))))
    "#
    .to_string();

    // Create three characters, `creator`, `alice` and `bob`.
    let creator_sk = StacksPrivateKey::new();
    let creator_addr = to_addr(&creator_sk);
    let creator_pd: PrincipalData = creator_addr.into();

    let alice_sk = StacksPrivateKey::new();
    let alice_addr = to_addr(&alice_sk);
    let alice_pd: PrincipalData = alice_addr.into();

    let bob_sk = StacksPrivateKey::new();
    let bob_addr = to_addr(&bob_sk);
    let bob_pd: PrincipalData = bob_addr.into();

    let (mut conf, _) = neon_integration_test_conf();

    // Create a schedule where we lower the read_count on Epoch2_05.
    conf.burnchain.epochs = Some(EpochList::new(&[
        StacksEpoch {
            epoch_id: StacksEpochId::Epoch10,
            start_height: 0,
            end_height: 0,
            block_limit: ExecutionCost {
                write_length: 100000000,
                write_count: 1000,
                read_length: 1000000000,
                read_count: 150,
                runtime: 5000000000,
            },
            network_epoch: PEER_VERSION_EPOCH_1_0,
        },
        StacksEpoch {
            epoch_id: StacksEpochId::Epoch20,
            start_height: 0,
            end_height: 215,
            block_limit: ExecutionCost {
                write_length: 100000000,
                write_count: 1000,
                read_length: 1000000000,
                read_count: 150,
                runtime: 5000000000,
            },
            network_epoch: PEER_VERSION_EPOCH_2_0,
        },
        StacksEpoch {
            epoch_id: StacksEpochId::Epoch2_05,
            start_height: 215,
            end_height: 10_002,
            block_limit: ExecutionCost {
                write_length: 100000000,
                write_count: 1000,
                read_length: 1000000000,
                read_count: 50,
                runtime: 5000000000,
            },
            network_epoch: PEER_VERSION_EPOCH_2_05,
        },
        StacksEpoch {
            epoch_id: StacksEpochId::Epoch21,
            start_height: 10_002,
            end_height: 9223372036854775807,
            block_limit: ExecutionCost {
                write_length: 100000000,
                write_count: 1000,
                read_length: 1000000000,
                read_count: 50,
                runtime: 5000000000,
            },
            network_epoch: PEER_VERSION_EPOCH_2_1,
        },
    ]));
    conf.burnchain.pox_2_activation = Some(10_003);

    conf.initial_balances.push(InitialBalance {
        address: alice_pd.clone(),
        amount: 10492300000,
    });
    conf.initial_balances.push(InitialBalance {
        address: bob_pd.clone(),
        amount: 10492300000,
    });
    conf.initial_balances.push(InitialBalance {
        address: creator_pd.clone(),
        amount: 10492300000,
    });

    test_observer::spawn();
    test_observer::register_any(&mut conf);

    let mut btcd_controller = BitcoinCoreController::new(conf.clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::new(conf.clone(), None);
    let http_origin = format!("http://{}", &conf.node.rpc_bind);

    btc_regtest_controller.bootstrap_chain(200);

    let mut run_loop = neon::RunLoop::new(conf.clone());
    let blocks_processed = run_loop.get_blocks_processed_arc();

    let channel = run_loop.get_coordinator_channel().unwrap();

    thread::spawn(move || run_loop.start(None, 0));

    // Wait until block 210, so we now that the burnchain is ready.
    wait_for_runloop(&blocks_processed);
    run_until_burnchain_height(&mut btc_regtest_controller, &blocks_processed, 210, &conf);

    // Publish the contract so we can use it.
    submit_tx(
        &http_origin,
        &make_contract_publish(
            &creator_sk,
            0,
            1100000,
            conf.burnchain.chain_id,
            "increment-contract",
            &giant_contract,
        ),
    );

    // Wait to make sure the contract is published.
    run_until_burnchain_height(&mut btc_regtest_controller, &blocks_processed, 212, &conf);

    // Check that we have defined the contract.
    let increment_contract_defines = select_transactions_where(
        &test_observer::get_blocks(),
        |transaction| match &transaction.payload {
            TransactionPayload::SmartContract(contract, ..) => {
                contract.name == ContractName::from("increment-contract")
            }
            _ => false,
        },
    );
    assert_eq!(increment_contract_defines.len(), 1);

    // Alice calls the contract and should succeed, because we have not lowered the block limit
    // yet.
    submit_tx(
        &http_origin,
        &make_contract_call(
            &alice_sk,
            0,
            1000,
            conf.burnchain.chain_id,
            &creator_addr,
            "increment-contract",
            "increment-many",
            &[],
        ),
    );

    // Wait for the contract call to process.
    run_until_burnchain_height(&mut btc_regtest_controller, &blocks_processed, 214, &conf);

    // Check that we have processed the contract successfully, by checking that the contract call
    // is in the block record.
    let increment_calls_alice = select_transactions_where(
        &test_observer::get_blocks(),
        |transaction| match &transaction.payload {
            TransactionPayload::ContractCall(contract) => {
                contract.contract_name == ContractName::from("increment-contract")
            }
            _ => false,
        },
    );
    assert_eq!(increment_calls_alice.len(), 1);

    // Clear the observer so we can look for Bob's transaction.
    test_observer::clear();

    // The cost contract was switched at height 220. So, now we expect Bob's call to fail.
    run_until_burnchain_height(&mut btc_regtest_controller, &blocks_processed, 216, &conf);
    submit_tx(
        &http_origin,
        &make_contract_call(
            &bob_sk,
            0,
            1000,
            conf.burnchain.chain_id,
            &creator_addr,
            "increment-contract",
            "increment-many",
            &[],
        ),
    );

    // Wait for the contract to finish.
    run_until_burnchain_height(&mut btc_regtest_controller, &blocks_processed, 218, &conf);

    // Bob's calls didn't work because he called after the block limit was lowered.
    let increment_calls_bob = select_transactions_where(
        &test_observer::get_blocks(),
        |transaction| match &transaction.payload {
            TransactionPayload::ContractCall(contract) => {
                contract.contract_name == ContractName::from("increment-contract")
            }
            _ => false,
        },
    );
    assert_eq!(increment_calls_bob.len(), 0);

    channel.stop_chains_coordinator();
}

// mine a stream of microblocks, and verify that microblock streams can get bigger after the epoch
// transition
#[test]
#[ignore]
fn bigger_microblock_streams_in_2_05() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let spender_sks: Vec<_> = (0..10).map(|_| StacksPrivateKey::new()).collect();
    let spender_addrs: Vec<PrincipalData> = spender_sks.iter().map(|x| to_addr(x).into()).collect();

    let (mut conf, miner_account) = neon_integration_test_conf();

    for spender_addr in spender_addrs.iter() {
        conf.initial_balances.push(InitialBalance {
            address: spender_addr.clone(),
            amount: 10492300000,
        });
    }

    conf.node.mine_microblocks = true;
    conf.node.wait_time_for_microblocks = 0;
    conf.node.microblock_frequency = 0;
    conf.node.max_microblocks = 65536;
    conf.burnchain.max_rbf = 1000000;

    conf.miner.first_attempt_time_ms = i64::MAX as u64;
    conf.miner.subsequent_attempt_time_ms = i64::MAX as u64;

    conf.burnchain.epochs = Some(EpochList::new(&[
        StacksEpoch {
            epoch_id: StacksEpochId::Epoch20,
            start_height: 0,
            end_height: 206,
            block_limit: ExecutionCost {
                write_length: 15000000,
                write_count: 7750,
                read_length: 100000000,
                read_count: 7750,
                runtime: 5000000000,
            },
            network_epoch: PEER_VERSION_EPOCH_2_0,
        },
        StacksEpoch {
            epoch_id: StacksEpochId::Epoch2_05,
            start_height: 206,
            end_height: 10_002,
            block_limit: ExecutionCost {
                write_length: 15000000,
                write_count: 7750 * 2,
                read_length: 100000000,
                read_count: 7750 * 2,
                runtime: 5000000000,
            },
            network_epoch: PEER_VERSION_EPOCH_2_05,
        },
        StacksEpoch {
            epoch_id: StacksEpochId::Epoch21,
            start_height: 10_002,
            end_height: 9223372036854775807,
            block_limit: ExecutionCost {
                write_length: 15000000,
                write_count: 7750 * 2,
                read_length: 100000000,
                read_count: 7750 * 2,
                runtime: 5000000000,
            },
            network_epoch: PEER_VERSION_EPOCH_2_05,
        },
    ]));
    conf.burnchain.pox_2_activation = Some(10_003);

    let txs: Vec<Vec<_>> = spender_sks
        .iter()
        .enumerate()
        .map(|(ix, spender_sk)| {
            // almost fills a whole block
            make_contract_publish_microblock_only(
                spender_sk,
                0,
                1049230,
                conf.burnchain.chain_id,
                &format!("large-{ix}"),
                &format!("
                    ;; a single one of these transactions consumes over half the runtime budget
                    (define-constant BUFF_TO_BYTE (list
                       0x00 0x01 0x02 0x03 0x04 0x05 0x06 0x07 0x08 0x09 0x0a 0x0b 0x0c 0x0d 0x0e 0x0f
                       0x10 0x11 0x12 0x13 0x14 0x15 0x16 0x17 0x18 0x19 0x1a 0x1b 0x1c 0x1d 0x1e 0x1f
                       0x20 0x21 0x22 0x23 0x24 0x25 0x26 0x27 0x28 0x29 0x2a 0x2b 0x2c 0x2d 0x2e 0x2f
                       0x30 0x31 0x32 0x33 0x34 0x35 0x36 0x37 0x38 0x39 0x3a 0x3b 0x3c 0x3d 0x3e 0x3f
                       0x40 0x41 0x42 0x43 0x44 0x45 0x46 0x47 0x48 0x49 0x4a 0x4b 0x4c 0x4d 0x4e 0x4f
                       0x50 0x51 0x52 0x53 0x54 0x55 0x56 0x57 0x58 0x59 0x5a 0x5b 0x5c 0x5d 0x5e 0x5f
                       0x60 0x61 0x62 0x63 0x64 0x65 0x66 0x67 0x68 0x69 0x6a 0x6b 0x6c 0x6d 0x6e 0x6f
                       0x70 0x71 0x72 0x73 0x74 0x75 0x76 0x77 0x78 0x79 0x7a 0x7b 0x7c 0x7d 0x7e 0x7f
                       0x80 0x81 0x82 0x83 0x84 0x85 0x86 0x87 0x88 0x89 0x8a 0x8b 0x8c 0x8d 0x8e 0x8f
                       0x90 0x91 0x92 0x93 0x94 0x95 0x96 0x97 0x98 0x99 0x9a 0x9b 0x9c 0x9d 0x9e 0x9f
                       0xa0 0xa1 0xa2 0xa3 0xa4 0xa5 0xa6 0xa7 0xa8 0xa9 0xaa 0xab 0xac 0xad 0xae 0xaf
                       0xb0 0xb1 0xb2 0xb3 0xb4 0xb5 0xb6 0xb7 0xb8 0xb9 0xba 0xbb 0xbc 0xbd 0xbe 0xbf
                       0xc0 0xc1 0xc2 0xc3 0xc4 0xc5 0xc6 0xc7 0xc8 0xc9 0xca 0xcb 0xcc 0xcd 0xce 0xcf
                       0xd0 0xd1 0xd2 0xd3 0xd4 0xd5 0xd6 0xd7 0xd8 0xd9 0xda 0xdb 0xdc 0xdd 0xde 0xdf
                       0xe0 0xe1 0xe2 0xe3 0xe4 0xe5 0xe6 0xe7 0xe8 0xe9 0xea 0xeb 0xec 0xed 0xee 0xef
                       0xf0 0xf1 0xf2 0xf3 0xf4 0xf5 0xf6 0xf7 0xf8 0xf9 0xfa 0xfb 0xfc 0xfd 0xfe 0xff
                    ))
                    (define-private (crash-me-folder (input (buff 1)) (ctr uint))
                        (begin
                            (unwrap-panic (index-of BUFF_TO_BYTE input))
                            (unwrap-panic (index-of BUFF_TO_BYTE input))
                            (unwrap-panic (index-of BUFF_TO_BYTE input))
                            (unwrap-panic (index-of BUFF_TO_BYTE input))
                            (unwrap-panic (index-of BUFF_TO_BYTE input))
                            (unwrap-panic (index-of BUFF_TO_BYTE input))
                            (unwrap-panic (index-of BUFF_TO_BYTE input))
                            (unwrap-panic (index-of BUFF_TO_BYTE input))
                            (+ u1 ctr)
                        )
                    )
                    (define-public (crash-me (name (string-ascii 128)))
                        (begin
                            (fold crash-me-folder BUFF_TO_BYTE u0)
                            (print name)
                            (ok u0)
                        )
                    )
                    (begin
                        (crash-me \"large-contract-{ix}\"))
                    "
                )
            )
        })
        .collect();

    test_observer::spawn();
    test_observer::register_any(&mut conf);

    let mut btcd_controller = BitcoinCoreController::new(conf.clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::new(conf.clone(), None);
    let http_origin = format!("http://{}", &conf.node.rpc_bind);

    btc_regtest_controller.bootstrap_chain(201);

    eprintln!("Chain bootstrapped...");

    let mut run_loop = neon::RunLoop::new(conf);
    let blocks_processed = run_loop.get_blocks_processed_arc();
    let microblocks_processed = run_loop.get_microblocks_processed_arc();

    let channel = run_loop.get_coordinator_channel().unwrap();

    thread::spawn(move || run_loop.start(None, 0));

    // give the run loop some time to start up!
    wait_for_runloop(&blocks_processed);

    // zeroth block wakes up the run loop
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // first block will hold our VRF registration
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // second block will be the first mined Stacks block
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // let's query the miner's account nonce:
    let account = get_account(&http_origin, &miner_account);
    assert_eq!(account.nonce, 1);
    assert_eq!(account.balance, 0);

    for spender_addr in spender_addrs.iter() {
        let account = get_account(&http_origin, &spender_addr);
        assert_eq!(account.nonce, 0);
        assert_eq!(account.balance, 10492300000);
    }

    let mut ctr = 0;
    while ctr < txs.len() {
        submit_tx(&http_origin, &txs[ctr]);
        if !wait_for_microblocks(&microblocks_processed, 30) {
            // we time out if we *can't* mine any more microblocks
            break;
        }
        ctr += 1;
    }
    microblocks_processed.store(0, Ordering::SeqCst);

    // only one fit
    assert_eq!(ctr, 1);
    sleep_ms(5_000);

    // confirm it
    eprintln!("confirm epoch 2.0 microblock stream");
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // send the rest of the transactions
    while ctr < txs.len() {
        submit_tx(&http_origin, &txs[ctr]);
        ctr += 1;
    }

    eprintln!("expect epoch transition");

    microblocks_processed.store(0, Ordering::SeqCst);

    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    // don't bother waiting for a microblock stream

    eprintln!("expect epoch 2.05 microblock stream");

    microblocks_processed.store(0, Ordering::SeqCst);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    wait_for_microblocks(&microblocks_processed, 180);

    microblocks_processed.store(0, Ordering::SeqCst);

    // this test can sometimes miss a mine block event.
    sleep_ms(120_000);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let mut epoch_20_stream_cost = ExecutionCost::ZERO;
    let mut epoch_205_stream_cost = ExecutionCost::ZERO;

    // max == largest number of transactions per stream in a given epoch (2.0 or 2.05)
    // total == number of transactions across all streams in a given epoch (2.0 or 2.05)
    let mut max_big_txs_per_microblock_20 = 0;
    let mut total_big_txs_per_microblock_20 = 0;

    let mut max_big_txs_per_microblock_205 = 0;
    let mut total_big_txs_per_microblock_205 = 0;

    let mut in_205;
    let mut have_confirmed_205_stream;

    for i in 0..10 {
        let blocks = test_observer::get_blocks();

        max_big_txs_per_microblock_20 = 0;
        total_big_txs_per_microblock_20 = 0;

        max_big_txs_per_microblock_205 = 0;
        total_big_txs_per_microblock_205 = 0;

        in_205 = false;
        have_confirmed_205_stream = false;

        // NOTE: this only counts the number of txs per stream, not in each microblock
        for block in blocks {
            let transactions = block.get("transactions").unwrap().as_array().unwrap();
            eprintln!("{}", transactions.len());

            let mut num_big_microblock_txs = 0;
            let mut total_execution_cost = ExecutionCost::ZERO;

            for tx in transactions.iter() {
                let raw_tx = tx.get("raw_tx").unwrap().as_str().unwrap();
                if raw_tx == "0x00" {
                    continue;
                }
                let tx_bytes = hex_bytes(&raw_tx[2..]).unwrap();
                let parsed = StacksTransaction::consensus_deserialize(&mut &tx_bytes[..]).unwrap();
                if let TransactionPayload::SmartContract(tsc, ..) = parsed.payload {
                    if tsc.name.to_string().contains("costs-2") {
                        in_205 = true;
                    } else if tsc.name.to_string().contains("large") {
                        num_big_microblock_txs += 1;
                        if in_205 {
                            total_big_txs_per_microblock_205 += 1;
                        } else {
                            total_big_txs_per_microblock_20 += 1;
                        }
                    }
                }
                let execution_cost = tx.get("execution_cost").unwrap();
                total_execution_cost.read_count +=
                    execution_cost.get("read_count").unwrap().as_i64().unwrap() as u64;
                total_execution_cost.read_length +=
                    execution_cost.get("read_length").unwrap().as_i64().unwrap() as u64;
                total_execution_cost.write_count +=
                    execution_cost.get("write_count").unwrap().as_i64().unwrap() as u64;
                total_execution_cost.write_length += execution_cost
                    .get("write_length")
                    .unwrap()
                    .as_i64()
                    .unwrap() as u64;
                total_execution_cost.runtime +=
                    execution_cost.get("runtime").unwrap().as_i64().unwrap() as u64;
            }
            if in_205 && num_big_microblock_txs > max_big_txs_per_microblock_205 {
                max_big_txs_per_microblock_205 = num_big_microblock_txs;
            }
            if !in_205 && num_big_microblock_txs > max_big_txs_per_microblock_20 {
                max_big_txs_per_microblock_20 = num_big_microblock_txs;
            }

            eprintln!("Epoch size: {total_execution_cost:?}");

            if !in_205 && total_execution_cost.exceeds(&epoch_20_stream_cost) {
                epoch_20_stream_cost = total_execution_cost;
                break;
            }
            if in_205 && total_execution_cost.exceeds(&ExecutionCost::ZERO) {
                have_confirmed_205_stream = true;
                epoch_205_stream_cost = total_execution_cost;
                break;
            }
        }

        if have_confirmed_205_stream {
            break;
        } else {
            eprintln!("Trying to confirm a stream again (attempt {})", i + 1);
            sleep_ms((i + 2) * 60_000);
            next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
        }
    }

    eprintln!(
        "max_big_txs_per_microblock_20: {max_big_txs_per_microblock_20}, total_big_txs_per_microblock_20: {total_big_txs_per_microblock_20}"
    );
    eprintln!(
        "max_big_txs_per_microblock_205: {max_big_txs_per_microblock_205}, total_big_txs_per_microblock_205: {total_big_txs_per_microblock_205}"
    );
    eprintln!("confirmed stream execution in 2.0: {epoch_20_stream_cost:?}");
    eprintln!("confirmed stream execution in 2.05: {epoch_205_stream_cost:?}");

    // stuff happened
    assert!(epoch_20_stream_cost.runtime > 0);
    assert!(epoch_205_stream_cost.runtime > 0);

    // more stuff happened in epoch 2.05
    assert!(epoch_205_stream_cost.read_count > epoch_20_stream_cost.read_count);
    assert!(epoch_205_stream_cost.read_length > epoch_20_stream_cost.read_length);
    assert!(epoch_205_stream_cost.write_count > epoch_20_stream_cost.write_count);
    assert!(epoch_205_stream_cost.write_length > epoch_20_stream_cost.write_length);

    // but epoch 2.05 was *cheaper* in terms of CPU
    assert!(epoch_205_stream_cost.runtime < epoch_20_stream_cost.runtime);

    test_observer::clear();
    channel.stop_chains_coordinator();
}
