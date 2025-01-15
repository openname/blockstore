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

use clarity::vm::clarity::ClarityConnection;
use clarity::vm::contexts::OwnedEnvironment;
use clarity::vm::costs::LimitedCostTracker;
use clarity::vm::tests::symbols_from_values;
use clarity::vm::types::{
    PrincipalData, QualifiedContractIdentifier, StacksAddressExtensions, TupleData,
};
use clarity::vm::Value::Principal;
use clarity::vm::{ClarityName, ClarityVersion, ContractName, Value};
use stacks_common::address::AddressHashMode;
use stacks_common::consts;
use stacks_common::consts::SIGNER_SLOTS_PER_USER;
use stacks_common::types::chainstate::{
    BurnchainHeaderHash, StacksAddress, StacksBlockId, StacksPrivateKey, StacksPublicKey,
};
use stacks_common::types::PublicKey;
use stacks_common::util::secp256k1::Secp256k1PublicKey;

use super::{RawRewardSetEntry, SIGNERS_PK_LEN};
use crate::burnchains::Burnchain;
use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::burn::BlockSnapshot;
use crate::chainstate::nakamoto::coordinator::tests::{boot_nakamoto, make_token_transfer};
use crate::chainstate::nakamoto::test_signers::TestSigners;
use crate::chainstate::nakamoto::tests::get_account;
use crate::chainstate::nakamoto::tests::node::TestStacker;
use crate::chainstate::stacks::address::PoxAddress;
use crate::chainstate::stacks::boot::pox_2_tests::with_clarity_db_ro;
use crate::chainstate::stacks::boot::pox_4_tests::{
    assert_latest_was_burn, get_last_block_sender_transactions, make_test_epochs_pox,
    prepare_pox4_test,
};
use crate::chainstate::stacks::boot::test::{
    instantiate_pox_peer_with_epoch, key_to_stacks_addr, make_pox_4_lockup, with_sortdb,
};
use crate::chainstate::stacks::boot::{NakamotoSignerEntry, SIGNERS_NAME, SIGNERS_VOTING_NAME};
use crate::chainstate::stacks::db::StacksChainState;
use crate::chainstate::stacks::index::marf::MarfConnection;
use crate::chainstate::stacks::{
    StacksTransaction, StacksTransactionSigner, TenureChangeCause, TransactionAuth,
    TransactionPayload, TransactionPostConditionMode, TransactionVersion,
};
use crate::clarity_vm::database::HeadersDBConn;
use crate::core::BITCOIN_REGTEST_FIRST_BLOCK_HASH;
use crate::net::stackerdb::{STACKERDB_CONFIG_FUNCTION, STACKERDB_INV_MAX};
use crate::net::test::{TestEventObserver, TestPeer};
use crate::util_lib::boot::{boot_code_addr, boot_code_id, boot_code_test_addr};

#[test]
fn make_signer_units() {
    assert_eq!(StacksChainState::make_signer_set(100, &[]), None);

    fn stub_entry(signer: u64, amount: u128) -> RawRewardSetEntry {
        let mut signer_bytes = [0; SIGNERS_PK_LEN];
        signer_bytes[0..8].copy_from_slice(&signer.to_be_bytes());
        RawRewardSetEntry {
            signer: Some(signer_bytes),
            stacker: None,
            reward_address: PoxAddress::standard_burn_address(false),
            amount_stacked: amount,
        }
    }
    fn stub_out(signer: u64, amount: u128, weight: u32) -> NakamotoSignerEntry {
        let mut signer_bytes = [0; SIGNERS_PK_LEN];
        signer_bytes[0..8].copy_from_slice(&signer.to_be_bytes());
        NakamotoSignerEntry {
            signing_key: signer_bytes,
            stacked_amt: amount,
            weight,
        }
    }

    fn perform_test(threshold: u128, input: &[(u64, u128)], expected: &[(u64, u128, u32)]) {
        let in_entries: Vec<_> = input
            .iter()
            .map(|(signer, amount)| stub_entry(*signer, *amount))
            .collect();
        let expected: Vec<_> = expected
            .iter()
            .map(|(signer, amount, weight)| stub_out(*signer, *amount, *weight))
            .collect();
        assert_eq!(
            StacksChainState::make_signer_set(threshold, &in_entries),
            Some(expected)
        );
    }

    let threshold = 10_000;
    let input_set = [
        (2, 10_001),
        (0, 10_000),
        (1, 10_000),
        (0, 30_000),
        (2, 9_999),
        (1, 1),
    ];
    let expected = [(0, 40_000, 4), (1, 10_001, 1), (2, 20_000, 2)];

    perform_test(threshold, &input_set, &expected);

    let threshold = 10_000;
    let input_set = [
        (2, 10_001),
        (0, 10_000),
        (1, 10_000),
        (0, 30_000),
        (2, 9_999),
        (1, 1),
        (3, 9_999),
    ];
    let expected = [(0, 40_000, 4), (1, 10_001, 1), (2, 20_000, 2)];

    perform_test(threshold, &input_set, &expected);
}

#[test]
#[should_panic]
fn make_signer_sanity_panic_0() {
    let bad_set = [
        RawRewardSetEntry {
            reward_address: PoxAddress::standard_burn_address(false),
            amount_stacked: 10,
            stacker: None,
            signer: Some([0; SIGNERS_PK_LEN]),
        },
        RawRewardSetEntry {
            reward_address: PoxAddress::standard_burn_address(false),
            amount_stacked: 10,
            stacker: None,
            signer: None,
        },
    ];
    StacksChainState::make_signer_set(5, &bad_set);
}

#[test]
#[should_panic]
fn make_signer_sanity_panic_1() {
    let bad_set = [
        RawRewardSetEntry {
            reward_address: PoxAddress::standard_burn_address(false),
            amount_stacked: 10,
            stacker: None,
            signer: None,
        },
        RawRewardSetEntry {
            reward_address: PoxAddress::standard_burn_address(false),
            amount_stacked: 10,
            stacker: None,
            signer: Some([0; SIGNERS_PK_LEN]),
        },
    ];
    StacksChainState::make_signer_set(5, &bad_set);
}

#[test]
fn signers_get_config() {
    let (burnchain, mut peer, keys, latest_block, ..) =
        prepare_pox4_test(function_name!(), None, false);

    assert_eq!(
        readonly_call(
            &mut peer,
            &latest_block,
            "signers".into(),
            STACKERDB_CONFIG_FUNCTION.into(),
            vec![],
        ),
        Value::okay(Value::Tuple(
            TupleData::from_data(vec![
                ("chunk-size".into(), Value::UInt(2 * 1024 * 1024)),
                ("write-freq".into(), Value::UInt(0)),
                ("max-writes".into(), Value::UInt(u32::MAX.into())),
                ("max-neighbors".into(), Value::UInt(32)),
                (
                    "hint-replicas".into(),
                    Value::cons_list_unsanitized(vec![]).unwrap()
                )
            ])
            .unwrap()
        ))
        .unwrap()
    );

    for signer_set in 0..2 {
        for message_id in 0..SIGNER_SLOTS_PER_USER {
            let contract_name = format!("signers-{}-{}", &signer_set, &message_id);
            let config = readonly_call(
                &mut peer,
                &latest_block,
                contract_name.as_str().into(),
                STACKERDB_CONFIG_FUNCTION.into(),
                vec![],
            );
            assert_eq!(
                config,
                Value::okay(Value::Tuple(
                    TupleData::from_data(vec![
                        ("chunk-size".into(), Value::UInt(2 * 1024 * 1024)),
                        ("write-freq".into(), Value::UInt(0)),
                        ("max-writes".into(), Value::UInt(u32::MAX.into())),
                        ("max-neighbors".into(), Value::UInt(32)),
                        (
                            "hint-replicas".into(),
                            Value::cons_list_unsanitized(vec![]).unwrap()
                        )
                    ])
                    .unwrap()
                ))
                .unwrap()
            )
        }
    }
}

#[test]
fn signers_get_signer_keys_from_stackerdb() {
    let stacker_1 = TestStacker::from_seed(&[3, 4]);
    let stacker_2 = TestStacker::from_seed(&[5, 6]);

    let (mut peer, test_signers, latest_block_id, _) = prepare_signers_test(
        function_name!(),
        vec![],
        &[stacker_1.clone(), stacker_2.clone()],
        None,
    );

    let private_key = peer.config.private_key.clone();

    let mut expected_signers: Vec<_> =
        [&stacker_1.signer_private_key, &stacker_2.signer_private_key]
            .iter()
            .map(|sk| {
                let pk = Secp256k1PublicKey::from_private(sk);
                let pk_bytes = pk.to_bytes_compressed();
                let signer_addr = StacksAddress::p2pkh(false, &pk);
                let stackerdb_entry = TupleData::from_data(vec![
                    ("signer".into(), PrincipalData::from(signer_addr).into()),
                    ("num-slots".into(), Value::UInt(1)),
                ])
                .unwrap();
                (pk_bytes, stackerdb_entry)
            })
            .collect();
    // should be sorted by the pk bytes
    expected_signers.sort_by_key(|x| x.0.clone());
    let expected_stackerdb_slots = Value::cons_list_unsanitized(
        expected_signers
            .into_iter()
            .map(|(_pk, entry)| Value::from(entry))
            .collect(),
    )
    .unwrap();

    let signers = readonly_call(
        &mut peer,
        &latest_block_id,
        "signers".into(),
        "stackerdb-get-signer-slots-page".into(),
        vec![Value::UInt(1)],
    )
    .expect_result_ok()
    .unwrap();

    assert_eq!(signers, expected_stackerdb_slots);
}

#[test]
fn signers_db_get_slots() {
    let stacker_1 = TestStacker::from_seed(&[3, 4]);
    let stacker_2 = TestStacker::from_seed(&[5, 6]);

    let (mut peer, test_signers, latest_block_id, _) = prepare_signers_test(
        function_name!(),
        vec![],
        &[stacker_1.clone(), stacker_2.clone()],
        None,
    );

    let private_key = peer.config.private_key.clone();

    let mut expected_signers: Vec<_> =
        [&stacker_1.signer_private_key, &stacker_2.signer_private_key]
            .iter()
            .map(|sk| {
                let pk = Secp256k1PublicKey::from_private(sk);
                let pk_bytes = pk.to_bytes_compressed();
                let signer_addr = StacksAddress::p2pkh(false, &pk);
                let stackerdb_entry = TupleData::from_data(vec![
                    ("signer".into(), PrincipalData::from(signer_addr).into()),
                    ("num-slots".into(), Value::UInt(1)),
                ])
                .unwrap();
                (pk_bytes, stackerdb_entry)
            })
            .collect();

    // should be sorted by the pk bytes
    expected_signers.sort_by_key(|x| x.0.clone());
    let expected_stackerdb_slots = Value::cons_list_unsanitized(
        expected_signers
            .into_iter()
            .map(|(_pk, entry)| Value::from(entry))
            .collect(),
    )
    .unwrap();

    for signer_set in 0..2 {
        for message_id in 0..SIGNER_SLOTS_PER_USER {
            let contract_name = format!("signers-{}-{}", &signer_set, &message_id);
            let signers = readonly_call(
                &mut peer,
                &latest_block_id,
                contract_name.as_str().into(),
                "stackerdb-get-signer-slots".into(),
                vec![],
            )
            .expect_result_ok()
            .unwrap();

            debug!("Check .{}", &contract_name);
            if signer_set == 0 {
                assert_eq!(signers.expect_list().unwrap(), vec![]);
            } else {
                assert_eq!(signers, expected_stackerdb_slots);
            }
        }
    }
}

pub fn prepare_signers_test<'a>(
    test_name: &str,
    initial_balances: Vec<(PrincipalData, u64)>,
    stackers: &[TestStacker],
    observer: Option<&'a TestEventObserver>,
) -> (TestPeer<'a>, TestSigners, StacksBlockId, u128) {
    let signer_keys = stackers
        .iter()
        .map(|s| s.signer_private_key.clone())
        .collect::<Vec<_>>();
    let mut test_signers = TestSigners::new(signer_keys);

    let mut peer = boot_nakamoto(
        test_name,
        initial_balances,
        &mut test_signers,
        stackers,
        observer,
    );

    let (burn_ops, mut tenure_change, miner_key) =
        peer.begin_nakamoto_tenure(TenureChangeCause::BlockFound);

    let (_, _, consensus_hash) = peer.next_burnchain_block(burn_ops);

    let vrf_proof = peer.make_nakamoto_vrf_proof(miner_key);

    tenure_change.tenure_consensus_hash = consensus_hash.clone();
    tenure_change.burn_view_consensus_hash = consensus_hash.clone();
    let tenure_change_tx = peer
        .miner
        .make_nakamoto_tenure_change(tenure_change.clone());
    let coinbase_tx = peer.miner.make_nakamoto_coinbase(None, vrf_proof);

    let blocks_and_sizes = peer.make_nakamoto_tenure(
        tenure_change_tx,
        coinbase_tx,
        &mut test_signers,
        |_miner, _chainstate, _sort_dbconn, _blocks| vec![],
    );
    let latest_block_id = blocks_and_sizes.last().unwrap().0.block_id();

    let current_reward_cycle = readonly_call(
        &mut peer,
        &latest_block_id,
        SIGNERS_VOTING_NAME.into(),
        "current-reward-cycle".into(),
        vec![],
    )
    .expect_u128()
    .unwrap();

    assert_eq!(current_reward_cycle, 7);

    let last_set_cycle = readonly_call(
        &mut peer,
        &latest_block_id,
        SIGNERS_NAME.into(),
        "get-last-set-cycle".into(),
        vec![],
    )
    .expect_result_ok()
    .unwrap()
    .expect_u128()
    .unwrap();

    assert_eq!(last_set_cycle, 7);

    (peer, test_signers, latest_block_id, current_reward_cycle)
}

fn advance_blocks(
    peer: &mut TestPeer,
    test_signers: &mut TestSigners,
    stacker_private_key: &StacksPrivateKey,
    num_blocks: u64,
) -> StacksBlockId {
    let current_height = peer.get_burnchain_view().unwrap().burn_block_height;

    //let key = peer.config.private_key;

    let (burn_ops, mut tenure_change, miner_key) =
        peer.begin_nakamoto_tenure(TenureChangeCause::BlockFound);

    let (_, _, consensus_hash) = peer.next_burnchain_block(burn_ops);

    let vrf_proof = peer.make_nakamoto_vrf_proof(miner_key);

    tenure_change.tenure_consensus_hash = consensus_hash.clone();
    tenure_change.burn_view_consensus_hash = consensus_hash.clone();
    let tenure_change_tx = peer
        .miner
        .make_nakamoto_tenure_change(tenure_change.clone());
    let coinbase_tx = peer.miner.make_nakamoto_coinbase(None, vrf_proof);
    let recipient_addr = boot_code_addr(false);
    let blocks_and_sizes = peer.make_nakamoto_tenure(
        tenure_change_tx,
        coinbase_tx.clone(),
        test_signers,
        |miner, chainstate, sortdb, blocks| {
            if blocks.len() < num_blocks as usize {
                let addr = key_to_stacks_addr(stacker_private_key);
                let account = get_account(chainstate, sortdb, &addr);
                let stx_transfer = make_token_transfer(
                    chainstate,
                    sortdb,
                    stacker_private_key,
                    account.nonce,
                    1,
                    1,
                    &recipient_addr,
                );
                vec![stx_transfer]
            } else {
                vec![]
            }
        },
    );
    info!("tenure length {}", blocks_and_sizes.len());
    let latest_block_id = blocks_and_sizes.last().unwrap().0.block_id();
    latest_block_id
}

pub fn readonly_call(
    peer: &mut TestPeer,
    tip: &StacksBlockId,
    boot_contract: ContractName,
    function_name: ClarityName,
    args: Vec<Value>,
) -> Value {
    with_sortdb(peer, |chainstate, sortdb| {
        readonly_call_with_sortdb(chainstate, sortdb, tip, boot_contract, function_name, args)
    })
}

pub fn readonly_call_with_sortdb(
    chainstate: &mut StacksChainState,
    sortdb: &SortitionDB,
    tip: &StacksBlockId,
    boot_contract: ContractName,
    function_name: ClarityName,
    args: Vec<Value>,
) -> Value {
    chainstate
        .with_read_only_clarity_tx(
            &sortdb.index_handle_at_block(chainstate, tip).unwrap(),
            tip,
            |connection| {
                connection
                    .with_readonly_clarity_env(
                        false,
                        0x80000000,
                        ClarityVersion::Clarity2,
                        PrincipalData::from(boot_code_addr(false)),
                        None,
                        LimitedCostTracker::new_free(),
                        |env| {
                            env.execute_contract_allow_private(
                                &boot_code_id(&boot_contract, false),
                                &function_name,
                                &symbols_from_values(args),
                                true,
                            )
                        },
                    )
                    .unwrap()
            },
        )
        .unwrap()
}

pub fn get_signer_index(
    peer: &mut TestPeer<'_>,
    latest_block_id: StacksBlockId,
    signer_address: StacksAddress,
    cycle_index: u128,
) -> u128 {
    let cycle_mod = cycle_index % 2;
    let signers = readonly_call(
        peer,
        &latest_block_id,
        "signers".into(),
        "stackerdb-get-signer-slots-page".into(),
        vec![Value::UInt(cycle_mod)],
    )
    .expect_result_ok()
    .unwrap()
    .expect_list()
    .unwrap();

    signers
        .iter()
        .position(|value| {
            value
                .clone()
                .expect_tuple()
                .unwrap()
                .get("signer")
                .unwrap()
                .clone()
                .expect_principal()
                .unwrap()
                == signer_address.to_account_principal()
        })
        .expect("signer not found") as u128
}
