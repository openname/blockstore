use std::collections::{HashMap, HashSet};
use std::{env, thread};

use ::core::str;
use clarity::vm::types::{PrincipalData, QualifiedContractIdentifier};
use clarity::vm::ClarityVersion;
use stacks::burnchains::bitcoin::address::{
    BitcoinAddress, LegacyBitcoinAddressType, SegwitBitcoinAddress,
};
use stacks::burnchains::bitcoin::BitcoinNetworkType;
use stacks::burnchains::{Burnchain, PoxConstants, Txid};
use stacks::chainstate::burn::db::sortdb::SortitionDB;
use stacks::chainstate::burn::operations::leader_block_commit::{
    BURN_BLOCK_MINED_AT_MODULUS, OUTPUTS_PER_COMMIT,
};
use stacks::chainstate::burn::operations::{
    BlockstackOperationType, LeaderBlockCommitOp, PreStxOp, TransferStxOp,
};
use stacks::chainstate::coordinator::comm::CoordinatorChannels;
use stacks::chainstate::stacks::address::PoxAddress;
use stacks::chainstate::stacks::db::StacksChainState;
use stacks::chainstate::stacks::miner::{
    set_mining_spend_amount, signal_mining_blocked, signal_mining_ready,
};
use stacks::chainstate::stacks::StacksBlockHeader;
use stacks::clarity_cli::vm_execute as execute;
use stacks::config::{Config, InitialBalance};
use stacks::core::{self, EpochList, BURNCHAIN_TX_SEARCH_WINDOW};
use stacks::util_lib::boot::boot_code_id;
use stacks_common::types::chainstate::{
    BlockHeaderHash, BurnchainHeaderHash, StacksAddress, StacksBlockId, VRFSeed,
};
use stacks_common::types::PrivateKey;
use stacks_common::util::hash::{Hash160, Sha256Sum};
use stacks_common::util::secp256k1::{Secp256k1PrivateKey, Secp256k1PublicKey};
use stacks_common::util::sleep_ms;

use crate::burnchains::bitcoin_regtest_controller::UTXO;
use crate::neon::RunLoopCounter;
use crate::operations::BurnchainOpSigner;
use crate::stacks_common::address::AddressHashMode;
use crate::stacks_common::types::Address;
use crate::stacks_common::util::hash::{bytes_to_hex, hex_bytes};
use crate::tests::bitcoin_regtest::BitcoinCoreController;
use crate::tests::neon_integrations::*;
use crate::tests::*;
use crate::{neon, BitcoinRegtestController, BurnchainController, Keychain};

const MINER_BURN_PUBLIC_KEY: &str =
    "03dc62fe0b8964d01fc9ca9a5eec0e22e557a12cc656919e648f04e0b26fea5faa";

fn advance_to_2_1(
    mut initial_balances: Vec<InitialBalance>,
    block_reward_recipient: Option<PrincipalData>,
    pox_constants: Option<PoxConstants>,
    segwit: bool,
) -> (
    Config,
    BitcoinCoreController,
    BitcoinRegtestController,
    RunLoopCounter,
    CoordinatorChannels,
) {
    let epoch_2_05 = 210;
    let epoch_2_1 = 215;

    test_observer::spawn();

    let (mut conf, miner_account) = neon_integration_test_conf();

    conf.burnchain.peer_host = "localhost".to_string();
    conf.initial_balances.append(&mut initial_balances);
    conf.miner.block_reward_recipient = block_reward_recipient;
    test_observer::register_any(&mut conf);

    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = epoch_2_05;
    epochs[StacksEpochId::Epoch2_05].start_height = epoch_2_05;
    epochs[StacksEpochId::Epoch2_05].end_height = epoch_2_1;
    epochs[StacksEpochId::Epoch21].start_height = epoch_2_1;

    conf.burnchain.epochs = Some(epochs);

    conf.miner.segwit = segwit;

    let mut burnchain_config = Burnchain::regtest(&conf.get_burn_db_path());

    let reward_cycle_len = 2000;
    let prepare_phase_len = 100;
    let pox_constants = pox_constants.unwrap_or(PoxConstants::new(
        reward_cycle_len,
        prepare_phase_len,
        4 * prepare_phase_len / 5,
        5,
        15,
        u64::MAX - 2,
        u64::MAX - 1,
        u32::MAX,
        u32::MAX,
        u32::MAX,
        u32::MAX,
    ));
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

    // give one coinbase to the miner, and then burn the rest
    // if segwit is supported, then give one coinbase to the segwit address as well as the legacy
    // address. This is needed to allow the miner to boot up into 2.1 through epochs 2.0 and 2.05.
    let mining_pubkey = if conf.miner.segwit {
        btc_regtest_controller.set_use_segwit(false);
        btc_regtest_controller.bootstrap_chain(1);

        btc_regtest_controller.set_use_segwit(conf.miner.segwit);
        btc_regtest_controller.bootstrap_chain(1);

        let mining_pubkey = btc_regtest_controller.get_mining_pubkey().unwrap();
        debug!("Mining pubkey is {mining_pubkey}");
        btc_regtest_controller.set_mining_pubkey(MINER_BURN_PUBLIC_KEY.to_string());

        mining_pubkey
    } else {
        btc_regtest_controller.bootstrap_chain(1);

        let mining_pubkey = btc_regtest_controller.get_mining_pubkey().unwrap();
        debug!("Mining pubkey is {mining_pubkey}");
        btc_regtest_controller.set_mining_pubkey(MINER_BURN_PUBLIC_KEY.to_string());

        btc_regtest_controller.bootstrap_chain(1);

        mining_pubkey
    };

    // bitcoin chain starts at epoch 2.05 boundary, minus 5 blocks to go
    btc_regtest_controller.bootstrap_chain(epoch_2_05 - 7);

    // only one UTXO for our mining pubkey (which is uncompressed, btw)
    // NOTE: if we're using segwit, then the mining pubkey will be compressed for the segwit UTXO
    // generation (i.e. it'll be treated as different from the uncompressed public key).
    let utxos = btc_regtest_controller
        .get_all_utxos(&Secp256k1PublicKey::from_hex(&mining_pubkey).unwrap());

    eprintln!(
        "UTXOs for {mining_pubkey} (segwit={}): {utxos:?}",
        conf.miner.segwit
    );
    assert_eq!(utxos.len(), 1);

    eprintln!("Chain bootstrapped...");

    let mut run_loop = neon::RunLoop::new(conf.clone());
    let blocks_processed = run_loop.get_blocks_processed_arc();

    let channel = run_loop.get_coordinator_channel().unwrap();

    let runloop_burnchain = burnchain_config.clone();
    thread::spawn(move || run_loop.start(Some(runloop_burnchain), 0));

    // give the run loop some time to start up!
    wait_for_runloop(&blocks_processed);

    // first block wakes up the run loop
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let tip_info = get_chain_info(&conf);
    assert_eq!(tip_info.burn_block_height, epoch_2_05 - 4);

    // first block will hold our VRF registration
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // second block will be the first mined Stacks block
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // cross the epoch 2.05 boundary
    for _i in 0..3 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    let tip_info = get_chain_info(&conf);
    assert_eq!(tip_info.burn_block_height, epoch_2_05 + 1);

    // these should all succeed across the epoch 2.1 boundary
    for _i in 0..5 {
        let tip_info = get_chain_info(&conf);
        let pox_info = get_pox_info(&http_origin).unwrap();

        eprintln!(
            "\nPoX info at {}\n{pox_info:?}\n\n",
            tip_info.burn_block_height
        );

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
            tip_info.burn_block_height,
        );

        if tip_info.burn_block_height >= epoch_2_1 {
            if tip_info.burn_block_height == epoch_2_1 {
                assert!(res);
            }

            // pox-2 should be initialized now
            let _ = get_contract_src(
                &http_origin,
                StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
                "pox-2".to_string(),
                true,
            )
            .unwrap();
        } else {
            assert!(!res);

            // pox-2 should NOT be initialized
            let e = get_contract_src(
                &http_origin,
                StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
                "pox-2".to_string(),
                true,
            )
            .unwrap_err();
            eprintln!("No pox-2: {e}");
        }

        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    let tip_info = get_chain_info(&conf);
    assert_eq!(tip_info.burn_block_height, epoch_2_1 + 1);

    let account = get_account(&http_origin, &miner_account);
    assert_eq!(account.nonce, 9);

    eprintln!("Begin Stacks 2.1");
    (
        conf,
        btcd_controller,
        btc_regtest_controller,
        blocks_processed,
        channel,
    )
}

#[test]
#[ignore]
fn transition_adds_burn_block_height() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    // very simple test to verify that after the 2.1 transition, get-burn-block-info? works as
    // expected

    let spender_sk = StacksPrivateKey::new();
    let spender_addr = PrincipalData::from(to_addr(&spender_sk));
    let spender_addr_c32 = to_addr(&spender_sk);

    let (conf, _btcd_controller, mut btc_regtest_controller, blocks_processed, coord_channel) =
        advance_to_2_1(
            vec![InitialBalance {
                address: spender_addr.clone(),
                amount: 200_000_000,
            }],
            None,
            None,
            false,
        );
    let http_origin = format!("http://{}", &conf.node.rpc_bind);

    // post epoch 2.1 -- we should be able to query any/all burnchain headers after the first
    // burnchain block height (not the genesis burnchain height, mind you, but the first burnchain
    // block height at which the Stacks blockchain begins).
    let contract = "
    (define-private (test-burn-headers-cls (height uint) (base uint))
        (begin
            (print { height: (+ base height), hash: (get-burn-block-info? header-hash (+ base height)) })
            base
        )
    )
    (define-public (test-burn-headers)
        (begin
            (fold test-burn-headers-cls
                (list u0 u1 u2 u3 u4 u5 u6 u7 u8 u9 u10 u11 u12 u13 u14 u15 u16 u17 u18 u19 u20 u21 u22 u23 u24)
                u0
            )
            (fold test-burn-headers-cls
                (list u0 u1 u2 u3 u4 u5 u6 u7 u8 u9 u10 u11 u12 u13 u14 u15 u16 u17 u18 u19 u20 u21 u22 u23 u24)
                u25
            )
            (fold test-burn-headers-cls
                (list u0 u1 u2 u3 u4 u5 u6 u7 u8 u9 u10 u11 u12 u13 u14 u15 u16 u17 u18 u19 u20 u21 u22 u23 u24)
                u50
            )
            (fold test-burn-headers-cls
                (list u0 u1 u2 u3 u4 u5 u6 u7 u8 u9 u10 u11 u12 u13 u14 u15 u16 u17 u18 u19 u20 u21 u22 u23 u24)
                u75
            )
            (fold test-burn-headers-cls
                (list u0 u1 u2 u3 u4 u5 u6 u7 u8 u9 u10 u11 u12 u13 u14 u15 u16 u17 u18 u19 u20 u21 u22 u23 u24)
                u100
            )
            (fold test-burn-headers-cls
                (list u0 u1 u2 u3 u4 u5 u6 u7 u8 u9 u10 u11 u12 u13 u14 u15 u16 u17 u18 u19 u20 u21 u22 u23 u24)
                u125
            )
            (fold test-burn-headers-cls
                (list u0 u1 u2 u3 u4 u5 u6 u7 u8 u9 u10 u11 u12 u13 u14 u15 u16 u17 u18 u19 u20 u21 u22 u23 u24)
                u150
            )
            (fold test-burn-headers-cls
                (list u0 u1 u2 u3 u4 u5 u6 u7 u8 u9 u10 u11 u12 u13 u14 u15 u16 u17 u18 u19 u20 u21 u22 u23 u24)
                u175
            )
            (fold test-burn-headers-cls
                (list u0 u1 u2 u3 u4 u5 u6 u7 u8 u9 u10 u11 u12 u13 u14 u15 u16 u17 u18 u19 u20 u21 u22 u23 u24)
                u200
            )
            (fold test-burn-headers-cls
                (list u0 u1 u2 u3 u4 u5 u6 u7 u8 u9 u10 u11 u12 u13 u14 u15 u16 u17 u18 u19 u20 u21 u22 u23 u24)
                u225
            )
            (ok u0)
        )
    )
    ";

    let tx = make_contract_publish(
        &spender_sk,
        0,
        (2 * contract.len()) as u64,
        conf.burnchain.chain_id,
        "test-burn-headers",
        contract,
    );
    submit_tx(&http_origin, &tx);

    // mine it
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let tx = make_contract_call(
        &spender_sk,
        1,
        (2 * contract.len()) as u64,
        conf.burnchain.chain_id,
        &spender_addr_c32,
        "test-burn-headers",
        "test-burn-headers",
        &[],
    );
    submit_tx(&http_origin, &tx);

    let cc_txid = StacksTransaction::consensus_deserialize(&mut &tx[..])
        .unwrap()
        .txid();

    // mine it
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // check it
    let mut header_hashes: HashMap<u64, Option<BurnchainHeaderHash>> = HashMap::new();
    let blocks = test_observer::get_blocks();
    for block in blocks {
        let transactions = block.get("transactions").unwrap().as_array().unwrap();
        let events = block.get("events").unwrap().as_array().unwrap();

        for tx in transactions.iter() {
            let raw_tx = tx.get("raw_tx").unwrap().as_str().unwrap();
            if raw_tx == "0x00" {
                continue;
            }
            let tx_bytes = hex_bytes(&raw_tx[2..]).unwrap();
            let parsed = StacksTransaction::consensus_deserialize(&mut &tx_bytes[..]).unwrap();
            if parsed.txid() == cc_txid {
                // check events for this block
                for event in events.iter() {
                    if let Some(cev) = event.get("contract_event") {
                        // strip leading `0x`
                        eprintln!("{cev:#?}");
                        let clarity_serialized_value = hex_bytes(
                            str::from_utf8(
                                &cev.get("raw_value").unwrap().as_str().unwrap().as_bytes()[2..],
                            )
                            .unwrap(),
                        )
                        .unwrap();
                        let clarity_value = Value::deserialize_read(
                            &mut &clarity_serialized_value[..],
                            None,
                            false,
                        )
                        .unwrap();
                        let pair = clarity_value.expect_tuple().unwrap();
                        let height =
                            pair.get("height").unwrap().clone().expect_u128().unwrap() as u64;
                        let bhh_opt = pair
                            .get("hash")
                            .unwrap()
                            .clone()
                            .expect_optional()
                            .unwrap()
                            .map(|inner_buff| {
                                let buff_bytes_vec = inner_buff.expect_buff(32).unwrap();
                                let mut buff_bytes = [0u8; 32];
                                buff_bytes.copy_from_slice(&buff_bytes_vec[0..32]);
                                BurnchainHeaderHash(buff_bytes)
                            });

                        header_hashes.insert(height, bhh_opt);
                    }
                }
            }
        }
    }

    let sortdb = btc_regtest_controller.sortdb_mut();
    let all_snapshots = sortdb.get_all_snapshots().unwrap();
    let tip = SortitionDB::get_canonical_burn_chain_tip(sortdb.conn()).unwrap();

    // all block headers are accounted for
    for sn in all_snapshots.iter() {
        match header_hashes.get(&sn.block_height) {
            Some(Some(bhh)) => {
                assert_eq!(&sn.burn_header_hash, bhh);

                // only defined up to the tip
                assert!(sn.block_height < tip.block_height);
            }
            Some(None) => {
                // must exceed the tip
                assert!(sn.block_height >= tip.block_height);
            }
            None => {
                panic!("Missing header for {}", sn.block_height);
            }
        }
    }

    test_observer::clear();
    coord_channel.stop_chains_coordinator();
}

#[test]
#[ignore]
fn transition_adds_pay_to_alt_recipient_contract() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }
    // very simple test to verify that the miner will automatically start sending block rewards to
    // a contract in the config file when it mines after epoch 2.1.
    let target_contract_address =
        QualifiedContractIdentifier::parse("ST000000000000000000002AMW42H.bns").unwrap();
    let (conf, _btcd_controller, mut btc_regtest_controller, blocks_processed, coord_channel) =
        advance_to_2_1(
            vec![],
            Some(PrincipalData::Contract(target_contract_address.clone())),
            None,
            false,
        );

    let http_origin = format!("http://{}", &conf.node.rpc_bind);
    let contract_account_before = get_account(&http_origin, &target_contract_address);

    for _i in 0..stacks_common::consts::MINER_REWARD_MATURITY + 1 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    let contract_account_after = get_account(&http_origin, &target_contract_address);

    assert!(contract_account_before.balance < contract_account_after.balance);

    test_observer::clear();
    coord_channel.stop_chains_coordinator();
}

#[test]
#[ignore]
fn transition_adds_pay_to_alt_recipient_principal() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }
    // very simple test to verify that the miner will automatically start sending block rewards to
    // an alternative principal in the config file when it mines after epoch 2.1.
    let target_principal_address =
        PrincipalData::parse("ST34CV1214XJF9S8WPT09TJNYJTM8GM4W6N7ZGKDF").unwrap();
    let (conf, _btcd_controller, mut btc_regtest_controller, blocks_processed, coord_channel) =
        advance_to_2_1(vec![], Some(target_principal_address.clone()), None, false);

    let http_origin = format!("http://{}", &conf.node.rpc_bind);
    let alt_account_before = get_account(&http_origin, &target_principal_address);

    for _i in 0..stacks_common::consts::MINER_REWARD_MATURITY + 1 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    let alt_account_after = get_account(&http_origin, &target_principal_address);

    assert!(alt_account_before.balance < alt_account_after.balance);

    test_observer::clear();
    coord_channel.stop_chains_coordinator();
}

#[test]
#[ignore]
fn transition_fixes_bitcoin_rigidity() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let spender_sk = StacksPrivateKey::from_hex(SK_1).unwrap();
    let spender_stx_addr: StacksAddress = to_addr(&spender_sk);
    let spender_addr: PrincipalData = spender_stx_addr.into();
    let _spender_btc_addr = BitcoinAddress::from_bytes_legacy(
        BitcoinNetworkType::Regtest,
        LegacyBitcoinAddressType::PublicKeyHash,
        &spender_stx_addr.bytes.0,
    )
    .unwrap();

    let spender_2_sk = StacksPrivateKey::from_hex(SK_2).unwrap();
    let spender_2_stx_addr: StacksAddress = to_addr(&spender_2_sk);
    let spender_2_addr: PrincipalData = spender_2_stx_addr.into();

    let epoch_2_05 = 210;
    let epoch_2_1 = 215;

    test_observer::spawn();

    let (mut conf, miner_account) = neon_integration_test_conf();
    let mut initial_balances = vec![
        InitialBalance {
            address: spender_addr.clone(),
            amount: 100300,
        },
        InitialBalance {
            address: spender_2_addr.clone(),
            amount: 100300,
        },
    ];

    conf.initial_balances.append(&mut initial_balances);
    test_observer::register_any(&mut conf);

    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = epoch_2_05;
    epochs[StacksEpochId::Epoch2_05].start_height = epoch_2_05;
    epochs[StacksEpochId::Epoch2_05].end_height = epoch_2_1;
    epochs[StacksEpochId::Epoch21].start_height = epoch_2_1;

    conf.burnchain.epochs = Some(epochs);

    let mut burnchain_config = Burnchain::regtest(&conf.get_burn_db_path());

    let reward_cycle_len = 2000;
    let prepare_phase_len = 100;
    let pox_constants = PoxConstants::new(
        reward_cycle_len,
        prepare_phase_len,
        4 * prepare_phase_len / 5,
        5,
        15,
        (16 * reward_cycle_len - 1).into(),
        (17 * reward_cycle_len).into(),
        u32::MAX,
        u32::MAX,
        u32::MAX,
        u32::MAX,
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

    // bitcoin chain starts at epoch 2.05 boundary, minus 5 blocks to go
    btc_regtest_controller.bootstrap_chain(epoch_2_05 - 5);

    eprintln!("Chain bootstrapped...");

    let mut run_loop = neon::RunLoop::new(conf.clone());
    let blocks_processed = run_loop.get_blocks_processed_arc();

    let channel = run_loop.get_coordinator_channel().unwrap();

    let runloop_burnchain = burnchain_config.clone();
    thread::spawn(move || run_loop.start(Some(runloop_burnchain), 0));

    // give the run loop some time to start up!
    wait_for_runloop(&blocks_processed);

    // first block wakes up the run loop
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let tip_info = get_chain_info(&conf);
    assert_eq!(tip_info.burn_block_height, epoch_2_05 - 4);

    // first block will hold our VRF registration
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // second block will be the first mined Stacks block
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // cross the epoch 2.05 boundary
    for _i in 0..3 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    let tip_info = get_chain_info(&conf);
    assert_eq!(tip_info.burn_block_height, epoch_2_05 + 1);

    // okay, let's send a pre-stx op for a transfer-stx op that will get mined before the 2.1 epoch
    let pre_stx_op = PreStxOp {
        output: spender_stx_addr,
        // to be filled in
        txid: Txid([0u8; 32]),
        vtxindex: 0,
        block_height: 0,
        burn_header_hash: BurnchainHeaderHash([0u8; 32]),
    };

    let mut miner_signer = Keychain::default(conf.node.seed.clone()).generate_op_signer();

    assert!(
        btc_regtest_controller
            .submit_operation(
                StacksEpochId::Epoch2_05,
                BlockstackOperationType::PreStx(pre_stx_op),
                &mut miner_signer,
                1
            )
            .is_ok(),
        "Pre-stx operation should submit successfully"
    );

    // mine it
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // let's fire off a transfer op that will not land in the Stacks 2.1 epoch.  It should not be
    // applied, even though it's within 6 blocks of the next Stacks block, which will be in epoch
    // 2.1.  This verifies that the new burnchain consideration window only applies to sortitions
    // that happen in Stacks 2.1.
    let recipient_sk = StacksPrivateKey::new();
    let recipient_addr = to_addr(&recipient_sk);
    let transfer_stx_op = TransferStxOp {
        sender: spender_stx_addr,
        recipient: recipient_addr,
        transfered_ustx: 100_000,
        memo: vec![],
        // to be filled in
        txid: Txid([0u8; 32]),
        vtxindex: 0,
        block_height: 0,
        burn_header_hash: BurnchainHeaderHash([0u8; 32]),
    };

    let mut spender_signer = BurnchainOpSigner::new(spender_sk, false);

    assert!(
        btc_regtest_controller
            .submit_operation(
                StacksEpochId::Epoch2_05,
                BlockstackOperationType::TransferStx(transfer_stx_op),
                &mut spender_signer,
                1
            )
            .is_ok(),
        "Transfer operation should submit successfully"
    );

    // mine it without a sortition
    btc_regtest_controller.build_next_block(1);

    // these should all succeed across the epoch 2.1 boundary
    for _i in 0..3 {
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

        if tip_info.burn_block_height >= epoch_2_1 {
            if tip_info.burn_block_height == epoch_2_1 {
                assert!(res);
            }

            // pox-2 should be initialized now
            let _ = get_contract_src(
                &http_origin,
                StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
                "pox-2".to_string(),
                true,
            )
            .unwrap();

            // costs-3 should be initialized now
            let _ = get_contract_src(
                &http_origin,
                StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
                "costs-3".to_string(),
                true,
            )
            .unwrap();
        } else {
            assert!(!res);

            // pox-2 should NOT be initialized
            let e = get_contract_src(
                &http_origin,
                StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
                "pox-2".to_string(),
                true,
            )
            .unwrap_err();
            eprintln!("No pox-2: {e}");

            // costs-3 should NOT be initialized
            let e = get_contract_src(
                &http_origin,
                StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
                "costs-3".to_string(),
                true,
            )
            .unwrap_err();
            eprintln!("No costs-3: {e}");
        }

        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    let tip_info = get_chain_info(&conf);
    assert_eq!(tip_info.burn_block_height, epoch_2_1 + 1);

    // stx-transfer did not go through -- it fell in a block before 2.1
    assert_eq!(get_balance(&http_origin, &spender_addr), 100_300);
    assert_eq!(get_balance(&http_origin, &recipient_addr), 0);
    assert_eq!(get_balance(&http_origin, &spender_2_addr), 100_300);

    let account = get_account(&http_origin, &miner_account);
    assert_eq!(account.nonce, 8);

    eprintln!("Begin Stacks 2.1");

    // let's query the spender's account:
    assert_eq!(get_balance(&http_origin, &spender_addr), 100300);

    // okay, let's send a pre-stx op.
    let pre_stx_op = PreStxOp {
        output: spender_stx_addr,
        // to be filled in
        txid: Txid([0u8; 32]),
        vtxindex: 0,
        block_height: 0,
        burn_header_hash: BurnchainHeaderHash([0u8; 32]),
    };

    let mut miner_signer = Keychain::default(conf.node.seed.clone()).generate_op_signer();

    assert!(
        btc_regtest_controller
            .submit_operation(
                StacksEpochId::Epoch21,
                BlockstackOperationType::PreStx(pre_stx_op),
                &mut miner_signer,
                1
            )
            .is_ok(),
        "Pre-stx operation should submit successfully"
    );

    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // let's fire off our transfer op.
    let recipient_sk = StacksPrivateKey::new();
    let recipient_addr = to_addr(&recipient_sk);
    let transfer_stx_op = TransferStxOp {
        sender: spender_stx_addr,
        recipient: recipient_addr,
        transfered_ustx: 100_000,
        memo: vec![],
        // to be filled in
        txid: Txid([0u8; 32]),
        vtxindex: 0,
        block_height: 0,
        burn_header_hash: BurnchainHeaderHash([0u8; 32]),
    };

    let mut spender_signer = BurnchainOpSigner::new(spender_sk, false);

    assert!(
        btc_regtest_controller
            .submit_operation(
                StacksEpochId::Epoch2_05,
                BlockstackOperationType::TransferStx(transfer_stx_op),
                &mut spender_signer,
                1
            )
            .is_ok(),
        "Transfer operation should submit successfully"
    );

    // build a couple bitcoin blocks without a stacks block to mine it, up to the edge of the
    // window
    for _i in 0..BURNCHAIN_TX_SEARCH_WINDOW {
        btc_regtest_controller.build_next_block(1);
    }

    // this block should process the transfer, even though it was mined in a sortition-less block
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    assert_eq!(get_balance(&http_origin, &spender_addr), 300);
    assert_eq!(get_balance(&http_origin, &recipient_addr), 100_000);
    assert_eq!(get_balance(&http_origin, &spender_2_addr), 100_300);

    // now let's do a pre-stx-op and a transfer op in the same burnchain block...
    // NOTE: bitcoind really doesn't want to return the utxo from the first op for some reason,
    //    so we have to get a little creative...

    // okay, let's send a pre-stx op.
    let pre_stx_op = PreStxOp {
        output: spender_2_stx_addr,
        // to be filled in
        txid: Txid([0u8; 32]),
        vtxindex: 0,
        block_height: 0,
        burn_header_hash: BurnchainHeaderHash([0u8; 32]),
    };

    let mut miner_signer = Keychain::default(conf.node.seed.clone()).generate_op_signer();

    let pre_stx_tx = btc_regtest_controller
        .submit_manual(
            StacksEpochId::Epoch21,
            BlockstackOperationType::PreStx(pre_stx_op),
            &mut miner_signer,
            None,
        )
        .expect("Pre-stx operation should submit successfully");

    let transfer_stx_utxo = UTXO {
        txid: pre_stx_tx.txid(),
        vout: 1,
        script_pub_key: pre_stx_tx.output[1].script_pubkey.clone(),
        amount: pre_stx_tx.output[1].value,
        confirmations: 0,
    };

    // let's fire off our transfer op.
    let transfer_stx_op = TransferStxOp {
        sender: spender_2_stx_addr,
        recipient: recipient_addr,
        transfered_ustx: 100_000,
        memo: vec![],
        // to be filled in
        txid: Txid([0u8; 32]),
        vtxindex: 0,
        block_height: 0,
        burn_header_hash: BurnchainHeaderHash([0u8; 32]),
    };

    let mut spender_signer = BurnchainOpSigner::new(spender_2_sk, false);

    btc_regtest_controller
        .submit_manual(
            StacksEpochId::Epoch21,
            BlockstackOperationType::TransferStx(transfer_stx_op),
            &mut spender_signer,
            Some(transfer_stx_utxo),
        )
        .expect("Transfer operation should submit successfully");

    // build a couple bitcoin blocks without a stacks block to mine it, up to the edge of the
    // window
    for _i in 0..BURNCHAIN_TX_SEARCH_WINDOW {
        btc_regtest_controller.build_next_block(1);
    }

    // should process the transfer
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    assert_eq!(get_balance(&http_origin, &spender_addr), 300);
    assert_eq!(get_balance(&http_origin, &recipient_addr), 200_000);
    assert_eq!(get_balance(&http_origin, &spender_2_addr), 300);

    // let's fire off another transfer op that will fall outside the window
    let pre_stx_op = PreStxOp {
        output: spender_2_stx_addr,
        // to be filled in
        txid: Txid([0u8; 32]),
        vtxindex: 0,
        block_height: 0,
        burn_header_hash: BurnchainHeaderHash([0u8; 32]),
    };

    let mut miner_signer = Keychain::default(conf.node.seed.clone()).generate_op_signer();

    let pre_stx_tx = btc_regtest_controller
        .submit_manual(
            StacksEpochId::Epoch21,
            BlockstackOperationType::PreStx(pre_stx_op),
            &mut miner_signer,
            None,
        )
        .expect("Pre-stx operation should submit successfully");

    let transfer_stx_utxo = UTXO {
        txid: pre_stx_tx.txid(),
        vout: 1,
        script_pub_key: pre_stx_tx.output[1].script_pubkey.clone(),
        amount: pre_stx_tx.output[1].value,
        confirmations: 0,
    };

    let transfer_stx_op = TransferStxOp {
        sender: spender_stx_addr,
        recipient: recipient_addr,
        transfered_ustx: 123,
        memo: vec![],
        // to be filled in
        txid: Txid([0u8; 32]),
        vtxindex: 0,
        block_height: 0,
        burn_header_hash: BurnchainHeaderHash([0u8; 32]),
    };

    let mut spender_signer = BurnchainOpSigner::new(spender_2_sk, false);

    btc_regtest_controller
        .submit_manual(
            StacksEpochId::Epoch21,
            BlockstackOperationType::TransferStx(transfer_stx_op),
            &mut spender_signer,
            Some(transfer_stx_utxo),
        )
        .expect("Transfer operation should submit successfully");

    // build a couple bitcoin blocks without a stacks block to mine it, up to the edge of the
    // window and then past it
    for _i in 0..(BURNCHAIN_TX_SEARCH_WINDOW + 1) {
        btc_regtest_controller.build_next_block(1);
    }

    // should NOT process the transfer
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    assert_eq!(get_balance(&http_origin, &spender_addr), 300);
    assert_eq!(get_balance(&http_origin, &recipient_addr), 200_000);
    assert_eq!(get_balance(&http_origin, &spender_2_addr), 300);

    test_observer::clear();
    channel.stop_chains_coordinator();
}

#[test]
#[ignore]
fn transition_adds_get_pox_addr_recipients() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    // very simple test to verify that when STX are stacked, the PoX address is recoverable from
    // `get-burn-block-info?`

    let reward_cycle_len = 10;
    let prepare_phase_len = 4;
    let v1_unlock_height = 220;
    let pox_constants = PoxConstants::new(
        reward_cycle_len,
        prepare_phase_len,
        4 * prepare_phase_len / 5,
        1,
        1,
        u64::MAX - 2,
        u64::MAX - 1,
        v1_unlock_height,
        u32::MAX,
        u32::MAX,
        u32::MAX,
    );

    let mut spender_sks = vec![];
    let mut initial_balances = vec![];
    let mut expected_pox_addrs = HashSet::new();

    let stacked = 100_000_000_000 * (core::MICROSTACKS_PER_STACKS as u64);

    for _i in 0..7 {
        let spender_sk = StacksPrivateKey::new();
        let spender_addr: PrincipalData = to_addr(&spender_sk).into();

        spender_sks.push(spender_sk);
        initial_balances.push(InitialBalance {
            address: spender_addr.clone(),
            amount: stacked + 100_000,
        });
    }

    let pox_pubkey = Secp256k1PublicKey::from_hex(
        "02f006a09b59979e2cb8449f58076152af6b124aa29b948a3714b8d5f15aa94ede",
    )
    .unwrap();
    let pox_pubkey_hash = bytes_to_hex(&Hash160::from_node_public_key(&pox_pubkey).to_bytes());

    let (conf, _btcd_controller, mut btc_regtest_controller, blocks_processed, coord_channel) =
        advance_to_2_1(initial_balances, None, Some(pox_constants.clone()), false);

    let mut sort_height = coord_channel.get_sortitions_processed();
    let http_origin = format!("http://{}", &conf.node.rpc_bind);

    let stack_sort_height = sort_height;

    // stack some STX to each standard PoX address variant
    for (i, addr_variant) in [
        AddressHashMode::SerializeP2PKH,
        AddressHashMode::SerializeP2SH,
        AddressHashMode::SerializeP2WPKH,
        AddressHashMode::SerializeP2WSH,
    ]
    .iter()
    .enumerate()
    {
        let spender_sk = spender_sks[i];
        let pox_addr_tuple = execute(
            &format!(
                "{{ hashbytes: 0x{pox_pubkey_hash}, version: 0x{:02x} }}",
                &(*addr_variant as u8)
            ),
            ClarityVersion::Clarity2,
        )
        .unwrap()
        .unwrap();
        let tx = make_contract_call(
            &spender_sk,
            0,
            300,
            conf.burnchain.chain_id,
            &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
            "pox-2",
            "stack-stx",
            &[
                Value::UInt(stacked.into()),
                pox_addr_tuple.clone(),
                Value::UInt(sort_height as u128),
                Value::UInt(2),
            ],
        );

        submit_tx(&http_origin, &tx);
        expected_pox_addrs.insert(pox_addr_tuple.to_string());
    }

    // stack some STX to segwit addressses
    for (i, spender_sk) in spender_sks.iter().enumerate().take(7).skip(4) {
        let pubk = Secp256k1PublicKey::from_private(spender_sk);
        let version = i as u8;
        let bytes = match i {
            4 => {
                // p2wpkh
                to_hex(&Hash160::from_node_public_key(&pubk).0)
            }
            _ => {
                // p2wsh or p2tr
                to_hex(&Sha256Sum::from_data(&pubk.to_bytes_compressed()).0)
            }
        };
        let pox_addr_tuple = execute(
            &format!("{{ hashbytes: 0x{bytes}, version: 0x{version:02x} }}"),
            ClarityVersion::Clarity2,
        )
        .unwrap()
        .unwrap();
        let tx = make_contract_call(
            spender_sk,
            0,
            300,
            conf.burnchain.chain_id,
            &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
            "pox-2",
            "stack-stx",
            &[
                Value::UInt(stacked.into()),
                pox_addr_tuple.clone(),
                Value::UInt(sort_height as u128),
                Value::UInt(2),
            ],
        );

        submit_tx(&http_origin, &tx);
        expected_pox_addrs.insert(pox_addr_tuple.to_string());
    }

    let contract = "
    (define-private (get-pox-addrs-at (idx uint) (base uint))
        (let (
            (burn-height (+ base idx))
        )
            (print { burn-height: burn-height, pox-addrs: (get-burn-block-info? pox-addrs burn-height) })
            base
        )
    )
    (define-public (test-get-pox-addrs (start-burn-height uint))
        (ok (fold get-pox-addrs-at
            (list u0 u1 u2 u3 u4 u5 u6 u7 u8 u9 u10 u11 u12 u13 u14 u15 u16 u17 u18 u19 u20 u21 u22 u23 u24)
            start-burn-height
        ))
    )
    ";

    let spender_addr_c32 = to_addr(&spender_sks[0]);
    let contract_tx = make_contract_publish(
        &spender_sks[0],
        1,
        (2 * contract.len()) as u64,
        conf.burnchain.chain_id,
        "test-get-pox-addrs",
        contract,
    );

    submit_tx(&http_origin, &contract_tx);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    eprintln!("Sort height: {sort_height}");
    test_observer::clear();

    // mine through two reward cycles
    // now let's mine until the next reward cycle starts ...
    while sort_height < stack_sort_height + (((2 * pox_constants.reward_cycle_length) + 1) as u64) {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
        sort_height = coord_channel.get_sortitions_processed();
        eprintln!("Sort height: {sort_height}");
    }

    let cc_tx = make_contract_call(
        &spender_sks[0],
        2,
        (2 * contract.len()) as u64,
        conf.burnchain.chain_id,
        &spender_addr_c32,
        "test-get-pox-addrs",
        "test-get-pox-addrs",
        &[Value::UInt((stack_sort_height).into())],
    );
    let cc_txid = StacksTransaction::consensus_deserialize(&mut &cc_tx[..])
        .unwrap()
        .txid();

    submit_tx(&http_origin, &cc_tx);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // check result of test-get-pox-addrs
    let blocks = test_observer::get_blocks();
    let mut found_pox_addrs = HashSet::new();
    for block in blocks {
        let transactions = block.get("transactions").unwrap().as_array().unwrap();
        let events = block.get("events").unwrap().as_array().unwrap();

        for tx in transactions.iter() {
            let raw_tx = tx.get("raw_tx").unwrap().as_str().unwrap();
            if raw_tx == "0x00" {
                continue;
            }
            let tx_bytes = hex_bytes(&raw_tx[2..]).unwrap();
            let parsed = StacksTransaction::consensus_deserialize(&mut &tx_bytes[..]).unwrap();
            if parsed.txid() == cc_txid {
                // check events for this block
                for event in events.iter() {
                    if let Some(cev) = event.get("contract_event") {
                        // strip leading `0x`
                        let clarity_serialized_value = hex_bytes(
                            str::from_utf8(
                                &cev.get("raw_value").unwrap().as_str().unwrap().as_bytes()[2..],
                            )
                            .unwrap(),
                        )
                        .unwrap();
                        let clarity_value = Value::deserialize_read(
                            &mut &clarity_serialized_value[..],
                            None,
                            false,
                        )
                        .unwrap();
                        let pair = clarity_value.expect_tuple().unwrap();
                        let burn_block_height = pair
                            .get("burn-height")
                            .unwrap()
                            .clone()
                            .expect_u128()
                            .unwrap() as u64;
                        let pox_addr_tuples_opt = pair
                            .get("pox-addrs")
                            .unwrap()
                            .clone()
                            .expect_optional()
                            .unwrap();

                        if let Some(pox_addr_tuples_list) = pox_addr_tuples_opt {
                            let pox_addrs_and_payout_tuple =
                                pox_addr_tuples_list.expect_tuple().unwrap();
                            let pox_addr_tuples = pox_addrs_and_payout_tuple
                                .get("addrs")
                                .unwrap()
                                .to_owned()
                                .expect_list()
                                .unwrap();

                            let payout = pox_addrs_and_payout_tuple
                                .get("payout")
                                .unwrap()
                                .to_owned()
                                .expect_u128()
                                .unwrap();

                            // NOTE: there's an even number of payouts here, so this works
                            eprintln!("payout at {burn_block_height} = {payout}");

                            if pox_constants.is_in_prepare_phase(0, burn_block_height) {
                                // in prepare phase
                                eprintln!("{burn_block_height} in prepare phase");
                                assert_eq!(payout, conf.burnchain.burn_fee_cap as u128);
                                assert_eq!(pox_addr_tuples.len(), 1);
                            } else {
                                // in reward phase
                                eprintln!("{burn_block_height} in reward phase");
                                assert_eq!(
                                    payout,
                                    (conf.burnchain.burn_fee_cap / (OUTPUTS_PER_COMMIT as u64))
                                        as u128
                                );
                                assert_eq!(pox_addr_tuples.len(), 2);
                            }

                            for pox_addr_value in pox_addr_tuples.into_iter() {
                                let pox_addr =
                                    PoxAddress::try_from_pox_tuple(false, &pox_addr_value)
                                        .unwrap_or_else(|| {
                                            panic!("FATAL: invalid PoX tuple {pox_addr_value:?}")
                                        });
                                eprintln!("at {burn_block_height}: {pox_addr:?}");
                                if !pox_addr.is_burn() {
                                    found_pox_addrs.insert(pox_addr);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    eprintln!("found pox addrs: {found_pox_addrs:?}");
    assert_eq!(found_pox_addrs.len(), 7);

    for addr in found_pox_addrs
        .into_iter()
        .map(|addr| Value::Tuple(addr.as_clarity_tuple().unwrap()))
    {
        eprintln!("Contains: {addr:?}");
        assert!(expected_pox_addrs.contains(&addr.to_string()));
    }
}

#[test]
#[ignore]
fn transition_adds_mining_from_segwit() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let reward_cycle_len = 10;
    let prepare_phase_len = 4;
    let v1_unlock_height = 220;
    let pox_constants = PoxConstants::new(
        reward_cycle_len,
        prepare_phase_len,
        4 * prepare_phase_len / 5,
        1,
        1,
        u64::MAX,
        u64::MAX,
        v1_unlock_height,
        u32::MAX,
        u32::MAX,
        u32::MAX,
    );

    let mut initial_balances = vec![];

    let stacked = 100_000_000_000 * (core::MICROSTACKS_PER_STACKS as u64);

    for _i in 0..7 {
        let spender_sk = StacksPrivateKey::new();
        let spender_addr: PrincipalData = to_addr(&spender_sk).into();

        initial_balances.push(InitialBalance {
            address: spender_addr.clone(),
            amount: stacked + 100_000,
        });
    }

    let (conf, _btcd_controller, mut btc_regtest_controller, blocks_processed, _coord_channel) =
        advance_to_2_1(initial_balances, None, Some(pox_constants.clone()), true);

    let utxos = btc_regtest_controller
        .get_all_utxos(&Secp256k1PublicKey::from_hex(MINER_BURN_PUBLIC_KEY).unwrap());

    assert!(!utxos.is_empty());

    // all UTXOs should be segwit
    for utxo in utxos.iter() {
        let utxo_addr = BitcoinAddress::from_scriptpubkey(
            BitcoinNetworkType::Testnet,
            &utxo.script_pub_key.clone().into_bytes(),
        );
        if let Some(BitcoinAddress::Segwit(SegwitBitcoinAddress::P2WPKH(..))) = &utxo_addr {
        } else {
            panic!("UTXO address was {utxo_addr:?}");
        }
    }

    eprintln!("Wake up miner");
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // mine a Stacks block
    let tip_info_before = get_chain_info(&conf);
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    let tip_info_after = get_chain_info(&conf);

    // we were able to do so
    assert_eq!(
        tip_info_before.burn_block_height + 1,
        tip_info_after.burn_block_height
    );
    assert_eq!(
        tip_info_before.stacks_tip_height + 1,
        tip_info_after.stacks_tip_height
    );

    // that block-commit we just sent consumed a segwit p2wpkh utxo and emitted a segwit p2wpkh
    // utxo
    let sortdb = btc_regtest_controller.sortdb_mut();
    let tip = SortitionDB::get_canonical_burn_chain_tip(sortdb.conn()).unwrap();
    let commits =
        SortitionDB::get_block_commits_by_block(sortdb.conn(), &tip.sortition_id).unwrap();
    assert_eq!(commits.len(), 1);

    let txid = commits[0].txid;
    let tx = btc_regtest_controller.get_raw_transaction(&txid);

    eprintln!("tx = {tx:?}");
    assert_eq!(tx.input[0].witness.len(), 2);
    let addr = BitcoinAddress::try_from_segwit(
        false,
        &tx.output
            .last()
            .as_ref()
            .unwrap()
            .script_pubkey
            .clone()
            .into_bytes(),
    );
    assert!(addr.is_some());
}

/// Verify that a sunset-in-progress will be halted by the epoch 2.1 transition
#[test]
#[ignore]
fn transition_removes_pox_sunset() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let spender_sk = StacksPrivateKey::new();
    let spender_addr: PrincipalData = to_addr(&spender_sk).into();
    let first_bal = 6_000_000_000 * (core::MICROSTACKS_PER_STACKS as u64);

    let pox_pubkey = Secp256k1PublicKey::from_hex(
        "02f006a09b59979e2cb8449f58076152af6b124aa29b948a3714b8d5f15aa94ede",
    )
    .unwrap();
    let pox_pubkey_hash = bytes_to_hex(&Hash160::from_node_public_key(&pox_pubkey).to_bytes());

    let (mut conf, miner_account) = neon_integration_test_conf();

    test_observer::spawn();
    test_observer::register_any(&mut conf);

    conf.initial_balances.push(InitialBalance {
        address: spender_addr.clone(),
        amount: first_bal,
    });

    conf.node.mine_microblocks = false;
    conf.burnchain.max_rbf = 1000000;
    conf.node.wait_time_for_microblocks = 0;
    conf.node.microblock_frequency = 1_000;
    conf.miner.first_attempt_time_ms = 5_000;
    conf.miner.subsequent_attempt_time_ms = 10_000;
    conf.node.wait_time_for_blocks = 0;

    // reward cycle length = 15, so 10 reward cycle slots + 5 prepare-phase burns
    let first_sortition_height = 201;
    let reward_cycle_len = 15;
    let prepare_phase_len = 5;
    let sunset_start_rc = 16;
    let sunset_end_rc = 20;
    let epoch_21_rc = 18;

    let epoch_21 = epoch_21_rc * reward_cycle_len + 1;

    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = 1;
    epochs[StacksEpochId::Epoch2_05].start_height = 1;
    epochs[StacksEpochId::Epoch2_05].end_height = epoch_21;
    epochs[StacksEpochId::Epoch21].start_height = epoch_21;

    conf.burnchain.epochs = Some(epochs);

    let mut btcd_controller = BitcoinCoreController::new(conf.clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut burnchain_config = Burnchain::regtest(&conf.get_burn_db_path());

    let pox_constants = PoxConstants::new(
        reward_cycle_len as u32,
        prepare_phase_len,
        4 * prepare_phase_len / 5,
        5,
        15,
        sunset_start_rc * reward_cycle_len - 1,
        sunset_end_rc * reward_cycle_len,
        (epoch_21 as u32) + 1,
        u32::MAX,
        u32::MAX,
        u32::MAX,
    );
    burnchain_config.pox_constants = pox_constants.clone();

    let mut btc_regtest_controller = BitcoinRegtestController::with_burnchain(
        conf.clone(),
        None,
        Some(burnchain_config.clone()),
        None,
    );
    let http_origin = format!("http://{}", &conf.node.rpc_bind);

    btc_regtest_controller.bootstrap_chain(first_sortition_height);

    eprintln!("Chain bootstrapped...");

    let mut run_loop = neon::RunLoop::new(conf.clone());
    let blocks_processed = run_loop.get_blocks_processed_arc();
    let channel = run_loop.get_coordinator_channel().unwrap();
    let thread_burnchain = burnchain_config.clone();

    thread::spawn(move || run_loop.start(Some(thread_burnchain), 0));

    // give the run loop some time to start up!
    wait_for_runloop(&blocks_processed);

    // first block wakes up the run loop
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // first block will hold our VRF registration
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // second block will be the first mined Stacks block
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    sleep_ms(10_000);

    let sort_height = channel.get_sortitions_processed();

    // let's query the miner's account nonce:
    let account = get_account(&http_origin, &miner_account);
    assert_eq!(account.balance, 0);
    assert!(account.nonce >= 1);

    // and our potential spenders:
    let account = get_account(&http_origin, &spender_addr);
    assert_eq!(account.balance, first_bal as u128);
    assert_eq!(account.nonce, 0);

    let pox_info = get_pox_info(&http_origin).unwrap();

    assert_eq!(&pox_info.contract_id, "ST000000000000000000002AMW42H.pox");
    assert!(!pox_info.current_cycle.is_pox_active);
    assert_eq!(pox_info.next_cycle.stacked_ustx, 0);

    let tx = make_contract_call(
        &spender_sk,
        0,
        260,
        conf.burnchain.chain_id,
        &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
        "pox",
        "stack-stx",
        &[
            Value::UInt(first_bal as u128 - 260 * 3),
            execute(
                &format!("{{ hashbytes: 0x{pox_pubkey_hash}, version: 0x00 }}"),
                ClarityVersion::Clarity1,
            )
            .unwrap()
            .unwrap(),
            Value::UInt(sort_height as u128),
            Value::UInt(12),
        ],
    );

    // okay, let's push that stacking transaction!
    submit_tx(&http_origin, &tx);

    let mut sort_height = channel.get_sortitions_processed();
    eprintln!("Sort height pox-1: {sort_height}");

    // advance to next reward cycle
    for _i in 0..(reward_cycle_len * 2 + 2) {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
        sort_height = channel.get_sortitions_processed();
        eprintln!("Sort height pox-1: {sort_height} <= {epoch_21}");
    }

    // pox must activate
    let pox_info = get_pox_info(&http_origin).unwrap();
    eprintln!("pox_info in pox-1 = {pox_info:?}");
    assert!(pox_info.current_cycle.is_pox_active);
    assert_eq!(&pox_info.contract_id, "ST000000000000000000002AMW42H.pox");

    // advance to 2.1
    while sort_height <= epoch_21 + 1 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
        sort_height = channel.get_sortitions_processed();
        eprintln!("Sort height pox-1: {sort_height} <= {epoch_21}");
    }

    let pox_info = get_pox_info(&http_origin).unwrap();

    // pox is still "active" despite unlock, because there's enough participation, and also even
    // though the v1 block height has passed, the pox-2 contract won't be managing reward sets
    // until the next reward cycle
    eprintln!("pox_info in pox-2 = {pox_info:?}");
    assert!(pox_info.current_cycle.is_pox_active);
    assert_eq!(&pox_info.contract_id, "ST000000000000000000002AMW42H.pox-2");

    // re-stack
    let tx = make_contract_call(
        &spender_sk,
        1,
        260 * 2,
        conf.burnchain.chain_id,
        &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
        "pox-2",
        "stack-stx",
        &[
            Value::UInt(first_bal as u128 - 260 * 3),
            execute(
                &format!("{{ hashbytes: 0x{pox_pubkey_hash}, version: 0x00 }}"),
                ClarityVersion::Clarity2,
            )
            .unwrap()
            .unwrap(),
            Value::UInt(sort_height as u128),
            Value::UInt(12),
        ],
    );

    // okay, let's push that stacking transaction!
    submit_tx(&http_origin, &tx);

    eprintln!("Try and confirm pox-2 stack-stx");

    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    sort_height = channel.get_sortitions_processed();
    eprintln!("Sort height pox-1 to pox-2 with stack-stx to pox-2: {sort_height}");

    let pox_info = get_pox_info(&http_origin).unwrap();
    assert!(pox_info.current_cycle.is_pox_active);

    // get pox back online
    while sort_height <= epoch_21 + reward_cycle_len {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
        sort_height = channel.get_sortitions_processed();
        eprintln!("Sort height pox-2: {sort_height}");
    }

    let pox_info = get_pox_info(&http_origin).unwrap();
    eprintln!("pox_info = {pox_info:?}");
    assert!(pox_info.current_cycle.is_pox_active);

    // first full reward cycle with pox-2
    assert_eq!(&pox_info.contract_id, "ST000000000000000000002AMW42H.pox-2");

    let burn_blocks = test_observer::get_burn_blocks();
    let mut pox_out_opt = None;
    for (i, block) in burn_blocks.into_iter().enumerate() {
        let recipients: Vec<(String, u64)> = block
            .get("reward_recipients")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|value| {
                let recipient: String = value
                    .get("recipient")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string();
                let amount = value.get("amt").unwrap().as_u64().unwrap();
                (recipient, amount)
            })
            .collect();

        if (i as u64) < (sunset_start_rc * reward_cycle_len) {
            // before sunset
            if !recipients.is_empty() {
                for (_, amt) in recipients.into_iter() {
                    pox_out_opt = if let Some(pox_out) = pox_out_opt {
                        Some(std::cmp::max(amt, pox_out))
                    } else {
                        Some(amt)
                    };
                }
            }
        } else if (i as u64) >= (sunset_start_rc * reward_cycle_len) && (i as u64) + 1 < epoch_21 {
            // some sunset burn happened
            let pox_out = pox_out_opt.unwrap();
            if !recipients.is_empty() {
                for (_, amt) in recipients.into_iter() {
                    assert!(amt < pox_out);
                }
            }
        } else if (i as u64) + 1 >= epoch_21 {
            // no sunset burn happened
            let pox_out = pox_out_opt.unwrap();
            if !recipients.is_empty() {
                for (_, amt) in recipients.into_iter() {
                    // NOTE: odd number of reward cycles
                    if !burnchain_config.is_in_prepare_phase((i + 2) as u64) {
                        assert_eq!(amt, pox_out);
                    }
                }
            }
        }
    }

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
    let epoch_2_1 = 215;

    let (mut conf, miner_account) = neon_integration_test_conf();

    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = epoch_2_05;
    epochs[StacksEpochId::Epoch2_05].start_height = epoch_2_05;
    epochs[StacksEpochId::Epoch2_05].end_height = epoch_2_1;
    epochs[StacksEpochId::Epoch21].start_height = epoch_2_1;

    conf.node.mine_microblocks = false;
    conf.burnchain.max_rbf = 1000000;
    conf.miner.first_attempt_time_ms = 5_000;
    conf.miner.subsequent_attempt_time_ms = 10_000;
    conf.node.wait_time_for_blocks = 0;

    conf.burnchain.epochs = Some(epochs);

    test_observer::spawn();
    test_observer::register_any(&mut conf);

    let keychain = Keychain::default(conf.node.seed.clone());
    let http_origin = format!("http://{}", &conf.node.rpc_bind);

    let mut burnchain_config = Burnchain::regtest(&conf.get_burn_db_path());

    let reward_cycle_len = 10;
    let prepare_phase_len = 3;
    let pox_constants = PoxConstants::new(
        reward_cycle_len,
        prepare_phase_len,
        4 * prepare_phase_len / 5,
        5,
        15,
        u64::MAX - 2,
        u64::MAX - 1,
        (epoch_2_1 + 1) as u32,
        u32::MAX,
        u32::MAX,
        u32::MAX,
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

    btc_regtest_controller.bootstrap_chain(201);

    eprintln!("Chain bootstrapped...");

    let mut run_loop = neon::RunLoop::new(conf.clone());
    let blocks_processed = run_loop.get_blocks_processed_arc();

    let channel = run_loop.get_coordinator_channel().unwrap();

    let runloop_burnchain_config = burnchain_config.clone();
    thread::spawn(move || run_loop.start(Some(runloop_burnchain_config), 0));

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

    let mut crossed_21_boundary = false;

    // these should all succeed across the epoch boundary
    for _i in 0..30 {
        // also, make *huge* block-commits with invalid marker bytes once we reach the new
        // epoch, and verify that it fails.
        let tip_info = get_chain_info(&conf);
        let pox_info = get_pox_info(&http_origin).unwrap();

        eprintln!(
            "\nPoX info at {}\n{pox_info:?}\n\n",
            tip_info.burn_block_height
        );

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

        if tip_info.burn_block_height == epoch_2_05 || tip_info.burn_block_height == epoch_2_1 {
            assert!(res);
            if tip_info.burn_block_height == epoch_2_1 {
                crossed_21_boundary = true;
            }
        } else {
            assert!(!res);
        }

        if tip_info.burn_block_height + 1 >= epoch_2_1 {
            let burn_fee_cap = 100000000; // 1 BTC
            let commit_outs = if !burnchain.is_in_prepare_phase(tip_info.burn_block_height + 1) {
                vec![
                    PoxAddress::standard_burn_address(conf.is_mainnet()),
                    PoxAddress::standard_burn_address(conf.is_mainnet()),
                ]
            } else {
                vec![PoxAddress::standard_burn_address(conf.is_mainnet())]
            };

            // let's commit
            let burn_parent_modulus =
                ((tip_info.burn_block_height + 1) % BURN_BLOCK_MINED_AT_MODULUS) as u8;
            let op = BlockstackOperationType::LeaderBlockCommit(LeaderBlockCommitOp {
                treatment: vec![],
                sunset_burn: 0,
                block_header_hash: BlockHeaderHash([0xff; 32]),
                burn_fee: burn_fee_cap,
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
            let res =
                bitcoin_controller.submit_operation(StacksEpochId::Epoch21, op, &mut op_signer, 1);
            assert!(res.is_ok(), "Failed to submit block-commit");
        }

        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    let account = get_account(&http_origin, &miner_account);
    assert!(crossed_21_boundary);
    assert!(account.nonce >= 31);

    let mut have_pox2 = false;
    let mut have_costs3 = false;

    let blocks = test_observer::get_blocks();
    for block in blocks {
        let transactions = block.get("transactions").unwrap().as_array().unwrap();
        for tx in transactions {
            let raw_tx = tx.get("raw_tx").unwrap().as_str().unwrap();
            if raw_tx == "0x00" {
                continue;
            }
            let tx_bytes = hex_bytes(&raw_tx[2..]).unwrap();
            let parsed = StacksTransaction::consensus_deserialize(&mut &tx_bytes[..]).unwrap();
            if let TransactionPayload::SmartContract(tsc, ..) = parsed.payload {
                if tsc.name == "pox-2".into() {
                    have_pox2 = true;
                }
                if tsc.name == "costs-3".into() {
                    have_costs3 = true;
                }
            }
        }
    }

    assert!(have_pox2);
    assert!(have_costs3);

    channel.stop_chains_coordinator();
}

/// Check to see if there are stragglers between a set of nodes syncing
pub fn wait_pox_stragglers(confs: &[Config], max_stacks_tip: u64, block_time_ms: u64) {
    loop {
        let mut straggler = false;
        let mut stacks_tip_ch = None;
        let mut stacks_tip_bhh = None;

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");

            if tip_info.stacks_tip_height < max_stacks_tip {
                straggler = true;
            }

            if let Some(stacks_tip_ch) = stacks_tip_ch.as_ref() {
                if *stacks_tip_ch != tip_info.stacks_tip_consensus_hash {
                    straggler = true;
                }
            } else {
                stacks_tip_ch = Some(tip_info.stacks_tip_consensus_hash);
            }
            if let Some(stacks_tip_bhh) = stacks_tip_bhh.as_ref() {
                if *stacks_tip_bhh != tip_info.stacks_tip {
                    straggler = true;
                }
            } else {
                stacks_tip_bhh = Some(tip_info.stacks_tip);
            }
        }
        if !straggler {
            break;
        }
        sleep_ms(block_time_ms);
    }
}

/// PoX reorg with three flaps.
/// Miner 0 mines and hides the anchor block for cycles 22.
/// Miner 1 mines and hides the anchor block for cycles 23 and 24, causing a PoX reorg in miner 0.
/// Miner 0 mines and hides the anchor block for cycles 25, 26, and 27, causing a PoX reorg in miner 1.
/// At the very end, miners stop hiding their blocks, and the test verifies that both miners
/// converge on having anchor blocks for cycles 22, 25, 26, and 27, but not 23 and 24.
#[test]
#[ignore]
fn test_pox_reorgs_three_flaps() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let num_miners = 2;

    let reward_cycle_len = 10;
    let prepare_phase_len = 3;
    let v1_unlock_height = 152;

    let (mut conf_template, _) = neon_integration_test_conf();
    let block_time_ms = 10_000;
    conf_template.node.mine_microblocks = true;
    conf_template.miner.microblock_attempt_time_ms = 2_000;
    conf_template.node.wait_time_for_microblocks = 0;
    conf_template.node.microblock_frequency = 0;
    conf_template.miner.first_attempt_time_ms = 2_000;
    conf_template.miner.subsequent_attempt_time_ms = 5_000;
    conf_template.burnchain.max_rbf = 1000000;
    conf_template.node.wait_time_for_blocks = 1_000;
    conf_template.burnchain.pox_2_activation = Some(v1_unlock_height);

    conf_template.node.require_affirmed_anchor_blocks = false;

    // make epoch 2.1 start in the middle of boot-up
    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = 101;
    epochs[StacksEpochId::Epoch2_05].start_height = 101;
    epochs[StacksEpochId::Epoch2_05].end_height = 151;
    epochs[StacksEpochId::Epoch21].start_height = 151;
    conf_template.burnchain.epochs = Some(epochs);

    let privks: Vec<_> = (0..5).map(|_| StacksPrivateKey::new()).collect();

    let stack_privks: Vec<_> = (0..5).map(|_| StacksPrivateKey::new()).collect();

    let balances: Vec<_> = privks
        .iter()
        .map(|privk| {
            let addr = to_addr(privk);
            InitialBalance {
                address: addr.into(),
                amount: 30_000_000,
            }
        })
        .collect();

    let stack_balances: Vec<_> = stack_privks
        .iter()
        .map(|privk| {
            let addr = to_addr(privk);
            InitialBalance {
                address: addr.into(),
                amount: 2_000_000_000_000_000,
            }
        })
        .collect();

    let mut confs = vec![];
    let mut burnchain_configs = vec![];
    let mut blocks_processed = vec![];
    let mut channels = vec![];
    let mut miner_status = vec![];

    for i in 0..num_miners {
        let seed = StacksPrivateKey::new().to_bytes();
        let (mut conf, _) = neon_integration_test_conf_with_seed(seed);

        conf.initial_balances.clear();
        conf.initial_balances.append(&mut balances.clone());
        conf.initial_balances.append(&mut stack_balances.clone());

        conf.node.mine_microblocks = conf_template.node.mine_microblocks;
        conf.miner.microblock_attempt_time_ms = conf_template.miner.microblock_attempt_time_ms;
        conf.node.wait_time_for_microblocks = conf_template.node.wait_time_for_microblocks;
        conf.node.microblock_frequency = conf_template.node.microblock_frequency;
        conf.miner.first_attempt_time_ms = conf_template.miner.first_attempt_time_ms;
        conf.miner.subsequent_attempt_time_ms = conf_template.miner.subsequent_attempt_time_ms;
        conf.node.wait_time_for_blocks = conf_template.node.wait_time_for_blocks;
        conf.burnchain.max_rbf = conf_template.burnchain.max_rbf;
        conf.burnchain.epochs = conf_template.burnchain.epochs.clone();
        conf.burnchain.pox_2_activation = conf_template.burnchain.pox_2_activation;
        conf.node.require_affirmed_anchor_blocks =
            conf_template.node.require_affirmed_anchor_blocks;

        // multiple nodes so they must download from each other
        conf.miner.wait_for_block_download = true;

        // nodes will selectively hide blocks from one another
        conf.node.fault_injection_hide_blocks = true;

        let rpc_port = 41043 + 10 * i;
        let p2p_port = 41043 + 10 * i + 1;
        conf.node.rpc_bind = format!("127.0.0.1:{rpc_port}");
        conf.node.data_url = format!("http://127.0.0.1:{rpc_port}");
        conf.node.p2p_bind = format!("127.0.0.1:{p2p_port}");

        // conf.connection_options.inv_reward_cycles = 10;

        confs.push(conf);
    }

    let node_privkey_1 = Secp256k1PrivateKey::from_seed(&confs[0].node.local_peer_seed);
    let chain_id = confs[0].burnchain.chain_id;
    let peer_version = confs[0].burnchain.peer_version;
    let p2p_bind = confs[0].node.p2p_bind.clone();
    for conf in confs.iter_mut().skip(1) {
        conf.node.set_bootstrap_nodes(
            format!(
                "{}@{p2p_bind}",
                &StacksPublicKey::from_private(&node_privkey_1).to_hex()
            ),
            chain_id,
            peer_version,
        );
    }

    // use short reward cycles
    for conf in &confs {
        let mut burnchain_config = Burnchain::regtest(&conf.get_burn_db_path());
        let pox_constants = PoxConstants::new(
            reward_cycle_len,
            prepare_phase_len,
            4 * prepare_phase_len / 5,
            5,
            15,
            (1600 * reward_cycle_len - 1).into(),
            (1700 * reward_cycle_len).into(),
            v1_unlock_height,
            u32::MAX,
            u32::MAX,
            u32::MAX,
        );
        burnchain_config.pox_constants = pox_constants.clone();

        burnchain_configs.push(burnchain_config);
    }

    let mut btcd_controller = BitcoinCoreController::new(confs[0].clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::with_burnchain(
        confs[0].clone(),
        None,
        Some(burnchain_configs[0].clone()),
        None,
    );

    btc_regtest_controller.bootstrap_chain(1);

    // make sure all miners have BTC
    for conf in confs.iter().skip(1) {
        let old_mining_pubkey = btc_regtest_controller.get_mining_pubkey().unwrap();
        btc_regtest_controller
            .set_mining_pubkey(conf.burnchain.local_mining_public_key.clone().unwrap());
        btc_regtest_controller.bootstrap_chain(1);
        btc_regtest_controller.set_mining_pubkey(old_mining_pubkey);
    }

    btc_regtest_controller.bootstrap_chain((199 - num_miners) as u64);

    eprintln!("Chain bootstrapped...");

    for (i, burnchain_config) in burnchain_configs.into_iter().enumerate() {
        let mut run_loop = neon::RunLoop::new(confs[i].clone());
        let blocks_processed_arc = run_loop.get_blocks_processed_arc();
        let channel = run_loop.get_coordinator_channel().unwrap();
        let this_miner_status = run_loop.get_miner_status();

        blocks_processed.push(blocks_processed_arc);
        channels.push(channel);
        miner_status.push(this_miner_status);

        thread::spawn(move || run_loop.start(Some(burnchain_config), 0));
    }

    let http_origin = format!("http://{}", &confs[0].node.rpc_bind);

    // give the run loops some time to start up!
    for bp in &blocks_processed {
        wait_for_runloop(bp);
    }

    // activate miners
    eprintln!("\n\nBoot miner 0\n\n");
    loop {
        let tip_info_opt = get_chain_info_opt(&confs[0]);
        if let Some(tip_info) = tip_info_opt {
            eprintln!("\n\nMiner 0: {tip_info:?}\n\n");
            if tip_info.stacks_tip_height > 0 {
                break;
            }
        } else {
            eprintln!("\n\nWaiting for miner 0...\n\n");
        }
        next_block_and_iterate(
            &mut btc_regtest_controller,
            &blocks_processed[0],
            block_time_ms,
        );
    }

    for (i, conf) in confs.iter().enumerate().skip(1) {
        eprintln!("\n\nBoot miner {i}\n\n");
        loop {
            let tip_info_opt = get_chain_info_opt(conf);
            if let Some(tip_info) = tip_info_opt {
                eprintln!("\n\nMiner {i}: {tip_info:?}\n\n");
                if tip_info.stacks_tip_height > 0 {
                    break;
                }
            } else {
                eprintln!("\n\nWaiting for miner {i}...\n\n");
            }
            next_block_and_iterate(&mut btc_regtest_controller, &blocks_processed[i], 5_000);
        }
    }

    eprintln!("\n\nBegin transactions\n\n");

    let pox_pubkey = Secp256k1PublicKey::from_hex(
        "02f006a09b59979e2cb8449f58076152af6b124aa29b948a3714b8d5f15aa94ede",
    )
    .unwrap();
    let pox_pubkey_hash = bytes_to_hex(&Hash160::from_node_public_key(&pox_pubkey).to_bytes());

    let sort_height = channels[0].get_sortitions_processed();

    // make everyone stack
    let stacking_txs: Vec<_> = stack_privks
        .iter()
        .map(|pk| {
            make_contract_call(
                pk,
                0,
                1360,
                conf_template.burnchain.chain_id,
                &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
                "pox-2",
                "stack-stx",
                &[
                    Value::UInt(2_000_000_000_000_000 - 30_000_000),
                    execute(
                        &format!("{{ hashbytes: 0x{pox_pubkey_hash}, version: 0x00 }}"),
                        ClarityVersion::Clarity1,
                    )
                    .unwrap()
                    .unwrap(),
                    Value::UInt((sort_height + 1) as u128),
                    Value::UInt(12),
                ],
            )
        })
        .collect();

    // keeps the mempool full, and makes it so miners will spend a nontrivial amount of time
    // building blocks
    let all_txs: Vec<_> = privks
        .iter()
        .enumerate()
        .map(|(i, pk)| {
            make_random_tx_chain(pk, (25 * i) as u64, conf_template.burnchain.chain_id, false)
        })
        .collect();

    // everyone locks up
    for (cnt, tx) in stacking_txs.iter().enumerate() {
        eprintln!("\n\nSubmit stacking tx {cnt}\n\n");
        submit_tx(&http_origin, tx);
    }

    // run a reward cycle
    let mut at_220 = false;
    while !at_220 {
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
            if tip_info.burn_block_height == 220 {
                at_220 = true;
            }
        }
    }

    // blast out the rest
    let mut cnt = 0;
    for tx_chain in all_txs {
        for tx in tx_chain {
            eprintln!("\n\nSubmit tx {cnt}\n\n");
            submit_tx(&http_origin, &tx);
            cnt += 1;
        }
    }

    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
        assert!(tip_info.burn_block_height <= 220);
    }

    eprintln!("\n\nBegin mining\n\n");

    info!("####################### end of cycle ##############################");
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");

        //assert_eq!(tip_info.affirmations.heaviest, AffirmationMap::decode("nnnnnnnnnnnnnnnnnnnnp").unwrap());
    }
    info!("####################### end of cycle ##############################");

    // prevent Stacks at these heights from propagating
    env::set_var(
        "STACKS_HIDE_BLOCKS_AT_HEIGHT",
        "[226,227,228,229,230,236,237,238,239,240,246,247,248,249,250,256,257,258,259,260,266,267,268,269,270,276,277,278,279,280,286,287,288,289,290]"
    );

    // miner 0 mines a prepare phase and confirms a hidden anchor block.
    // miner 1 is disabled for these prepare phases
    for i in 0..10 {
        eprintln!("\n\nBuild block {i}\n\n");
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
        }

        if i >= reward_cycle_len - prepare_phase_len - 2 {
            signal_mining_blocked(miner_status[1].clone());
        }
    }
    signal_mining_ready(miner_status[1].clone());

    info!("####################### end of cycle ##############################");
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
    }
    info!("####################### end of cycle ##############################");

    // miner 1 mines a prepare phase and confirms a hidden anchor block.
    // miner 0 is disabled for this prepare phase
    for i in 0..10 {
        eprintln!("\n\nBuild block {i}\n\n");
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
        }

        if i >= reward_cycle_len - prepare_phase_len - 2 {
            signal_mining_blocked(miner_status[0].clone());
        }
    }
    signal_mining_ready(miner_status[0].clone());

    info!("####################### end of cycle ##############################");
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");

        // miner 1's history overtakes miner 0's.
        // Miner 1 didn't see cycle 22's anchor block, but it just mined an anchor block for cycle
        // 23 and affirmed cycle 22's anchor block's absence.
    }
    info!("####################### end of cycle ##############################");

    // miner 1 mines a prepare phase and confirms a hidden anchor block.
    // miner 0 is disabled for this prepare phase
    for i in 0..10 {
        eprintln!("\n\nBuild block {i}\n\n");
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
        }

        if i >= reward_cycle_len - prepare_phase_len - 2 {
            signal_mining_blocked(miner_status[0].clone());
        }
    }
    signal_mining_ready(miner_status[0].clone());

    info!("####################### end of cycle ##############################");
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");

        // miner 1's history continues to overtake miner 0's.
        // Miner 1 didn't see cycle 22's anchor block, but it just mined an anchor block for cycle
        // 23 and cycle 24 which both affirm cycle 22's anchor block's absence.
    }
    info!("####################### end of cycle ##############################");

    // miner 0 mines a prepare phase and confirms a hidden anchor block.
    // miner 1 is disabled for these prepare phases
    for i in 0..10 {
        eprintln!("\n\nBuild block {i}\n\n");
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
        }

        if i >= reward_cycle_len - prepare_phase_len - 2 {
            signal_mining_blocked(miner_status[1].clone());
        }
    }
    signal_mining_ready(miner_status[1].clone());

    info!("####################### end of cycle ##############################");
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");

        // miner 0 may have won here, but its affirmation map isn't yet the heaviest.
    }
    info!("####################### end of cycle ##############################");

    // miner 0 mines a prepare phase and confirms a hidden anchor block.
    // miner 1 is disabled for these prepare phases
    for i in 0..10 {
        eprintln!("\n\nBuild block {i}\n\n");
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
        }

        if i >= reward_cycle_len - prepare_phase_len - 2 {
            signal_mining_blocked(miner_status[1].clone());
        }
    }
    signal_mining_ready(miner_status[1].clone());

    info!("####################### end of cycle ##############################");
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");

        // miner 0's affirmation map now becomes the heaviest.
    }
    info!("####################### end of cycle ##############################");

    // miner 0 mines a prepare phase and confirms a hidden anchor block.
    // miner 1 is disabled for these prepare phases
    for i in 0..10 {
        eprintln!("\n\nBuild block {i}\n\n");
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
        }

        if i >= reward_cycle_len - prepare_phase_len - 2 {
            signal_mining_blocked(miner_status[1].clone());
        }
    }
    signal_mining_ready(miner_status[1].clone());

    info!("####################### end of cycle ##############################");
    let mut max_stacks_tip = 0;
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");

        // miner 0's affirmation map is now the heaviest, and there's no longer a tie.
        max_stacks_tip = std::cmp::max(tip_info.stacks_tip_height, max_stacks_tip);
    }
    info!("####################### end of cycle ##############################");

    // advance to start of next reward cycle
    eprintln!("\n\nBuild final block\n\n");
    btc_regtest_controller.build_next_block(1);
    sleep_ms(block_time_ms);

    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
    }

    // resume block propagation
    env::set_var("STACKS_HIDE_BLOCKS_AT_HEIGHT", "[]");

    // wait for all blocks to propagate
    eprintln!("Wait for all blocks to propagate; max tip is {max_stacks_tip}");
    wait_pox_stragglers(&confs, max_stacks_tip, block_time_ms);

    // nodes now agree on affirmation maps
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Final tip for miner {i}: {tip_info:?}");
    }
}

/// PoX reorg with just one flap.
/// Miner 0 mines and hides the anchor block for cycle 22.
/// Miner 1 mines and hides the anchor block for cycle 23, causing a PoX reorg in miner 0.
/// At the very end, miners stop hiding their blocks, and the test verifies that both miners
/// converge on having anchor blocks for cycles 22 and 24, but not 23.
#[test]
#[ignore]
fn test_pox_reorg_one_flap() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let num_miners = 2;

    let reward_cycle_len = 10;
    let prepare_phase_len = 3;
    let v1_unlock_height = 152;

    let (mut conf_template, _) = neon_integration_test_conf();
    let block_time_ms = 10_000;
    conf_template.node.mine_microblocks = true;
    conf_template.miner.microblock_attempt_time_ms = 2_000;
    conf_template.node.wait_time_for_microblocks = 0;
    conf_template.node.microblock_frequency = 0;
    conf_template.miner.first_attempt_time_ms = 2_000;
    conf_template.miner.subsequent_attempt_time_ms = 5_000;
    conf_template.burnchain.max_rbf = 1000000;
    conf_template.node.wait_time_for_blocks = 1_000;
    conf_template.burnchain.pox_2_activation = Some(v1_unlock_height);

    conf_template.node.require_affirmed_anchor_blocks = false;

    // make epoch 2.1 start in the middle of boot-up
    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = 101;
    epochs[StacksEpochId::Epoch2_05].start_height = 101;
    epochs[StacksEpochId::Epoch2_05].end_height = 151;
    epochs[StacksEpochId::Epoch21].start_height = 151;
    conf_template.burnchain.epochs = Some(epochs);

    let privks: Vec<_> = (0..5).map(|_| StacksPrivateKey::new()).collect();

    let stack_privks: Vec<_> = (0..5).map(|_| StacksPrivateKey::new()).collect();

    let balances: Vec<_> = privks
        .iter()
        .map(|privk| {
            let addr = to_addr(privk);
            InitialBalance {
                address: addr.into(),
                amount: 30_000_000,
            }
        })
        .collect();

    let stack_balances: Vec<_> = stack_privks
        .iter()
        .map(|privk| {
            let addr = to_addr(privk);
            InitialBalance {
                address: addr.into(),
                amount: 2_000_000_000_000_000,
            }
        })
        .collect();

    let mut confs = vec![];
    let mut burnchain_configs = vec![];
    let mut blocks_processed = vec![];
    let mut channels = vec![];
    let mut miner_status = vec![];

    for i in 0..num_miners {
        let seed = StacksPrivateKey::new().to_bytes();
        let (mut conf, _) = neon_integration_test_conf_with_seed(seed);

        conf.initial_balances.clear();
        conf.initial_balances.append(&mut balances.clone());
        conf.initial_balances.append(&mut stack_balances.clone());

        conf.node.mine_microblocks = conf_template.node.mine_microblocks;
        conf.miner.microblock_attempt_time_ms = conf_template.miner.microblock_attempt_time_ms;
        conf.node.wait_time_for_microblocks = conf_template.node.wait_time_for_microblocks;
        conf.node.microblock_frequency = conf_template.node.microblock_frequency;
        conf.miner.first_attempt_time_ms = conf_template.miner.first_attempt_time_ms;
        conf.miner.subsequent_attempt_time_ms = conf_template.miner.subsequent_attempt_time_ms;
        conf.node.wait_time_for_blocks = conf_template.node.wait_time_for_blocks;
        conf.burnchain.max_rbf = conf_template.burnchain.max_rbf;
        conf.burnchain.epochs = conf_template.burnchain.epochs.clone();
        conf.burnchain.pox_2_activation = conf_template.burnchain.pox_2_activation;
        conf.node.require_affirmed_anchor_blocks =
            conf_template.node.require_affirmed_anchor_blocks;

        // multiple nodes so they must download from each other
        conf.miner.wait_for_block_download = true;

        // nodes will selectively hide blocks from one another
        conf.node.fault_injection_hide_blocks = true;

        let rpc_port = 41063 + 10 * i;
        let p2p_port = 41063 + 10 * i + 1;
        conf.node.rpc_bind = format!("127.0.0.1:{rpc_port}");
        conf.node.data_url = format!("http://127.0.0.1:{rpc_port}");
        conf.node.p2p_bind = format!("127.0.0.1:{p2p_port}");

        confs.push(conf);
    }

    let node_privkey_1 = Secp256k1PrivateKey::from_seed(&confs[0].node.local_peer_seed);
    let chain_id = confs[0].burnchain.chain_id;
    let peer_version = confs[0].burnchain.peer_version;
    let p2p_bind = confs[0].node.p2p_bind.clone();
    for conf in confs.iter_mut().skip(1) {
        conf.node.set_bootstrap_nodes(
            format!(
                "{}@{p2p_bind}",
                &StacksPublicKey::from_private(&node_privkey_1).to_hex()
            ),
            chain_id,
            peer_version,
        );
    }

    // use short reward cycles
    for conf in &confs {
        let mut burnchain_config = Burnchain::regtest(&conf.get_burn_db_path());
        let pox_constants = PoxConstants::new(
            reward_cycle_len,
            prepare_phase_len,
            4 * prepare_phase_len / 5,
            5,
            15,
            (1600 * reward_cycle_len - 1).into(),
            (1700 * reward_cycle_len).into(),
            v1_unlock_height,
            u32::MAX,
            u32::MAX,
            u32::MAX,
        );
        burnchain_config.pox_constants = pox_constants.clone();

        burnchain_configs.push(burnchain_config);
    }

    let mut btcd_controller = BitcoinCoreController::new(confs[0].clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::with_burnchain(
        confs[0].clone(),
        None,
        Some(burnchain_configs[0].clone()),
        None,
    );

    btc_regtest_controller.bootstrap_chain(1);

    // make sure all miners have BTC
    for conf in confs.iter().skip(1) {
        let old_mining_pubkey = btc_regtest_controller.get_mining_pubkey().unwrap();
        btc_regtest_controller
            .set_mining_pubkey(conf.burnchain.local_mining_public_key.clone().unwrap());
        btc_regtest_controller.bootstrap_chain(1);
        btc_regtest_controller.set_mining_pubkey(old_mining_pubkey);
    }

    btc_regtest_controller.bootstrap_chain((199 - num_miners) as u64);

    eprintln!("Chain bootstrapped...");

    for (i, burnchain_config) in burnchain_configs.into_iter().enumerate() {
        let mut run_loop = neon::RunLoop::new(confs[i].clone());
        let blocks_processed_arc = run_loop.get_blocks_processed_arc();
        let channel = run_loop.get_coordinator_channel().unwrap();
        let this_miner_status = run_loop.get_miner_status();

        blocks_processed.push(blocks_processed_arc);
        channels.push(channel);
        miner_status.push(this_miner_status);

        thread::spawn(move || run_loop.start(Some(burnchain_config), 0));
    }

    let http_origin = format!("http://{}", &confs[0].node.rpc_bind);

    // give the run loops some time to start up!
    for bp in &blocks_processed {
        wait_for_runloop(bp);
    }

    // activate miners
    eprintln!("\n\nBoot miner 0\n\n");
    loop {
        let tip_info_opt = get_chain_info_opt(&confs[0]);
        if let Some(tip_info) = tip_info_opt {
            eprintln!("\n\nMiner 0: {tip_info:?}\n\n");
            if tip_info.stacks_tip_height > 0 {
                break;
            }
        } else {
            eprintln!("\n\nWaiting for miner 0...\n\n");
        }
        next_block_and_iterate(
            &mut btc_regtest_controller,
            &blocks_processed[0],
            block_time_ms,
        );
    }

    for (i, conf) in confs.iter().enumerate().skip(1) {
        eprintln!("\n\nBoot miner {i}\n\n");
        loop {
            let tip_info_opt = get_chain_info_opt(conf);
            if let Some(tip_info) = tip_info_opt {
                eprintln!("\n\nMiner {i}: {tip_info:?}\n\n");
                if tip_info.stacks_tip_height > 0 {
                    break;
                }
            } else {
                eprintln!("\n\nWaiting for miner {i}...\n\n");
            }
            next_block_and_iterate(&mut btc_regtest_controller, &blocks_processed[i], 5_000);
        }
    }

    eprintln!("\n\nBegin transactions\n\n");

    let pox_pubkey = Secp256k1PublicKey::from_hex(
        "02f006a09b59979e2cb8449f58076152af6b124aa29b948a3714b8d5f15aa94ede",
    )
    .unwrap();
    let pox_pubkey_hash = bytes_to_hex(&Hash160::from_node_public_key(&pox_pubkey).to_bytes());

    let sort_height = channels[0].get_sortitions_processed();

    // make everyone stack
    let stacking_txs: Vec<_> = stack_privks
        .iter()
        .map(|pk| {
            make_contract_call(
                pk,
                0,
                1360,
                conf_template.burnchain.chain_id,
                &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
                "pox-2",
                "stack-stx",
                &[
                    Value::UInt(2_000_000_000_000_000 - 30_000_000),
                    execute(
                        &format!("{{ hashbytes: 0x{pox_pubkey_hash}, version: 0x00 }}"),
                        ClarityVersion::Clarity1,
                    )
                    .unwrap()
                    .unwrap(),
                    Value::UInt((sort_height + 1) as u128),
                    Value::UInt(12),
                ],
            )
        })
        .collect();

    // keeps the mempool full, and makes it so miners will spend a nontrivial amount of time
    // building blocks
    let all_txs: Vec<_> = privks
        .iter()
        .enumerate()
        .map(|(i, pk)| {
            make_random_tx_chain(pk, (25 * i) as u64, conf_template.burnchain.chain_id, false)
        })
        .collect();

    // everyone locks up
    for (cnt, tx) in stacking_txs.iter().enumerate() {
        eprintln!("\n\nSubmit stacking tx {cnt}\n\n");
        submit_tx(&http_origin, tx);
    }

    // run a reward cycle
    let mut at_220 = false;
    while !at_220 {
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
            if tip_info.burn_block_height == 220 {
                at_220 = true;
            }
        }
    }

    // blast out the rest
    let mut cnt = 0;
    for tx_chain in all_txs {
        for tx in tx_chain {
            eprintln!("\n\nSubmit tx {cnt}\n\n");
            submit_tx(&http_origin, &tx);
            cnt += 1;
        }
    }

    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
        assert!(tip_info.burn_block_height <= 220);
    }

    eprintln!("\n\nBegin mining\n\n");

    info!("####################### end of cycle ##############################");
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
    }
    info!("####################### end of cycle ##############################");

    // prevent Stacks at these heights from propagating
    env::set_var(
        "STACKS_HIDE_BLOCKS_AT_HEIGHT",
        "[226,227,228,229,230,236,237,238,239,240,246,247,248,249,250,256,257,258,259,260,266,267,268,269,270,276,277,278,279,280,286,287,288,289,290]"
    );

    // miner 0 mines a prepare phase and confirms a hidden anchor block.
    // miner 1 is disabled for these prepare phases
    for i in 0..10 {
        eprintln!("\n\nBuild block {i}\n\n");
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
        }

        if i >= reward_cycle_len - prepare_phase_len - 2 {
            signal_mining_blocked(miner_status[1].clone());
        }
    }
    signal_mining_ready(miner_status[1].clone());

    info!("####################### end of cycle ##############################");
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
    }
    info!("####################### end of cycle ##############################");

    // miner 1 mines a prepare phase and confirms a hidden anchor block.
    // miner 0 is disabled for this prepare phase
    for i in 0..10 {
        eprintln!("\n\nBuild block {i}\n\n");
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
        }

        if i >= reward_cycle_len - prepare_phase_len - 2 {
            signal_mining_blocked(miner_status[0].clone());
        }
    }
    signal_mining_ready(miner_status[0].clone());

    info!("####################### end of cycle ##############################");
    let mut max_stacks_tip = 0;
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");

        // miner 1's history overtakes miner 0's.
        // Miner 1 didn't see cycle 22's anchor block, but it just mined an anchor block for cycle
        // 23 and affirmed cycle 22's anchor block's absence.
        max_stacks_tip = std::cmp::max(tip_info.stacks_tip_height, max_stacks_tip);
    }
    info!("####################### end of cycle ##############################");

    // advance to start of next reward cycle
    eprintln!("\n\nBuild final block\n\n");
    btc_regtest_controller.build_next_block(1);
    sleep_ms(block_time_ms);

    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
    }

    // resume block propagation
    env::set_var("STACKS_HIDE_BLOCKS_AT_HEIGHT", "[]");

    // wait for all blocks to propagate
    eprintln!("Wait for all blocks to propagate; stacks tip height is {max_stacks_tip}");
    wait_pox_stragglers(&confs, max_stacks_tip, block_time_ms);

    // nodes now agree on stacks affirmation map
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Final tip for miner {i}: {tip_info:?}");
    }
}

/// PoX reorg tests where two miners take turn mining hidden anchor blocks.
/// Both miners mine in the reward phase, and in doing so, confirm their hidden anchor blocks.
/// The heaviest affirmation map grows as n+pppa+
#[test]
#[ignore]
fn test_pox_reorg_flap_duel() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let num_miners = 2;

    let reward_cycle_len = 10;
    let prepare_phase_len = 3;
    let v1_unlock_height = 152;

    let (mut conf_template, _) = neon_integration_test_conf();
    let block_time_ms = 10_000;
    conf_template.node.mine_microblocks = true;
    conf_template.miner.microblock_attempt_time_ms = 2_000;
    conf_template.node.wait_time_for_microblocks = 0;
    conf_template.node.microblock_frequency = 0;
    conf_template.miner.first_attempt_time_ms = 2_000;
    conf_template.miner.subsequent_attempt_time_ms = 5_000;
    conf_template.burnchain.max_rbf = 1000000;
    conf_template.node.wait_time_for_blocks = 1_000;
    conf_template.burnchain.pox_2_activation = Some(v1_unlock_height);

    conf_template.node.require_affirmed_anchor_blocks = false;

    // make epoch 2.1 start in the middle of boot-up
    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = 101;
    epochs[StacksEpochId::Epoch2_05].start_height = 101;
    epochs[StacksEpochId::Epoch2_05].end_height = 151;
    epochs[StacksEpochId::Epoch21].start_height = 151;
    conf_template.burnchain.epochs = Some(epochs);

    let privks: Vec<_> = (0..5).map(|_| StacksPrivateKey::new()).collect();

    let stack_privks: Vec<_> = (0..5).map(|_| StacksPrivateKey::new()).collect();

    let balances: Vec<_> = privks
        .iter()
        .map(|privk| {
            let addr = to_addr(privk);
            InitialBalance {
                address: addr.into(),
                amount: 30_000_000,
            }
        })
        .collect();

    let stack_balances: Vec<_> = stack_privks
        .iter()
        .map(|privk| {
            let addr = to_addr(privk);
            InitialBalance {
                address: addr.into(),
                amount: 2_000_000_000_000_000,
            }
        })
        .collect();

    let mut confs = vec![];
    let mut burnchain_configs = vec![];
    let mut blocks_processed = vec![];
    let mut channels = vec![];
    let mut miner_status = vec![];

    for i in 0..num_miners {
        let seed = StacksPrivateKey::new().to_bytes();
        let (mut conf, _) = neon_integration_test_conf_with_seed(seed);

        conf.initial_balances.clear();
        conf.initial_balances.append(&mut balances.clone());
        conf.initial_balances.append(&mut stack_balances.clone());

        conf.node.mine_microblocks = conf_template.node.mine_microblocks;
        conf.miner.microblock_attempt_time_ms = conf_template.miner.microblock_attempt_time_ms;
        conf.node.wait_time_for_microblocks = conf_template.node.wait_time_for_microblocks;
        conf.node.microblock_frequency = conf_template.node.microblock_frequency;
        conf.miner.first_attempt_time_ms = conf_template.miner.first_attempt_time_ms;
        conf.miner.subsequent_attempt_time_ms = conf_template.miner.subsequent_attempt_time_ms;
        conf.node.wait_time_for_blocks = conf_template.node.wait_time_for_blocks;
        conf.burnchain.max_rbf = conf_template.burnchain.max_rbf;
        conf.burnchain.epochs = conf_template.burnchain.epochs.clone();
        conf.burnchain.pox_2_activation = conf_template.burnchain.pox_2_activation;
        conf.node.require_affirmed_anchor_blocks =
            conf_template.node.require_affirmed_anchor_blocks;

        // multiple nodes so they must download from each other
        conf.miner.wait_for_block_download = true;

        // nodes will selectively hide blocks from one another
        conf.node.fault_injection_hide_blocks = true;

        // conf.connection_options.inv_reward_cycles = 10;

        let rpc_port = 41083 + 10 * i;
        let p2p_port = 41083 + 10 * i + 1;
        conf.node.rpc_bind = format!("127.0.0.1:{rpc_port}");
        conf.node.data_url = format!("http://127.0.0.1:{rpc_port}");
        conf.node.p2p_bind = format!("127.0.0.1:{p2p_port}");

        confs.push(conf);
    }

    let node_privkey_1 = Secp256k1PrivateKey::from_seed(&confs[0].node.local_peer_seed);
    let chain_id = confs[0].burnchain.chain_id;
    let peer_version = confs[0].burnchain.peer_version;
    let p2p_bind = confs[0].node.p2p_bind.clone();

    for conf in confs.iter_mut().skip(1) {
        conf.node.set_bootstrap_nodes(
            format!(
                "{}@{p2p_bind}",
                &StacksPublicKey::from_private(&node_privkey_1).to_hex()
            ),
            chain_id,
            peer_version,
        );
    }

    // use short reward cycles
    for conf in &confs {
        let mut burnchain_config = Burnchain::regtest(&conf.get_burn_db_path());
        let pox_constants = PoxConstants::new(
            reward_cycle_len,
            prepare_phase_len,
            4 * prepare_phase_len / 5,
            5,
            15,
            (1600 * reward_cycle_len - 1).into(),
            (1700 * reward_cycle_len).into(),
            v1_unlock_height,
            u32::MAX,
            u32::MAX,
            u32::MAX,
        );
        burnchain_config.pox_constants = pox_constants.clone();

        burnchain_configs.push(burnchain_config);
    }

    let mut btcd_controller = BitcoinCoreController::new(confs[0].clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::with_burnchain(
        confs[0].clone(),
        None,
        Some(burnchain_configs[0].clone()),
        None,
    );

    btc_regtest_controller.bootstrap_chain(1);

    // make sure all miners have BTC
    for conf in confs.iter().skip(1) {
        let old_mining_pubkey = btc_regtest_controller.get_mining_pubkey().unwrap();
        btc_regtest_controller
            .set_mining_pubkey(conf.burnchain.local_mining_public_key.clone().unwrap());
        btc_regtest_controller.bootstrap_chain(1);
        btc_regtest_controller.set_mining_pubkey(old_mining_pubkey);
    }

    btc_regtest_controller.bootstrap_chain((199 - num_miners) as u64);

    eprintln!("Chain bootstrapped...");

    for (i, burnchain_config) in burnchain_configs.into_iter().enumerate() {
        let mut run_loop = neon::RunLoop::new(confs[i].clone());
        let blocks_processed_arc = run_loop.get_blocks_processed_arc();
        let channel = run_loop.get_coordinator_channel().unwrap();
        let this_miner_status = run_loop.get_miner_status();

        blocks_processed.push(blocks_processed_arc);
        channels.push(channel);
        miner_status.push(this_miner_status);

        thread::spawn(move || run_loop.start(Some(burnchain_config), 0));
    }

    let http_origin = format!("http://{}", &confs[0].node.rpc_bind);

    // give the run loops some time to start up!
    for bp in &blocks_processed {
        wait_for_runloop(bp);
    }

    // activate miners
    eprintln!("\n\nBoot miner 0\n\n");
    loop {
        let tip_info_opt = get_chain_info_opt(&confs[0]);
        if let Some(tip_info) = tip_info_opt {
            eprintln!("\n\nMiner 0: {tip_info:?}\n\n");
            if tip_info.stacks_tip_height > 0 {
                break;
            }
        } else {
            eprintln!("\n\nWaiting for miner 0...\n\n");
        }
        next_block_and_iterate(
            &mut btc_regtest_controller,
            &blocks_processed[0],
            block_time_ms,
        );
    }

    for (i, conf) in confs.iter().enumerate().skip(1) {
        eprintln!("\n\nBoot miner {i}\n\n");
        loop {
            let tip_info_opt = get_chain_info_opt(conf);
            if let Some(tip_info) = tip_info_opt {
                eprintln!("\n\nMiner {i}: {tip_info:?}\n\n");
                if tip_info.stacks_tip_height > 0 {
                    break;
                }
            } else {
                eprintln!("\n\nWaiting for miner {i}...\n\n");
            }
            next_block_and_iterate(&mut btc_regtest_controller, &blocks_processed[i], 5_000);
        }
    }

    eprintln!("\n\nBegin transactions\n\n");

    let pox_pubkey = Secp256k1PublicKey::from_hex(
        "02f006a09b59979e2cb8449f58076152af6b124aa29b948a3714b8d5f15aa94ede",
    )
    .unwrap();
    let pox_pubkey_hash = bytes_to_hex(&Hash160::from_node_public_key(&pox_pubkey).to_bytes());

    let sort_height = channels[0].get_sortitions_processed();

    // make everyone stack
    let stacking_txs: Vec<_> = stack_privks
        .iter()
        .map(|pk| {
            make_contract_call(
                pk,
                0,
                1360,
                conf_template.burnchain.chain_id,
                &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
                "pox-2",
                "stack-stx",
                &[
                    Value::UInt(2_000_000_000_000_000 - 30_000_000),
                    execute(
                        &format!("{{ hashbytes: 0x{pox_pubkey_hash}, version: 0x00 }}"),
                        ClarityVersion::Clarity1,
                    )
                    .unwrap()
                    .unwrap(),
                    Value::UInt((sort_height + 1) as u128),
                    Value::UInt(12),
                ],
            )
        })
        .collect();

    // keeps the mempool full, and makes it so miners will spend a nontrivial amount of time
    // building blocks
    let all_txs: Vec<_> = privks
        .iter()
        .enumerate()
        .map(|(i, pk)| {
            make_random_tx_chain(pk, (25 * i) as u64, conf_template.burnchain.chain_id, false)
        })
        .collect();

    // everyone locks up
    for (cnt, tx) in stacking_txs.iter().enumerate() {
        eprintln!("\n\nSubmit stacking tx {cnt}\n\n");
        submit_tx(&http_origin, tx);
    }

    // run a reward cycle
    let mut at_220 = false;
    while !at_220 {
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
            if tip_info.burn_block_height == 220 {
                at_220 = true;
            }
        }
    }

    // blast out the rest
    let mut cnt = 0;
    for tx_chain in all_txs {
        for tx in tx_chain {
            eprintln!("\n\nSubmit tx {cnt}\n\n");
            submit_tx(&http_origin, &tx);
            cnt += 1;
        }
    }

    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
        assert!(tip_info.burn_block_height <= 220);
    }

    eprintln!("\n\nBegin mining\n\n");

    info!("####################### end of cycle ##############################");
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");

        //assert_eq!(tip_info.affirmations.heaviest, AffirmationMap::decode("nnnnnnnnnnnnnnnnnnnnp").unwrap());
    }
    info!("####################### end of cycle ##############################");

    // prevent Stacks at these heights from propagating.
    // This means that both nodes affirm the absence of each others' anchor blocks, and the
    // heaviest affirmation map will always look like n+pppa+
    env::set_var(
        "STACKS_HIDE_BLOCKS_AT_HEIGHT",
        "[226,227,228,229,230,236,237,238,239,240,246,247,248,249,250,256,257,258,259,260,266,267,268,269,270,276,277,278,279,280,286,287,288,289,290]"
    );

    let mut max_stacks_tip = 0;

    // miners 0 and 1 take turns mining anchor blocks.
    // this should cause them both to flip/flop their sortition histories multiple times
    for _c in 0..3 {
        // miner 0 mines a prepare phase and confirms a hidden anchor block.
        // miner 1 is disabled for these prepare phases
        for i in 0..10 {
            eprintln!("\n\nBuild block {i}\n\n");
            btc_regtest_controller.build_next_block(1);
            sleep_ms(block_time_ms);

            for (i, c) in confs.iter().enumerate() {
                let tip_info = get_chain_info(c);
                info!("Tip for miner {i}: {tip_info:?}");
                max_stacks_tip = std::cmp::max(tip_info.stacks_tip_height, max_stacks_tip);
            }

            if i >= reward_cycle_len - prepare_phase_len - 2 {
                signal_mining_blocked(miner_status[1].clone());
            }
        }
        signal_mining_ready(miner_status[1].clone());

        info!("####################### end of cycle ##############################");
        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
        }
        info!("####################### end of cycle ##############################");

        // miner 1 mines a prepare phase and confirms a hidden anchor block.
        // miner 0 is disabled for this prepare phase
        for i in 0..10 {
            eprintln!("\n\nBuild block {i}\n\n");
            btc_regtest_controller.build_next_block(1);
            sleep_ms(block_time_ms);

            for (i, c) in confs.iter().enumerate() {
                let tip_info = get_chain_info(c);
                info!("Tip for miner {i}: {tip_info:?}");
            }

            if i >= reward_cycle_len - prepare_phase_len - 2 {
                signal_mining_blocked(miner_status[0].clone());
            }
        }
        signal_mining_ready(miner_status[0].clone());

        info!("####################### end of cycle ##############################");
        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");

            // miner 1's history overtakes miner 0's.
            // Miner 1 didn't see cycle 22's anchor block, but it just mined an anchor block for cycle
            // 23 and affirmed cycle 22's anchor block's absence.
            max_stacks_tip = std::cmp::max(tip_info.stacks_tip_height, max_stacks_tip);
        }
        info!("####################### end of cycle ##############################");
    }

    // advance to start of next reward cycle
    eprintln!("\n\nBuild final block\n\n");
    btc_regtest_controller.build_next_block(1);
    sleep_ms(block_time_ms);

    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
    }

    // resume block propagation
    env::set_var("STACKS_HIDE_BLOCKS_AT_HEIGHT", "[]");

    // wait for all blocks to propagate
    // NOTE: the stacks affirmation maps will differ from the heaviest affirmation map, because the
    // act of flapping back and forth so much will have caused these nodes to forget about some of
    // their anchor blocks.  This is an artifact of the test.
    eprintln!("Wait for all blocks to propagate; stacks tip height is {max_stacks_tip}");
    wait_pox_stragglers(&confs, max_stacks_tip, block_time_ms);

    // nodes now agree on stacks affirmation map
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Final tip for miner {i}: {tip_info:?}");
    }
}

/// PoX reorg tests where two miners take turn mining hidden reward cycles.
/// Miners take turn mining entire reward cycles, and deny each other to build on them.
#[test]
#[ignore]
fn test_pox_reorg_flap_reward_cycles() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let num_miners = 2;

    let reward_cycle_len = 10;
    let prepare_phase_len = 3;
    let v1_unlock_height = 152;

    let (mut conf_template, _) = neon_integration_test_conf();
    let block_time_ms = 10_000;
    conf_template.node.mine_microblocks = true;
    conf_template.miner.microblock_attempt_time_ms = 2_000;
    conf_template.node.wait_time_for_microblocks = 0;
    conf_template.node.microblock_frequency = 0;
    conf_template.miner.first_attempt_time_ms = 2_000;
    conf_template.miner.subsequent_attempt_time_ms = 5_000;
    conf_template.burnchain.max_rbf = 1000000;
    conf_template.node.wait_time_for_blocks = 1_000;
    conf_template.burnchain.pox_2_activation = Some(v1_unlock_height);

    conf_template.node.require_affirmed_anchor_blocks = false;

    // make epoch 2.1 start in the middle of boot-up
    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = 101;
    epochs[StacksEpochId::Epoch2_05].start_height = 101;
    epochs[StacksEpochId::Epoch2_05].end_height = 151;
    epochs[StacksEpochId::Epoch21].start_height = 151;
    conf_template.burnchain.epochs = Some(epochs);

    let privks: Vec<_> = (0..5).map(|_| StacksPrivateKey::new()).collect();

    let stack_privks: Vec<_> = (0..5).map(|_| StacksPrivateKey::new()).collect();

    let balances: Vec<_> = privks
        .iter()
        .map(|privk| {
            let addr = to_addr(privk);
            InitialBalance {
                address: addr.into(),
                amount: 30_000_000,
            }
        })
        .collect();

    let stack_balances: Vec<_> = stack_privks
        .iter()
        .map(|privk| {
            let addr = to_addr(privk);
            InitialBalance {
                address: addr.into(),
                amount: 2_000_000_000_000_000,
            }
        })
        .collect();

    let mut confs = vec![];
    let mut burnchain_configs = vec![];
    let mut blocks_processed = vec![];
    let mut channels = vec![];
    let mut miner_status = vec![];

    for i in 0..num_miners {
        let seed = StacksPrivateKey::new().to_bytes();
        let (mut conf, _) = neon_integration_test_conf_with_seed(seed);

        conf.initial_balances.clear();
        conf.initial_balances.append(&mut balances.clone());
        conf.initial_balances.append(&mut stack_balances.clone());

        conf.node.mine_microblocks = conf_template.node.mine_microblocks;
        conf.miner.microblock_attempt_time_ms = conf_template.miner.microblock_attempt_time_ms;
        conf.node.wait_time_for_microblocks = conf_template.node.wait_time_for_microblocks;
        conf.node.microblock_frequency = conf_template.node.microblock_frequency;
        conf.miner.first_attempt_time_ms = conf_template.miner.first_attempt_time_ms;
        conf.miner.subsequent_attempt_time_ms = conf_template.miner.subsequent_attempt_time_ms;
        conf.node.wait_time_for_blocks = conf_template.node.wait_time_for_blocks;
        conf.burnchain.max_rbf = conf_template.burnchain.max_rbf;
        conf.burnchain.epochs = conf_template.burnchain.epochs.clone();
        conf.burnchain.pox_2_activation = conf_template.burnchain.pox_2_activation;
        conf.node.require_affirmed_anchor_blocks =
            conf_template.node.require_affirmed_anchor_blocks;

        // multiple nodes so they must download from each other
        conf.miner.wait_for_block_download = true;

        // nodes will selectively hide blocks from one another
        conf.node.fault_injection_hide_blocks = true;

        let rpc_port = 41123 + 10 * i;
        let p2p_port = 41123 + 10 * i + 1;
        conf.node.rpc_bind = format!("127.0.0.1:{rpc_port}");
        conf.node.data_url = format!("http://127.0.0.1:{rpc_port}");
        conf.node.p2p_bind = format!("127.0.0.1:{p2p_port}");

        confs.push(conf);
    }

    let node_privkey_1 = Secp256k1PrivateKey::from_seed(&confs[0].node.local_peer_seed);
    let chain_id = confs[0].burnchain.chain_id;
    let peer_version = confs[0].burnchain.peer_version;
    let p2p_bind = confs[0].node.p2p_bind.clone();
    for conf in confs.iter_mut().skip(1) {
        conf.node.set_bootstrap_nodes(
            format!(
                "{}@{p2p_bind}",
                &StacksPublicKey::from_private(&node_privkey_1).to_hex()
            ),
            chain_id,
            peer_version,
        );
    }

    // use short reward cycles
    for conf in confs.iter() {
        let mut burnchain_config = Burnchain::regtest(&conf.get_burn_db_path());
        let pox_constants = PoxConstants::new(
            reward_cycle_len,
            prepare_phase_len,
            4 * prepare_phase_len / 5,
            5,
            15,
            (1600 * reward_cycle_len - 1).into(),
            (1700 * reward_cycle_len).into(),
            v1_unlock_height,
            u32::MAX,
            u32::MAX,
            u32::MAX,
        );
        burnchain_config.pox_constants = pox_constants.clone();

        burnchain_configs.push(burnchain_config);
    }

    let mut btcd_controller = BitcoinCoreController::new(confs[0].clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::with_burnchain(
        confs[0].clone(),
        None,
        Some(burnchain_configs[0].clone()),
        None,
    );

    btc_regtest_controller.bootstrap_chain(1);

    // make sure all miners have BTC
    for conf in confs.iter().skip(1) {
        let old_mining_pubkey = btc_regtest_controller.get_mining_pubkey().unwrap();
        btc_regtest_controller
            .set_mining_pubkey(conf.burnchain.local_mining_public_key.clone().unwrap());
        btc_regtest_controller.bootstrap_chain(1);
        btc_regtest_controller.set_mining_pubkey(old_mining_pubkey);
    }

    btc_regtest_controller.bootstrap_chain((199 - num_miners) as u64);

    eprintln!("Chain bootstrapped...");

    for (i, burnchain_config) in burnchain_configs.into_iter().enumerate() {
        let mut run_loop = neon::RunLoop::new(confs[i].clone());
        let blocks_processed_arc = run_loop.get_blocks_processed_arc();
        let channel = run_loop.get_coordinator_channel().unwrap();
        let this_miner_status = run_loop.get_miner_status();

        blocks_processed.push(blocks_processed_arc);
        channels.push(channel);
        miner_status.push(this_miner_status);

        thread::spawn(move || run_loop.start(Some(burnchain_config), 0));
    }

    let http_origin = format!("http://{}", &confs[0].node.rpc_bind);

    // give the run loops some time to start up!
    for bp in &blocks_processed {
        wait_for_runloop(bp);
    }

    // activate miners
    eprintln!("\n\nBoot miner 0\n\n");
    loop {
        let tip_info_opt = get_chain_info_opt(&confs[0]);
        if let Some(tip_info) = tip_info_opt {
            eprintln!("\n\nMiner 0: {tip_info:?}\n\n");
            if tip_info.stacks_tip_height > 0 {
                break;
            }
        } else {
            eprintln!("\n\nWaiting for miner 0...\n\n");
        }
        next_block_and_iterate(
            &mut btc_regtest_controller,
            &blocks_processed[0],
            block_time_ms,
        );
    }

    for (i, conf) in confs.iter().enumerate().skip(1) {
        eprintln!("\n\nBoot miner {i}\n\n");
        loop {
            let tip_info_opt = get_chain_info_opt(conf);
            if let Some(tip_info) = tip_info_opt {
                eprintln!("\n\nMiner {i}: {tip_info:?}\n\n");
                if tip_info.stacks_tip_height > 0 {
                    break;
                }
            } else {
                eprintln!("\n\nWaiting for miner {i}...\n\n");
            }
            next_block_and_iterate(&mut btc_regtest_controller, &blocks_processed[i], 5_000);
        }
    }

    eprintln!("\n\nBegin transactions\n\n");

    let pox_pubkey = Secp256k1PublicKey::from_hex(
        "02f006a09b59979e2cb8449f58076152af6b124aa29b948a3714b8d5f15aa94ede",
    )
    .unwrap();
    let pox_pubkey_hash = bytes_to_hex(&Hash160::from_node_public_key(&pox_pubkey).to_bytes());

    let sort_height = channels[0].get_sortitions_processed();

    // make everyone stack
    let stacking_txs: Vec<_> = stack_privks
        .iter()
        .map(|pk| {
            make_contract_call(
                pk,
                0,
                1360,
                conf_template.burnchain.chain_id,
                &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
                "pox-2",
                "stack-stx",
                &[
                    Value::UInt(2_000_000_000_000_000 - 30_000_000),
                    execute(
                        &format!("{{ hashbytes: 0x{pox_pubkey_hash}, version: 0x00 }}"),
                        ClarityVersion::Clarity1,
                    )
                    .unwrap()
                    .unwrap(),
                    Value::UInt((sort_height + 1) as u128),
                    Value::UInt(12),
                ],
            )
        })
        .collect();

    // keeps the mempool full, and makes it so miners will spend a nontrivial amount of time
    // building blocks
    let all_txs: Vec<_> = privks
        .iter()
        .enumerate()
        .map(|(i, pk)| {
            make_random_tx_chain(pk, (25 * i) as u64, conf_template.burnchain.chain_id, false)
        })
        .collect();

    // everyone locks up
    for (cnt, tx) in stacking_txs.iter().enumerate() {
        eprintln!("\n\nSubmit stacking tx {cnt}\n\n");
        submit_tx(&http_origin, tx);
    }

    // run a reward cycle
    let mut at_220 = false;
    while !at_220 {
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
            if tip_info.burn_block_height == 220 {
                at_220 = true;
            }
        }
    }

    // blast out the rest
    let mut cnt = 0;
    for tx_chain in all_txs {
        for tx in tx_chain {
            eprintln!("\n\nSubmit tx {cnt}\n\n");
            submit_tx(&http_origin, &tx);
            cnt += 1;
        }
    }

    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
        assert!(tip_info.burn_block_height <= 220);
    }

    eprintln!("\n\nBegin mining\n\n");

    info!("####################### end of cycle ##############################");
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
    }
    info!("####################### end of cycle ##############################");

    // prevent Stacks at these heights from propagating.
    // This means that both nodes affirm the absence of each others' anchor blocks.
    env::set_var(
        "STACKS_HIDE_BLOCKS_AT_HEIGHT",
        "[220,221,222,223,224,225,226,227,228,229,230,231,232,232,234,235,236,237,238,239,240,241,242,243,244,245,246,247,248,249,250,251,252,253,254,255,256,257,258,259,260,261,262,263,264,265,266,267,268,269,270,271,272,273,274,275,276,277,278,279,280,281,282,283,284,285,286,287,288,289,290,291,292,293,294,295,296,297,298,299,300,301]"
    );

    let mut max_stacks_tip = 0;

    // miners 0 and 1 take turns mining anchor blocks.
    // this should cause them both to flip/flop their sortition histories multiple times
    for _c in 0..2 {
        // miner 0 mines two reward cycles and confirms a hidden anchor block.
        // miner 1 is disabled for this reward cycle
        signal_mining_blocked(miner_status[1].clone());
        for i in 0..20 {
            eprintln!("\n\nBuild block {i}\n\n");
            btc_regtest_controller.build_next_block(1);
            sleep_ms(block_time_ms);

            for (i, c) in confs.iter().enumerate() {
                let tip_info = get_chain_info(c);
                info!("Tip for miner {i}: {tip_info:?}");
                max_stacks_tip = std::cmp::max(tip_info.stacks_tip_height, max_stacks_tip);
            }
        }
        signal_mining_ready(miner_status[1].clone());

        info!("####################### end of cycle ##############################");
        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
        }
        info!("####################### end of cycle ##############################");

        // miner 1 mines two reward cycles and confirms a hidden anchor block.
        // miner 0 is disabled for this reward cycle
        signal_mining_blocked(miner_status[0].clone());
        for i in 0..20 {
            eprintln!("\n\nBuild block {i}\n\n");
            btc_regtest_controller.build_next_block(1);
            sleep_ms(block_time_ms);

            for (i, c) in confs.iter().enumerate() {
                let tip_info = get_chain_info(c);
                info!("Tip for miner {i}: {tip_info:?}");
            }
        }
        signal_mining_ready(miner_status[0].clone());

        info!("####################### end of cycle ##############################");
        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");

            // miner 1's history overtakes miner 0's.
            // Miner 1 didn't see cycle 22's anchor block, but it just mined an anchor block for cycle
            // 23 and affirmed cycle 22's anchor block's absence.
            max_stacks_tip = std::cmp::max(tip_info.stacks_tip_height, max_stacks_tip);
        }
        info!("####################### end of cycle ##############################");
    }

    // advance to start of next reward cycle
    eprintln!("\n\nBuild final block\n\n");
    btc_regtest_controller.build_next_block(1);
    sleep_ms(block_time_ms);

    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
    }

    // resume block propagation
    env::set_var("STACKS_HIDE_BLOCKS_AT_HEIGHT", "[]");

    // wait for all blocks to propagate
    // NOTE: the stacks affirmation maps will differ from the heaviest affirmation map, because the
    // act of flapping back and forth so much will have caused these nodes to forget about some of
    // their anchor blocks.  This is an artifact of the test.
    eprintln!("Wait for all blocks to propagate; stacks tip height is {max_stacks_tip}");
    wait_pox_stragglers(&confs, max_stacks_tip, block_time_ms);

    // nodes now agree on stacks affirmation map
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Final tip for miner {i}: {tip_info:?}");
    }
}

/// Make sure the node can boot despite missing a ton of anchor blocks.
/// Miner 0 mines anchor blocks for cycles 22, 23, 24, 25, 26 and hides them.
/// Miner 1 doesn't see them until the start of cycle 27
/// The test verifies that miner 1 is still able to sync with miner 0, despite having mined in the
/// absence of these anchor blocks while miner 1 was hiding blocks.
#[test]
#[ignore]
fn test_pox_missing_five_anchor_blocks() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let num_miners = 2;

    let reward_cycle_len = 10;
    let prepare_phase_len = 3;
    let v1_unlock_height = 152;

    let (mut conf_template, _) = neon_integration_test_conf();
    let block_time_ms = 10_000;
    conf_template.node.mine_microblocks = true;
    conf_template.miner.microblock_attempt_time_ms = 2_000;
    conf_template.node.wait_time_for_microblocks = 0;
    conf_template.node.microblock_frequency = 0;
    conf_template.miner.first_attempt_time_ms = 2_000;
    conf_template.miner.subsequent_attempt_time_ms = 5_000;
    conf_template.burnchain.max_rbf = 1000000;
    conf_template.node.wait_time_for_blocks = 1_000;
    conf_template.burnchain.pox_2_activation = Some(v1_unlock_height);

    conf_template.node.require_affirmed_anchor_blocks = false;

    // make epoch 2.1 start in the middle of boot-up
    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = 101;
    epochs[StacksEpochId::Epoch2_05].start_height = 101;
    epochs[StacksEpochId::Epoch2_05].end_height = 151;
    epochs[StacksEpochId::Epoch21].start_height = 151;
    conf_template.burnchain.epochs = Some(epochs);

    let privks: Vec<_> = (0..5).map(|_| StacksPrivateKey::new()).collect();

    let stack_privks: Vec<_> = (0..5).map(|_| StacksPrivateKey::new()).collect();

    let balances: Vec<_> = privks
        .iter()
        .map(|privk| {
            let addr = to_addr(privk);
            InitialBalance {
                address: addr.into(),
                amount: 30_000_000,
            }
        })
        .collect();

    let stack_balances: Vec<_> = stack_privks
        .iter()
        .map(|privk| {
            let addr = to_addr(privk);
            InitialBalance {
                address: addr.into(),
                amount: 2_000_000_000_000_000,
            }
        })
        .collect();

    let mut confs = vec![];
    let mut burnchain_configs = vec![];
    let mut blocks_processed = vec![];
    let mut channels = vec![];
    let mut miner_status = vec![];

    for i in 0..num_miners {
        let seed = StacksPrivateKey::new().to_bytes();
        let (mut conf, _) = neon_integration_test_conf_with_seed(seed);

        conf.initial_balances.clear();
        conf.initial_balances.append(&mut balances.clone());
        conf.initial_balances.append(&mut stack_balances.clone());

        conf.node.mine_microblocks = conf_template.node.mine_microblocks;
        conf.miner.microblock_attempt_time_ms = conf_template.miner.microblock_attempt_time_ms;
        conf.node.wait_time_for_microblocks = conf_template.node.wait_time_for_microblocks;
        conf.node.microblock_frequency = conf_template.node.microblock_frequency;
        conf.miner.first_attempt_time_ms = conf_template.miner.first_attempt_time_ms;
        conf.miner.subsequent_attempt_time_ms = conf_template.miner.subsequent_attempt_time_ms;
        conf.node.wait_time_for_blocks = conf_template.node.wait_time_for_blocks;
        conf.burnchain.max_rbf = conf_template.burnchain.max_rbf;
        conf.burnchain.epochs = conf_template.burnchain.epochs.clone();
        conf.burnchain.pox_2_activation = conf_template.burnchain.pox_2_activation;
        conf.node.require_affirmed_anchor_blocks =
            conf_template.node.require_affirmed_anchor_blocks;

        // multiple nodes so they must download from each other
        conf.miner.wait_for_block_download = true;

        // nodes will selectively hide blocks from one another
        conf.node.fault_injection_hide_blocks = true;

        let rpc_port = 41103 + 10 * i;
        let p2p_port = 41103 + 10 * i + 1;
        conf.node.rpc_bind = format!("127.0.0.1:{rpc_port}");
        conf.node.data_url = format!("http://127.0.0.1:{rpc_port}");
        conf.node.p2p_bind = format!("127.0.0.1:{p2p_port}");

        confs.push(conf);
    }

    let node_privkey_1 = Secp256k1PrivateKey::from_seed(&confs[0].node.local_peer_seed);
    let chain_id = confs[0].burnchain.chain_id;
    let peer_version = confs[0].burnchain.peer_version;
    let p2p_bind = confs[0].node.p2p_bind.clone();
    for conf in confs.iter_mut().skip(1) {
        conf.node.set_bootstrap_nodes(
            format!(
                "{}@{p2p_bind}",
                &StacksPublicKey::from_private(&node_privkey_1).to_hex()
            ),
            chain_id,
            peer_version,
        );
    }

    // use short reward cycles
    for conf in &confs {
        let mut burnchain_config = Burnchain::regtest(&conf.get_burn_db_path());
        let pox_constants = PoxConstants::new(
            reward_cycle_len,
            prepare_phase_len,
            4 * prepare_phase_len / 5,
            5,
            15,
            (1600 * reward_cycle_len - 1).into(),
            (1700 * reward_cycle_len).into(),
            v1_unlock_height,
            u32::MAX,
            u32::MAX,
            u32::MAX,
        );
        burnchain_config.pox_constants = pox_constants.clone();

        burnchain_configs.push(burnchain_config);
    }

    let mut btcd_controller = BitcoinCoreController::new(confs[0].clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::with_burnchain(
        confs[0].clone(),
        None,
        Some(burnchain_configs[0].clone()),
        None,
    );

    btc_regtest_controller.bootstrap_chain(1);

    // make sure all miners have BTC
    for conf in confs.iter().skip(1) {
        let old_mining_pubkey = btc_regtest_controller.get_mining_pubkey().unwrap();
        btc_regtest_controller
            .set_mining_pubkey(conf.burnchain.local_mining_public_key.clone().unwrap());
        btc_regtest_controller.bootstrap_chain(1);
        btc_regtest_controller.set_mining_pubkey(old_mining_pubkey);
    }

    btc_regtest_controller.bootstrap_chain((199 - num_miners) as u64);

    eprintln!("Chain bootstrapped...");

    for (i, burnchain_config) in burnchain_configs.into_iter().enumerate() {
        let mut run_loop = neon::RunLoop::new(confs[i].clone());
        let blocks_processed_arc = run_loop.get_blocks_processed_arc();
        let channel = run_loop.get_coordinator_channel().unwrap();
        let this_miner_status = run_loop.get_miner_status();

        blocks_processed.push(blocks_processed_arc);
        channels.push(channel);
        miner_status.push(this_miner_status);

        thread::spawn(move || run_loop.start(Some(burnchain_config), 0));
    }

    let http_origin = format!("http://{}", &confs[0].node.rpc_bind);

    // give the run loops some time to start up!
    for bp in &blocks_processed {
        wait_for_runloop(bp);
    }

    // activate miners
    eprintln!("\n\nBoot miner 0\n\n");
    loop {
        let tip_info_opt = get_chain_info_opt(&confs[0]);
        if let Some(tip_info) = tip_info_opt {
            eprintln!("\n\nMiner 0: {tip_info:?}\n\n");
            if tip_info.stacks_tip_height > 0 {
                break;
            }
        } else {
            eprintln!("\n\nWaiting for miner 0...\n\n");
        }
        next_block_and_iterate(
            &mut btc_regtest_controller,
            &blocks_processed[0],
            block_time_ms,
        );
    }

    for (i, conf) in confs.iter().enumerate().skip(1) {
        eprintln!("\n\nBoot miner {i}\n\n");
        loop {
            let tip_info_opt = get_chain_info_opt(conf);
            if let Some(tip_info) = tip_info_opt {
                eprintln!("\n\nMiner {i}: {tip_info:?}\n\n");
                if tip_info.stacks_tip_height > 0 {
                    break;
                }
            } else {
                eprintln!("\n\nWaiting for miner {i}...\n\n");
            }
            next_block_and_iterate(&mut btc_regtest_controller, &blocks_processed[i], 5_000);
        }
    }

    eprintln!("\n\nBegin transactions\n\n");

    let pox_pubkey = Secp256k1PublicKey::from_hex(
        "02f006a09b59979e2cb8449f58076152af6b124aa29b948a3714b8d5f15aa94ede",
    )
    .unwrap();
    let pox_pubkey_hash = bytes_to_hex(&Hash160::from_node_public_key(&pox_pubkey).to_bytes());

    let sort_height = channels[0].get_sortitions_processed();

    // make everyone stack
    let stacking_txs: Vec<_> = stack_privks
        .iter()
        .map(|pk| {
            make_contract_call(
                pk,
                0,
                1360,
                conf_template.burnchain.chain_id,
                &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
                "pox-2",
                "stack-stx",
                &[
                    Value::UInt(2_000_000_000_000_000 - 30_000_000),
                    execute(
                        &format!("{{ hashbytes: 0x{pox_pubkey_hash}, version: 0x00 }}"),
                        ClarityVersion::Clarity1,
                    )
                    .unwrap()
                    .unwrap(),
                    Value::UInt((sort_height + 1) as u128),
                    Value::UInt(12),
                ],
            )
        })
        .collect();

    // keeps the mempool full, and makes it so miners will spend a nontrivial amount of time
    // building blocks
    let all_txs: Vec<_> = privks
        .iter()
        .enumerate()
        .map(|(i, pk)| {
            make_random_tx_chain(pk, (25 * i) as u64, conf_template.burnchain.chain_id, false)
        })
        .collect();

    // everyone locks up
    for (cnt, tx) in stacking_txs.iter().enumerate() {
        eprintln!("\n\nSubmit stacking tx {cnt}\n\n");
        submit_tx(&http_origin, tx);
    }

    // run a reward cycle
    let mut at_220 = false;
    while !at_220 {
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
            if tip_info.burn_block_height == 220 {
                at_220 = true;
            }
        }
    }

    // blast out the rest
    let mut cnt = 0;
    for tx_chain in all_txs {
        for tx in tx_chain {
            eprintln!("\n\nSubmit tx {cnt}\n\n");
            submit_tx(&http_origin, &tx);
            cnt += 1;
        }
    }

    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
        assert!(tip_info.burn_block_height <= 220);
    }

    eprintln!("\n\nBegin mining\n\n");

    info!("####################### end of cycle ##############################");
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
    }
    info!("####################### end of cycle ##############################");

    // prevent Stacks at these heights from propagating
    env::set_var(
        "STACKS_HIDE_BLOCKS_AT_HEIGHT",
        "[226,227,228,229,230,236,237,238,239,240,246,247,248,249,250,256,257,258,259,260,266,267,268,269,270,276,277,278,279,280,286,287,288,289,290]"
    );

    let mut max_stacks_tip = 0;
    for c in 0..5 {
        // miner 0 mines a prepare phase and confirms a hidden anchor block.
        // miner 1 is disabled for these prepare phases
        for i in 0..10 {
            eprintln!("\n\nBuild block {i} cycle {c}\n\n");
            btc_regtest_controller.build_next_block(1);
            sleep_ms(block_time_ms);

            for (i, c) in confs.iter().enumerate() {
                let tip_info = get_chain_info(c);
                info!("Tip for miner {i}: {tip_info:?}");
            }

            if i >= reward_cycle_len - prepare_phase_len - 2 {
                signal_mining_blocked(miner_status[1].clone());
            }
        }
        signal_mining_ready(miner_status[1].clone());
        info!("####################### end of cycle ##############################");
        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
            max_stacks_tip = std::cmp::max(tip_info.stacks_tip_height, max_stacks_tip);
        }
        info!("####################### end of cycle ##############################");
    }

    // advance to start of next reward cycle
    eprintln!("\n\nBuild final block\n\n");
    btc_regtest_controller.build_next_block(1);
    sleep_ms(block_time_ms);

    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
    }

    // resume block propagation
    env::set_var("STACKS_HIDE_BLOCKS_AT_HEIGHT", "[]");

    // wait for all blocks to propagate.
    // miner 1 should learn about all of miner 0's blocks
    info!("Wait for all blocks to propagate; stacks tip height is {max_stacks_tip}",);
    wait_pox_stragglers(&confs, max_stacks_tip, block_time_ms);

    // nodes now agree on stacks affirmation map
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Final tip for miner {i}: {tip_info:?}");
    }
}

#[test]
#[ignore]
/// Verify that if the sortition AM declares that an anchor block is present in epoch 2.05, but the
/// heaviest AM declares it absent, that this is _not_ treated as a divergence of this behavior
/// manifests in epoch 2.05.
fn test_sortition_divergence_pre_21() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let num_miners = 2;

    let reward_cycle_len = 10;
    let prepare_phase_len = 5;
    let v1_unlock_height = 242;

    let (mut conf_template, _) = neon_integration_test_conf();
    let block_time_ms = 10_000;
    conf_template.node.mine_microblocks = true;
    conf_template.miner.microblock_attempt_time_ms = 2_000;
    conf_template.node.wait_time_for_microblocks = 0;
    conf_template.node.microblock_frequency = 0;
    conf_template.miner.first_attempt_time_ms = 2_000;
    conf_template.miner.subsequent_attempt_time_ms = 5_000;
    conf_template.burnchain.max_rbf = 1000000;
    conf_template.node.wait_time_for_blocks = 1_000;
    conf_template.burnchain.pox_2_activation = Some(v1_unlock_height);

    conf_template.node.require_affirmed_anchor_blocks = false;
    conf_template.node.always_use_affirmation_maps = false;

    // make epoch 2.1 start after we have created this error condition
    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = 101;
    epochs[StacksEpochId::Epoch2_05].start_height = 101;
    epochs[StacksEpochId::Epoch2_05].end_height = 241;
    epochs[StacksEpochId::Epoch21].start_height = 241;
    conf_template.burnchain.epochs = Some(epochs);

    let privks: Vec<_> = (0..5).map(|_| StacksPrivateKey::new()).collect();

    let stack_privks: Vec<_> = (0..5).map(|_| StacksPrivateKey::new()).collect();

    let balances: Vec<_> = privks
        .iter()
        .map(|privk| {
            let addr = to_addr(privk);
            InitialBalance {
                address: addr.into(),
                amount: 30_000_000,
            }
        })
        .collect();

    let stack_balances: Vec<_> = stack_privks
        .iter()
        .map(|privk| {
            let addr = to_addr(privk);
            InitialBalance {
                address: addr.into(),
                amount: 2_000_000_000_000_000,
            }
        })
        .collect();

    let mut confs = vec![];
    let mut burnchain_configs = vec![];
    let mut blocks_processed = vec![];
    let mut channels = vec![];
    let mut miner_status = vec![];

    for i in 0..num_miners {
        let seed = StacksPrivateKey::new().to_bytes();
        let (mut conf, _) = neon_integration_test_conf_with_seed(seed);

        conf.initial_balances.clear();
        conf.initial_balances.append(&mut balances.clone());
        conf.initial_balances.append(&mut stack_balances.clone());

        conf.node.mine_microblocks = conf_template.node.mine_microblocks;
        conf.miner.microblock_attempt_time_ms = conf_template.miner.microblock_attempt_time_ms;
        conf.node.wait_time_for_microblocks = conf_template.node.wait_time_for_microblocks;
        conf.node.microblock_frequency = conf_template.node.microblock_frequency;
        conf.miner.first_attempt_time_ms = conf_template.miner.first_attempt_time_ms;
        conf.miner.subsequent_attempt_time_ms = conf_template.miner.subsequent_attempt_time_ms;
        conf.node.wait_time_for_blocks = conf_template.node.wait_time_for_blocks;
        conf.burnchain.max_rbf = conf_template.burnchain.max_rbf;
        conf.burnchain.epochs = conf_template.burnchain.epochs.clone();
        conf.burnchain.pox_2_activation = conf_template.burnchain.pox_2_activation;
        conf.node.require_affirmed_anchor_blocks =
            conf_template.node.require_affirmed_anchor_blocks;

        conf.node.always_use_affirmation_maps = false;

        // multiple nodes so they must download from each other
        conf.miner.wait_for_block_download = true;

        // nodes will selectively hide blocks from one another
        conf.node.fault_injection_hide_blocks = true;

        conf.connection_options.inv_sync_interval = 6;

        let rpc_port = 41113 + 10 * i;
        let p2p_port = 41113 + 10 * i + 1;
        conf.node.rpc_bind = format!("127.0.0.1:{rpc_port}");
        conf.node.data_url = format!("http://127.0.0.1:{rpc_port}");
        conf.node.p2p_bind = format!("127.0.0.1:{p2p_port}");

        confs.push(conf);
    }

    let node_privkey_1 = Secp256k1PrivateKey::from_seed(&confs[0].node.local_peer_seed);
    let chain_id = confs[0].burnchain.chain_id;
    let peer_version = confs[0].burnchain.peer_version;
    let p2p_bind = confs[0].node.p2p_bind.clone();
    for conf in confs.iter_mut().skip(1) {
        conf.node.set_bootstrap_nodes(
            format!(
                "{}@{p2p_bind}",
                &StacksPublicKey::from_private(&node_privkey_1).to_hex()
            ),
            chain_id,
            peer_version,
        );
    }

    // use short reward cycles
    for conf in &confs {
        let mut burnchain_config = Burnchain::regtest(&conf.get_burn_db_path());
        let pox_constants = PoxConstants::new(
            reward_cycle_len,
            prepare_phase_len,
            3,
            5,
            15,
            (1600 * reward_cycle_len - 1).into(),
            (1700 * reward_cycle_len).into(),
            v1_unlock_height,
            u32::MAX,
            u32::MAX,
            u32::MAX,
        );
        burnchain_config.pox_constants = pox_constants.clone();

        burnchain_configs.push(burnchain_config);
    }

    let mut btcd_controller = BitcoinCoreController::new(confs[0].clone());
    btcd_controller
        .start_bitcoind()
        .map_err(|_e| ())
        .expect("Failed starting bitcoind");

    let mut btc_regtest_controller = BitcoinRegtestController::with_burnchain(
        confs[0].clone(),
        None,
        Some(burnchain_configs[0].clone()),
        None,
    );

    btc_regtest_controller.bootstrap_chain(1);

    // make sure all miners have BTC
    for conf in confs.iter().skip(1) {
        let old_mining_pubkey = btc_regtest_controller.get_mining_pubkey().unwrap();
        btc_regtest_controller
            .set_mining_pubkey(conf.burnchain.local_mining_public_key.clone().unwrap());
        btc_regtest_controller.bootstrap_chain(1);
        btc_regtest_controller.set_mining_pubkey(old_mining_pubkey);
    }

    btc_regtest_controller.bootstrap_chain((199 - num_miners) as u64);

    eprintln!("Chain bootstrapped...");

    for (i, burnchain_config) in burnchain_configs.into_iter().enumerate() {
        let mut run_loop = neon::RunLoop::new(confs[i].clone());
        let blocks_processed_arc = run_loop.get_blocks_processed_arc();
        let channel = run_loop.get_coordinator_channel().unwrap();
        let this_miner_status = run_loop.get_miner_status();

        blocks_processed.push(blocks_processed_arc);
        channels.push(channel);
        miner_status.push(this_miner_status);

        thread::spawn(move || run_loop.start(Some(burnchain_config), 0));
    }

    let http_origin = format!("http://{}", &confs[0].node.rpc_bind);

    // give the run loops some time to start up!
    for bp in &blocks_processed {
        wait_for_runloop(bp);
    }

    // activate miners
    eprintln!("\n\nBoot miner 0\n\n");
    loop {
        let tip_info_opt = get_chain_info_opt(&confs[0]);
        if let Some(tip_info) = tip_info_opt {
            eprintln!("\n\nMiner 0: {tip_info:?}\n\n");
            if tip_info.stacks_tip_height > 0 {
                break;
            }
        } else {
            eprintln!("\n\nWaiting for miner 0...\n\n");
        }
        next_block_and_iterate(
            &mut btc_regtest_controller,
            &blocks_processed[0],
            block_time_ms,
        );
    }

    for (i, conf) in confs.iter().enumerate().skip(1) {
        eprintln!("\n\nBoot miner {i}\n\n");
        loop {
            let tip_info_opt = get_chain_info_opt(conf);
            if let Some(tip_info) = tip_info_opt {
                eprintln!("\n\nMiner {i}: {tip_info:?}\n\n");
                if tip_info.stacks_tip_height > 0 {
                    break;
                }
            } else {
                eprintln!("\n\nWaiting for miner {i}...\n\n");
            }
            next_block_and_iterate(&mut btc_regtest_controller, &blocks_processed[i], 5_000);
        }
    }

    eprintln!("\n\nBegin transactions\n\n");

    let pox_pubkey = Secp256k1PublicKey::from_hex(
        "02f006a09b59979e2cb8449f58076152af6b124aa29b948a3714b8d5f15aa94ede",
    )
    .unwrap();
    let pox_pubkey_hash = bytes_to_hex(&Hash160::from_node_public_key(&pox_pubkey).to_bytes());

    let sort_height = channels[0].get_sortitions_processed();

    // make everyone stack
    let stacking_txs: Vec<_> = stack_privks
        .iter()
        .map(|pk| {
            make_contract_call(
                pk,
                0,
                1360,
                conf_template.burnchain.chain_id,
                &StacksAddress::from_string("ST000000000000000000002AMW42H").unwrap(),
                "pox",
                "stack-stx",
                &[
                    Value::UInt(2_000_000_000_000_000 - 30_000_000),
                    execute(
                        &format!("{{ hashbytes: 0x{pox_pubkey_hash}, version: 0x00 }}"),
                        ClarityVersion::Clarity1,
                    )
                    .unwrap()
                    .unwrap(),
                    Value::UInt((sort_height + 1) as u128),
                    Value::UInt(12),
                ],
            )
        })
        .collect();

    // keeps the mempool full, and makes it so miners will spend a nontrivial amount of time
    // building blocks
    let all_txs: Vec<_> = privks
        .iter()
        .enumerate()
        .map(|(i, pk)| {
            make_random_tx_chain(pk, (25 * i) as u64, conf_template.burnchain.chain_id, false)
        })
        .collect();

    // everyone locks up
    for (cnt, tx) in stacking_txs.iter().enumerate() {
        eprintln!("\n\nSubmit stacking tx {cnt}\n\n");
        submit_tx(&http_origin, tx);
    }

    // run a reward cycle
    let mut at_220 = false;
    while !at_220 {
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
            if tip_info.burn_block_height == 220 {
                at_220 = true;
            }
        }
    }

    // blast out the rest
    let mut cnt = 0;
    for tx_chain in all_txs {
        for tx in tx_chain {
            eprintln!("\n\nSubmit tx {cnt}\n\n");
            submit_tx(&http_origin, &tx);
            cnt += 1;
        }
    }

    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
        assert!(tip_info.burn_block_height <= 220);
    }

    eprintln!("\n\nBegin mining\n\n");

    info!("####################### end of cycle ##############################");
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
    }
    info!("####################### end of cycle ##############################");

    let mut max_stacks_tip = 0;

    // prevent Stacks at these heights from propagating
    env::set_var(
        "STACKS_HIDE_BLOCKS_AT_HEIGHT",
        "[223,224,225,226,227,228,229,230]",
    );

    // mine a reward cycle in which the 2.05 rules choose a PoX anchor block, but the 2.1 rules do
    // not.
    for i in 0..10 {
        eprintln!("\n\nBuild block {i}\n\n");
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
        }

        if i >= reward_cycle_len - prepare_phase_len && i < reward_cycle_len - prepare_phase_len + 3
        {
            // only miner 0 mines.
            signal_mining_ready(miner_status[0].clone());
            signal_mining_blocked(miner_status[1].clone());
        } else if i >= reward_cycle_len - prepare_phase_len + 3 && i < reward_cycle_len {
            // only miner 1 mines, and they mine *hard*.
            signal_mining_blocked(miner_status[0].clone());
            signal_mining_ready(miner_status[1].clone());
            set_mining_spend_amount(
                miner_status[1].clone(),
                10 * conf_template.burnchain.burn_fee_cap,
            );
        }
    }

    signal_mining_ready(miner_status[0].clone());
    signal_mining_ready(miner_status[1].clone());
    set_mining_spend_amount(
        miner_status[1].clone(),
        conf_template.burnchain.burn_fee_cap,
    );

    info!("####################### end of cycle ##############################");
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
        max_stacks_tip = std::cmp::max(tip_info.stacks_tip_height, max_stacks_tip);
    }
    info!("####################### end of cycle ##############################");

    for i in 0..10 {
        eprintln!("\n\nBuild block {i}\n\n");
        btc_regtest_controller.build_next_block(1);
        sleep_ms(block_time_ms);

        for (i, c) in confs.iter().enumerate() {
            let tip_info = get_chain_info(c);
            info!("Tip for miner {i}: {tip_info:?}");
        }
    }

    info!("####################### end of cycle ##############################");
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
        max_stacks_tip = std::cmp::max(tip_info.stacks_tip_height, max_stacks_tip);
    }
    info!("####################### end of cycle ##############################");

    // run some cycles in 2.1
    for _ in 0..2 {
        for i in 0..10 {
            eprintln!("\n\nBuild block {i}\n\n");
            btc_regtest_controller.build_next_block(1);
            sleep_ms(block_time_ms);

            for (i, c) in confs.iter().enumerate() {
                let tip_info = get_chain_info(c);
                info!("Tip for miner {i}: {tip_info:?}");
            }
        }
    }

    // advance to start of next reward cycle
    eprintln!("\n\nBuild final block\n\n");
    btc_regtest_controller.build_next_block(1);
    sleep_ms(block_time_ms);

    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Tip for miner {i}: {tip_info:?}");
    }

    env::set_var("STACKS_HIDE_BLOCKS_AT_HEIGHT", "[]");

    // wait for all blocks to propagate.
    // miner 1 should learn about all of miner 0's blocks
    info!("Wait for all blocks to propagate; stacks tip height is {max_stacks_tip}");
    wait_pox_stragglers(&confs, max_stacks_tip, block_time_ms);

    // nodes now agree on stacks affirmation map
    for (i, c) in confs.iter().enumerate() {
        let tip_info = get_chain_info(c);
        info!("Final tip for miner {i}: {tip_info:?}");
    }
}

#[test]
#[ignore]
/// test to verify that a 2.05 contract which use a pre-2.1 trait
///  can be invoked by a post-2.1 contract through *static* and *dynamic* invocation
fn trait_invocation_cross_epoch() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let spender_sk = StacksPrivateKey::new();
    let spender_addr = PrincipalData::from(to_addr(&spender_sk));
    let spender_addr_c32 = to_addr(&spender_sk);

    let trait_contract = "(define-trait simple-method ((foo (uint) (response uint uint)) ))";
    let impl_contract =
        "(impl-trait .simple-trait.simple-method) (define-read-only (foo (x uint)) (ok x))";
    let use_contract = "(use-trait simple .simple-trait.simple-method)
                        (define-public (call-simple (s <simple>)) (contract-call? s foo u0))";
    let invoke_contract = "
        (use-trait simple .simple-trait.simple-method)
        (define-public (invocation-1)
          (contract-call? .use-simple call-simple .impl-simple))
        (define-public (invocation-2 (st <simple>))
          (contract-call? .use-simple call-simple st))
    ";

    let epoch_2_05 = 210;
    let epoch_2_1 = 215;

    test_observer::spawn();

    let (mut conf, _) = neon_integration_test_conf();
    let mut initial_balances = vec![InitialBalance {
        address: spender_addr.clone(),
        amount: 200_000_000,
    }];
    conf.initial_balances.append(&mut initial_balances);
    test_observer::register_any(&mut conf);
    let mut epochs = EpochList::new(&*core::STACKS_EPOCHS_REGTEST);
    epochs[StacksEpochId::Epoch20].end_height = epoch_2_05;
    epochs[StacksEpochId::Epoch2_05].start_height = epoch_2_05;
    epochs[StacksEpochId::Epoch2_05].end_height = epoch_2_1;
    epochs[StacksEpochId::Epoch21].start_height = epoch_2_1;
    conf.burnchain.epochs = Some(epochs);

    let mut burnchain_config = Burnchain::regtest(&conf.get_burn_db_path());

    let reward_cycle_len = 2000;
    let prepare_phase_len = 100;
    let pox_constants = PoxConstants::new(
        reward_cycle_len,
        prepare_phase_len,
        4 * prepare_phase_len / 5,
        5,
        15,
        (16 * reward_cycle_len - 1).into(),
        (17 * reward_cycle_len).into(),
        u32::MAX,
        u32::MAX,
        u32::MAX,
        u32::MAX,
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

    // bitcoin chain starts at epoch 2.05 boundary
    btc_regtest_controller.bootstrap_chain(epoch_2_05 - 5);

    eprintln!("Chain bootstrapped...");

    let mut run_loop = neon::RunLoop::new(conf.clone());
    let blocks_processed = run_loop.get_blocks_processed_arc();
    let channel = run_loop.get_coordinator_channel().unwrap();

    let runloop_burnchain = burnchain_config.clone();
    thread::spawn(move || run_loop.start(Some(runloop_burnchain), 0));

    // give the run loop some time to start up!
    wait_for_runloop(&blocks_processed);

    // first block wakes up the run loop
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    let tip_info = get_chain_info(&conf);
    assert_eq!(tip_info.burn_block_height, epoch_2_05 - 4);

    // first block will hold our VRF registration
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // second block will be the first mined Stacks block
    next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);

    // cross the epoch 2.05 boundary
    for _i in 0..3 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    let tip_info = get_chain_info(&conf);
    assert_eq!(tip_info.burn_block_height, epoch_2_05 + 1);

    let tx = make_contract_publish(
        &spender_sk,
        0,
        10_000,
        conf.burnchain.chain_id,
        "simple-trait",
        trait_contract,
    );
    let trait_txid = submit_tx(&http_origin, &tx);

    let tx = make_contract_publish(
        &spender_sk,
        1,
        10_000,
        conf.burnchain.chain_id,
        "impl-simple",
        impl_contract,
    );
    let impl_txid = submit_tx(&http_origin, &tx);

    let tx = make_contract_publish(
        &spender_sk,
        2,
        10_000,
        conf.burnchain.chain_id,
        "use-simple",
        use_contract,
    );
    let use_txid = submit_tx(&http_origin, &tx);

    // mine the transactions and advance to epoch 2.1
    for _ in 0..5 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    let tip_info = get_chain_info(&conf);
    assert_eq!(tip_info.burn_block_height, epoch_2_1 + 1);

    let tx = make_contract_publish(
        &spender_sk,
        3,
        10_000,
        conf.burnchain.chain_id,
        "invoke-simple",
        invoke_contract,
    );
    let invoke_txid = submit_tx(&http_origin, &tx);

    for _ in 0..2 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    let tx = make_contract_call(
        &spender_sk,
        4,
        10_000,
        conf.burnchain.chain_id,
        &spender_addr_c32,
        "invoke-simple",
        "invocation-1",
        &[],
    );
    let invoke_1_txid = submit_tx(&http_origin, &tx);

    let tx = make_contract_call(
        &spender_sk,
        5,
        10_000,
        conf.burnchain.chain_id,
        &spender_addr_c32,
        "invoke-simple",
        "invocation-2",
        &[Value::Principal(PrincipalData::Contract(
            QualifiedContractIdentifier::parse(&format!("{spender_addr_c32}.impl-simple")).unwrap(),
        ))],
    );
    let invoke_2_txid = submit_tx(&http_origin, &tx);

    for _ in 0..2 {
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    let interesting_txids = [
        invoke_txid.clone(),
        invoke_1_txid.clone(),
        invoke_2_txid.clone(),
        use_txid.clone(),
        impl_txid.clone(),
        trait_txid.clone(),
    ];

    let blocks = test_observer::get_blocks();
    let mut results = vec![];
    for block in blocks {
        let transactions = block.get("transactions").unwrap().as_array().unwrap();

        for tx in transactions.iter() {
            let raw_tx = tx.get("raw_tx").unwrap().as_str().unwrap();
            if raw_tx == "0x00" {
                continue;
            }
            let tx_bytes = hex_bytes(&raw_tx[2..]).unwrap();
            let parsed = StacksTransaction::consensus_deserialize(&mut &tx_bytes[..]).unwrap();
            if interesting_txids.contains(&parsed.txid().to_string()) {
                eprintln!(
                    "{} => {}",
                    parsed.txid(),
                    tx.get("status").unwrap().as_str().unwrap()
                );
                results.push(tx.get("status").unwrap().as_str().unwrap().to_string());
                eprintln!(
                    "{} => {}",
                    parsed.txid(),
                    tx.get("raw_result").unwrap().as_str().unwrap()
                );
            }
        }
    }

    assert_eq!(results.len(), 6);

    for result in results.iter() {
        assert_eq!(result, "success");
    }

    test_observer::clear();
    channel.stop_chains_coordinator();
}

#[test]
#[ignore]
/// Verify that it is acceptable to launch PoX-2 at the end of a reward cycle, and set v1 unlock
/// height to be at the start of the subsequent reward cycle.
///
/// Verify that PoX-1 stackers continue to receive PoX payouts after v1 unlock height, and that
/// PoX-2 stackers only begin receiving rewards at the start of the reward cycle following the one
/// that contains v1 unlock height.
fn test_v1_unlock_height_with_current_stackers() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let reward_cycle_len = 5;
    let prepare_phase_len = 3;
    let epoch_2_05 = 205;
    let epoch_2_1 = 210;
    let v1_unlock_height = 211;

    let stacked = 100_000_000_000 * (core::MICROSTACKS_PER_STACKS as u64);

    let spender_sk = StacksPrivateKey::new();
    let spender_addr: PrincipalData = to_addr(&spender_sk).into();
    let mut initial_balances = vec![];

    initial_balances.push(InitialBalance {
        address: spender_addr.clone(),
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
        u32::MAX,
        u32::MAX,
        u32::MAX,
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

    // stack right away
    let sort_height = channel.get_sortitions_processed() + 1;
    let pox_addr_tuple_1 = execute(
        &format!("{{ hashbytes: 0x{pox_pubkey_hash_1}, version: 0x00 }}"),
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

    // wait until epoch 2.1
    loop {
        let tip_info = get_chain_info(&conf);
        if tip_info.burn_block_height >= epoch_2_1 {
            break;
        }
        next_block_and_wait(&mut btc_regtest_controller, &blocks_processed);
    }

    info!("Test passed processing 2.1");

    let sort_height = channel.get_sortitions_processed() + 1;
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
    submit_tx(&http_origin, &tx);

    // that it can mine _at all_ is a success criterion
    let mut last_block_height = get_chain_info(&conf).burn_block_height;
    for _i in 0..10 {
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

    for height in 211..tip_info.burn_block_height {
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

        if height < 215 {
            if !burnchain_config.is_in_prepare_phase(height) {
                assert_eq!(pox_addrs.len(), 2);
                for addr_tuple in pox_addrs {
                    assert_eq!(addr_tuple, pox_addr_tuple_1);
                }
            }
        } else if !burnchain_config.is_in_prepare_phase(height) {
            assert_eq!(pox_addrs.len(), 2);
            for addr_tuple in pox_addrs {
                assert_eq!(addr_tuple, pox_addr_tuple_2);
            }
        }
    }

    test_observer::clear();
    channel.stop_chains_coordinator();
}

#[test]
#[ignore]
/// Verify that it is acceptable to launch PoX-2 at the end of a reward cycle, and set v1 unlock
/// height to be at the start of the subsequent reward cycle.
///
/// Verify that PoX-1 stackers continue to receive PoX payouts after v1 unlock height, and that
/// PoX-2 stackers only begin receiving rewards at the start of the reward cycle following the one
/// that contains v1 unlock height.
///
/// Verify that both of the above work even if miners do not mine in the same block as the PoX-2
/// start height or v1 unlock height (e.g. suppose there's a delay).
fn test_v1_unlock_height_with_delay_and_current_stackers() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }

    let reward_cycle_len = 10;
    let prepare_phase_len = 3;
    let epoch_2_05 = 215;
    let epoch_2_1 = 230;
    let v1_unlock_height = 231;

    let stacked = 100_000_000_000 * (core::MICROSTACKS_PER_STACKS as u64);

    let spender_sk = StacksPrivateKey::new();
    let spender_addr: PrincipalData = to_addr(&spender_sk).into();
    let mut initial_balances = vec![];

    initial_balances.push(InitialBalance {
        address: spender_addr.clone(),
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
        u32::MAX,
        u32::MAX,
        u32::MAX,
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

    for height in 211..tip_info.burn_block_height {
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
        if !burnchain_config.is_in_prepare_phase(height) {
            let mut have_expected_payout = false;
            if height < epoch_2_1 + (reward_cycle_len as u64) {
                if !pox_addrs.is_empty() {
                    assert_eq!(pox_addrs.len(), 2);
                    for addr_tuple in pox_addrs {
                        // can either pay to pox tuple 1, or burn
                        assert_ne!(addr_tuple, pox_addr_tuple_2);
                        if addr_tuple == pox_addr_tuple_1 {
                            have_expected_payout = true;
                        }
                    }
                }
            } else if !pox_addrs.is_empty() {
                assert_eq!(pox_addrs.len(), 2);
                for addr_tuple in pox_addrs {
                    // can either pay to pox tuple 2, or burn
                    assert_ne!(addr_tuple, pox_addr_tuple_1);
                    if addr_tuple == pox_addr_tuple_2 {
                        have_expected_payout = true;
                    }
                }
            }
            assert!(have_expected_payout);
        }
    }

    test_observer::clear();
    channel.stop_chains_coordinator();
}
