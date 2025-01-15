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

use clarity::vm::types::QualifiedContractIdentifier;
use clarity::vm::{ClarityVersion, ContractName};
use stacks_common::address::AddressHashMode;
use stacks_common::types::chainstate::{
    BurnchainHeaderHash, StacksAddress, StacksPrivateKey, StacksPublicKey,
};
use stacks_common::types::StacksEpoch;
use stacks_common::util::hash::Hash160;

use crate::burnchains::Burnchain;
use crate::chainstate::stacks::boot::test::{
    instantiate_pox_peer, instantiate_pox_peer_with_epoch,
};
use crate::chainstate::stacks::{
    StacksTransaction, StacksTransactionSigner, TransactionAuth, TransactionPayload,
    TransactionVersion,
};
use crate::core::{StacksEpochExtension, BITCOIN_REGTEST_FIRST_BLOCK_HASH};
use crate::net::test::TestEventObserver;
use crate::net::{Error as net_error, NeighborAddress, PeerAddress, StackerDBConfig};

fn make_smart_contract(
    name: &str,
    code_body: &str,
    pk: &StacksPrivateKey,
    nonce: u64,
    fee: u64,
) -> StacksTransaction {
    let mut tx_contract = StacksTransaction::new(
        TransactionVersion::Testnet,
        TransactionAuth::from_p2pkh(pk).unwrap(),
        TransactionPayload::new_smart_contract(name, code_body, None).unwrap(),
    );

    tx_contract.chain_id = 0x80000000;
    tx_contract.auth.set_origin_nonce(nonce);
    tx_contract.set_tx_fee(fee);

    let mut tx_signer = StacksTransactionSigner::new(&tx_contract);
    tx_signer.sign_origin(pk).unwrap();
    let tx_contract_signed = tx_signer.get_tx().unwrap();

    tx_contract_signed
}
/// ;; Any StackerDB smart contract must conform to this trait.
/// (define-trait stackerdb-trait
///
///     ;; Get the list of (signer, num-slots) that make up this DB
///     (define-public (stackerdb-get-signer-slots) (response (list 4096 { signer: principal, num-slots: uint }) uint))
///
///     ;; Get the control metadata for this DB
///     (define-public (stackerdb-get-config)
///         (response {
///             chunk-size: uint,
///             write-freq: uint,
///             max-writes: uint,
///             max-neighbors: uint,
///             hint-replicas: (list 128 { addr: (list 16 uint), port: uint, public-key-hash: (buff 20) })
///         },
///         uint))
/// )

#[test]
fn test_valid_and_invalid_stackerdb_configs() {
    let AUTO_UNLOCK_HEIGHT = 12;
    let EXPECTED_FIRST_V2_CYCLE = 8;
    // the sim environment produces 25 empty sortitions before
    //  tenures start being tracked.
    let EMPTY_SORTITIONS = 25;

    let mut burnchain = Burnchain::default_unittest(
        0,
        &BurnchainHeaderHash::from_hex(BITCOIN_REGTEST_FIRST_BLOCK_HASH).unwrap(),
    );
    burnchain.pox_constants.reward_cycle_length = 5;
    burnchain.pox_constants.prepare_length = 2;
    burnchain.pox_constants.anchor_threshold = 1;
    burnchain.pox_constants.v1_unlock_height = AUTO_UNLOCK_HEIGHT + EMPTY_SORTITIONS;

    let first_v2_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.v1_unlock_height as u64)
        .unwrap()
        + 1;

    assert_eq!(first_v2_cycle, EXPECTED_FIRST_V2_CYCLE);

    let epochs = StacksEpoch::all(0, 0, EMPTY_SORTITIONS as u64 + 10);

    let observer = TestEventObserver::new();

    let (mut peer, mut keys) = instantiate_pox_peer_with_epoch(
        &burnchain,
        "test_valid_and_invalid_stackerdb_configs",
        Some(epochs.clone()),
        Some(&observer),
    );

    peer.config.check_pox_invariants =
        Some((EXPECTED_FIRST_V2_CYCLE, EXPECTED_FIRST_V2_CYCLE + 10));

    let contract_owner = keys.pop().unwrap();
    let mut coinbase_nonce = 0;
    let mut txs = vec![];

    let testcases = vec![
        (
            // valid
            r#"
            (define-public (stackerdb-get-signer-slots)
                (ok (list { signer: 'ST2TFVBMRPS5SSNP98DQKQ5JNB2B6NZM91C4K3P7B, num-slots: u3 })))

            (define-public (stackerdb-get-config)
                (ok {
                    chunk-size: u123,
                    write-freq: u4,
                    max-writes: u56,
                    max-neighbors: u7,
                    hint-replicas: (list
                        {
                            addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u142 u150 u80 u100),
                            port: u8901,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            Some(StackerDBConfig {
                chunk_size: 123,
                signers: vec![(
                    StacksAddress {
                        version: 26,
                        bytes: Hash160::from_hex("b4fdae98b64b9cd6c9436f3b965558966afe890b")
                            .unwrap(),
                    },
                    3,
                )],
                write_freq: 4,
                max_writes: 56,
                hint_replicas: vec![NeighborAddress {
                    addrbytes: PeerAddress::from_ipv4(142, 150, 80, 100),
                    port: 8901,
                    public_key_hash: Hash160::from_hex("0123456789abcdef0123456789abcdef01234567")
                        .unwrap(),
                }],
                max_neighbors: 7,
            }),
        ),
        (
            // valid
            r#"
            (define-read-only (stackerdb-get-signer-slots)
                (ok (list { signer: 'ST2TFVBMRPS5SSNP98DQKQ5JNB2B6NZM91C4K3P7B, num-slots: u3 })))

            (define-read-only (stackerdb-get-config)
                (ok {
                    chunk-size: u123,
                    write-freq: u4,
                    max-writes: u56,
                    max-neighbors: u7,
                    hint-replicas: (list
                        {
                            addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u142 u150 u80 u100),
                            port: u8901,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            Some(StackerDBConfig {
                chunk_size: 123,
                signers: vec![(
                    StacksAddress {
                        version: 26,
                        bytes: Hash160::from_hex("b4fdae98b64b9cd6c9436f3b965558966afe890b")
                            .unwrap(),
                    },
                    3,
                )],
                write_freq: 4,
                max_writes: 56,
                hint_replicas: vec![NeighborAddress {
                    addrbytes: PeerAddress::from_ipv4(142, 150, 80, 100),
                    port: 8901,
                    public_key_hash: Hash160::from_hex("0123456789abcdef0123456789abcdef01234567")
                        .unwrap(),
                }],
                max_neighbors: 7,
            }),
        ),
        (
            // invalid -- missing function
            r#"
            (define-public (stackerdb-get-config)
                (ok {
                    chunk-size: u123,
                    write-freq: u4,
                    max-writes: u56,
                    max-neighbors: u7,
                    hint-replicas: (list
                        {
                            addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u142 u150 u80 u100),
                            port: u8901,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            None,
        ),
        (
            // invalid -- bad function signature (argument)
            r#"
            (define-public (stackerdb-get-signer-slots (bad-arg uint))
                (ok (list { signer: 'ST2TFVBMRPS5SSNP98DQKQ5JNB2B6NZM91C4K3P7B, num-slots: u3 })))

            (define-public (stackerdb-get-config)
                (ok {
                    chunk-size: u123,
                    write-freq: u4,
                    max-writes: u56,
                    max-neighbors: u7,
                    hint-replicas: (list
                        {
                            addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u142 u150 u80 u100),
                            port: u8901,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            None,
        ),
        (
            // invalid -- bad return type
            r#"
            (define-public (stackerdb-get-signer-slots)
                (ok true))

            (define-public (stackerdb-get-config)
                (ok {
                    chunk-size: u123,
                    write-freq: u4,
                    max-writes: u56,
                    max-neighbors: u7,
                    hint-replicas: (list
                        {
                            addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u142 u150 u80 u100),
                            port: u8901,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            None,
        ),
        (
            // invalid -- bad signer (can't be a contract)
            r#"
            (define-public (stackerdb-get-signer-slots)
                (ok (list { signer: 'ST2TFVBMRPS5SSNP98DQKQ5JNB2B6NZM91C4K3P7B.nope, num-slots: u3 })))

            (define-public (stackerdb-get-config)
                (ok {
                    chunk-size: u123,
                    write-freq: u4,
                    max-writes: u56,
                    max-neighbors: u7,
                    hint-replicas: (list
                        {
                            addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u142 u150 u80 u100),
                            port: u8901,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            None,
        ),
        (
            // invalid -- num-slots too big
            r#"
            (define-public (stackerdb-get-signer-slots)
                (ok (list { signer: 'ST2TFVBMRPS5SSNP98DQKQ5JNB2B6NZM91C4K3P7B, num-slots: u30000 })))

            (define-public (stackerdb-get-config)
                (ok {
                    chunk-size: u123,
                    write-freq: u4,
                    max-writes: u56,
                    max-neighbors: u7,
                    hint-replicas: (list
                        {
                            addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u142 u150 u80 u100),
                            port: u8901,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            None,
        ),
        (
            // invalid -- chunk-size too big
            r#"
            (define-public (stackerdb-get-signer-slots)
                (ok (list { signer: 'ST2TFVBMRPS5SSNP98DQKQ5JNB2B6NZM91C4K3P7B, num-slots: u3 })))

            (define-public (stackerdb-get-config)
                (ok {
                    chunk-size: (+ (* u16 u1048576) u1),
                    write-freq: u4,
                    max-writes: u56,
                    max-neighbors: u7,
                    hint-replicas: (list
                        {
                            addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u142 u150 u80 u100),
                            port: u8901,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            None,
        ),
        (
            // invalid -- write-freq too big
            r#"
            (define-public (stackerdb-get-signer-slots)
                (ok (list { signer: 'ST2TFVBMRPS5SSNP98DQKQ5JNB2B6NZM91C4K3P7B, num-slots: u3 })))

            (define-public (stackerdb-get-config)
                (ok {
                    chunk-size: u123,
                    write-freq: u18446744073709551617,
                    max-writes: u56,
                    max-neighbors: u7,
                    hint-replicas: (list
                        {
                            addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u142 u150 u80 u100),
                            port: u8901,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            None,
        ),
        (
            // invalid -- max-writes too big
            r#"
            (define-public (stackerdb-get-signer-slots)
                (ok (list { signer: 'ST2TFVBMRPS5SSNP98DQKQ5JNB2B6NZM91C4K3P7B, num-slots: u3 })))

            (define-public (stackerdb-get-config)
                (ok {
                    chunk-size: u123,
                    write-freq: u4,
                    max-writes: u4294967297,
                    max-neighbors: u7,
                    hint-replicas: (list
                        {
                            addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u142 u150 u80 u100),
                            port: u8901,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            None,
        ),
        (
            // invalid -- max-neighbors too big
            r#"
            (define-public (stackerdb-get-signer-slots)
                (ok (list { signer: 'ST2TFVBMRPS5SSNP98DQKQ5JNB2B6NZM91C4K3P7B, num-slots: u3 })))

            (define-public (stackerdb-get-config)
                (ok {
                    chunk-size: u123,
                    write-freq: u4,
                    max-writes: u56,
                    max-neighbors: u18446744073709551617,
                    hint-replicas: (list
                        {
                            addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u142 u150 u80 u100),
                            port: u8901,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            None,
        ),
        (
            // invalid -- bad hint-replicas address
            r#"
            (define-public (stackerdb-get-signer-slots)
                (ok (list { signer: 'ST2TFVBMRPS5SSNP98DQKQ5JNB2B6NZM91C4K3P7B, num-slots: u3 })))

            (define-public (stackerdb-get-config)
                (ok {
                    chunk-size: u123,
                    write-freq: u4,
                    max-writes: u56,
                    max-neighbors: u7,
                    hint-replicas: (list
                        {
                            addr: (unwrap-panic (as-max-len? (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u127 u0 u0) u16)),
                            port: u8901,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            None,
        ),
        (
            // invalid -- bad port (too small)
            r#"
            (define-public (stackerdb-get-signer-slots)
                (ok (list { signer: 'ST2TFVBMRPS5SSNP98DQKQ5JNB2B6NZM91C4K3P7B, num-slots: u3 })))

            (define-public (stackerdb-get-config)
                (ok {
                    chunk-size: u123,
                    write-freq: u4,
                    max-writes: u56,
                    max-neighbors: u7,
                    hint-replicas: (list
                        {
                            addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u142 u150 u80 u100),
                            port: u1,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            None,
        ),
        (
            // invalid -- bad port (too big)
            r#"
            (define-public (stackerdb-get-signer-slots)
                (ok (list { signer: 'ST2TFVBMRPS5SSNP98DQKQ5JNB2B6NZM91C4K3P7B, num-slots: u3 })))

            (define-public (stackerdb-get-config)
                (ok {
                    chunk-size: u123,
                    write-freq: u4,
                    max-writes: u56,
                    max-neighbors: u7,
                    hint-replicas: (list
                        {
                            addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u142 u150 u80 u100),
                            port: u65537,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            None,
        ),
        (
            // valid, but private IP and absurd max neighbors are both handled
            r#"
            (define-public (stackerdb-get-signer-slots)
                (ok (list { signer: 'ST2TFVBMRPS5SSNP98DQKQ5JNB2B6NZM91C4K3P7B, num-slots: u3 })))

            (define-public (stackerdb-get-config)
                (ok {
                    chunk-size: u123,
                    write-freq: u4,
                    max-writes: u56,
                    max-neighbors: u1024,
                    hint-replicas: (list
                        {
                            addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u192 u168 u0 u1),
                            port: u8901,
                            public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                        })
                }))
            "#,
            Some(StackerDBConfig {
                chunk_size: 123,
                signers: vec![(
                    StacksAddress {
                        version: 26,
                        bytes: Hash160::from_hex("b4fdae98b64b9cd6c9436f3b965558966afe890b")
                            .unwrap(),
                    },
                    3,
                )],
                write_freq: 4,
                max_writes: 56,
                // no neighbors
                hint_replicas: vec![],
                // max neighbors is truncated
                max_neighbors: 32,
            }),
        ),
    ];

    for (i, (code, _result)) in testcases.iter().enumerate() {
        let tx = make_smart_contract(
            &format!("test-{}", i),
            code,
            &contract_owner,
            i as u64,
            10000,
        );
        txs.push(tx);
    }

    peer.tenure_with_txs(&txs, &mut coinbase_nonce);

    for (i, (code, result)) in testcases.iter().enumerate() {
        let contract_id = QualifiedContractIdentifier::new(
            StacksAddress::from_public_keys(
                26,
                &AddressHashMode::SerializeP2PKH,
                1,
                &vec![StacksPublicKey::from_private(&contract_owner)],
            )
            .unwrap()
            .into(),
            ContractName::try_from(format!("test-{}", i)).unwrap(),
        );
        peer.with_db_state(|sortdb, chainstate, _, _| {
            match StackerDBConfig::from_smart_contract(chainstate, sortdb, &contract_id, 32, None) {
                Ok(config) => {
                    let expected = result
                        .clone()
                        .expect(&format!("FATAL: parsed a bad contract\n{}", code));
                    assert_eq!(config, expected);
                }
                Err(net_error::InvalidStackerDBContract(..)) => {
                    assert!(
                        result.is_none(),
                        "FATAL: valid contract treated as invalid\n{}",
                        code
                    );
                }
                Err(e) => {
                    panic!("Unexpected error: {:?}", &e);
                }
            }
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn test_hint_replicas_override() {
    let AUTO_UNLOCK_HEIGHT = 12;
    let EXPECTED_FIRST_V2_CYCLE = 8;
    // the sim environment produces 25 empty sortitions before
    //  tenures start being tracked.
    let EMPTY_SORTITIONS = 25;

    let mut burnchain = Burnchain::default_unittest(
        0,
        &BurnchainHeaderHash::from_hex(BITCOIN_REGTEST_FIRST_BLOCK_HASH).unwrap(),
    );
    burnchain.pox_constants.reward_cycle_length = 5;
    burnchain.pox_constants.prepare_length = 2;
    burnchain.pox_constants.anchor_threshold = 1;
    burnchain.pox_constants.v1_unlock_height = AUTO_UNLOCK_HEIGHT + EMPTY_SORTITIONS;

    let first_v2_cycle = burnchain
        .block_height_to_reward_cycle(burnchain.pox_constants.v1_unlock_height as u64)
        .unwrap()
        + 1;

    assert_eq!(first_v2_cycle, EXPECTED_FIRST_V2_CYCLE);

    let epochs = StacksEpoch::all(0, 0, EMPTY_SORTITIONS as u64 + 10);

    let observer = TestEventObserver::new();

    let (mut peer, mut keys) = instantiate_pox_peer_with_epoch(
        &burnchain,
        "test_valid_and_invalid_stackerdb_configs",
        Some(epochs.clone()),
        Some(&observer),
    );

    let contract_owner = keys.pop().unwrap();
    let contract_id = QualifiedContractIdentifier::new(
        StacksAddress::from_public_keys(
            26,
            &AddressHashMode::SerializeP2PKH,
            1,
            &vec![StacksPublicKey::from_private(&contract_owner)],
        )
        .unwrap()
        .into(),
        ContractName::try_from("test-0").unwrap(),
    );

    peer.config.check_pox_invariants =
        Some((EXPECTED_FIRST_V2_CYCLE, EXPECTED_FIRST_V2_CYCLE + 10));

    let override_replica = NeighborAddress {
        addrbytes: PeerAddress([2u8; 16]),
        port: 123,
        public_key_hash: Hash160([3u8; 20]),
    };

    let mut coinbase_nonce = 0;
    let mut txs = vec![];

    let config_contract = r#"
        (define-public (stackerdb-get-signer-slots)
            (ok (list { signer: 'ST2TFVBMRPS5SSNP98DQKQ5JNB2B6NZM91C4K3P7B, num-slots: u3 })))

        (define-public (stackerdb-get-config)
            (ok {
                chunk-size: u123,
                write-freq: u4,
                max-writes: u56,
                max-neighbors: u7,
                hint-replicas: (list
                    {
                        addr: (list u0 u0 u0 u0 u0 u0 u0 u0 u0 u0 u255 u255 u142 u150 u80 u100),
                        port: u8901,
                        public-key-hash: 0x0123456789abcdef0123456789abcdef01234567
                    })
            }))
        "#;

    let expected_config = StackerDBConfig {
        chunk_size: 123,
        signers: vec![(
            StacksAddress {
                version: 26,
                bytes: Hash160::from_hex("b4fdae98b64b9cd6c9436f3b965558966afe890b").unwrap(),
            },
            3,
        )],
        write_freq: 4,
        max_writes: 56,
        hint_replicas: vec![override_replica.clone()],
        max_neighbors: 7,
    };

    let tx = make_smart_contract("test-0", config_contract, &contract_owner, 0, 10000);
    txs.push(tx);

    peer.tenure_with_txs(&txs, &mut coinbase_nonce);

    peer.with_db_state(|sortdb, chainstate, _, _| {
        match StackerDBConfig::from_smart_contract(
            chainstate,
            sortdb,
            &contract_id,
            32,
            Some(vec![override_replica.clone()]),
        ) {
            Ok(config) => {
                assert_eq!(config, expected_config);
            }
            Err(e) => {
                panic!("Unexpected error: {:?}", &e);
            }
        }
        Ok(())
    })
    .unwrap();
}
