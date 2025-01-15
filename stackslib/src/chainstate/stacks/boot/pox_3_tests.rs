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

use std::collections::{HashMap, HashSet, VecDeque};

use clarity::vm::clarity::ClarityConnection;
use clarity::vm::contexts::OwnedEnvironment;
use clarity::vm::contracts::Contract;
use clarity::vm::costs::{CostOverflowingMath, LimitedCostTracker};
use clarity::vm::database::*;
use clarity::vm::errors::{
    CheckErrors, Error, IncomparableError, InterpreterError, InterpreterResult, RuntimeErrorType,
};
use clarity::vm::eval;
use clarity::vm::events::StacksTransactionEvent;
use clarity::vm::representations::SymbolicExpression;
use clarity::vm::tests::{execute, is_committed, is_err_code, symbols_from_values};
use clarity::vm::types::Value::Response;
use clarity::vm::types::{
    BuffData, OptionalData, PrincipalData, QualifiedContractIdentifier, ResponseData, SequenceData,
    StacksAddressExtensions, StandardPrincipalData, TupleData, TupleTypeSignature, TypeSignature,
    Value, NONE,
};
use stacks_common::address::AddressHashMode;
use stacks_common::types::chainstate::{
    BlockHeaderHash, BurnchainHeaderHash, StacksAddress, StacksBlockId, VRFSeed,
};
use stacks_common::types::Address;
use stacks_common::util::hash::{hex_bytes, to_hex, Sha256Sum, Sha512Trunc256Sum};

use super::test::*;
use super::RawRewardSetEntry;
use crate::burnchains::{Burnchain, PoxConstants};
use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::burn::operations::*;
use crate::chainstate::burn::{BlockSnapshot, ConsensusHash};
use crate::chainstate::stacks::address::{PoxAddress, PoxAddressType20, PoxAddressType32};
use crate::chainstate::stacks::boot::pox_2_tests::{
    check_pox_print_event, check_stacking_state_invariants, generate_pox_clarity_value,
    get_partial_stacked, get_reward_cycle_total, get_reward_set_entries_at, get_stacking_state_pox,
    get_stacking_state_pox_2, get_stx_account_at, PoxPrintFields, StackingStateCheckData,
};
use crate::chainstate::stacks::boot::{
    BOOT_CODE_COST_VOTING_TESTNET as BOOT_CODE_COST_VOTING, BOOT_CODE_POX_TESTNET, POX_2_NAME,
    POX_3_NAME,
};
use crate::chainstate::stacks::db::{
    MinerPaymentSchedule, StacksChainState, StacksHeaderInfo, MINER_REWARD_MATURITY,
};
use crate::chainstate::stacks::events::TransactionOrigin;
use crate::chainstate::stacks::index::marf::MarfConnection;
use crate::chainstate::stacks::index::MarfTrieId;
use crate::chainstate::stacks::tests::make_coinbase;
use crate::chainstate::stacks::*;
use crate::clarity_vm::clarity::{ClarityBlockConnection, Error as ClarityError};
use crate::clarity_vm::database::marf::{MarfedKV, WritableMarfStore};
use crate::clarity_vm::database::HeadersDBConn;
use crate::core::*;
use crate::net::test::{TestEventObserver, TestPeer};
use crate::util_lib::boot::boot_code_id;
use crate::util_lib::db::{DBConn, FromRow};

const USTX_PER_HOLDER: u128 = 1_000_000;

/// Return the BlockSnapshot for the latest sortition in the provided
///  SortitionDB option-reference. Panics on any errors.
fn get_tip(sortdb: Option<&SortitionDB>) -> BlockSnapshot {
    SortitionDB::get_canonical_burn_chain_tip(sortdb.unwrap().conn()).unwrap()
}

fn make_test_epochs_pox() -> (EpochList, PoxConstants) {
    let EMPTY_SORTITIONS = 25;
    let EPOCH_2_1_HEIGHT = EMPTY_SORTITIONS + 11; // 36
    let EPOCH_2_2_HEIGHT = EPOCH_2_1_HEIGHT + 14; // 50
    let EPOCH_2_3_HEIGHT = EPOCH_2_2_HEIGHT + 2; // 52
                                                 // epoch-2.4 will start at the first block of cycle 11!
                                                 //  this means that cycle 11 should also be treated like a "burn"
    let EPOCH_2_4_HEIGHT = EPOCH_2_2_HEIGHT + 6; // 56

    // cycle 11 = 60

    let epochs = EpochList::new(&[
        StacksEpoch {
            epoch_id: StacksEpochId::Epoch10,
            start_height: 0,
            end_height: 0,
            block_limit: ExecutionCost::max_value(),
            network_epoch: PEER_VERSION_EPOCH_1_0,
        },
        StacksEpoch {
            epoch_id: StacksEpochId::Epoch20,
            start_height: 0,
            end_height: 0,
            block_limit: ExecutionCost::max_value(),
            network_epoch: PEER_VERSION_EPOCH_2_0,
        },
        StacksEpoch {
            epoch_id: StacksEpochId::Epoch2_05,
            start_height: 0,
            end_height: EPOCH_2_1_HEIGHT,
            block_limit: ExecutionCost::max_value(),
            network_epoch: PEER_VERSION_EPOCH_2_05,
        },
        StacksEpoch {
            epoch_id: StacksEpochId::Epoch21,
            start_height: EPOCH_2_1_HEIGHT,
            end_height: EPOCH_2_2_HEIGHT,
            block_limit: ExecutionCost::max_value(),
            network_epoch: PEER_VERSION_EPOCH_2_1,
        },
        StacksEpoch {
            epoch_id: StacksEpochId::Epoch22,
            start_height: EPOCH_2_2_HEIGHT,
            end_height: EPOCH_2_3_HEIGHT,
            block_limit: ExecutionCost::max_value(),
            network_epoch: PEER_VERSION_EPOCH_2_2,
        },
        StacksEpoch {
            epoch_id: StacksEpochId::Epoch23,
            start_height: EPOCH_2_3_HEIGHT,
            end_height: EPOCH_2_4_HEIGHT,
            block_limit: ExecutionCost::max_value(),
            network_epoch: PEER_VERSION_EPOCH_2_3,
        },
        StacksEpoch {
            epoch_id: StacksEpochId::Epoch24,
            start_height: EPOCH_2_4_HEIGHT,
            end_height: STACKS_EPOCH_MAX,
            block_limit: ExecutionCost::max_value(),
            network_epoch: PEER_VERSION_EPOCH_2_4,
        },
    ]);

    let mut pox_constants = PoxConstants::mainnet_default();
    pox_constants.reward_cycle_length = 5;
    pox_constants.prepare_length = 2;
    pox_constants.anchor_threshold = 1;
    pox_constants.v1_unlock_height = (EPOCH_2_1_HEIGHT + 1) as u32;
    pox_constants.v2_unlock_height = (EPOCH_2_2_HEIGHT + 1) as u32;
    pox_constants.v3_unlock_height = u32::MAX;
    pox_constants.pox_3_activation_height = (EPOCH_2_4_HEIGHT + 1) as u32;
    pox_constants.pox_4_activation_height = u32::MAX;

    (epochs, pox_constants)
}

/// In this test case, two Stackers, Alice and Bob stack and interact with the
///  PoX v1 contract and PoX v2 contract across the epoch transition and then
///  again with the PoX v3 contract.
///
/// Alice: stacks via PoX v1 for 4 cycles. The third of these cycles occurs after
///        the PoX v1 -> v2 transition, and so Alice gets "early unlocked".
///        After the early unlock, Alice re-stacks in PoX v2
/// Bob:   stacks via PoX v2 for 6 cycles. He attempted to stack via PoX v1 as well,
///        but is forbidden because he has already placed an account lock via PoX v2.
///        
/// After the PoX-3 contract is instantiated, Alice and Bob both stack via PoX v3.
///
#[test]
fn simple_pox_lockup_transition_pox_2() {
    let EXPECTED_FIRST_V2_CYCLE = 8;
    // the sim environment produces 25 empty sortitions before
    //  tenures start being tracked.
    let EMPTY_SORTITIONS = 25;

    let (epochs, pox_constants) = make_test_epochs_pox();

    let mut burnchain = Burnchain::default_unittest(
        0,
        &BurnchainHeaderHash::from_hex(BITCOIN_REGTEST_FIRST_BLOCK_HASH).unwrap(),
    );
    burnchain.pox_constants = pox_constants.clone();

    let first_v2_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.v1_unlock_height as u64)
        .unwrap()
        + 1;

    assert_eq!(first_v2_cycle, EXPECTED_FIRST_V2_CYCLE);

    eprintln!("First v2 cycle = {}", first_v2_cycle);

    let observer = TestEventObserver::new();

    let (mut peer, mut keys) = instantiate_pox_peer_with_epoch(
        &burnchain,
        function_name!(),
        Some(epochs.clone()),
        Some(&observer),
    );

    peer.config.check_pox_invariants =
        Some((EXPECTED_FIRST_V2_CYCLE, EXPECTED_FIRST_V2_CYCLE + 20));

    let alice = keys.pop().unwrap();
    let bob = keys.pop().unwrap();
    let charlie = keys.pop().unwrap();

    let EXPECTED_ALICE_FIRST_REWARD_CYCLE = 6;

    let mut coinbase_nonce = 0;

    // our "tenure counter" is now at 0
    let tip = get_tip(peer.sortdb.as_ref());
    assert_eq!(tip.block_height, 0 + EMPTY_SORTITIONS as u64);

    // first tenure is empty
    peer.tenure_with_txs(&[], &mut coinbase_nonce);

    let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
    assert_eq!(alice_balance, 1024 * POX_THRESHOLD_STEPS_USTX);

    let alice_account = get_account(&mut peer, &key_to_stacks_addr(&alice).into());
    assert_eq!(
        alice_account.stx_balance.amount_unlocked(),
        1024 * POX_THRESHOLD_STEPS_USTX
    );
    assert_eq!(alice_account.stx_balance.amount_locked(), 0);
    assert_eq!(alice_account.stx_balance.unlock_height(), 0);

    // next tenure include Alice's lockup
    let tip = get_tip(peer.sortdb.as_ref());
    let alice_lockup = make_pox_lockup(
        &alice,
        0,
        1024 * POX_THRESHOLD_STEPS_USTX,
        AddressHashMode::SerializeP2PKH,
        key_to_stacks_addr(&alice).bytes,
        4,
        tip.block_height,
    );

    // our "tenure counter" is now at 1
    assert_eq!(tip.block_height, 1 + EMPTY_SORTITIONS as u64);

    let tip_index_block = peer.tenure_with_txs(&[alice_lockup], &mut coinbase_nonce);

    // check the stacking minimum
    let total_liquid_ustx = get_liquid_ustx(&mut peer);
    let min_ustx = with_sortdb(&mut peer, |ref mut chainstate, sortdb| {
        chainstate.get_stacking_minimum(sortdb, &tip_index_block)
    })
    .unwrap();
    assert_eq!(
        min_ustx,
        total_liquid_ustx / POX_TESTNET_STACKING_THRESHOLD_25
    );

    // no reward addresses
    let reward_addrs = with_sortdb(&mut peer, |ref mut chainstate, sortdb| {
        get_reward_addresses_with_par_tip(chainstate, &burnchain, sortdb, &tip_index_block)
    })
    .unwrap();
    assert_eq!(reward_addrs.len(), 0);

    // check the first reward cycle when Alice's tokens get stacked
    let tip_burn_block_height = get_par_burn_block_height(peer.chainstate(), &tip_index_block);
    let alice_first_reward_cycle = 1 + burnchain
        .block_height_to_reward_cycle(tip_burn_block_height)
        .unwrap() as u128;

    assert_eq!(alice_first_reward_cycle, EXPECTED_ALICE_FIRST_REWARD_CYCLE);

    // alice locked, so balance should be 0
    let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
    assert_eq!(alice_balance, 0);

    // produce blocks until immediately before the 2.1 epoch switch
    while get_tip(peer.sortdb.as_ref()).block_height < epochs[StacksEpochId::Epoch21].start_height {
        peer.tenure_with_txs(&[], &mut coinbase_nonce);

        // alice is still locked, balance should be 0
        let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
        assert_eq!(alice_balance, 0);
    }

    // Have Charlie try to use the PoX2 contract. This transaction
    //  should be accepted (checked via the tx receipt). Also, importantly,
    //  the cost tracker should assign costs to Charlie's transaction.
    //  This is also checked by the transaction receipt.
    let tip = get_tip(peer.sortdb.as_ref());

    let test = make_pox_2_contract_call(
        &charlie,
        0,
        "delegate-stx",
        vec![
            Value::UInt(1_000_000),
            PrincipalData::from(key_to_stacks_addr(&charlie)).into(),
            Value::none(),
            Value::none(),
        ],
    );
    peer.tenure_with_txs(&[test], &mut coinbase_nonce);

    // alice is still locked, balance should be 0
    let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
    assert_eq!(alice_balance, 0);

    // in the next tenure, PoX 2 should now exist.
    // Lets have Bob lock up for v2
    // this will lock for cycles 8, 9, 10, and 11
    //  the first v2 cycle will be 8
    let tip = get_tip(peer.sortdb.as_ref());

    let bob_lockup = make_pox_2_lockup(
        &bob,
        0,
        512 * POX_THRESHOLD_STEPS_USTX,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&bob).bytes,
        ),
        6,
        tip.block_height,
    );

    let block_id = peer.tenure_with_txs(&[bob_lockup], &mut coinbase_nonce);

    assert_eq!(
        get_tip(peer.sortdb.as_ref()).block_height as u32,
        pox_constants.v1_unlock_height + 1,
        "Test should have reached 1 + PoX-v1 unlock height"
    );

    // Auto unlock height is reached, Alice balance should be unlocked
    let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
    assert_eq!(alice_balance, 1024 * POX_THRESHOLD_STEPS_USTX);

    // Now, Bob tries to lock in PoX v1 too, but it shouldn't work!
    let tip = get_tip(peer.sortdb.as_ref());

    let bob_lockup = make_pox_lockup(
        &bob,
        1,
        512 * POX_THRESHOLD_STEPS_USTX,
        AddressHashMode::SerializeP2PKH,
        key_to_stacks_addr(&bob).bytes,
        4,
        tip.block_height,
    );

    let block_id = peer.tenure_with_txs(&[bob_lockup], &mut coinbase_nonce);

    // At this point, the auto unlock height for v1 accounts has been reached.
    //  let Alice stack in PoX v2
    let tip = get_tip(peer.sortdb.as_ref());

    let alice_lockup = make_pox_2_lockup(
        &alice,
        1,
        512 * POX_THRESHOLD_STEPS_USTX,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&alice).bytes,
        ),
        12,
        tip.block_height,
    );
    peer.tenure_with_txs(&[alice_lockup], &mut coinbase_nonce);

    // Alice locked half her balance in PoX 2
    let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
    assert_eq!(alice_balance, 512 * POX_THRESHOLD_STEPS_USTX);

    // now, let's roll the chain forward until just before Epoch-2.2
    while get_tip(peer.sortdb.as_ref()).block_height < epochs[StacksEpochId::Epoch22].start_height {
        peer.tenure_with_txs(&[], &mut coinbase_nonce);
        // at this point, alice's balance should always include this half lockup
        let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
        assert_eq!(alice_balance, 512 * POX_THRESHOLD_STEPS_USTX);
    }

    // this block is mined in epoch-2.2
    peer.tenure_with_txs(&[], &mut coinbase_nonce);
    let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
    assert_eq!(alice_balance, 512 * POX_THRESHOLD_STEPS_USTX);
    // this block should unlock alice's balance
    peer.tenure_with_txs(&[], &mut coinbase_nonce);
    let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
    assert_eq!(alice_balance, 1024 * POX_THRESHOLD_STEPS_USTX);

    // now, roll the chain forward to Epoch-2.4
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch24].start_height
    {
        peer.tenure_with_txs(&[], &mut coinbase_nonce);
        // at this point, alice's balance should always be unlocked
        let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
        assert_eq!(alice_balance, 1024 * POX_THRESHOLD_STEPS_USTX);
    }

    let tip = get_tip(peer.sortdb.as_ref()).block_height;
    let bob_lockup = make_pox_3_lockup(
        &bob,
        2,
        512 * POX_THRESHOLD_STEPS_USTX,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&bob).bytes,
        ),
        6,
        tip,
    );

    let alice_lockup = make_pox_3_lockup(
        &alice,
        2,
        512 * POX_THRESHOLD_STEPS_USTX,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&alice).bytes,
        ),
        6,
        tip,
    );

    peer.tenure_with_txs(&[bob_lockup, alice_lockup], &mut coinbase_nonce);

    let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
    assert_eq!(alice_balance, 512 * POX_THRESHOLD_STEPS_USTX);
    let bob_balance = get_balance(&mut peer, &key_to_stacks_addr(&bob).into());
    assert_eq!(bob_balance, 512 * POX_THRESHOLD_STEPS_USTX);

    // now let's check some tx receipts

    let alice_address = key_to_stacks_addr(&alice);
    let bob_address = key_to_stacks_addr(&bob);
    let blocks = observer.get_blocks();

    let mut alice_txs = HashMap::new();
    let mut bob_txs = HashMap::new();
    let mut charlie_txs = HashMap::new();

    debug!("Alice addr: {}, Bob addr: {}", alice_address, bob_address);

    let mut tested_charlie = false;

    for b in blocks.into_iter() {
        for r in b.receipts.into_iter() {
            if let TransactionOrigin::Stacks(ref t) = r.transaction {
                let addr = t.auth.origin().address_testnet();
                debug!("Transaction addr: {}", addr);
                if addr == alice_address {
                    alice_txs.insert(t.auth.get_origin_nonce(), r);
                } else if addr == bob_address {
                    bob_txs.insert(t.auth.get_origin_nonce(), r);
                } else if addr == key_to_stacks_addr(&charlie) {
                    assert!(
                        r.execution_cost != ExecutionCost::ZERO,
                        "Execution cost is not zero!"
                    );
                    charlie_txs.insert(t.auth.get_origin_nonce(), r);

                    tested_charlie = true;
                }
            }
        }
    }

    assert!(tested_charlie, "Charlie TX must be tested");
    // Alice should have three accepted transactions:
    //  TX0 -> Alice's initial lockup in PoX 1
    //  TX1 -> Alice's PoX 2 lockup
    //  TX2 -> Alice's PoX 3 lockup
    assert_eq!(alice_txs.len(), 3, "Alice should have 3 confirmed txs");
    // Bob should have two accepted transactions:
    //  TX0 -> Bob's initial lockup in PoX 2
    //  TX1 -> Bob's attempt to lock again in PoX 1 -- this one should fail
    //         because PoX 1 is now defunct. Checked via the tx receipt.
    //  TX2 -> Bob's PoX 3 lockup
    assert_eq!(bob_txs.len(), 3, "Bob should have 3 confirmed txs");
    // Charlie should have one accepted transactions:
    //  TX0 -> Charlie's delegation in PoX 2. This tx just checks that the
    //         initialization code tracks costs in txs that occur after the
    //         initialization code (which uses a free tracker).
    assert_eq!(charlie_txs.len(), 1, "Charlie should have 1 confirmed txs");

    //  TX0 -> Alice's initial lockup in PoX 1
    assert!(
        match alice_txs.get(&0).unwrap().result {
            Value::Response(ref r) => r.committed,
            _ => false,
        },
        "Alice tx0 should have committed okay"
    );

    //  TX1 -> Alice's PoX 2 lockup
    assert!(
        match alice_txs.get(&1).unwrap().result {
            Value::Response(ref r) => r.committed,
            _ => false,
        },
        "Alice tx1 should have committed okay"
    );

    //  TX2 -> Alice's PoX 3 lockup
    assert!(
        match alice_txs.get(&1).unwrap().result {
            Value::Response(ref r) => r.committed,
            _ => false,
        },
        "Alice tx3 should have committed okay"
    );

    //  TX0 -> Bob's initial lockup in PoX 2
    assert!(
        match bob_txs.get(&0).unwrap().result {
            Value::Response(ref r) => r.committed,
            _ => false,
        },
        "Bob tx0 should have committed okay"
    );

    //  TX1 -> Bob's attempt to lock again in PoX 1 -- this one should fail
    //         because PoX 1 is now defunct. Checked via the tx receipt.
    assert_eq!(
        bob_txs.get(&1).unwrap().result,
        Value::err_none(),
        "Bob tx1 should have resulted in a runtime error"
    );

    //  TX0 -> Charlie's delegation in PoX 2. This tx just checks that the
    //         initialization code tracks costs in txs that occur after the
    //         initialization code (which uses a free tracker).
    assert!(
        match charlie_txs.get(&0).unwrap().result {
            Value::Response(ref r) => r.committed,
            _ => false,
        },
        "Charlie tx0 should have committed okay"
    );
}

#[test]
fn pox_auto_unlock_ab() {
    pox_auto_unlock(true)
}

#[test]
fn pox_auto_unlock_ba() {
    pox_auto_unlock(false)
}

/// In this test case, two Stackers, Alice and Bob stack and interact with the
///  PoX v1 contract and PoX v2 contract across the epoch transition, and then again
///  in PoX v3.
///
/// Alice: stacks via PoX v1 for 4 cycles. The third of these cycles occurs after
///        the PoX v1 -> v2 transition, and so Alice gets "early unlocked".
///        After the early unlock, Alice re-stacks in PoX v2
/// Bob:   stacks via PoX v2 for 6 cycles. He attempted to stack via PoX v1 as well,
///        but is forbidden because he has already placed an account lock via PoX v2.
///
/// Note: this test is symmetric over the order of alice and bob's stacking calls.
///       when alice goes first, the auto-unlock code doesn't need to perform a "move"
///       when bob goes first, the auto-unlock code does need to perform a "move"
fn pox_auto_unlock(alice_first: bool) {
    let EXPECTED_FIRST_V2_CYCLE = 8;
    // the sim environment produces 25 empty sortitions before
    //  tenures start being tracked.
    let EMPTY_SORTITIONS = 25;

    let (epochs, pox_constants) = make_test_epochs_pox();

    let mut burnchain = Burnchain::default_unittest(
        0,
        &BurnchainHeaderHash::from_hex(BITCOIN_REGTEST_FIRST_BLOCK_HASH).unwrap(),
    );
    burnchain.pox_constants = pox_constants.clone();

    let first_v2_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.v1_unlock_height as u64)
        .unwrap()
        + 1;

    let first_v3_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.pox_3_activation_height as u64)
        .unwrap()
        + 1;

    assert_eq!(first_v2_cycle, EXPECTED_FIRST_V2_CYCLE);

    eprintln!("First v2 cycle = {}", first_v2_cycle);

    let observer = TestEventObserver::new();

    let (mut peer, mut keys) = instantiate_pox_peer_with_epoch(
        &burnchain,
        &format!("{}-{}", function_name!(), alice_first),
        Some(epochs.clone()),
        Some(&observer),
    );

    peer.config.check_pox_invariants =
        Some((EXPECTED_FIRST_V2_CYCLE, EXPECTED_FIRST_V2_CYCLE + 10));

    let alice = keys.pop().unwrap();
    let bob = keys.pop().unwrap();

    let mut coinbase_nonce = 0;

    // produce blocks until epoch 2.1
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch21].start_height
    {
        peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    // in the next tenure, PoX 2 should now exist.
    // Lets have Bob lock up for v2
    // this will lock for cycles 8, 9, 10, and 11
    //  the first v2 cycle will be 8
    let tip = get_tip(peer.sortdb.as_ref());

    let alice_lockup = make_pox_2_lockup(
        &alice,
        0,
        1024 * POX_THRESHOLD_STEPS_USTX,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&alice).bytes,
        ),
        6,
        tip.block_height,
    );

    let bob_lockup = make_pox_2_lockup(
        &bob,
        0,
        1 * POX_THRESHOLD_STEPS_USTX,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&bob).bytes,
        ),
        6,
        tip.block_height,
    );

    let txs = if alice_first {
        [alice_lockup, bob_lockup]
    } else {
        [bob_lockup, alice_lockup]
    };
    let mut latest_block = peer.tenure_with_txs(&txs, &mut coinbase_nonce);

    // check that the "raw" reward set will contain entries for alice and bob
    //  for the pox-2 cycles
    for cycle_number in EXPECTED_FIRST_V2_CYCLE..first_v3_cycle {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 2);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&bob).bytes.0.to_vec()
        );
        assert_eq!(
            reward_set_entries[1].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
    }

    // we'll produce blocks until the next reward cycle gets through the "handled start" code
    //  this is one block after the reward cycle starts
    let height_target = burnchain.reward_cycle_to_block_height(EXPECTED_FIRST_V2_CYCLE) + 1;

    // but first, check that bob has locked tokens at (height_target + 1)
    let bob_bal = get_stx_account_at(
        &mut peer,
        &latest_block,
        &key_to_stacks_addr(&bob).to_account_principal(),
    );
    assert_eq!(bob_bal.amount_locked(), POX_THRESHOLD_STEPS_USTX);

    while get_tip(peer.sortdb.as_ref()).block_height < height_target {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    let first_auto_unlock_coinbase = height_target - 1 - EMPTY_SORTITIONS;

    // check that the "raw" reward sets for all cycles just contains entries for alice
    //  at the cycle start
    for cycle_number in EXPECTED_FIRST_V2_CYCLE..first_v3_cycle {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
    }

    // now check that bob has an unlock height of `height_target`
    let bob_bal = get_stx_account_at(
        &mut peer,
        &latest_block,
        &key_to_stacks_addr(&bob).to_account_principal(),
    );
    assert_eq!(bob_bal.unlock_height(), height_target);

    // but bob's still locked at (height_target): the unlock is accelerated to the "next" burn block
    assert_eq!(bob_bal.amount_locked(), 10000000000);

    // check that the total reward cycle amounts have decremented correctly
    for cycle_number in EXPECTED_FIRST_V2_CYCLE..first_v3_cycle {
        assert_eq!(
            get_reward_cycle_total(&mut peer, &latest_block, cycle_number),
            1024 * POX_THRESHOLD_STEPS_USTX
        );
    }

    // check that bob is fully unlocked at next block
    latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);

    let bob_bal = get_stx_account_at(
        &mut peer,
        &latest_block,
        &key_to_stacks_addr(&bob).to_account_principal(),
    );
    assert_eq!(bob_bal.unlock_height(), 0);
    assert_eq!(bob_bal.amount_locked(), 0);

    // check that the total reward cycle amounts have decremented correctly
    for cycle_number in EXPECTED_FIRST_V2_CYCLE..first_v3_cycle {
        assert_eq!(
            get_reward_cycle_total(&mut peer, &latest_block, cycle_number),
            1024 * POX_THRESHOLD_STEPS_USTX
        );
    }

    // check that bob's stacking-state is gone and alice's stacking-state is correct
    assert!(
        get_stacking_state_pox_2(
            &mut peer,
            &latest_block,
            &key_to_stacks_addr(&bob).to_account_principal()
        )
        .is_none(),
        "Bob should not have a stacking-state entry"
    );

    let alice_state = get_stacking_state_pox_2(
        &mut peer,
        &latest_block,
        &key_to_stacks_addr(&alice).to_account_principal(),
    )
    .expect("Alice should have stacking-state entry")
    .expect_tuple()
    .unwrap();
    let reward_indexes_str = format!("{}", alice_state.get("reward-set-indexes").unwrap());
    assert_eq!(reward_indexes_str, "(u0 u0 u0 u0 u0 u0)");

    // now, lets check behavior in Epochs 2.2-2.4, with pox-3 auto unlock tests

    // produce blocks until epoch 2.2
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch22].start_height
    {
        peer.tenure_with_txs(&[], &mut coinbase_nonce);
        let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
        assert_eq!(alice_balance, 0);
    }

    // check that alice is unlocked now
    peer.tenure_with_txs(&[], &mut coinbase_nonce);
    let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
    assert_eq!(alice_balance, 1024 * POX_THRESHOLD_STEPS_USTX);

    // produce blocks until epoch 2.4
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch24].start_height
    {
        peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    // repeat the lockups as before, so we can test the pox-3 auto unlock behavior
    let tip = get_tip(peer.sortdb.as_ref());

    let alice_lockup = make_pox_3_lockup(
        &alice,
        1,
        1024 * POX_THRESHOLD_STEPS_USTX,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&alice).bytes,
        ),
        6,
        tip.block_height,
    );

    let bob_lockup = make_pox_3_lockup(
        &bob,
        1,
        1 * POX_THRESHOLD_STEPS_USTX,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&bob).bytes,
        ),
        6,
        tip.block_height,
    );

    let txs = if alice_first {
        [alice_lockup, bob_lockup]
    } else {
        [bob_lockup, alice_lockup]
    };
    latest_block = peer.tenure_with_txs(&txs, &mut coinbase_nonce);

    // check that the "raw" reward set will contain entries for alice and bob
    //  for the pox-3 cycles
    for cycle_number in first_v3_cycle..(first_v3_cycle + 6) {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 2);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&bob).bytes.0.to_vec()
        );
        assert_eq!(
            reward_set_entries[1].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
    }

    // we'll produce blocks until the next reward cycle gets through the "handled start" code
    //  this is one block after the reward cycle starts
    let height_target = burnchain.reward_cycle_to_block_height(first_v3_cycle) + 1;
    let second_auto_unlock_coinbase = height_target - 1 - EMPTY_SORTITIONS;

    // but first, check that bob has locked tokens at (height_target + 1)
    let bob_bal = get_stx_account_at(
        &mut peer,
        &latest_block,
        &key_to_stacks_addr(&bob).to_account_principal(),
    );
    assert_eq!(bob_bal.amount_locked(), POX_THRESHOLD_STEPS_USTX);

    while get_tip(peer.sortdb.as_ref()).block_height < height_target {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    // check that the "raw" reward sets for all cycles just contains entries for alice
    //  at the cycle start
    for cycle_number in first_v3_cycle..(first_v3_cycle + 6) {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
    }

    // now check that bob has an unlock height of `height_target`
    let bob_bal = get_stx_account_at(
        &mut peer,
        &latest_block,
        &key_to_stacks_addr(&bob).to_account_principal(),
    );
    assert_eq!(bob_bal.unlock_height(), height_target);
    // but bob's still locked at (height_target): the unlock is accelerated to the "next" burn block
    assert_eq!(bob_bal.amount_locked(), 10000000000);

    // check that the total reward cycle amounts have decremented correctly
    for cycle_number in first_v3_cycle..(first_v3_cycle + 6) {
        assert_eq!(
            get_reward_cycle_total(&mut peer, &latest_block, cycle_number),
            1024 * POX_THRESHOLD_STEPS_USTX
        );
    }

    // check that bob's stacking-state is gone and alice's stacking-state is correct
    assert!(
        get_stacking_state_pox(
            &mut peer,
            &latest_block,
            &key_to_stacks_addr(&bob).to_account_principal(),
            POX_3_NAME,
        )
        .is_none(),
        "Bob should not have a stacking-state entry"
    );

    let alice_state = get_stacking_state_pox(
        &mut peer,
        &latest_block,
        &key_to_stacks_addr(&alice).to_account_principal(),
        POX_3_NAME,
    )
    .expect("Alice should have stacking-state entry")
    .expect_tuple()
    .unwrap();
    let reward_indexes_str = format!("{}", alice_state.get("reward-set-indexes").unwrap());
    assert_eq!(reward_indexes_str, "(u0 u0 u0 u0 u0 u0)");

    // check that bob is fully unlocked at next block
    latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);

    let bob_bal = get_stx_account_at(
        &mut peer,
        &latest_block,
        &key_to_stacks_addr(&bob).to_account_principal(),
    );
    assert_eq!(bob_bal.unlock_height(), 0);
    assert_eq!(bob_bal.amount_locked(), 0);

    // now let's check some tx receipts

    let alice_address = key_to_stacks_addr(&alice);
    let bob_address = key_to_stacks_addr(&bob);
    let blocks = observer.get_blocks();

    let mut alice_txs = HashMap::new();
    let mut bob_txs = HashMap::new();
    let mut coinbase_txs = vec![];

    for b in blocks.into_iter() {
        for (i, r) in b.receipts.into_iter().enumerate() {
            if i == 0 {
                coinbase_txs.push(r);
                continue;
            }
            match r.transaction {
                TransactionOrigin::Stacks(ref t) => {
                    let addr = t.auth.origin().address_testnet();
                    if addr == alice_address {
                        alice_txs.insert(t.auth.get_origin_nonce(), r);
                    } else if addr == bob_address {
                        bob_txs.insert(t.auth.get_origin_nonce(), r);
                    }
                }
                _ => {}
            }
        }
    }

    assert_eq!(alice_txs.len(), 2);
    assert_eq!(bob_txs.len(), 2);

    //  TX0 -> Bob's initial lockup in PoX 2
    assert!(
        match bob_txs.get(&0).unwrap().result {
            Value::Response(ref r) => r.committed,
            _ => false,
        },
        "Bob tx0 should have committed okay"
    );

    assert_eq!(coinbase_txs.len(), 38);

    info!(
        "Expected first auto-unlock coinbase index: {}",
        first_auto_unlock_coinbase
    );

    // Check that the event produced by "handle-unlock" has a well-formed print event
    // and that this event is included as part of the coinbase tx
    for unlock_coinbase_index in [first_auto_unlock_coinbase, second_auto_unlock_coinbase] {
        // expect the unlock to occur 1 block after the handle-unlock method was invoked.
        let expected_unlock_height = unlock_coinbase_index + EMPTY_SORTITIONS + 1;
        let expected_cycle = pox_constants
            .block_height_to_reward_cycle(0, expected_unlock_height)
            .unwrap();

        let auto_unlock_tx = coinbase_txs[unlock_coinbase_index as usize].events[0].clone();
        let pox_addr_val = generate_pox_clarity_value("60c59ab11f7063ef44c16d3dc856f76bbb915eba");
        let auto_unlock_op_data = HashMap::from([
            ("first-cycle-locked", Value::UInt(expected_cycle.into())),
            ("first-unlocked-cycle", Value::UInt(expected_cycle.into())),
            ("pox-addr", pox_addr_val),
        ]);
        let common_data = PoxPrintFields {
            op_name: "handle-unlock".to_string(),
            stacker: Value::Principal(
                StacksAddress::from_string("ST1GCB6NH3XR67VT4R5PKVJ2PYXNVQ4AYQATXNP4P")
                    .unwrap()
                    .to_account_principal(),
            ),
            balance: Value::UInt(10230000000000),
            locked: Value::UInt(10000000000),
            burnchain_unlock_height: Value::UInt(expected_unlock_height.into()),
        };
        check_pox_print_event(&auto_unlock_tx, common_data, auto_unlock_op_data);
    }
}

/// In this test case, Alice delegates to Bob.
///  Bob stacks Alice's funds via PoX v2 for 6 cycles. In the third cycle,
///  Bob increases Alice's stacking amount.
///
#[test]
fn delegate_stack_increase() {
    let EXPECTED_FIRST_V2_CYCLE = 8;
    // the sim environment produces 25 empty sortitions before
    //  tenures start being tracked.
    let EMPTY_SORTITIONS = 25;

    let (epochs, pox_constants) = make_test_epochs_pox();

    let mut burnchain = Burnchain::default_unittest(
        0,
        &BurnchainHeaderHash::from_hex(BITCOIN_REGTEST_FIRST_BLOCK_HASH).unwrap(),
    );
    burnchain.pox_constants = pox_constants.clone();

    let first_v2_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.v1_unlock_height as u64)
        .unwrap()
        + 1;

    let first_v3_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.pox_3_activation_height as u64)
        .unwrap()
        + 1;

    assert_eq!(first_v2_cycle, EXPECTED_FIRST_V2_CYCLE);

    let observer = TestEventObserver::new();

    let (mut peer, mut keys) = instantiate_pox_peer_with_epoch(
        &burnchain,
        function_name!(),
        Some(epochs.clone()),
        Some(&observer),
    );

    peer.config.check_pox_invariants =
        Some((EXPECTED_FIRST_V2_CYCLE, EXPECTED_FIRST_V2_CYCLE + 10));

    let num_blocks = 35;

    let alice = keys.pop().unwrap();
    let alice_address = key_to_stacks_addr(&alice);
    let alice_principal = PrincipalData::from(alice_address.clone());
    let bob = keys.pop().unwrap();
    let bob_address = key_to_stacks_addr(&bob);
    let bob_principal = PrincipalData::from(bob_address.clone());
    let bob_pox_addr = make_pox_addr(AddressHashMode::SerializeP2PKH, bob_address.bytes.clone());
    let mut alice_nonce = 0;
    let mut bob_nonce = 0;

    let alice_delegation_amount = 1023 * POX_THRESHOLD_STEPS_USTX;
    let alice_first_lock_amount = 512 * POX_THRESHOLD_STEPS_USTX;

    let mut coinbase_nonce = 0;

    // produce blocks until epoch 2.1
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch21].start_height
    {
        peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    // in the next tenure, PoX 2 should now exist.
    let tip = get_tip(peer.sortdb.as_ref());

    // submit delegation tx
    let alice_delegation_1 = make_pox_2_contract_call(
        &alice,
        alice_nonce,
        "delegate-stx",
        vec![
            Value::UInt(alice_delegation_amount),
            bob_principal.clone().into(),
            Value::none(),
            Value::none(),
        ],
    );

    let alice_delegation_pox_2_nonce = alice_nonce;
    alice_nonce += 1;

    let delegate_stack_tx = make_pox_2_contract_call(
        &bob,
        bob_nonce,
        "delegate-stack-stx",
        vec![
            alice_principal.clone().into(),
            Value::UInt(alice_first_lock_amount),
            bob_pox_addr.clone(),
            Value::UInt(tip.block_height as u128),
            Value::UInt(6),
        ],
    );

    bob_nonce += 1;

    let mut latest_block = peer.tenure_with_txs(
        &[alice_delegation_1, delegate_stack_tx],
        &mut coinbase_nonce,
    );

    let expected_pox_2_unlock_ht =
        burnchain.reward_cycle_to_block_height(EXPECTED_FIRST_V2_CYCLE + 6) - 1;
    let alice_bal = get_stx_account_at(&mut peer, &latest_block, &alice_principal);
    assert_eq!(alice_bal.amount_locked(), alice_first_lock_amount);
    assert_eq!(alice_bal.unlock_height(), expected_pox_2_unlock_ht);

    // check that the partial stacking state contains entries for bob
    for cycle_number in EXPECTED_FIRST_V2_CYCLE..(EXPECTED_FIRST_V2_CYCLE + 6) {
        let partial_stacked = get_partial_stacked(
            &mut peer,
            &latest_block,
            &bob_pox_addr,
            cycle_number,
            &bob_principal,
            POX_2_NAME,
        );
        assert_eq!(partial_stacked, 512 * POX_THRESHOLD_STEPS_USTX);
    }

    // we'll produce blocks until the 1st reward cycle gets through the "handled start" code
    //  this is one block after the reward cycle starts
    let height_target = burnchain.reward_cycle_to_block_height(EXPECTED_FIRST_V2_CYCLE + 1) + 1;

    while get_tip(peer.sortdb.as_ref()).block_height < height_target {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    let alice_bal = get_stx_account_at(&mut peer, &latest_block, &alice_principal);

    assert_eq!(alice_bal.amount_locked(), alice_first_lock_amount);

    // check that the partial stacking state contains entries for bob
    for cycle_number in EXPECTED_FIRST_V2_CYCLE..(EXPECTED_FIRST_V2_CYCLE + 6) {
        let partial_stacked = get_partial_stacked(
            &mut peer,
            &latest_block,
            &bob_pox_addr,
            cycle_number,
            &bob_principal,
            POX_2_NAME,
        );
        assert_eq!(partial_stacked, 512 * POX_THRESHOLD_STEPS_USTX);
    }

    let mut txs_to_submit = vec![];

    let fail_direct_increase_delegation = alice_nonce;
    txs_to_submit.push(make_pox_2_contract_call(
        &alice,
        alice_nonce,
        "stack-increase",
        vec![Value::UInt(1)],
    ));
    alice_nonce += 1;

    let fail_delegate_too_much_locked = bob_nonce;
    txs_to_submit.push(make_pox_2_contract_call(
        &bob,
        bob_nonce,
        "delegate-stack-increase",
        vec![
            alice_principal.clone().into(),
            bob_pox_addr.clone(),
            Value::UInt(alice_delegation_amount - alice_first_lock_amount + 1),
        ],
    ));
    bob_nonce += 1;

    let fail_invalid_amount = bob_nonce;
    txs_to_submit.push(make_pox_2_contract_call(
        &bob,
        bob_nonce,
        "delegate-stack-increase",
        vec![
            alice_principal.clone().into(),
            bob_pox_addr.clone(),
            Value::UInt(0),
        ],
    ));
    bob_nonce += 1;

    let fail_insufficient_funds = bob_nonce;
    txs_to_submit.push(make_pox_2_contract_call(
        &bob,
        bob_nonce,
        "delegate-stack-increase",
        vec![
            alice_principal.clone().into(),
            bob_pox_addr.clone(),
            Value::UInt(alice_bal.amount_unlocked() + 1),
        ],
    ));
    bob_nonce += 1;

    txs_to_submit.push(make_pox_2_contract_call(
        &bob,
        bob_nonce,
        "delegate-stack-increase",
        vec![
            alice_principal.clone().into(),
            bob_pox_addr.clone(),
            Value::UInt(alice_delegation_amount - alice_first_lock_amount),
        ],
    ));
    let bob_delegate_increase_pox_2_nonce = bob_nonce;
    bob_nonce += 1;

    latest_block = peer.tenure_with_txs(&txs_to_submit, &mut coinbase_nonce);

    let alice_bal = get_stx_account_at(&mut peer, &latest_block, &alice_principal);
    assert_eq!(alice_bal.amount_locked(), alice_delegation_amount);
    assert_eq!(alice_bal.unlock_height(), expected_pox_2_unlock_ht);

    // check that the partial stacking state contains entries for bob and they've incremented correctly
    for cycle_number in (EXPECTED_FIRST_V2_CYCLE)..(EXPECTED_FIRST_V2_CYCLE + 2) {
        let partial_stacked = get_partial_stacked(
            &mut peer,
            &latest_block,
            &bob_pox_addr,
            cycle_number,
            &bob_principal,
            POX_2_NAME,
        );
        assert_eq!(partial_stacked, alice_first_lock_amount);
    }

    for cycle_number in (EXPECTED_FIRST_V2_CYCLE + 2)..(EXPECTED_FIRST_V2_CYCLE + 6) {
        let partial_stacked = get_partial_stacked(
            &mut peer,
            &latest_block,
            &bob_pox_addr,
            cycle_number,
            &bob_principal,
            POX_2_NAME,
        );
        assert_eq!(partial_stacked, alice_delegation_amount,);
    }

    // okay, now let's progress through epochs 2.2-2.4, and perform the delegation tests
    //  on pox-3

    // roll the chain forward until just before Epoch-2.2
    while get_tip(peer.sortdb.as_ref()).block_height < epochs[StacksEpochId::Epoch22].start_height {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        // at this point, alice's balance should always include this half lockup
        assert_eq!(
            get_stx_account_at(&mut peer, &latest_block, &alice_principal).amount_locked(),
            alice_delegation_amount
        );
        assert_eq!(
            get_stx_account_at(&mut peer, &latest_block, &bob_principal).amount_locked(),
            0,
        );
    }

    // this block is mined in epoch-2.2
    latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    assert_eq!(
        get_stx_account_at(&mut peer, &latest_block, &alice_principal).amount_locked(),
        alice_delegation_amount
    );
    assert_eq!(
        get_stx_account_at(&mut peer, &latest_block, &bob_principal).amount_locked(),
        0,
    );
    // this block should unlock alice's balance
    latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    assert_eq!(
        get_stx_account_at(&mut peer, &latest_block, &alice_principal).amount_locked(),
        0,
    );
    assert_eq!(
        get_stx_account_at(&mut peer, &latest_block, &bob_principal).amount_locked(),
        0,
    );
    assert_eq!(
        get_stx_account_at(&mut peer, &latest_block, &alice_principal).amount_unlocked(),
        1024 * POX_THRESHOLD_STEPS_USTX
    );
    assert_eq!(
        get_stx_account_at(&mut peer, &latest_block, &bob_principal).amount_unlocked(),
        1024 * POX_THRESHOLD_STEPS_USTX
    );

    // Roll to Epoch-2.4 and re-do the above tests
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch24].start_height
    {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    let tip = get_tip(peer.sortdb.as_ref());

    // submit delegation tx
    let alice_delegation_1 = make_pox_3_contract_call(
        &alice,
        alice_nonce,
        "delegate-stx",
        vec![
            Value::UInt(alice_delegation_amount),
            bob_principal.clone().into(),
            Value::none(),
            Value::none(),
        ],
    );
    let alice_delegation_pox_3_nonce = alice_nonce;
    alice_nonce += 1;

    let delegate_stack_tx = make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "delegate-stack-stx",
        vec![
            alice_principal.clone().into(),
            Value::UInt(alice_first_lock_amount),
            bob_pox_addr.clone(),
            Value::UInt(tip.block_height as u128),
            Value::UInt(6),
        ],
    );

    bob_nonce += 1;

    latest_block = peer.tenure_with_txs(
        &[alice_delegation_1, delegate_stack_tx],
        &mut coinbase_nonce,
    );

    let expected_pox_3_unlock_ht = burnchain.reward_cycle_to_block_height(first_v3_cycle + 6) - 1;
    let alice_bal = get_stx_account_at(&mut peer, &latest_block, &alice_principal);
    assert_eq!(alice_bal.amount_locked(), alice_first_lock_amount);
    assert_eq!(alice_bal.unlock_height(), expected_pox_3_unlock_ht);

    // check that the partial stacking state contains entries for bob
    for cycle_number in first_v3_cycle..(first_v3_cycle + 6) {
        let partial_stacked = get_partial_stacked(
            &mut peer,
            &latest_block,
            &bob_pox_addr,
            cycle_number,
            &bob_principal,
            POX_3_NAME,
        );
        assert_eq!(partial_stacked, 512 * POX_THRESHOLD_STEPS_USTX);
    }

    // we'll produce blocks until the 3rd reward cycle gets through the "handled start" code
    //  this is one block after the reward cycle starts
    let height_target = burnchain.reward_cycle_to_block_height(first_v3_cycle + 3) + 1;

    while get_tip(peer.sortdb.as_ref()).block_height < height_target {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    let alice_bal = get_stx_account_at(&mut peer, &latest_block, &alice_principal);
    assert_eq!(alice_bal.amount_locked(), alice_first_lock_amount);
    let bob_bal = get_stx_account_at(&mut peer, &latest_block, &bob_principal);
    assert_eq!(bob_bal.amount_locked(), 0);

    // check that the partial stacking state contains entries for bob
    for cycle_number in first_v3_cycle..(first_v3_cycle + 6) {
        let partial_stacked = get_partial_stacked(
            &mut peer,
            &latest_block,
            &bob_pox_addr,
            cycle_number,
            &bob_principal,
            POX_3_NAME,
        );
        assert_eq!(partial_stacked, 512 * POX_THRESHOLD_STEPS_USTX);
    }

    let mut txs_to_submit = vec![];

    let pox_3_fail_direct_increase_delegation = alice_nonce;
    txs_to_submit.push(make_pox_3_contract_call(
        &alice,
        alice_nonce,
        "stack-increase",
        vec![Value::UInt(1)],
    ));
    alice_nonce += 1;

    let pox_3_fail_delegate_too_much_locked = bob_nonce;
    txs_to_submit.push(make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "delegate-stack-increase",
        vec![
            alice_principal.clone().into(),
            bob_pox_addr.clone(),
            Value::UInt(alice_delegation_amount - alice_first_lock_amount + 1),
        ],
    ));
    bob_nonce += 1;

    let pox_3_fail_invalid_amount = bob_nonce;
    txs_to_submit.push(make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "delegate-stack-increase",
        vec![
            alice_principal.clone().into(),
            bob_pox_addr.clone(),
            Value::UInt(0),
        ],
    ));
    bob_nonce += 1;

    let pox_3_fail_insufficient_funds = bob_nonce;
    txs_to_submit.push(make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "delegate-stack-increase",
        vec![
            alice_principal.clone().into(),
            bob_pox_addr.clone(),
            Value::UInt(alice_bal.amount_unlocked() + 1),
        ],
    ));
    bob_nonce += 1;

    txs_to_submit.push(make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "delegate-stack-increase",
        vec![
            alice_principal.clone().into(),
            bob_pox_addr.clone(),
            Value::UInt(alice_delegation_amount - alice_first_lock_amount),
        ],
    ));
    let bob_delegate_increase_pox_3_nonce = bob_nonce;
    bob_nonce += 1;

    latest_block = peer.tenure_with_txs(&txs_to_submit, &mut coinbase_nonce);

    assert_eq!(
        get_stx_account_at(&mut peer, &latest_block, &alice_principal).amount_locked(),
        alice_delegation_amount
    );

    assert_eq!(
        get_stx_account_at(&mut peer, &latest_block, &alice_principal).unlock_height(),
        expected_pox_3_unlock_ht,
    );

    // check that the partial stacking state contains entries for bob and they've incremented correctly
    for cycle_number in first_v3_cycle..(first_v3_cycle + 4) {
        let partial_stacked = get_partial_stacked(
            &mut peer,
            &latest_block,
            &bob_pox_addr,
            cycle_number,
            &bob_principal,
            POX_3_NAME,
        );
        assert_eq!(
            partial_stacked,
            alice_first_lock_amount,
            "Unexpected partially stacked amount in cycle: {} = {} + {}",
            cycle_number,
            first_v3_cycle,
            first_v3_cycle - cycle_number,
        );
    }

    for cycle_number in (first_v3_cycle + 4)..(first_v3_cycle + 6) {
        let partial_stacked = get_partial_stacked(
            &mut peer,
            &latest_block,
            &bob_pox_addr,
            cycle_number,
            &bob_principal,
            POX_3_NAME,
        );
        assert_eq!(partial_stacked, alice_delegation_amount);
    }

    // now let's check some tx receipts

    let alice_address = key_to_stacks_addr(&alice);
    let blocks = observer.get_blocks();

    let mut alice_txs = HashMap::new();
    let mut bob_txs = HashMap::new();

    for b in blocks.into_iter() {
        for r in b.receipts.into_iter() {
            if let TransactionOrigin::Stacks(ref t) = r.transaction {
                let addr = t.auth.origin().address_testnet();
                if addr == alice_address {
                    alice_txs.insert(t.auth.get_origin_nonce(), r);
                } else if addr == bob_address {
                    bob_txs.insert(t.auth.get_origin_nonce(), r);
                }
            }
        }
    }

    assert_eq!(alice_txs.len() as u64, 4);
    assert_eq!(bob_txs.len() as u64, 10);

    // transaction should fail because Alice cannot increase her own stacking amount while delegating
    assert_eq!(
        &alice_txs[&fail_direct_increase_delegation]
            .result
            .to_string(),
        "(err 20)"
    );

    // transaction should fail because Alice did not delegate enough funds to Bob
    assert_eq!(
        &bob_txs[&fail_delegate_too_much_locked].result.to_string(),
        "(err 22)"
    );

    // transaction should fail because Alice doesn't have enough funds
    assert_eq!(
        &bob_txs[&fail_insufficient_funds].result.to_string(),
        "(err 1)"
    );

    // transaction should fail because the amount supplied is invalid (i.e., 0)
    assert_eq!(
        &bob_txs[&fail_invalid_amount].result.to_string(),
        "(err 18)"
    );

    assert_eq!(
        &alice_txs[&pox_3_fail_direct_increase_delegation]
            .result
            .to_string(),
        "(err 30)"
    );

    // transaction should fail because Alice did not delegate enough funds to Bob
    assert_eq!(
        &bob_txs[&pox_3_fail_delegate_too_much_locked]
            .result
            .to_string(),
        "(err 22)"
    );

    // transaction should fail because Alice doesn't have enough funds
    assert_eq!(
        &bob_txs[&pox_3_fail_insufficient_funds].result.to_string(),
        "(err 1)"
    );

    // transaction should fail because the amount supplied is invalid (i.e., 0)
    assert_eq!(
        &bob_txs[&pox_3_fail_invalid_amount].result.to_string(),
        "(err 18)"
    );

    for delegation_nonce in [alice_delegation_pox_2_nonce, alice_delegation_pox_3_nonce] {
        let delegate_stx_tx = &alice_txs.get(&delegation_nonce).unwrap().clone().events[0];
        let delegate_stx_op_data = HashMap::from([
            ("pox-addr", Value::none()),
            ("amount-ustx", Value::UInt(10230000000000)),
            ("unlock-burn-height", Value::none()),
            (
                "delegate-to",
                Value::Principal(
                    StacksAddress::from_string("ST1GCB6NH3XR67VT4R5PKVJ2PYXNVQ4AYQATXNP4P")
                        .unwrap()
                        .to_account_principal(),
                ),
            ),
        ]);
        let common_data = PoxPrintFields {
            op_name: "delegate-stx".to_string(),
            stacker: Value::Principal(
                StacksAddress::from_string("ST2Q1B4S2DY2Y96KYNZTVCCZZD1V9AGWCS5MFXM4C")
                    .unwrap()
                    .to_account_principal(),
            ),
            balance: Value::UInt(10240000000000),
            locked: Value::UInt(0),
            burnchain_unlock_height: Value::UInt(0),
        };
        check_pox_print_event(delegate_stx_tx, common_data, delegate_stx_op_data);
    }

    // Check that the call to `delegate-stack-increase` has a well-formed print event.
    for (unlock_height, del_increase_nonce) in [
        (expected_pox_2_unlock_ht, bob_delegate_increase_pox_2_nonce),
        (expected_pox_3_unlock_ht, bob_delegate_increase_pox_3_nonce),
    ] {
        let delegate_stack_increase_tx =
            &bob_txs.get(&del_increase_nonce).unwrap().clone().events[0];
        let pox_addr_val = generate_pox_clarity_value("60c59ab11f7063ef44c16d3dc856f76bbb915eba");
        let delegate_op_data = HashMap::from([
            ("pox-addr", pox_addr_val),
            ("increase-by", Value::UInt(5110000000000)),
            ("total-locked", Value::UInt(10230000000000)),
            (
                "delegator",
                Value::Principal(
                    StacksAddress::from_string("ST1GCB6NH3XR67VT4R5PKVJ2PYXNVQ4AYQATXNP4P")
                        .unwrap()
                        .to_account_principal(),
                ),
            ),
        ]);
        let common_data = PoxPrintFields {
            op_name: "delegate-stack-increase".to_string(),
            stacker: Value::Principal(
                StacksAddress::from_string("ST2Q1B4S2DY2Y96KYNZTVCCZZD1V9AGWCS5MFXM4C")
                    .unwrap()
                    .to_account_principal(),
            ),
            balance: Value::UInt(5120000000000),
            locked: Value::UInt(5120000000000),
            burnchain_unlock_height: Value::UInt(unlock_height.into()),
        };
        check_pox_print_event(delegate_stack_increase_tx, common_data, delegate_op_data);
    }
}

#[test]
fn stack_increase() {
    let EXPECTED_FIRST_V2_CYCLE = 8;
    // the sim environment produces 25 empty sortitions before
    //  tenures start being tracked.
    let EMPTY_SORTITIONS = 25;

    let (epochs, pox_constants) = make_test_epochs_pox();

    let mut burnchain = Burnchain::default_unittest(
        0,
        &BurnchainHeaderHash::from_hex(BITCOIN_REGTEST_FIRST_BLOCK_HASH).unwrap(),
    );
    burnchain.pox_constants = pox_constants.clone();

    let first_v2_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.v1_unlock_height as u64)
        .unwrap()
        + 1;

    let first_v3_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.pox_3_activation_height as u64)
        .unwrap()
        + 1;

    assert_eq!(first_v2_cycle, EXPECTED_FIRST_V2_CYCLE);

    let observer = TestEventObserver::new();

    let (mut peer, mut keys) = instantiate_pox_peer_with_epoch(
        &burnchain,
        function_name!(),
        Some(epochs.clone()),
        Some(&observer),
    );

    peer.config.check_pox_invariants =
        Some((EXPECTED_FIRST_V2_CYCLE, EXPECTED_FIRST_V2_CYCLE + 10));

    let num_blocks = 35;

    let alice = keys.pop().unwrap();
    let alice_address = key_to_stacks_addr(&alice);
    let alice_principal = PrincipalData::from(alice_address.clone());
    let mut alice_nonce = 0;

    let mut coinbase_nonce = 0;

    let first_lockup_amt = 512 * POX_THRESHOLD_STEPS_USTX;
    let total_balance = 1024 * POX_THRESHOLD_STEPS_USTX;
    let increase_amt = total_balance - first_lockup_amt;

    // produce blocks until epoch 2.1
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch21].start_height
    {
        peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    // in the next tenure, PoX 2 should now exist.
    let tip = get_tip(peer.sortdb.as_ref());

    // submit an increase: this should fail, because Alice is not yet locked
    let fail_no_lock_tx = alice_nonce;
    let alice_increase = make_pox_2_increase(&alice, alice_nonce, increase_amt);
    alice_nonce += 1;

    let alice_lockup = make_pox_2_lockup(
        &alice,
        alice_nonce,
        first_lockup_amt,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&alice).bytes,
        ),
        6,
        tip.block_height,
    );
    alice_nonce += 1;

    let mut latest_block =
        peer.tenure_with_txs(&[alice_increase, alice_lockup], &mut coinbase_nonce);

    let expected_pox_2_unlock_ht =
        burnchain.reward_cycle_to_block_height(EXPECTED_FIRST_V2_CYCLE + 6) - 1;
    let alice_bal = get_stx_account_at(&mut peer, &latest_block, &alice_principal);
    assert_eq!(alice_bal.amount_locked(), first_lockup_amt);
    assert_eq!(alice_bal.unlock_height(), expected_pox_2_unlock_ht);
    assert_eq!(alice_bal.get_total_balance().unwrap(), total_balance,);

    // check that the "raw" reward set will contain entries for alice at the cycle start
    for cycle_number in EXPECTED_FIRST_V2_CYCLE..first_v3_cycle {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
        assert_eq!(reward_set_entries[0].amount_stacked, first_lockup_amt,);
    }

    // we'll produce blocks until the 1st reward cycle gets through the "handled start" code
    //  this is one block after the reward cycle starts
    let height_target = burnchain.reward_cycle_to_block_height(EXPECTED_FIRST_V2_CYCLE + 1) + 1;

    while get_tip(peer.sortdb.as_ref()).block_height < height_target {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    // check that the "raw" reward sets for all cycles contains entries for alice
    for cycle_number in EXPECTED_FIRST_V2_CYCLE..first_v3_cycle {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
        assert_eq!(reward_set_entries[0].amount_stacked, first_lockup_amt,);
    }

    let mut txs_to_submit = vec![];
    let fail_bad_amount = alice_nonce;
    txs_to_submit.push(make_pox_2_increase(&alice, alice_nonce, 0));
    alice_nonce += 1;

    // this stack-increase tx should work
    let pox_2_success_increase = alice_nonce;
    txs_to_submit.push(make_pox_2_increase(&alice, alice_nonce, increase_amt));
    alice_nonce += 1;

    // increase by an amount we don't have!
    let fail_not_enough_funds = alice_nonce;
    txs_to_submit.push(make_pox_2_increase(&alice, alice_nonce, 1));
    alice_nonce += 1;

    latest_block = peer.tenure_with_txs(&txs_to_submit, &mut coinbase_nonce);

    let alice_bal = get_stx_account_at(&mut peer, &latest_block, &alice_principal);
    assert_eq!(alice_bal.amount_locked(), first_lockup_amt + increase_amt,);
    assert_eq!(alice_bal.unlock_height(), expected_pox_2_unlock_ht);
    assert_eq!(alice_bal.get_total_balance().unwrap(), total_balance,);

    // check that the total reward cycle amounts have incremented correctly
    for cycle_number in first_v2_cycle..(first_v2_cycle + 2) {
        assert_eq!(
            get_reward_cycle_total(&mut peer, &latest_block, cycle_number),
            first_lockup_amt,
        );
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
        assert_eq!(reward_set_entries[0].amount_stacked, first_lockup_amt,);
    }

    assert!(
        first_v2_cycle + 2 < first_v3_cycle,
        "Make sure that we can actually test a stack-increase in pox-2 before pox-3 activates"
    );

    for cycle_number in (first_v2_cycle + 2)..first_v3_cycle {
        assert_eq!(
            get_reward_cycle_total(&mut peer, &latest_block, cycle_number),
            first_lockup_amt + increase_amt,
        );
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
        assert_eq!(
            reward_set_entries[0].amount_stacked,
            first_lockup_amt + increase_amt,
        );
    }

    // Roll to Epoch-2.4 and re-do the above tests
    // okay, now let's progress through epochs 2.2-2.4, and perform the delegation tests
    //  on pox-3

    // roll the chain forward until just before Epoch-2.2
    while get_tip(peer.sortdb.as_ref()).block_height < epochs[StacksEpochId::Epoch22].start_height {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        // at this point, alice's balance should always include this half lockup
        assert_eq!(
            get_stx_account_at(&mut peer, &latest_block, &alice_principal).amount_locked(),
            first_lockup_amt + increase_amt,
        );
    }

    // this block is mined in epoch-2.2
    latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    assert_eq!(
        get_stx_account_at(&mut peer, &latest_block, &alice_principal).amount_locked(),
        first_lockup_amt + increase_amt,
    );

    // this block should unlock alice's balance

    latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    assert_eq!(
        get_stx_account_at(&mut peer, &latest_block, &alice_principal).amount_locked(),
        0,
    );
    assert_eq!(
        get_stx_account_at(&mut peer, &latest_block, &alice_principal).amount_unlocked(),
        total_balance,
    );

    // Roll to Epoch-2.4 and re-do the above stack-increase tests
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch24].start_height
    {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    // in the next tenure, PoX 3 should now exist.
    let tip = get_tip(peer.sortdb.as_ref());

    // submit an increase: this should fail, because Alice is not yet locked
    let pox_3_fail_no_lock_tx = alice_nonce;
    let alice_increase = make_pox_3_contract_call(
        &alice,
        alice_nonce,
        "stack-increase",
        vec![Value::UInt(increase_amt)],
    );
    alice_nonce += 1;

    let alice_lockup = make_pox_3_lockup(
        &alice,
        alice_nonce,
        first_lockup_amt,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&alice).bytes,
        ),
        6,
        tip.block_height,
    );
    alice_nonce += 1;

    let mut latest_block =
        peer.tenure_with_txs(&[alice_increase, alice_lockup], &mut coinbase_nonce);

    let expected_pox_3_unlock_ht = burnchain.reward_cycle_to_block_height(first_v3_cycle + 6) - 1;
    let alice_bal = get_stx_account_at(&mut peer, &latest_block, &alice_principal);
    assert_eq!(alice_bal.amount_locked(), first_lockup_amt);
    assert_eq!(alice_bal.unlock_height(), expected_pox_3_unlock_ht);
    assert_eq!(alice_bal.get_total_balance().unwrap(), total_balance,);

    // check that the "raw" reward set will contain entries for alice at the cycle start
    for cycle_number in first_v3_cycle..(first_v3_cycle + 6) {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
        assert_eq!(reward_set_entries[0].amount_stacked, first_lockup_amt,);
    }

    // we'll produce blocks until the 3rd reward cycle gets through the "handled start" code
    //  this is one block after the reward cycle starts
    let height_target = burnchain.reward_cycle_to_block_height(first_v3_cycle + 3) + 1;

    while get_tip(peer.sortdb.as_ref()).block_height < height_target {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    // check that the "raw" reward set will contain entries for alice at the cycle start
    for cycle_number in first_v3_cycle..(first_v3_cycle + 6) {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
        assert_eq!(reward_set_entries[0].amount_stacked, first_lockup_amt,);
    }

    let mut txs_to_submit = vec![];
    let pox_3_fail_bad_amount = alice_nonce;
    let bad_amount_tx =
        make_pox_3_contract_call(&alice, alice_nonce, "stack-increase", vec![Value::UInt(0)]);
    txs_to_submit.push(bad_amount_tx);
    alice_nonce += 1;

    // this stack-increase tx should work
    let pox_3_success_increase = alice_nonce;
    let good_amount_tx = make_pox_3_contract_call(
        &alice,
        alice_nonce,
        "stack-increase",
        vec![Value::UInt(increase_amt)],
    );
    txs_to_submit.push(good_amount_tx);
    alice_nonce += 1;

    // increase by an amount we don't have!
    let pox_3_fail_not_enough_funds = alice_nonce;
    let not_enough_tx =
        make_pox_3_contract_call(&alice, alice_nonce, "stack-increase", vec![Value::UInt(1)]);
    txs_to_submit.push(not_enough_tx);
    alice_nonce += 1;

    latest_block = peer.tenure_with_txs(&txs_to_submit, &mut coinbase_nonce);

    let alice_bal = get_stx_account_at(&mut peer, &latest_block, &alice_principal);
    assert_eq!(alice_bal.amount_locked(), first_lockup_amt + increase_amt,);
    assert_eq!(alice_bal.unlock_height(), expected_pox_3_unlock_ht);
    assert_eq!(alice_bal.get_total_balance().unwrap(), total_balance,);

    // check that the total reward cycle amounts have incremented correctly
    for cycle_number in first_v3_cycle..(first_v3_cycle + 4) {
        assert_eq!(
            get_reward_cycle_total(&mut peer, &latest_block, cycle_number),
            first_lockup_amt,
        );
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
        assert_eq!(reward_set_entries[0].amount_stacked, first_lockup_amt,);
    }

    for cycle_number in (first_v3_cycle + 4)..(first_v3_cycle + 6) {
        assert_eq!(
            get_reward_cycle_total(&mut peer, &latest_block, cycle_number),
            first_lockup_amt + increase_amt,
        );
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
        assert_eq!(
            reward_set_entries[0].amount_stacked,
            first_lockup_amt + increase_amt,
        );
    }

    // now let's check some tx receipts
    let blocks = observer.get_blocks();

    let mut alice_txs = HashMap::new();

    for b in blocks.into_iter() {
        for r in b.receipts.into_iter() {
            if let TransactionOrigin::Stacks(ref t) = r.transaction {
                let addr = t.auth.origin().address_testnet();
                if addr == alice_address {
                    alice_txs.insert(t.auth.get_origin_nonce(), r);
                }
            }
        }
    }

    assert_eq!(alice_txs.len() as u64, alice_nonce);

    // transaction should fail because lock isn't applied
    assert_eq!(&alice_txs[&fail_no_lock_tx].result.to_string(), "(err 27)");

    // transaction should fail because Alice doesn't have enough funds
    assert_eq!(
        &alice_txs[&fail_not_enough_funds].result.to_string(),
        "(err 1)"
    );

    // transaction should fail because the amount supplied is invalid (i.e., 0)
    assert_eq!(&alice_txs[&fail_bad_amount].result.to_string(), "(err 18)");

    // transaction should fail because lock isn't applied
    assert_eq!(
        &alice_txs[&pox_3_fail_no_lock_tx].result.to_string(),
        "(err 27)"
    );

    // transaction should fail because Alice doesn't have enough funds
    assert_eq!(
        &alice_txs[&pox_3_fail_not_enough_funds].result.to_string(),
        "(err 1)"
    );

    // transaction should fail because the amount supplied is invalid (i.e., 0)
    assert_eq!(
        &alice_txs[&pox_3_fail_bad_amount].result.to_string(),
        "(err 18)"
    );

    // Check that the call to `stack-increase` has a well-formed print event.
    for (increase_nonce, unlock_height) in [
        (pox_2_success_increase, expected_pox_2_unlock_ht),
        (pox_3_success_increase, expected_pox_3_unlock_ht),
    ] {
        let stack_increase_tx = &alice_txs.get(&increase_nonce).unwrap().clone().events[0];
        let pox_addr_val = generate_pox_clarity_value("ae1593226f85e49a7eaff5b633ff687695438cc9");
        let stack_op_data = HashMap::from([
            ("increase-by", Value::UInt(5120000000000)),
            ("total-locked", Value::UInt(10240000000000)),
            ("pox-addr", pox_addr_val),
        ]);
        let common_data = PoxPrintFields {
            op_name: "stack-increase".to_string(),
            stacker: Value::Principal(
                StacksAddress::from_string("ST2Q1B4S2DY2Y96KYNZTVCCZZD1V9AGWCS5MFXM4C")
                    .unwrap()
                    .to_account_principal(),
            ),
            balance: Value::UInt(5120000000000),
            locked: Value::UInt(5120000000000),
            burnchain_unlock_height: Value::UInt(unlock_height.into()),
        };
        check_pox_print_event(stack_increase_tx, common_data, stack_op_data);
    }
}

#[test]
fn pox_extend_transition() {
    let EXPECTED_FIRST_V2_CYCLE = 8;
    // the sim environment produces 25 empty sortitions before
    //  tenures start being tracked.
    let EMPTY_SORTITIONS = 25;

    let (epochs, pox_constants) = make_test_epochs_pox();

    let mut burnchain = Burnchain::default_unittest(
        0,
        &BurnchainHeaderHash::from_hex(BITCOIN_REGTEST_FIRST_BLOCK_HASH).unwrap(),
    );
    burnchain.pox_constants = pox_constants.clone();

    let first_v2_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.v1_unlock_height as u64)
        .unwrap()
        + 1;

    let first_v3_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.pox_3_activation_height as u64)
        .unwrap()
        + 1;

    assert_eq!(first_v2_cycle, EXPECTED_FIRST_V2_CYCLE);

    let observer = TestEventObserver::new();

    let (mut peer, mut keys) = instantiate_pox_peer_with_epoch(
        &burnchain,
        function_name!(),
        Some(epochs.clone()),
        Some(&observer),
    );

    peer.config.check_pox_invariants =
        Some((EXPECTED_FIRST_V2_CYCLE, EXPECTED_FIRST_V2_CYCLE + 10));

    let alice = keys.pop().unwrap();
    let bob = keys.pop().unwrap();
    let alice_address = key_to_stacks_addr(&alice);
    let alice_principal = PrincipalData::from(alice_address.clone());
    let bob_address = key_to_stacks_addr(&bob);
    let bob_principal = PrincipalData::from(bob_address.clone());

    let EXPECTED_ALICE_FIRST_REWARD_CYCLE = 6;
    let mut coinbase_nonce = 0;

    let INITIAL_BALANCE = 1024 * POX_THRESHOLD_STEPS_USTX;
    let ALICE_LOCKUP = 1024 * POX_THRESHOLD_STEPS_USTX;
    let BOB_LOCKUP = 512 * POX_THRESHOLD_STEPS_USTX;

    // these checks should pass between Alice's first reward cycle,
    //  and the start of V2 reward cycles
    let alice_rewards_to_v2_start_checks = |tip_index_block, peer: &mut TestPeer| {
        let tip_burn_block_height = get_par_burn_block_height(peer.chainstate(), &tip_index_block);
        let cur_reward_cycle = burnchain
            .block_height_to_reward_cycle(tip_burn_block_height)
            .unwrap() as u128;
        let (min_ustx, reward_addrs, total_stacked) = with_sortdb(peer, |ref mut c, sortdb| {
            (
                c.get_stacking_minimum(sortdb, &tip_index_block).unwrap(),
                get_reward_addresses_with_par_tip(c, &burnchain, sortdb, &tip_index_block).unwrap(),
                c.test_get_total_ustx_stacked(sortdb, &tip_index_block, cur_reward_cycle)
                    .unwrap(),
            )
        });

        assert!(
            cur_reward_cycle >= EXPECTED_ALICE_FIRST_REWARD_CYCLE
                && cur_reward_cycle < first_v2_cycle as u128
        );
        //  Alice is the only Stacker, so check that.
        let (amount_ustx, pox_addr, lock_period, first_reward_cycle) =
            get_stacker_info(peer, &key_to_stacks_addr(&alice).into()).unwrap();
        eprintln!(
            "\nAlice: {} uSTX stacked for {} cycle(s); addr is {:?}; first reward cycle is {}\n",
            amount_ustx, lock_period, &pox_addr, first_reward_cycle
        );

        // one reward address, and it's Alice's
        // either way, there's a single reward address
        assert_eq!(reward_addrs.len(), 1);
        assert_eq!(
            (reward_addrs[0].0).version(),
            AddressHashMode::SerializeP2PKH as u8
        );
        assert_eq!(
            (reward_addrs[0].0).hash160(),
            key_to_stacks_addr(&alice).bytes
        );
        assert_eq!(reward_addrs[0].1, ALICE_LOCKUP);
    };

    // these checks should pass after the start of V2 reward cycles
    let v2_rewards_checks = |tip_index_block, peer: &mut TestPeer| {
        let tip_burn_block_height = get_par_burn_block_height(peer.chainstate(), &tip_index_block);
        let cur_reward_cycle = burnchain
            .block_height_to_reward_cycle(tip_burn_block_height)
            .unwrap() as u128;
        let (min_ustx, reward_addrs, total_stacked) = with_sortdb(peer, |ref mut c, sortdb| {
            (
                c.get_stacking_minimum(sortdb, &tip_index_block).unwrap(),
                get_reward_addresses_with_par_tip(c, &burnchain, sortdb, &tip_index_block).unwrap(),
                c.test_get_total_ustx_stacked(sortdb, &tip_index_block, cur_reward_cycle)
                    .unwrap(),
            )
        });

        eprintln!(
            "reward_cycle = {}, reward_addrs = {}, total_stacked = {}",
            cur_reward_cycle,
            reward_addrs.len(),
            total_stacked
        );

        assert!(cur_reward_cycle >= first_v2_cycle as u128);
        // v2 reward cycles have begun, so reward addrs should be read from PoX2 which is Bob + Alice
        assert_eq!(reward_addrs.len(), 2);
        assert_eq!(
            (reward_addrs[0].0).version(),
            AddressHashMode::SerializeP2PKH as u8
        );
        assert_eq!(
            (reward_addrs[0].0).hash160(),
            key_to_stacks_addr(&bob).bytes
        );
        assert_eq!(reward_addrs[0].1, BOB_LOCKUP);

        assert_eq!(
            (reward_addrs[1].0).version(),
            AddressHashMode::SerializeP2PKH as u8
        );
        assert_eq!(
            (reward_addrs[1].0).hash160(),
            key_to_stacks_addr(&alice).bytes
        );
        assert_eq!(reward_addrs[1].1, ALICE_LOCKUP);
    };

    // first tenure is empty
    let mut latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);

    let alice_account = get_account(&mut peer, &key_to_stacks_addr(&alice).into());
    assert_eq!(alice_account.stx_balance.amount_unlocked(), INITIAL_BALANCE);
    assert_eq!(alice_account.stx_balance.amount_locked(), 0);
    assert_eq!(alice_account.stx_balance.unlock_height(), 0);

    // next tenure include Alice's lockup
    let tip = get_tip(peer.sortdb.as_ref());
    let alice_lockup = make_pox_lockup(
        &alice,
        0,
        ALICE_LOCKUP,
        AddressHashMode::SerializeP2PKH,
        key_to_stacks_addr(&alice).bytes,
        4,
        tip.block_height,
    );

    let tip_index_block = peer.tenure_with_txs(&[alice_lockup], &mut coinbase_nonce);

    // check the stacking minimum
    let total_liquid_ustx = get_liquid_ustx(&mut peer);
    let min_ustx = with_sortdb(&mut peer, |ref mut chainstate, sortdb| {
        chainstate.get_stacking_minimum(sortdb, &tip_index_block)
    })
    .unwrap();
    assert_eq!(
        min_ustx,
        total_liquid_ustx / POX_TESTNET_STACKING_THRESHOLD_25
    );

    // no reward addresses
    let reward_addrs = with_sortdb(&mut peer, |ref mut chainstate, sortdb| {
        get_reward_addresses_with_par_tip(chainstate, &burnchain, sortdb, &tip_index_block)
    })
    .unwrap();
    assert_eq!(reward_addrs.len(), 0);

    // check the first reward cycle when Alice's tokens get stacked
    let tip_burn_block_height = get_par_burn_block_height(peer.chainstate(), &tip_index_block);
    let alice_first_reward_cycle = 1 + burnchain
        .block_height_to_reward_cycle(tip_burn_block_height)
        .unwrap();

    assert_eq!(
        alice_first_reward_cycle as u128,
        EXPECTED_ALICE_FIRST_REWARD_CYCLE
    );
    let height_target = burnchain.reward_cycle_to_block_height(alice_first_reward_cycle) + 1;

    // alice locked, so balance should be 0
    let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
    assert_eq!(alice_balance, 0);

    while get_tip(peer.sortdb.as_ref()).block_height < height_target {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    // produce blocks until epoch 2.1
    while get_tip(peer.sortdb.as_ref()).block_height < epochs[StacksEpochId::Epoch21].start_height {
        peer.tenure_with_txs(&[], &mut coinbase_nonce);
        alice_rewards_to_v2_start_checks(latest_block, &mut peer);
    }

    // in the next tenure, PoX 2 should now exist.
    // Lets have Bob lock up for v2
    // this will lock for cycles 8, 9, 10
    //  the first v2 cycle will be 8
    let tip = get_tip(peer.sortdb.as_ref());

    let bob_lockup = make_pox_2_lockup(
        &bob,
        0,
        BOB_LOCKUP,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&bob).bytes,
        ),
        3,
        tip.block_height,
    );

    // Alice _will_ auto-unlock: she can stack-extend in PoX v2
    let alice_lockup = make_pox_2_extend(
        &alice,
        1,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&alice).bytes,
        ),
        6,
    );

    latest_block = peer.tenure_with_txs(&[bob_lockup, alice_lockup], &mut coinbase_nonce);
    alice_rewards_to_v2_start_checks(latest_block, &mut peer);

    // Extend bob's lockup via `stack-extend` for 1 more cycle
    let bob_extend = make_pox_2_extend(
        &bob,
        1,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&bob).bytes,
        ),
        1,
    );

    latest_block = peer.tenure_with_txs(&[bob_extend], &mut coinbase_nonce);

    alice_rewards_to_v2_start_checks(latest_block, &mut peer);

    // produce blocks until the v2 reward cycles start
    let height_target = burnchain.reward_cycle_to_block_height(first_v2_cycle) - 1;
    while get_tip(peer.sortdb.as_ref()).block_height < height_target {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        // alice is still locked, balance should be 0
        let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
        assert_eq!(alice_balance, 0);

        alice_rewards_to_v2_start_checks(latest_block, &mut peer);
    }

    latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    v2_rewards_checks(latest_block, &mut peer);

    // Roll to Epoch-2.4 and re-do the above tests

    // roll the chain forward until just before Epoch-2.2
    while get_tip(peer.sortdb.as_ref()).block_height < epochs[StacksEpochId::Epoch22].start_height {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        // at this point, alice's balance should be locked, and so should bob's
        let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
        assert_eq!(alice_balance, 0);
        let bob_balance = get_balance(&mut peer, &key_to_stacks_addr(&bob).into());
        assert_eq!(bob_balance, 512 * POX_THRESHOLD_STEPS_USTX);
    }

    // this block is mined in epoch-2.2
    latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
    assert_eq!(alice_balance, 0);
    let bob_balance = get_balance(&mut peer, &key_to_stacks_addr(&bob).into());
    assert_eq!(bob_balance, 512 * POX_THRESHOLD_STEPS_USTX);

    // this block should unlock alice and bob's balance

    latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    let alice_account = get_stx_account_at(&mut peer, &latest_block, &alice_principal);
    let bob_account = get_stx_account_at(&mut peer, &latest_block, &bob_principal);
    assert_eq!(alice_account.amount_locked(), 0);
    assert_eq!(alice_account.amount_unlocked(), INITIAL_BALANCE);
    assert_eq!(bob_account.amount_locked(), 0);
    assert_eq!(bob_account.amount_unlocked(), INITIAL_BALANCE);

    // Roll to Epoch-2.4 and re-do the above stack-extend tests
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch24].start_height
    {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    let tip = get_tip(peer.sortdb.as_ref());
    let alice_lockup = make_pox_3_lockup(
        &alice,
        2,
        ALICE_LOCKUP,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&alice).bytes,
        ),
        4,
        tip.block_height,
    );
    let alice_pox_3_lock_nonce = 2;
    let alice_first_pox_3_unlock_height =
        burnchain.reward_cycle_to_block_height(first_v3_cycle + 4) - 1;
    let alice_pox_3_start_burn_height = tip.block_height;

    latest_block = peer.tenure_with_txs(&[alice_lockup], &mut coinbase_nonce);

    // check that the "raw" reward set will contain entries for alice at the cycle start
    for cycle_number in first_v3_cycle..(first_v3_cycle + 4) {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
        assert_eq!(reward_set_entries[0].amount_stacked, ALICE_LOCKUP,);
    }

    // check the first reward cycle when Alice's tokens get stacked
    let tip_burn_block_height = get_par_burn_block_height(peer.chainstate(), &latest_block);
    let alice_first_v3_reward_cycle = 1 + burnchain
        .block_height_to_reward_cycle(tip_burn_block_height)
        .unwrap();

    let height_target = burnchain.reward_cycle_to_block_height(alice_first_v3_reward_cycle) + 1;

    // alice locked, so balance should be 0
    let alice_balance = get_balance(&mut peer, &alice_principal);
    assert_eq!(alice_balance, 0);

    // advance to the first v3 reward cycle
    while get_tip(peer.sortdb.as_ref()).block_height < height_target {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    let tip = get_tip(peer.sortdb.as_ref());
    let bob_lockup = make_pox_3_lockup(
        &bob,
        2,
        BOB_LOCKUP,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&bob).bytes,
        ),
        3,
        tip.block_height,
    );

    // Alice can stack-extend in PoX v2
    let alice_lockup = make_pox_3_extend(
        &alice,
        3,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&alice).bytes,
        ),
        6,
    );

    let alice_pox_3_extend_nonce = 3;
    let alice_extend_pox_3_unlock_height =
        burnchain.reward_cycle_to_block_height(first_v3_cycle + 10) - 1;

    latest_block = peer.tenure_with_txs(&[bob_lockup, alice_lockup], &mut coinbase_nonce);

    // check that the "raw" reward set will contain entries for alice at the cycle start
    for cycle_number in first_v3_cycle..(first_v3_cycle + 1) {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
        assert_eq!(reward_set_entries[0].amount_stacked, ALICE_LOCKUP,);
    }

    for cycle_number in (first_v3_cycle + 1)..(first_v3_cycle + 4) {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 2);
        assert_eq!(
            reward_set_entries[1].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
        assert_eq!(reward_set_entries[1].amount_stacked, ALICE_LOCKUP,);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&bob).bytes.0.to_vec()
        );
        assert_eq!(reward_set_entries[0].amount_stacked, BOB_LOCKUP,);
    }

    for cycle_number in (first_v3_cycle + 4)..(first_v3_cycle + 10) {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&alice).bytes.0.to_vec()
        );
        assert_eq!(reward_set_entries[0].amount_stacked, ALICE_LOCKUP,);
    }

    // now let's check some tx receipts

    let alice_address = key_to_stacks_addr(&alice);
    let bob_address = key_to_stacks_addr(&bob);
    let blocks = observer.get_blocks();

    let mut alice_txs = HashMap::new();
    let mut bob_txs = HashMap::new();

    for b in blocks.into_iter() {
        for r in b.receipts.into_iter() {
            if let TransactionOrigin::Stacks(ref t) = r.transaction {
                let addr = t.auth.origin().address_testnet();
                eprintln!("TX addr: {}", addr);
                if addr == alice_address {
                    alice_txs.insert(t.auth.get_origin_nonce(), r);
                } else if addr == bob_address {
                    bob_txs.insert(t.auth.get_origin_nonce(), r);
                }
            }
        }
    }

    assert_eq!(alice_txs.len(), 4);
    assert_eq!(bob_txs.len(), 3);

    for tx in alice_txs.iter() {
        assert!(
            if let Value::Response(ref r) = tx.1.result {
                r.committed
            } else {
                false
            },
            "Alice txs should all have committed okay"
        );
    }

    for tx in bob_txs.iter() {
        assert!(
            if let Value::Response(ref r) = tx.1.result {
                r.committed
            } else {
                false
            },
            "Bob txs should all have committed okay"
        );
    }

    // Check that the call to `stack-stx` has a well-formed print event.
    let stack_tx = &alice_txs
        .get(&alice_pox_3_lock_nonce)
        .unwrap()
        .clone()
        .events[0];
    let pox_addr_val = generate_pox_clarity_value("ae1593226f85e49a7eaff5b633ff687695438cc9");
    let stack_op_data = HashMap::from([
        ("lock-amount", Value::UInt(ALICE_LOCKUP)),
        (
            "unlock-burn-height",
            Value::UInt(alice_first_pox_3_unlock_height.into()),
        ),
        (
            "start-burn-height",
            Value::UInt(alice_pox_3_start_burn_height.into()),
        ),
        ("pox-addr", pox_addr_val.clone()),
        ("lock-period", Value::UInt(4)),
    ]);
    let common_data = PoxPrintFields {
        op_name: "stack-stx".to_string(),
        stacker: Value::Principal(alice_principal.clone()),
        balance: Value::UInt(10240000000000),
        locked: Value::UInt(0),
        burnchain_unlock_height: Value::UInt(0),
    };
    check_pox_print_event(stack_tx, common_data, stack_op_data);

    // Check that the call to `stack-extend` has a well-formed print event.
    let stack_extend_tx = &alice_txs
        .get(&alice_pox_3_extend_nonce)
        .unwrap()
        .clone()
        .events[0];
    let stack_ext_op_data = HashMap::from([
        ("extend-count", Value::UInt(6)),
        ("pox-addr", pox_addr_val),
        (
            "unlock-burn-height",
            Value::UInt(alice_extend_pox_3_unlock_height.into()),
        ),
    ]);
    let common_data = PoxPrintFields {
        op_name: "stack-extend".to_string(),
        stacker: Value::Principal(alice_principal.clone()),
        balance: Value::UInt(0),
        locked: Value::UInt(ALICE_LOCKUP),
        burnchain_unlock_height: Value::UInt(alice_first_pox_3_unlock_height.into()),
    };
    check_pox_print_event(stack_extend_tx, common_data, stack_ext_op_data);
}

#[test]
fn delegate_extend_pox_3() {
    // the sim environment produces 25 empty sortitions before
    //  tenures start being tracked.
    let EMPTY_SORTITIONS = 25;

    let (epochs, pox_constants) = make_test_epochs_pox();

    let mut burnchain = Burnchain::default_unittest(
        0,
        &BurnchainHeaderHash::from_hex(BITCOIN_REGTEST_FIRST_BLOCK_HASH).unwrap(),
    );
    burnchain.pox_constants = pox_constants.clone();

    let first_v3_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.pox_3_activation_height as u64)
        .unwrap()
        + 1;

    let observer = TestEventObserver::new();

    let (mut peer, mut keys) = instantiate_pox_peer_with_epoch(
        &burnchain,
        function_name!(),
        Some(epochs.clone()),
        Some(&observer),
    );

    peer.config.check_pox_invariants = Some((first_v3_cycle, first_v3_cycle + 10));

    let alice = keys.pop().unwrap();
    let bob = keys.pop().unwrap();
    let charlie = keys.pop().unwrap();

    let alice_address = key_to_stacks_addr(&alice);
    let bob_address = key_to_stacks_addr(&bob);
    let charlie_address = key_to_stacks_addr(&charlie);

    let mut coinbase_nonce = 0;

    let INITIAL_BALANCE = 1024 * POX_THRESHOLD_STEPS_USTX;
    let LOCKUP_AMT = 1024 * POX_THRESHOLD_STEPS_USTX;

    // our "tenure counter" is now at 0
    let tip = get_tip(peer.sortdb.as_ref());
    assert_eq!(tip.block_height, 0 + EMPTY_SORTITIONS as u64);

    // first tenure is empty
    let mut latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);

    // Roll to Epoch-2.4 and perform the delegate-stack-extend tests
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch24].start_height
    {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    // in the next tenure, PoX 3 should now exist.
    //  charlie will lock bob and alice through the delegation interface
    let tip = get_tip(peer.sortdb.as_ref());

    let mut alice_nonce = 0;
    let mut bob_nonce = 0;
    let mut charlie_nonce = 0;

    let bob_delegate_tx = make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "delegate-stx",
        vec![
            Value::UInt(2048 * POX_THRESHOLD_STEPS_USTX),
            PrincipalData::from(charlie_address.clone()).into(),
            Value::none(),
            Value::none(),
        ],
    );
    bob_nonce += 1;

    let alice_delegate_tx = make_pox_3_contract_call(
        &alice,
        alice_nonce,
        "delegate-stx",
        vec![
            Value::UInt(2048 * POX_THRESHOLD_STEPS_USTX),
            PrincipalData::from(charlie_address.clone()).into(),
            Value::none(),
            Value::none(),
        ],
    );
    alice_nonce += 1;

    let delegate_stack_tx = make_pox_3_contract_call(
        &charlie,
        charlie_nonce,
        "delegate-stack-stx",
        vec![
            PrincipalData::from(bob_address.clone()).into(),
            Value::UInt(LOCKUP_AMT),
            make_pox_addr(
                AddressHashMode::SerializeP2PKH,
                charlie_address.bytes.clone(),
            ),
            Value::UInt(tip.block_height as u128),
            Value::UInt(3),
        ],
    );
    let delegate_stack_stx_nonce = charlie_nonce;
    let delegate_stack_stx_unlock_ht =
        burnchain.reward_cycle_to_block_height(first_v3_cycle + 3) - 1;
    let delegate_stack_stx_lock_ht = tip.block_height;
    charlie_nonce += 1;

    let delegate_alice_stack_tx = make_pox_3_contract_call(
        &charlie,
        charlie_nonce,
        "delegate-stack-stx",
        vec![
            PrincipalData::from(alice_address.clone()).into(),
            Value::UInt(LOCKUP_AMT),
            make_pox_addr(
                AddressHashMode::SerializeP2PKH,
                charlie_address.bytes.clone(),
            ),
            Value::UInt(tip.block_height as u128),
            Value::UInt(6),
        ],
    );
    charlie_nonce += 1;

    // Charlie agg commits the first 3 cycles, but wait until delegate-extended bob to
    //   agg commit the 4th cycle
    // aggregate commit to each cycle delegate-stack-stx locked for (cycles 6, 7, 8, 9)
    let agg_commit_txs = [0, 1, 2].map(|ix| {
        let tx = make_pox_3_contract_call(
            &charlie,
            charlie_nonce,
            "stack-aggregation-commit",
            vec![
                make_pox_addr(
                    AddressHashMode::SerializeP2PKH,
                    charlie_address.bytes.clone(),
                ),
                Value::UInt(first_v3_cycle as u128 + ix),
            ],
        );
        charlie_nonce += 1;
        tx
    });
    let mut txs = vec![
        bob_delegate_tx,
        alice_delegate_tx,
        delegate_stack_tx,
        delegate_alice_stack_tx,
    ];

    txs.extend(agg_commit_txs);

    latest_block = peer.tenure_with_txs(txs.as_slice(), &mut coinbase_nonce);

    for cycle_number in first_v3_cycle..(first_v3_cycle + 3) {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&charlie).bytes.0.to_vec()
        );
        assert_eq!(reward_set_entries[0].amount_stacked, 2 * LOCKUP_AMT);
    }

    for cycle_number in (first_v3_cycle + 3)..(first_v3_cycle + 6) {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 0);
    }

    let alice_principal = alice_address.clone().into();
    let bob_principal = bob_address.clone().into();
    let charlie_principal: PrincipalData = charlie_address.clone().into();

    let StackingStateCheckData {
        first_cycle: alice_first_cycle,
        lock_period: alice_lock_period,
        ..
    } = check_stacking_state_invariants(
        &mut peer,
        &latest_block,
        &alice_principal,
        false,
        POX_3_NAME,
    );
    let StackingStateCheckData {
        first_cycle: bob_first_cycle,
        lock_period: bob_lock_period,
        ..
    } = check_stacking_state_invariants(
        &mut peer,
        &latest_block,
        &bob_principal,
        false,
        POX_3_NAME,
    );

    assert_eq!(
        alice_first_cycle as u64, first_v3_cycle,
        "Alice's first cycle in PoX-3 stacking state is the next cycle, which is 12"
    );
    assert_eq!(alice_lock_period, 6);
    assert_eq!(
        bob_first_cycle as u64, first_v3_cycle,
        "Bob's first cycle in PoX-3 stacking state is the next cycle, which is 12"
    );
    assert_eq!(bob_lock_period, 3);

    // Extend bob's lockup via `delegate-stack-extend` for 1 more cycle
    let delegate_extend_tx = make_pox_3_contract_call(
        &charlie,
        charlie_nonce,
        "delegate-stack-extend",
        vec![
            PrincipalData::from(bob_address.clone()).into(),
            make_pox_addr(
                AddressHashMode::SerializeP2PKH,
                charlie_address.bytes.clone(),
            ),
            Value::UInt(1),
        ],
    );
    let delegate_stack_extend_nonce = charlie_nonce;
    let delegate_stack_extend_unlock_ht =
        burnchain.reward_cycle_to_block_height(first_v3_cycle + 4) - 1;
    charlie_nonce += 1;

    let agg_commit_tx = make_pox_3_contract_call(
        &charlie,
        charlie_nonce,
        "stack-aggregation-commit",
        vec![
            make_pox_addr(
                AddressHashMode::SerializeP2PKH,
                charlie_address.bytes.clone(),
            ),
            Value::UInt(first_v3_cycle as u128 + 3),
        ],
    );
    let stack_agg_nonce = charlie_nonce;
    let stack_agg_cycle = first_v3_cycle + 3;
    let delegate_stack_extend_unlock_ht =
        burnchain.reward_cycle_to_block_height(first_v3_cycle + 4) - 1;
    charlie_nonce += 1;

    latest_block = peer.tenure_with_txs(&[delegate_extend_tx, agg_commit_tx], &mut coinbase_nonce);
    let StackingStateCheckData {
        first_cycle: alice_first_cycle,
        lock_period: alice_lock_period,
        ..
    } = check_stacking_state_invariants(
        &mut peer,
        &latest_block,
        &alice_principal,
        false,
        POX_3_NAME,
    );
    let StackingStateCheckData {
        first_cycle: bob_first_cycle,
        lock_period: bob_lock_period,
        ..
    } = check_stacking_state_invariants(
        &mut peer,
        &latest_block,
        &bob_principal,
        false,
        POX_3_NAME,
    );

    assert_eq!(
        alice_first_cycle as u64, first_v3_cycle,
        "Alice's first cycle in PoX-2 stacking state is the next cycle, which is 8"
    );
    assert_eq!(alice_lock_period, 6);
    assert_eq!(
        bob_first_cycle as u64, first_v3_cycle,
        "Bob's first cycle in PoX-2 stacking state is the next cycle, which is 8"
    );
    assert_eq!(bob_lock_period, 4);

    for cycle_number in first_v3_cycle..(first_v3_cycle + 4) {
        let cycle_start = burnchain.reward_cycle_to_block_height(cycle_number);
        let reward_set_entries = get_reward_set_entries_at(&mut peer, &latest_block, cycle_start);
        assert_eq!(reward_set_entries.len(), 1);
        assert_eq!(
            reward_set_entries[0].reward_address.bytes(),
            key_to_stacks_addr(&charlie).bytes.0.to_vec()
        );
        assert_eq!(reward_set_entries[0].amount_stacked, 2 * LOCKUP_AMT);
    }

    let height_target = burnchain.reward_cycle_to_block_height(first_v3_cycle) + 1;
    while get_tip(peer.sortdb.as_ref()).block_height < height_target {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        let alice_balance = get_balance(&mut peer, &key_to_stacks_addr(&alice).into());
        assert_eq!(alice_balance, 0);
    }

    let tip = get_tip(peer.sortdb.as_ref());

    // Extend bob's lockup via `delegate-stack-extend` for 1 more cycle
    //  so that we can check the first-reward-cycle is correctly updated
    let delegate_extend_tx = make_pox_3_contract_call(
        &charlie,
        charlie_nonce,
        "delegate-stack-extend",
        vec![
            PrincipalData::from(bob_address.clone()).into(),
            make_pox_addr(
                AddressHashMode::SerializeP2PKH,
                charlie_address.bytes.clone(),
            ),
            Value::UInt(3),
        ],
    );
    charlie_nonce += 1;

    latest_block = peer.tenure_with_txs(&[delegate_extend_tx], &mut coinbase_nonce);
    let StackingStateCheckData {
        first_cycle: alice_first_cycle,
        lock_period: alice_lock_period,
        ..
    } = check_stacking_state_invariants(
        &mut peer,
        &latest_block,
        &alice_principal,
        false,
        POX_3_NAME,
    );
    let StackingStateCheckData {
        first_cycle: bob_first_cycle,
        lock_period: bob_lock_period,
        ..
    } = check_stacking_state_invariants(
        &mut peer,
        &latest_block,
        &bob_principal,
        false,
        POX_3_NAME,
    );

    assert_eq!(
        alice_first_cycle as u64, first_v3_cycle,
        "Alice's first cycle in PoX-2 stacking state is the next cycle, which is 8"
    );
    assert_eq!(alice_lock_period, 6);
    assert_eq!(
        bob_first_cycle as u64, first_v3_cycle,
        "Bob's first cycle in PoX-2 stacking state is the next cycle, which is 8"
    );
    assert_eq!(bob_lock_period, 7);

    // now let's check some tx receipts
    let blocks = observer.get_blocks();

    let mut alice_txs = HashMap::new();
    let mut bob_txs = HashMap::new();
    let mut charlie_txs = HashMap::new();

    for b in blocks.into_iter() {
        for r in b.receipts.into_iter() {
            if let TransactionOrigin::Stacks(ref t) = r.transaction {
                let addr = t.auth.origin().address_testnet();
                eprintln!("TX addr: {}", addr);
                if addr == alice_address {
                    alice_txs.insert(t.auth.get_origin_nonce(), r);
                } else if addr == bob_address {
                    bob_txs.insert(t.auth.get_origin_nonce(), r);
                } else if addr == charlie_address {
                    charlie_txs.insert(t.auth.get_origin_nonce(), r);
                }
            }
        }
    }

    assert_eq!(alice_txs.len(), alice_nonce as usize);
    assert_eq!(bob_txs.len(), bob_nonce as usize);
    assert_eq!(charlie_txs.len(), charlie_nonce as usize);

    for tx in alice_txs.iter() {
        assert!(
            if let Value::Response(ref r) = tx.1.result {
                r.committed
            } else {
                false
            },
            "Alice txs should all have committed okay"
        );
    }
    for tx in bob_txs.iter() {
        assert!(
            if let Value::Response(ref r) = tx.1.result {
                r.committed
            } else {
                false
            },
            "Bob txs should all have committed okay"
        );
    }
    for tx in charlie_txs.iter() {
        assert!(
            if let Value::Response(ref r) = tx.1.result {
                r.committed
            } else {
                false
            },
            "Charlie txs should all have committed okay"
        );
    }

    // Check that the call to `delegate-stack-stx` has a well-formed print event.
    let delegate_stack_tx = &charlie_txs
        .get(&delegate_stack_stx_nonce)
        .unwrap()
        .clone()
        .events[0];
    let pox_addr_val = generate_pox_clarity_value("12d93ae7b61e5b7d905c85828d4320e7c221f433");
    let delegate_op_data = HashMap::from([
        ("lock-amount", Value::UInt(LOCKUP_AMT)),
        (
            "unlock-burn-height",
            Value::UInt(delegate_stack_stx_unlock_ht.into()),
        ),
        (
            "start-burn-height",
            Value::UInt(delegate_stack_stx_lock_ht.into()),
        ),
        ("pox-addr", pox_addr_val.clone()),
        ("lock-period", Value::UInt(3)),
        ("delegator", Value::Principal(charlie_principal.clone())),
    ]);
    let common_data = PoxPrintFields {
        op_name: "delegate-stack-stx".to_string(),
        stacker: Value::Principal(bob_principal.clone()),
        balance: Value::UInt(LOCKUP_AMT),
        locked: Value::UInt(0),
        burnchain_unlock_height: Value::UInt(0),
    };
    check_pox_print_event(delegate_stack_tx, common_data, delegate_op_data);

    // Check that the call to `delegate-stack-extend` has a well-formed print event.
    let delegate_stack_extend_tx = &charlie_txs
        .get(&delegate_stack_extend_nonce)
        .unwrap()
        .clone()
        .events[0];
    let delegate_ext_op_data = HashMap::from([
        ("pox-addr", pox_addr_val.clone()),
        (
            "unlock-burn-height",
            Value::UInt(delegate_stack_extend_unlock_ht.into()),
        ),
        ("extend-count", Value::UInt(1)),
        ("delegator", Value::Principal(charlie_principal.clone())),
    ]);
    let common_data = PoxPrintFields {
        op_name: "delegate-stack-extend".to_string(),
        stacker: Value::Principal(bob_principal.clone()),
        balance: Value::UInt(0),
        locked: Value::UInt(LOCKUP_AMT),
        burnchain_unlock_height: Value::UInt(delegate_stack_stx_unlock_ht.into()),
    };
    check_pox_print_event(delegate_stack_extend_tx, common_data, delegate_ext_op_data);

    // Check that the call to `stack-aggregation-commit` has a well-formed print event.
    let stack_agg_commit_tx = &charlie_txs.get(&stack_agg_nonce).unwrap().clone().events[0];
    let stack_agg_commit_op_data = HashMap::from([
        ("pox-addr", pox_addr_val),
        ("reward-cycle", Value::UInt(stack_agg_cycle.into())),
        ("amount-ustx", Value::UInt(2 * LOCKUP_AMT)),
    ]);
    let common_data = PoxPrintFields {
        op_name: "stack-aggregation-commit".to_string(),
        stacker: Value::Principal(charlie_principal.clone()),
        balance: Value::UInt(LOCKUP_AMT),
        locked: Value::UInt(0),
        burnchain_unlock_height: Value::UInt(0),
    };
    check_pox_print_event(stack_agg_commit_tx, common_data, stack_agg_commit_op_data);
}

#[test]
fn pox_3_getters() {
    // the sim environment produces 25 empty sortitions before
    //  tenures start being tracked.
    let EMPTY_SORTITIONS = 25;

    let (epochs, pox_constants) = make_test_epochs_pox();

    let mut burnchain = Burnchain::default_unittest(
        0,
        &BurnchainHeaderHash::from_hex(BITCOIN_REGTEST_FIRST_BLOCK_HASH).unwrap(),
    );
    burnchain.pox_constants = pox_constants.clone();

    let first_v3_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.pox_3_activation_height as u64)
        .unwrap()
        + 1;

    let observer = TestEventObserver::new();

    let (mut peer, mut keys) = instantiate_pox_peer_with_epoch(
        &burnchain,
        function_name!(),
        Some(epochs.clone()),
        Some(&observer),
    );

    peer.config.check_pox_invariants = Some((first_v3_cycle, first_v3_cycle + 10));

    let alice = keys.pop().unwrap();
    let bob = keys.pop().unwrap();
    let charlie = keys.pop().unwrap();
    let danielle = keys.pop().unwrap();

    let alice_address = key_to_stacks_addr(&alice);
    let bob_address = key_to_stacks_addr(&bob);
    let charlie_address = key_to_stacks_addr(&charlie);
    let mut coinbase_nonce = 0;

    let mut latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    // Roll to Epoch-2.4 and perform the delegate-stack-extend tests
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch24].start_height
    {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    let tip = get_tip(peer.sortdb.as_ref());
    let LOCKUP_AMT = 1024 * POX_THRESHOLD_STEPS_USTX;

    // alice locks in v2
    let alice_lockup = make_pox_3_lockup(
        &alice,
        0,
        LOCKUP_AMT,
        PoxAddress::from_legacy(
            AddressHashMode::SerializeP2PKH,
            key_to_stacks_addr(&alice).bytes,
        ),
        4,
        tip.block_height,
    );

    // bob delegates to charlie
    let bob_delegate_tx = make_pox_3_contract_call(
        &bob,
        0,
        "delegate-stx",
        vec![
            Value::UInt(LOCKUP_AMT),
            PrincipalData::from(charlie_address.clone()).into(),
            Value::none(),
            Value::none(),
        ],
    );

    // charlie calls delegate-stack-stx for bob
    let charlie_delegate_stack_tx = make_pox_3_contract_call(
        &charlie,
        0,
        "delegate-stack-stx",
        vec![
            PrincipalData::from(bob_address.clone()).into(),
            Value::UInt(LOCKUP_AMT),
            make_pox_addr(
                AddressHashMode::SerializeP2PKH,
                charlie_address.bytes.clone(),
            ),
            Value::UInt(tip.block_height as u128),
            Value::UInt(4),
        ],
    );

    let agg_commit_tx_1 = make_pox_3_contract_call(
        &charlie,
        1,
        "stack-aggregation-commit",
        vec![
            make_pox_addr(
                AddressHashMode::SerializeP2PKH,
                charlie_address.bytes.clone(),
            ),
            Value::UInt(first_v3_cycle as u128),
        ],
    );

    let agg_commit_tx_2 = make_pox_3_contract_call(
        &charlie,
        2,
        "stack-aggregation-commit",
        vec![
            make_pox_addr(
                AddressHashMode::SerializeP2PKH,
                charlie_address.bytes.clone(),
            ),
            Value::UInt(first_v3_cycle as u128 + 1),
        ],
    );

    let agg_commit_tx_3 = make_pox_3_contract_call(
        &charlie,
        3,
        "stack-aggregation-commit",
        vec![
            make_pox_addr(
                AddressHashMode::SerializeP2PKH,
                charlie_address.bytes.clone(),
            ),
            Value::UInt(first_v3_cycle as u128 + 2),
        ],
    );

    let reject_pox = make_pox_3_contract_call(&danielle, 0, "reject-pox", vec![]);

    peer.tenure_with_txs(
        &[
            alice_lockup,
            bob_delegate_tx,
            charlie_delegate_stack_tx,
            agg_commit_tx_1,
            agg_commit_tx_2,
            agg_commit_tx_3,
            reject_pox,
        ],
        &mut coinbase_nonce,
    );

    let result = eval_at_tip(&mut peer, "pox-3", &format!("
    {{
        ;; should be none
        get-delegation-info-alice: (get-delegation-info '{}),
        ;; should be (some $charlie_address)
        get-delegation-info-bob: (get-delegation-info '{}),
        ;; should be none
        get-allowance-contract-callers: (get-allowance-contract-callers '{} '{}),
        ;; should be 1
        get-num-reward-set-pox-addresses-current: (get-num-reward-set-pox-addresses u{}),
        ;; should be 0
        get-num-reward-set-pox-addresses-future: (get-num-reward-set-pox-addresses u1000),
        ;; should be 0
        get-partial-stacked-by-cycle-bob-0: (get-partial-stacked-by-cycle {{ version: 0x00, hashbytes: 0x{} }} u{} '{}),
        get-partial-stacked-by-cycle-bob-1: (get-partial-stacked-by-cycle {{ version: 0x00, hashbytes: 0x{} }} u{} '{}),
        get-partial-stacked-by-cycle-bob-2: (get-partial-stacked-by-cycle {{ version: 0x00, hashbytes: 0x{} }} u{} '{}),
        ;; should be LOCKUP_AMT
        get-partial-stacked-by-cycle-bob-3: (get-partial-stacked-by-cycle {{ version: 0x00, hashbytes: 0x{} }} u{} '{}),
        ;; should be LOCKUP_AMT
        get-total-pox-rejection-now: (get-total-pox-rejection u{}),
        ;; should be 0
        get-total-pox-rejection-next: (get-total-pox-rejection u{}),
        ;; should be 0
        get-total-pox-rejection-future: (get-total-pox-rejection u{})
    }}", &alice_address,
        &bob_address,
        &bob_address, &format!("{}.hello-world", &charlie_address), first_v3_cycle + 1,
        &charlie_address.bytes, first_v3_cycle + 0, &charlie_address,
        &charlie_address.bytes, first_v3_cycle + 1, &charlie_address,
        &charlie_address.bytes, first_v3_cycle + 2, &charlie_address,
        &charlie_address.bytes, first_v3_cycle + 3, &charlie_address,
        first_v3_cycle,
        first_v3_cycle + 1,
        first_v3_cycle + 2,
    ));

    eprintln!("{}", &result);
    let data = result.expect_tuple().unwrap().data_map;

    let alice_delegation_info = data
        .get("get-delegation-info-alice")
        .cloned()
        .unwrap()
        .expect_optional()
        .unwrap();
    assert!(alice_delegation_info.is_none());

    let bob_delegation_info = data
        .get("get-delegation-info-bob")
        .cloned()
        .unwrap()
        .expect_optional()
        .unwrap()
        .unwrap()
        .expect_tuple()
        .unwrap()
        .data_map;
    let bob_delegation_addr = bob_delegation_info
        .get("delegated-to")
        .cloned()
        .unwrap()
        .expect_principal()
        .unwrap();
    let bob_delegation_amt = bob_delegation_info
        .get("amount-ustx")
        .cloned()
        .unwrap()
        .expect_u128()
        .unwrap();
    let bob_pox_addr_opt = bob_delegation_info
        .get("pox-addr")
        .cloned()
        .unwrap()
        .expect_optional()
        .unwrap();
    assert_eq!(bob_delegation_addr, charlie_address.to_account_principal());
    assert_eq!(bob_delegation_amt, LOCKUP_AMT);
    assert!(bob_pox_addr_opt.is_none());

    let allowance = data
        .get("get-allowance-contract-callers")
        .cloned()
        .unwrap()
        .expect_optional()
        .unwrap();
    assert!(allowance.is_none());

    let current_num_reward_addrs = data
        .get("get-num-reward-set-pox-addresses-current")
        .cloned()
        .unwrap()
        .expect_u128()
        .unwrap();
    assert_eq!(current_num_reward_addrs, 2);

    let future_num_reward_addrs = data
        .get("get-num-reward-set-pox-addresses-future")
        .cloned()
        .unwrap()
        .expect_u128()
        .unwrap();
    assert_eq!(future_num_reward_addrs, 0);

    for i in 0..3 {
        let key =
            ClarityName::try_from(format!("get-partial-stacked-by-cycle-bob-{}", &i)).unwrap();
        let partial_stacked = data.get(&key).cloned().unwrap().expect_optional().unwrap();
        assert!(partial_stacked.is_none());
    }
    let partial_stacked = data
        .get("get-partial-stacked-by-cycle-bob-3")
        .cloned()
        .unwrap()
        .expect_optional()
        .unwrap()
        .unwrap()
        .expect_tuple()
        .unwrap()
        .data_map
        .get("stacked-amount")
        .cloned()
        .unwrap()
        .expect_u128()
        .unwrap();
    assert_eq!(partial_stacked, LOCKUP_AMT);

    let rejected = data
        .get("get-total-pox-rejection-now")
        .cloned()
        .unwrap()
        .expect_u128()
        .unwrap();
    assert_eq!(rejected, LOCKUP_AMT);

    let rejected = data
        .get("get-total-pox-rejection-next")
        .cloned()
        .unwrap()
        .expect_u128()
        .unwrap();
    assert_eq!(rejected, 0);

    let rejected = data
        .get("get-total-pox-rejection-future")
        .cloned()
        .unwrap()
        .expect_u128()
        .unwrap();
    assert_eq!(rejected, 0);
}

fn get_burn_pox_addr_info(peer: &mut TestPeer) -> (Vec<PoxAddress>, u128) {
    let tip = get_tip(peer.sortdb.as_ref());
    let tip_index_block = tip.get_canonical_stacks_block_id();
    let burn_height = tip.block_height - 1;
    let addrs_and_payout = with_sortdb(peer, |ref mut chainstate, ref mut sortdb| {
        let addrs = chainstate
            .maybe_read_only_clarity_tx(
                &sortdb.index_handle_at_tip(),
                &tip_index_block,
                |clarity_tx| {
                    clarity_tx
                        .with_readonly_clarity_env(
                            false,
                            0x80000000,
                            ClarityVersion::Clarity2,
                            PrincipalData::Standard(StandardPrincipalData::transient()),
                            None,
                            LimitedCostTracker::new_free(),
                            |env| {
                                env.eval_read_only(
                                    &boot_code_id("pox-2", false),
                                    &format!("(get-burn-block-info? pox-addrs u{})", &burn_height),
                                )
                            },
                        )
                        .unwrap()
                },
            )
            .unwrap();
        addrs
    })
    .unwrap()
    .expect_optional()
    .unwrap()
    .expect("FATAL: expected list")
    .expect_tuple()
    .unwrap();

    let addrs = addrs_and_payout
        .get("addrs")
        .unwrap()
        .to_owned()
        .expect_list()
        .unwrap()
        .into_iter()
        .map(|tuple| PoxAddress::try_from_pox_tuple(false, &tuple).unwrap())
        .collect();

    let payout = addrs_and_payout
        .get("payout")
        .unwrap()
        .to_owned()
        .expect_u128()
        .unwrap();
    (addrs, payout)
}

#[test]
fn get_pox_addrs() {
    // the sim environment produces 25 empty sortitions before
    //  tenures start being tracked.
    let EMPTY_SORTITIONS = 25;

    let (epochs, pox_constants) = make_test_epochs_pox();

    let mut burnchain = Burnchain::default_unittest(
        0,
        &BurnchainHeaderHash::from_hex(BITCOIN_REGTEST_FIRST_BLOCK_HASH).unwrap(),
    );
    burnchain.pox_constants = pox_constants.clone();

    let first_v2_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.v1_unlock_height as u64)
        .unwrap()
        + 1;

    let first_v3_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.pox_3_activation_height as u64)
        .unwrap()
        + 1;

    let (mut peer, keys) =
        instantiate_pox_peer_with_epoch(&burnchain, function_name!(), Some(epochs.clone()), None);

    assert_eq!(burnchain.pox_constants.reward_slots(), 6);
    let mut coinbase_nonce = 0;

    let assert_latest_was_burn = |peer: &mut TestPeer| {
        let tip = get_tip(peer.sortdb.as_ref());
        let tip_index_block = tip.get_canonical_stacks_block_id();
        let burn_height = tip.block_height - 1;

        let conn = peer.sortdb().conn();

        // check the *parent* burn block, because that's what we'll be
        //  checking with get_burn_pox_addr_info
        let mut burn_ops =
            SortitionDB::get_block_commits_by_block(conn, &tip.parent_sortition_id).unwrap();
        assert_eq!(burn_ops.len(), 1);
        let commit = burn_ops.pop().unwrap();
        assert!(commit.all_outputs_burn());
        assert!(commit.burn_fee > 0);

        let (addrs, payout) = get_burn_pox_addr_info(peer);
        let tip = get_tip(peer.sortdb.as_ref());
        let tip_index_block = tip.get_canonical_stacks_block_id();
        let burn_height = tip.block_height - 1;
        info!("Checking burn outputs at burn_height = {}", burn_height);
        if peer.config.burnchain.is_in_prepare_phase(burn_height) {
            assert_eq!(addrs.len(), 1);
            assert_eq!(payout, 1000);
            assert!(addrs[0].is_burn());
        } else {
            assert_eq!(addrs.len(), 2);
            assert_eq!(payout, 500);
            assert!(addrs[0].is_burn());
            assert!(addrs[1].is_burn());
        }
    };

    let assert_latest_was_pox = |peer: &mut TestPeer| {
        let tip = get_tip(peer.sortdb.as_ref());
        let tip_index_block = tip.get_canonical_stacks_block_id();
        let burn_height = tip.block_height - 1;

        let conn = peer.sortdb().conn();

        // check the *parent* burn block, because that's what we'll be
        //  checking with get_burn_pox_addr_info
        let mut burn_ops =
            SortitionDB::get_block_commits_by_block(conn, &tip.parent_sortition_id).unwrap();
        assert_eq!(burn_ops.len(), 1);
        let commit = burn_ops.pop().unwrap();
        assert!(!commit.all_outputs_burn());
        let commit_addrs = commit.commit_outs;

        let (addrs, payout) = get_burn_pox_addr_info(peer);
        info!(
            "Checking pox outputs at burn_height = {}, commit_addrs = {:?}, fetch_addrs = {:?}",
            burn_height, commit_addrs, addrs
        );
        assert_eq!(addrs.len(), 2);
        assert_eq!(payout, 500);
        assert!(commit_addrs.contains(&addrs[0]));
        assert!(commit_addrs.contains(&addrs[1]));
        addrs
    };

    // produce blocks until epoch 2.2
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch24].start_height
    {
        peer.tenure_with_txs(&[], &mut coinbase_nonce);
        // if we reach epoch 2.1, perform the check
        if get_tip(peer.sortdb.as_ref()).block_height > epochs[StacksEpochId::Epoch21].start_height
        {
            assert_latest_was_burn(&mut peer);
        }
    }

    let mut txs = vec![];
    let tip_height = get_tip(peer.sortdb.as_ref()).block_height;
    let stackers: Vec<_> = keys
        .iter()
        .zip([
            AddressHashMode::SerializeP2PKH,
            AddressHashMode::SerializeP2SH,
            AddressHashMode::SerializeP2WPKH,
            AddressHashMode::SerializeP2WSH,
        ])
        .map(|(key, hash_mode)| {
            let pox_addr = PoxAddress::from_legacy(hash_mode, key_to_stacks_addr(key).bytes);
            txs.push(make_pox_3_lockup(
                key,
                0,
                1024 * POX_THRESHOLD_STEPS_USTX,
                pox_addr.clone(),
                2,
                tip_height,
            ));
            pox_addr
        })
        .collect();

    let mut latest_block = peer.tenure_with_txs(&txs, &mut coinbase_nonce);
    assert_latest_was_burn(&mut peer);

    let target_height = burnchain.reward_cycle_to_block_height(first_v3_cycle);
    // produce blocks until the first reward phase that everyone should be in
    while get_tip(peer.sortdb.as_ref()).block_height < target_height {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        assert_latest_was_burn(&mut peer);
    }

    // now we should be in the reward phase, produce the reward blocks
    let reward_blocks =
        burnchain.pox_constants.reward_cycle_length - burnchain.pox_constants.prepare_length;
    let mut rewarded = HashSet::new();
    for i in 0..reward_blocks {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        // only the first 2 reward blocks contain pox outputs, because there are 6 slots and only 4 are occuppied
        if i < 2 {
            assert_latest_was_pox(&mut peer)
                .into_iter()
                .filter(|addr| !addr.is_burn())
                .for_each(|addr| {
                    rewarded.insert(addr);
                });
        } else {
            assert_latest_was_burn(&mut peer);
        }
    }

    assert_eq!(rewarded.len(), 4);
    for stacker in stackers.iter() {
        assert!(
            rewarded.contains(stacker),
            "Reward cycle should include {}",
            stacker
        );
    }

    // now we should be back in a prepare phase
    for _i in 0..burnchain.pox_constants.prepare_length {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        assert_latest_was_burn(&mut peer);
    }

    // now we should be in the reward phase, produce the reward blocks
    let mut rewarded = HashSet::new();
    for i in 0..reward_blocks {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        // only the first 2 reward blocks contain pox outputs, because there are 6 slots and only 4 are occuppied
        if i < 2 {
            assert_latest_was_pox(&mut peer)
                .into_iter()
                .filter(|addr| !addr.is_burn())
                .for_each(|addr| {
                    rewarded.insert(addr);
                });
        } else {
            assert_latest_was_burn(&mut peer);
        }
    }

    assert_eq!(rewarded.len(), 4);
    for stacker in stackers.iter() {
        assert!(
            rewarded.contains(stacker),
            "Reward cycle should include {}",
            stacker
        );
    }

    // now we should be back in a prepare phase
    for _i in 0..burnchain.pox_constants.prepare_length {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        assert_latest_was_burn(&mut peer);
    }

    // now we're in the next reward cycle, but everyone is unstacked
    for _i in 0..burnchain.pox_constants.reward_cycle_length {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        assert_latest_was_burn(&mut peer);
    }
}

#[test]
fn stack_with_segwit() {
    // the sim environment produces 25 empty sortitions before
    //  tenures start being tracked.
    let EMPTY_SORTITIONS = 25;

    let (epochs, pox_constants) = make_test_epochs_pox();

    let mut burnchain = Burnchain::default_unittest(
        0,
        &BurnchainHeaderHash::from_hex(BITCOIN_REGTEST_FIRST_BLOCK_HASH).unwrap(),
    );
    burnchain.pox_constants = pox_constants.clone();

    let first_v2_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.v1_unlock_height as u64)
        .unwrap()
        + 1;

    let first_v3_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.pox_3_activation_height as u64)
        .unwrap()
        + 1;

    let (mut peer, keys) =
        instantiate_pox_peer_with_epoch(&burnchain, function_name!(), Some(epochs.clone()), None);

    peer.config.check_pox_invariants = Some((first_v3_cycle, first_v3_cycle + 10));

    assert_eq!(burnchain.pox_constants.reward_slots(), 6);
    let mut coinbase_nonce = 0;

    let assert_latest_was_burn = |peer: &mut TestPeer| {
        let tip = get_tip(peer.sortdb.as_ref());
        let tip_index_block = tip.get_canonical_stacks_block_id();
        let burn_height = tip.block_height - 1;

        let conn = peer.sortdb().conn();

        // check the *parent* burn block, because that's what we'll be
        //  checking with get_burn_pox_addr_info
        let mut burn_ops =
            SortitionDB::get_block_commits_by_block(conn, &tip.parent_sortition_id).unwrap();
        assert_eq!(burn_ops.len(), 1);
        let commit = burn_ops.pop().unwrap();
        assert!(commit.all_outputs_burn());
        assert!(commit.burn_fee > 0);

        let (addrs, payout) = get_burn_pox_addr_info(peer);
        let tip = get_tip(peer.sortdb.as_ref());
        let tip_index_block = tip.get_canonical_stacks_block_id();
        let burn_height = tip.block_height - 1;
        info!("Checking burn outputs at burn_height = {}", burn_height);
        if peer.config.burnchain.is_in_prepare_phase(burn_height) {
            assert_eq!(addrs.len(), 1);
            assert_eq!(payout, 1000);
            assert!(addrs[0].is_burn());
        } else {
            assert_eq!(addrs.len(), 2);
            assert_eq!(payout, 500);
            assert!(addrs[0].is_burn());
            assert!(addrs[1].is_burn());
        }
    };

    let assert_latest_was_pox = |peer: &mut TestPeer| {
        let tip = get_tip(peer.sortdb.as_ref());
        let tip_index_block = tip.get_canonical_stacks_block_id();
        let burn_height = tip.block_height - 1;

        let conn = peer.sortdb().conn();

        // check the *parent* burn block, because that's what we'll be
        //  checking with get_burn_pox_addr_info
        let mut burn_ops =
            SortitionDB::get_block_commits_by_block(conn, &tip.parent_sortition_id).unwrap();
        assert_eq!(burn_ops.len(), 1);
        let commit = burn_ops.pop().unwrap();
        assert!(!commit.all_outputs_burn());
        let commit_addrs = commit.commit_outs;

        let (addrs, payout) = get_burn_pox_addr_info(peer);
        info!(
            "Checking pox outputs at burn_height = {}, commit_addrs = {:?}, fetch_addrs = {:?}",
            burn_height, commit_addrs, addrs
        );
        assert_eq!(addrs.len(), 2);
        assert_eq!(payout, 500);
        assert!(commit_addrs.contains(&addrs[0]));
        assert!(commit_addrs.contains(&addrs[1]));
        addrs
    };

    // produce blocks until epoch 2.2
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch24].start_height
    {
        peer.tenure_with_txs(&[], &mut coinbase_nonce);
        // if we reach epoch 2.1, perform the check
        if get_tip(peer.sortdb.as_ref()).block_height > epochs[StacksEpochId::Epoch21].start_height
        {
            assert_latest_was_burn(&mut peer);
        }
    }

    let mut txs = vec![];
    let tip_height = get_tip(peer.sortdb.as_ref()).block_height;
    let stackers: Vec<_> = keys
        .iter()
        .zip([
            PoxAddress::Addr20(false, PoxAddressType20::P2WPKH, [0x01; 20]),
            PoxAddress::Addr32(false, PoxAddressType32::P2WSH, [0x02; 32]),
            PoxAddress::Addr32(false, PoxAddressType32::P2TR, [0x03; 32]),
            PoxAddress::from_legacy(AddressHashMode::SerializeP2PKH, Hash160([0x04; 20])),
        ])
        .map(|(key, pox_addr)| {
            txs.push(make_pox_3_lockup(
                key,
                0,
                1024 * POX_THRESHOLD_STEPS_USTX,
                pox_addr.clone(),
                2,
                tip_height,
            ));
            pox_addr
        })
        .collect();

    let mut latest_block = peer.tenure_with_txs(&txs, &mut coinbase_nonce);
    assert_latest_was_burn(&mut peer);

    let target_height = burnchain.reward_cycle_to_block_height(first_v3_cycle);
    // produce blocks until the first reward phase that everyone should be in
    while get_tip(peer.sortdb.as_ref()).block_height < target_height {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        assert_latest_was_burn(&mut peer);
    }

    // now we should be in the reward phase, produce the reward blocks
    let reward_blocks =
        burnchain.pox_constants.reward_cycle_length - burnchain.pox_constants.prepare_length;
    let mut rewarded = HashSet::new();
    for i in 0..reward_blocks {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        // only the first 2 reward blocks contain pox outputs, because there are 6 slots and only 4 are occuppied
        if i < 2 {
            assert_latest_was_pox(&mut peer)
                .into_iter()
                .filter(|addr| !addr.is_burn())
                .for_each(|addr| {
                    rewarded.insert(addr);
                });
        } else {
            assert_latest_was_burn(&mut peer);
        }
    }

    assert_eq!(rewarded.len(), 4);
    for stacker in stackers.iter() {
        assert!(
            rewarded.contains(stacker),
            "Reward cycle should include {}",
            stacker
        );
    }

    // now we should be back in a prepare phase
    for _i in 0..burnchain.pox_constants.prepare_length {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        assert_latest_was_burn(&mut peer);
    }

    // now we should be in the reward phase, produce the reward blocks
    let mut rewarded = HashSet::new();
    for i in 0..reward_blocks {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        // only the first 2 reward blocks contain pox outputs, because there are 6 slots and only 4 are occuppied
        if i < 2 {
            assert_latest_was_pox(&mut peer)
                .into_iter()
                .filter(|addr| !addr.is_burn())
                .for_each(|addr| {
                    rewarded.insert(addr);
                });
        } else {
            assert_latest_was_burn(&mut peer);
        }
    }

    assert_eq!(rewarded.len(), 4);
    for stacker in stackers.iter() {
        assert!(
            rewarded.contains(stacker),
            "Reward cycle should include {}",
            stacker
        );
    }

    // now we should be back in a prepare phase
    for _i in 0..burnchain.pox_constants.prepare_length {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        assert_latest_was_burn(&mut peer);
    }

    // now we're in the next reward cycle, but everyone is unstacked
    for _i in 0..burnchain.pox_constants.reward_cycle_length {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
        assert_latest_was_burn(&mut peer);
    }
}

/// In this test case, Alice delegates to Bob.
///  Bob stacks Alice's funds via PoX v2 for 6 cycles. In the third cycle,
///  Bob increases Alice's stacking amount by less than the stacking min.
///  Bob is able to increase the pool's aggregate amount anyway.
///
#[test]
fn stack_aggregation_increase() {
    // the sim environment produces 25 empty sortitions before
    //  tenures start being tracked.
    let EMPTY_SORTITIONS = 25;

    let (epochs, pox_constants) = make_test_epochs_pox();

    let mut burnchain = Burnchain::default_unittest(
        0,
        &BurnchainHeaderHash::from_hex(BITCOIN_REGTEST_FIRST_BLOCK_HASH).unwrap(),
    );
    burnchain.pox_constants = pox_constants.clone();

    let first_v3_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.pox_3_activation_height as u64)
        .unwrap()
        + 1;

    let observer = TestEventObserver::new();

    let (mut peer, mut keys) = instantiate_pox_peer_with_epoch(
        &burnchain,
        function_name!(),
        Some(epochs.clone()),
        Some(&observer),
    );

    peer.config.check_pox_invariants = Some((first_v3_cycle, first_v3_cycle + 10));

    let alice = keys.pop().unwrap();
    let alice_address = key_to_stacks_addr(&alice);
    let alice_principal = PrincipalData::from(alice_address.clone());
    let bob = keys.pop().unwrap();
    let bob_address = key_to_stacks_addr(&bob);
    let bob_principal = PrincipalData::from(bob_address.clone());
    let bob_pox_addr = make_pox_addr(AddressHashMode::SerializeP2PKH, bob_address.bytes.clone());
    let charlie = keys.pop().unwrap();
    let charlie_address = key_to_stacks_addr(&charlie);
    let charlie_pox_addr = make_pox_addr(
        AddressHashMode::SerializeP2PKH,
        charlie_address.bytes.clone(),
    );
    let dan = keys.pop().unwrap();
    let dan_address = key_to_stacks_addr(&dan);
    let dan_principal = PrincipalData::from(dan_address.clone());
    let dan_pox_addr = make_pox_addr(AddressHashMode::SerializeP2PKH, dan_address.bytes.clone());
    let alice_nonce = 0;
    let mut bob_nonce = 0;
    let mut charlie_nonce = 0;
    let mut dan_nonce = 0;

    let alice_first_lock_amount = 512 * POX_THRESHOLD_STEPS_USTX;
    let alice_delegation_amount = alice_first_lock_amount + 1;
    let dan_delegation_amount = alice_first_lock_amount + 1;
    let dan_stack_amount = 511 * POX_THRESHOLD_STEPS_USTX;

    let mut coinbase_nonce = 0;

    // first tenure is empty
    let mut latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);

    // Roll to Epoch-2.4 and perform the delegate-stack-extend tests
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch24].start_height
    {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    let tip = get_tip(peer.sortdb.as_ref());

    // submit delegation tx for alice
    let alice_delegation_1 = make_pox_3_contract_call(
        &alice,
        alice_nonce,
        "delegate-stx",
        vec![
            Value::UInt(alice_delegation_amount),
            bob_principal.clone().into(),
            Value::none(),
            Value::none(),
        ],
    );

    // bob locks some of alice's tokens
    let delegate_stack_tx_bob = make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "delegate-stack-stx",
        vec![
            alice_principal.clone().into(),
            Value::UInt(alice_first_lock_amount),
            bob_pox_addr.clone(),
            Value::UInt(tip.block_height as u128),
            Value::UInt(6),
        ],
    );
    bob_nonce += 1;

    // dan stacks some tokens
    let stack_tx_dan = make_pox_3_lockup(
        &dan,
        dan_nonce,
        dan_stack_amount,
        PoxAddress::from_legacy(AddressHashMode::SerializeP2PKH, dan_address.bytes.clone()),
        12,
        tip.block_height,
    );
    dan_nonce += 1;

    latest_block = peer.tenure_with_txs(
        &[alice_delegation_1, delegate_stack_tx_bob, stack_tx_dan],
        &mut coinbase_nonce,
    );

    // check that the partial stacking state contains entries for bob
    for cycle_number in first_v3_cycle..(first_v3_cycle + 6) {
        let partial_stacked = get_partial_stacked(
            &mut peer,
            &latest_block,
            &bob_pox_addr,
            cycle_number,
            &bob_principal,
            POX_3_NAME,
        );
        assert_eq!(partial_stacked, 512 * POX_THRESHOLD_STEPS_USTX);
    }

    // we'll produce blocks until the 3rd reward cycle gets through the "handled start" code
    //  this is one block after the reward cycle starts
    let height_target = burnchain.reward_cycle_to_block_height(first_v3_cycle + 3) + 1;

    while get_tip(peer.sortdb.as_ref()).block_height < height_target {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    let expected_alice_unlock = burnchain.reward_cycle_to_block_height(first_v3_cycle + 6) - 1;
    let expected_dan_unlock = burnchain.reward_cycle_to_block_height(first_v3_cycle + 12) - 1;

    let alice_bal = get_stx_account_at(&mut peer, &latest_block, &alice_principal);
    assert_eq!(alice_bal.amount_locked(), alice_first_lock_amount);
    assert_eq!(alice_bal.unlock_height(), expected_alice_unlock);

    let dan_bal = get_stx_account_at(&mut peer, &latest_block, &dan_principal);
    assert_eq!(dan_bal.amount_locked(), dan_stack_amount);
    assert_eq!(dan_bal.unlock_height(), expected_dan_unlock);

    // check that the partial stacking state still contains entries for bob
    for cycle_number in first_v3_cycle..(first_v3_cycle + 6) {
        let partial_stacked = get_partial_stacked(
            &mut peer,
            &latest_block,
            &bob_pox_addr,
            cycle_number,
            &bob_principal,
            POX_3_NAME,
        );
        assert_eq!(partial_stacked, 512 * POX_THRESHOLD_STEPS_USTX);
    }

    let tip = get_tip(peer.sortdb.as_ref());
    let cur_reward_cycle = burnchain
        .block_height_to_reward_cycle(tip.block_height)
        .unwrap();

    let mut txs_to_submit = vec![];

    // bob locks in alice's tokens to a PoX address,
    // which clears the partially-stacked state
    txs_to_submit.push(make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "stack-aggregation-commit-indexed",
        vec![
            bob_pox_addr.clone(),
            Value::UInt((cur_reward_cycle + 1) as u128),
        ],
    ));
    let bob_stack_aggregation_commit_indexed = bob_nonce;
    bob_nonce += 1;

    // bob tries to lock tokens in a reward cycle that's already committed (should fail with
    // ERR_STACKING_NO_SUCH_PRINCIPAL)
    txs_to_submit.push(make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "stack-aggregation-increase",
        vec![
            bob_pox_addr.clone(),
            Value::UInt((cur_reward_cycle + 1) as u128),
            Value::UInt(0),
        ],
    ));
    let bob_err_stacking_no_such_principal = bob_nonce;
    bob_nonce += 1;

    // bob locks up 1 more of alice's tokens
    // takes effect in the _next_ reward cycle
    txs_to_submit.push(make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "delegate-stack-increase",
        vec![
            alice_principal.clone().into(),
            bob_pox_addr.clone(),
            Value::UInt(1),
        ],
    ));
    bob_nonce += 1;

    latest_block = peer.tenure_with_txs(&txs_to_submit, &mut coinbase_nonce);
    let tip = get_tip(peer.sortdb.as_ref());
    let cur_reward_cycle = burnchain
        .block_height_to_reward_cycle(tip.block_height)
        .unwrap();

    // locked up more tokens, but unlock height is unchanged
    let alice_bal = get_stx_account_at(&mut peer, &latest_block, &alice_principal);
    assert_eq!(alice_bal.amount_locked(), alice_delegation_amount);
    assert_eq!(alice_bal.unlock_height(), expected_alice_unlock);

    // only 1 uSTX to lock in this next cycle for Alice
    let partial_stacked = get_partial_stacked(
        &mut peer,
        &latest_block,
        &bob_pox_addr,
        cur_reward_cycle + 1,
        &bob_principal,
        POX_3_NAME,
    );
    assert_eq!(partial_stacked, 1);

    for cycle_number in (cur_reward_cycle + 2)..(first_v3_cycle + 6) {
        // alice has 512 * POX_THRESHOLD_STEPS_USTX partially-stacked STX in all cycles after
        let partial_stacked = get_partial_stacked(
            &mut peer,
            &latest_block,
            &bob_pox_addr,
            cycle_number,
            &bob_principal,
            POX_3_NAME,
        );
        assert_eq!(partial_stacked, alice_delegation_amount);
    }

    let mut txs_to_submit = vec![];

    // charlie tries to lock alice's additional tokens to his own PoX address (should fail with
    // ERR_STACKING_NO_SUCH_PRINCIPAL)
    txs_to_submit.push(make_pox_3_contract_call(
        &charlie,
        charlie_nonce,
        "stack-aggregation-increase",
        vec![
            charlie_pox_addr.clone(),
            Value::UInt(cur_reward_cycle as u128),
            Value::UInt(0),
        ],
    ));
    let charlie_err_stacking_no_principal = charlie_nonce;
    charlie_nonce += 1;

    // charlie tries to lock alice's additional tokens to bob's PoX address (should fail with
    // ERR_STACKING_NO_SUCH_PRINCIPAL)
    txs_to_submit.push(make_pox_3_contract_call(
        &charlie,
        charlie_nonce,
        "stack-aggregation-increase",
        vec![
            bob_pox_addr.clone(),
            Value::UInt(cur_reward_cycle as u128),
            Value::UInt(0),
        ],
    ));
    let charlie_err_stacking_no_principal_2 = charlie_nonce;
    charlie_nonce += 1;

    // bob tries to retcon a reward cycle lockup (should fail with ERR_STACKING_INVALID_LOCK_PERIOD)
    txs_to_submit.push(make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "stack-aggregation-increase",
        vec![
            bob_pox_addr.clone(),
            Value::UInt(cur_reward_cycle as u128),
            Value::UInt(0),
        ],
    ));
    let bob_err_stacking_invalid_lock_period = bob_nonce;
    bob_nonce += 1;

    // bob tries to lock tokens in a reward cycle that has no tokens stacked in it yet (should
    // fail with ERR_DELEGATION_NO_REWARD_CYCLE)
    txs_to_submit.push(make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "stack-aggregation-increase",
        vec![
            bob_pox_addr.clone(),
            Value::UInt((cur_reward_cycle + 13) as u128),
            Value::UInt(0),
        ],
    ));
    let bob_err_delegation_no_reward_cycle = bob_nonce;
    bob_nonce += 1;

    // bob tries to lock tokens to a non-existant PoX reward address (should fail with
    // ERR_DELEGATION_NO_REWARD_SLOT)
    txs_to_submit.push(make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "stack-aggregation-increase",
        vec![
            bob_pox_addr.clone(),
            Value::UInt((cur_reward_cycle + 1) as u128),
            Value::UInt(2),
        ],
    ));
    let bob_err_delegation_no_reward_slot = bob_nonce;
    bob_nonce += 1;

    // bob tries to lock tokens to the wrong PoX address (should fail with ERR_DELEGATION_WRONG_REWARD_SLOT).
    // slot 0 belongs to dan.
    txs_to_submit.push(make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "stack-aggregation-increase",
        vec![
            bob_pox_addr.clone(),
            Value::UInt((cur_reward_cycle + 1) as u128),
            Value::UInt(0),
        ],
    ));
    let bob_err_delegation_wrong_reward_slot = bob_nonce;
    bob_nonce += 1;

    // bob locks tokens for Alice (bob's previous stack-aggregation-commit put his PoX address in
    // slot 1 for this reward cycle)
    txs_to_submit.push(make_pox_3_contract_call(
        &bob,
        bob_nonce,
        "stack-aggregation-increase",
        vec![
            bob_pox_addr.clone(),
            Value::UInt((cur_reward_cycle + 1) as u128),
            Value::UInt(1),
        ],
    ));
    bob_nonce += 1;

    latest_block = peer.tenure_with_txs(&txs_to_submit, &mut coinbase_nonce);

    assert_eq!(
        get_stx_account_at(&mut peer, &latest_block, &alice_principal).amount_locked(),
        alice_delegation_amount
    );

    // now let's check some tx receipts

    let alice_address = key_to_stacks_addr(&alice);
    let blocks = observer.get_blocks();

    let mut alice_txs = HashMap::new();
    let mut bob_txs = HashMap::new();
    let mut charlie_txs = HashMap::new();

    for b in blocks.into_iter() {
        for r in b.receipts.into_iter() {
            if let TransactionOrigin::Stacks(ref t) = r.transaction {
                let addr = t.auth.origin().address_testnet();
                if addr == alice_address {
                    alice_txs.insert(t.auth.get_origin_nonce(), r);
                } else if addr == bob_address {
                    bob_txs.insert(t.auth.get_origin_nonce(), r);
                } else if addr == charlie_address {
                    charlie_txs.insert(t.auth.get_origin_nonce(), r);
                }
            }
        }
    }

    assert_eq!(alice_txs.len(), 1);
    assert_eq!(bob_txs.len(), 9);
    assert_eq!(charlie_txs.len(), 2);

    // bob's stack-aggregation-commit-indexed succeeded and returned the right index
    assert_eq!(
        &bob_txs[&bob_stack_aggregation_commit_indexed]
            .result
            .to_string(),
        "(ok u1)"
    );

    // check bob's errors
    assert_eq!(
        &bob_txs[&bob_err_stacking_no_such_principal]
            .result
            .to_string(),
        "(err 4)"
    );
    assert_eq!(
        &bob_txs[&bob_err_stacking_invalid_lock_period]
            .result
            .to_string(),
        "(err 2)"
    );
    assert_eq!(
        &bob_txs[&bob_err_delegation_no_reward_cycle]
            .result
            .to_string(),
        "(err 4)"
    );
    assert_eq!(
        &bob_txs[&bob_err_delegation_no_reward_slot]
            .result
            .to_string(),
        "(err 28)"
    );
    assert_eq!(
        &bob_txs[&bob_err_delegation_wrong_reward_slot]
            .result
            .to_string(),
        "(err 29)"
    );

    // check charlie's errors
    assert_eq!(
        &charlie_txs[&charlie_err_stacking_no_principal]
            .result
            .to_string(),
        "(err 4)"
    );
    assert_eq!(
        &charlie_txs[&charlie_err_stacking_no_principal_2]
            .result
            .to_string(),
        "(err 4)"
    );
}

/// Verify that delegate-stx validates the PoX addr, if given
#[test]
fn pox_3_delegate_stx_addr_validation() {
    // the sim environment produces 25 empty sortitions before
    //  tenures start being tracked.
    let EMPTY_SORTITIONS = 25;

    let (epochs, pox_constants) = make_test_epochs_pox();

    let mut burnchain = Burnchain::default_unittest(
        0,
        &BurnchainHeaderHash::from_hex(BITCOIN_REGTEST_FIRST_BLOCK_HASH).unwrap(),
    );
    burnchain.pox_constants = pox_constants.clone();

    let first_v3_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.pox_3_activation_height as u64)
        .unwrap()
        + 1;

    let (mut peer, mut keys) =
        instantiate_pox_peer_with_epoch(&burnchain, function_name!(), Some(epochs.clone()), None);

    peer.config.check_pox_invariants = Some((first_v3_cycle, first_v3_cycle + 10));

    let mut coinbase_nonce = 0;
    let alice = keys.pop().unwrap();
    let bob = keys.pop().unwrap();
    let charlie = keys.pop().unwrap();
    let danielle = keys.pop().unwrap();
    let alice_address = key_to_stacks_addr(&alice);
    let bob_address = key_to_stacks_addr(&bob);
    let charlie_address = key_to_stacks_addr(&charlie);
    let LOCKUP_AMT = 1024 * POX_THRESHOLD_STEPS_USTX;

    // first tenure is empty
    let mut latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);

    // Roll to Epoch-2.4 and perform the delegate-stack-extend tests
    while get_tip(peer.sortdb.as_ref()).block_height <= epochs[StacksEpochId::Epoch24].start_height
    {
        latest_block = peer.tenure_with_txs(&[], &mut coinbase_nonce);
    }

    let tip = get_tip(peer.sortdb.as_ref());
    let cur_reward_cycle = burnchain
        .block_height_to_reward_cycle(tip.block_height)
        .unwrap();

    // alice delegates to charlie in v3 to a valid address
    let alice_delegation = make_pox_3_contract_call(
        &alice,
        0,
        "delegate-stx",
        vec![
            Value::UInt(LOCKUP_AMT),
            PrincipalData::from(charlie_address.clone()).into(),
            Value::none(),
            Value::some(make_pox_addr(
                AddressHashMode::SerializeP2PKH,
                alice_address.bytes.clone(),
            ))
            .unwrap(),
        ],
    );

    let bob_bad_pox_addr = Value::Tuple(
        TupleData::from_data(vec![
            (
                ClarityName::try_from("version".to_owned()).unwrap(),
                Value::buff_from_byte(0xff),
            ),
            (
                ClarityName::try_from("hashbytes".to_owned()).unwrap(),
                Value::Sequence(SequenceData::Buffer(BuffData {
                    data: bob_address.bytes.as_bytes().to_vec(),
                })),
            ),
        ])
        .unwrap(),
    );

    // bob delegates to charlie in v3 with an invalid address
    let bob_delegation = make_pox_3_contract_call(
        &bob,
        0,
        "delegate-stx",
        vec![
            Value::UInt(LOCKUP_AMT),
            PrincipalData::from(charlie_address.clone()).into(),
            Value::none(),
            Value::some(bob_bad_pox_addr).unwrap(),
        ],
    );

    peer.tenure_with_txs(&[alice_delegation, bob_delegation], &mut coinbase_nonce);

    let result = eval_at_tip(
        &mut peer,
        "pox-3",
        &format!(
            "
    {{
        ;; should be (some $charlie_address)
        get-delegation-info-alice: (get-delegation-info '{}),
        ;; should be none
        get-delegation-info-bob: (get-delegation-info '{}),
    }}",
            &alice_address, &bob_address,
        ),
    );

    eprintln!("{}", &result);
    let data = result.expect_tuple().unwrap().data_map;

    // bob had an invalid PoX address
    let bob_delegation_info = data
        .get("get-delegation-info-bob")
        .cloned()
        .unwrap()
        .expect_optional()
        .unwrap();
    assert!(bob_delegation_info.is_none());

    // alice was valid
    let alice_delegation_info = data
        .get("get-delegation-info-alice")
        .cloned()
        .unwrap()
        .expect_optional()
        .unwrap()
        .unwrap()
        .expect_tuple()
        .unwrap()
        .data_map;
    let alice_delegation_addr = alice_delegation_info
        .get("delegated-to")
        .cloned()
        .unwrap()
        .expect_principal()
        .unwrap();
    let alice_delegation_amt = alice_delegation_info
        .get("amount-ustx")
        .cloned()
        .unwrap()
        .expect_u128()
        .unwrap();
    let alice_pox_addr_opt = alice_delegation_info
        .get("pox-addr")
        .cloned()
        .unwrap()
        .expect_optional()
        .unwrap();
    assert_eq!(
        alice_delegation_addr,
        charlie_address.to_account_principal()
    );
    assert_eq!(alice_delegation_amt, LOCKUP_AMT);
    assert!(alice_pox_addr_opt.is_some());

    let alice_pox_addr = alice_pox_addr_opt.unwrap();

    assert_eq!(
        alice_pox_addr,
        make_pox_addr(AddressHashMode::SerializeP2PKH, alice_address.bytes.clone(),)
    );
}
