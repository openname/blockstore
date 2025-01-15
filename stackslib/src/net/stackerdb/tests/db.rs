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
use std::path::Path;

use clarity::vm::types::QualifiedContractIdentifier;
use clarity::vm::ContractName;
use libstackerdb::SlotMetadata;
use rusqlite::params;
use stacks_common::address::{
    AddressHashMode, C32_ADDRESS_VERSION_MAINNET_MULTISIG, C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
};
use stacks_common::types::chainstate::{
    ConsensusHash, StacksAddress, StacksPrivateKey, StacksPublicKey,
};
use stacks_common::util::hash::{Hash160, Sha512Trunc256Sum};
use stacks_common::util::secp256k1::MessageSignature;

use crate::net::stackerdb::db::SlotValidation;
use crate::net::stackerdb::{StackerDBConfig, StackerDBs};
use crate::net::{Error as net_error, StackerDBChunkData};

fn setup_test_path(path: &str) {
    let dirname = Path::new(path).parent().unwrap().to_str().unwrap();
    if fs::metadata(&dirname).is_err() {
        fs::create_dir_all(&dirname).unwrap();
    }
    if fs::metadata(path).is_ok() {
        fs::remove_file(path).unwrap();
    }
}

/// Test that we can instantiate a stacker DB instance
#[test]
fn test_stackerdb_connect() {
    let path = "/tmp/stacks-node-tests/test_stackerdb_connect.sqlite";
    setup_test_path(path);

    let _ = StackerDBs::connect(path, true).unwrap();
}

/// Test that we can create, enumerate, and drop StackerDB tables.
#[test]
fn test_stackerdb_create_list_delete() {
    let path = "/tmp/stacks-node-tests/test_stackerdb_create_list_delete.sqlite";
    setup_test_path(path);

    let mut db = StackerDBs::connect(path, true).unwrap();
    let tx = db.tx_begin(StackerDBConfig::noop()).unwrap();

    let slots = [(
        StacksAddress {
            version: 0x02,
            bytes: Hash160([0x02; 20]),
        },
        1,
    )];

    // databases with one chunk
    tx.create_stackerdb(
        &QualifiedContractIdentifier::new(
            StacksAddress {
                version: 0x01,
                bytes: Hash160([0x01; 20]),
            }
            .into(),
            ContractName::try_from("db1").unwrap(),
        ),
        &[(
            StacksAddress {
                version: 0x01,
                bytes: Hash160([0x01; 20]),
            },
            1,
        )],
    )
    .unwrap();
    tx.create_stackerdb(
        &QualifiedContractIdentifier::new(
            StacksAddress {
                version: 0x02,
                bytes: Hash160([0x02; 20]),
            }
            .into(),
            ContractName::try_from("db2").unwrap(),
        ),
        &[(
            StacksAddress {
                version: 0x02,
                bytes: Hash160([0x02; 20]),
            },
            1,
        )],
    )
    .unwrap();
    tx.create_stackerdb(
        &QualifiedContractIdentifier::new(
            StacksAddress {
                version: 0x03,
                bytes: Hash160([0x03; 20]),
            }
            .into(),
            ContractName::try_from("db3").unwrap(),
        ),
        &[(
            StacksAddress {
                version: 0x03,
                bytes: Hash160([0x03; 20]),
            },
            1,
        )],
    )
    .unwrap();

    tx.commit().unwrap();

    let mut dbs = db.get_stackerdb_contract_ids().unwrap();
    dbs.sort();

    assert_eq!(
        dbs,
        vec![
            QualifiedContractIdentifier::new(
                StacksAddress {
                    version: 0x01,
                    bytes: Hash160([0x01; 20])
                }
                .into(),
                ContractName::try_from("db1").unwrap()
            ),
            QualifiedContractIdentifier::new(
                StacksAddress {
                    version: 0x02,
                    bytes: Hash160([0x02; 20])
                }
                .into(),
                ContractName::try_from("db2").unwrap()
            ),
            QualifiedContractIdentifier::new(
                StacksAddress {
                    version: 0x03,
                    bytes: Hash160([0x03; 20])
                }
                .into(),
                ContractName::try_from("db3").unwrap()
            ),
        ]
    );

    // adding the same DB errors out
    let tx = db.tx_begin(StackerDBConfig::noop()).unwrap();
    if let net_error::StackerDBExists(..) = tx
        .create_stackerdb(
            &QualifiedContractIdentifier::new(
                StacksAddress {
                    version: 0x01,
                    bytes: Hash160([0x01; 20]),
                }
                .into(),
                ContractName::try_from("db1").unwrap(),
            ),
            &[],
        )
        .unwrap_err()
    {
    } else {
        panic!("Did not error on creating the same stacker DB twice");
    }
    tx.commit().unwrap();

    let mut dbs = db.get_stackerdb_contract_ids().unwrap();
    dbs.sort();

    assert_eq!(
        dbs,
        vec![
            QualifiedContractIdentifier::new(
                StacksAddress {
                    version: 0x01,
                    bytes: Hash160([0x01; 20])
                }
                .into(),
                ContractName::try_from("db1").unwrap()
            ),
            QualifiedContractIdentifier::new(
                StacksAddress {
                    version: 0x02,
                    bytes: Hash160([0x02; 20])
                }
                .into(),
                ContractName::try_from("db2").unwrap()
            ),
            QualifiedContractIdentifier::new(
                StacksAddress {
                    version: 0x03,
                    bytes: Hash160([0x03; 20])
                }
                .into(),
                ContractName::try_from("db3").unwrap()
            ),
        ]
    );

    // each DB's single chunk exists
    for sc in dbs.iter() {
        db.get_latest_chunk(sc, 0).unwrap().expect("missing chunk");
    }

    // remove a db
    let tx = db.tx_begin(StackerDBConfig::noop()).unwrap();
    tx.delete_stackerdb(&QualifiedContractIdentifier::new(
        StacksAddress {
            version: 0x01,
            bytes: Hash160([0x01; 20]),
        }
        .into(),
        ContractName::try_from("db1").unwrap(),
    ))
    .unwrap();
    tx.commit().unwrap();

    let mut dbs = db.get_stackerdb_contract_ids().unwrap();
    dbs.sort();

    assert_eq!(
        dbs,
        vec![
            QualifiedContractIdentifier::new(
                StacksAddress {
                    version: 0x02,
                    bytes: Hash160([0x02; 20])
                }
                .into(),
                ContractName::try_from("db2").unwrap()
            ),
            QualifiedContractIdentifier::new(
                StacksAddress {
                    version: 0x03,
                    bytes: Hash160([0x03; 20])
                }
                .into(),
                ContractName::try_from("db3").unwrap()
            ),
        ]
    );

    // only existing DBs still have chunks
    for sc in dbs.iter() {
        db.get_latest_chunk(sc, 0).unwrap().expect("missing chunk");
    }

    // deletion is idempotent
    let tx = db.tx_begin(StackerDBConfig::noop()).unwrap();
    tx.delete_stackerdb(&QualifiedContractIdentifier::new(
        StacksAddress {
            version: 0x01,
            bytes: Hash160([0x01; 20]),
        }
        .into(),
        ContractName::try_from("db1").unwrap(),
    ))
    .unwrap();
    tx.commit().unwrap();

    let mut dbs = db.get_stackerdb_contract_ids().unwrap();
    dbs.sort();

    assert_eq!(
        dbs,
        vec![
            QualifiedContractIdentifier::new(
                StacksAddress {
                    version: 0x02,
                    bytes: Hash160([0x02; 20])
                }
                .into(),
                ContractName::try_from("db2").unwrap()
            ),
            QualifiedContractIdentifier::new(
                StacksAddress {
                    version: 0x03,
                    bytes: Hash160([0x03; 20])
                }
                .into(),
                ContractName::try_from("db3").unwrap()
            ),
        ]
    );
    // only existing DBs still have chunks
    for sc in dbs.iter() {
        db.get_latest_chunk(sc, 0).unwrap().expect("missing chunk");
    }
}

/// Test that we can set up a StackerDB with a given config, and clear it.
#[test]
fn test_stackerdb_prepare_clear_slots() {
    let path = "/tmp/test_stackerdb_prepare_clear_slots.sqlite";
    setup_test_path(path);

    let sc = QualifiedContractIdentifier::new(
        StacksAddress {
            version: 0x01,
            bytes: Hash160([0x01; 20]),
        }
        .into(),
        ContractName::try_from("db1").unwrap(),
    );

    let mut db = StackerDBs::connect(path, true).unwrap();
    let tx = db.tx_begin(StackerDBConfig::noop()).unwrap();

    tx.create_stackerdb(
        &sc,
        &[
            (
                StacksAddress {
                    version: 0x02,
                    bytes: Hash160([0x02; 20]),
                },
                2,
            ),
            (
                StacksAddress {
                    version: 0x03,
                    bytes: Hash160([0x03; 20]),
                },
                3,
            ),
            (
                StacksAddress {
                    version: 0x04,
                    bytes: Hash160([0x04; 20]),
                },
                4,
            ),
        ],
    )
    .unwrap();

    tx.commit().unwrap();

    // slots must all be inserted in the right places and quantities
    for slot_id in 0..(2 + 3 + 4) {
        let slot_metadata = db.get_slot_metadata(&sc, slot_id).unwrap().unwrap();
        let slot_validation = db.get_slot_validation(&sc, slot_id).unwrap().unwrap();

        if slot_id < 2 {
            // belongs to 0x02
            assert_eq!(
                slot_validation.signer,
                StacksAddress {
                    version: 0x02,
                    bytes: Hash160([0x02; 20])
                }
            );
        } else if slot_id >= 2 && slot_id < 2 + 3 {
            // belongs to 0x03
            assert_eq!(
                slot_validation.signer,
                StacksAddress {
                    version: 0x03,
                    bytes: Hash160([0x03; 20])
                }
            );
        } else if slot_id >= 2 + 3 && slot_id < 2 + 3 + 4 {
            // belongs to 0x03
            assert_eq!(
                slot_validation.signer,
                StacksAddress {
                    version: 0x04,
                    bytes: Hash160([0x04; 20])
                }
            );
        } else {
            unreachable!()
        }

        assert_eq!(slot_metadata.slot_id, slot_id);
        assert_eq!(slot_metadata.slot_version, 0);
        assert_eq!(slot_metadata.data_hash, Sha512Trunc256Sum([0x00; 32]));
        assert_eq!(slot_metadata.signature, MessageSignature::empty());

        assert_eq!(slot_validation.version, 0);
        assert_eq!(slot_validation.write_time, 0);
    }

    let tx = db.tx_begin(StackerDBConfig::noop()).unwrap();
    tx.clear_stackerdb_slots(&sc).unwrap();
    tx.commit().unwrap();

    // no more slots
    for slot_id in 0..(2 + 3 + 4) {
        let slot_metadata = db.get_slot_metadata(&sc, slot_id).unwrap();
        assert!(slot_metadata.is_none());

        let slot_validation = db.get_slot_validation(&sc, slot_id).unwrap();
        assert!(slot_validation.is_none());
    }
}

/// Test that we can insert and query chunks to a StackerDB.
/// * verifies that they must be signed
/// * verifies that they mut not be stale
/// * verifies that they cannot exceed the config-given wall-clock write frequency
/// * verifies that they cannot exceed the per-chunk write count
#[test]
fn test_stackerdb_insert_query_chunks() {
    let path = "/tmp/test_stackerdb_insert_query_chunks.sqlite";
    setup_test_path(path);

    let sc = QualifiedContractIdentifier::new(
        StacksAddress {
            version: 0x01,
            bytes: Hash160([0x01; 20]),
        }
        .into(),
        ContractName::try_from("db1").unwrap(),
    );

    let mut db = StackerDBs::connect(path, true).unwrap();

    let mut db_config = StackerDBConfig::noop();
    db_config.max_writes = 3;
    db_config.write_freq = 120;

    let tx = db.tx_begin(db_config.clone()).unwrap();

    let pks: Vec<_> = (0..10).map(|_| StacksPrivateKey::new()).collect();
    let addrs: Vec<_> = pks
        .iter()
        .map(|pk| {
            StacksAddress::from_public_keys(
                C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
                &AddressHashMode::SerializeP2PKH,
                1,
                &vec![StacksPublicKey::from_private(pk)],
            )
            .unwrap()
        })
        .collect();

    tx.create_stackerdb(
        &sc,
        &addrs
            .clone()
            .into_iter()
            .map(|addr| (addr, 1))
            .collect::<Vec<_>>(),
    )
    .unwrap();

    // store some data
    for (i, pk) in pks.iter().enumerate() {
        let mut chunk_data = StackerDBChunkData {
            slot_id: i as u32,
            slot_version: 1,
            sig: MessageSignature::empty(),
            data: vec![i as u8; 128],
        };

        chunk_data.sign(pk).unwrap();

        let slot_metadata = tx.get_slot_metadata(&sc, i as u32).unwrap().unwrap();
        assert_eq!(slot_metadata.slot_id, i as u32);
        assert_eq!(slot_metadata.slot_version, 0);
        assert_eq!(slot_metadata.data_hash, Sha512Trunc256Sum([0x00; 32]));
        assert_eq!(slot_metadata.signature, MessageSignature::empty());

        // should succeed
        tx.try_replace_chunk(&sc, &chunk_data.get_slot_metadata(), &chunk_data.data)
            .unwrap();

        let slot_metadata = tx.get_slot_metadata(&sc, i as u32).unwrap().unwrap();
        assert_eq!(slot_metadata.slot_id, i as u32);
        assert_eq!(slot_metadata.slot_version, chunk_data.slot_version);
        assert_eq!(slot_metadata.data_hash, chunk_data.data_hash());
        assert_eq!(slot_metadata.signature, chunk_data.sig);

        // should fail -- stale version
        if let Err(net_error::StaleChunk {
            supplied_version,
            latest_version,
        }) = tx.try_replace_chunk(&sc, &chunk_data.get_slot_metadata(), &chunk_data.data)
        {
            assert_eq!(supplied_version, 1);
            assert_eq!(latest_version, 1);
        } else {
            panic!("Did not get StaleChunk");
        }

        // should fail -- too many writes version
        chunk_data.slot_version = db_config.max_writes + 1;
        chunk_data.sign(pk).unwrap();
        if let Err(net_error::TooManySlotWrites {
            supplied_version,
            max_writes,
        }) = tx.try_replace_chunk(&sc, &chunk_data.get_slot_metadata(), &chunk_data.data)
        {
            assert_eq!(max_writes, db_config.max_writes);
            assert_eq!(supplied_version, 1);
        } else {
            panic!("Did not get TooManySlotWrites");
        }

        // should fail -- bad signature
        chunk_data.slot_version = 2;
        if let Err(net_error::BadSlotSigner(stacker, slot_id)) =
            tx.try_replace_chunk(&sc, &chunk_data.get_slot_metadata(), &chunk_data.data)
        {
            assert_eq!(stacker, addrs[i]);
            assert_eq!(slot_id, i as u32);
        } else {
            eprintln!(
                "{}",
                tx.try_replace_chunk(&sc, &chunk_data.get_slot_metadata(), &chunk_data.data)
                    .unwrap_err()
            );
            panic!("Did not get BadSlotSigner");
        }
    }

    tx.commit().unwrap();

    // test queries against the data
    for (i, addr) in addrs.iter().enumerate() {
        let signer = db.get_slot_signer(&sc, i as u32).unwrap().unwrap();
        assert_eq!(signer, *addr);

        let chunk = db.get_latest_chunk(&sc, i as u32).unwrap().unwrap();
        assert_eq!(chunk, vec![i as u8; 128]);

        // correct version
        let chunk = db.get_chunk(&sc, i as u32, 1).unwrap().unwrap();
        assert_eq!(chunk.data, vec![i as u8; 128]);
        assert_eq!(chunk.slot_version, 1);
        assert_eq!(chunk.slot_id, i as u32);
        assert!(chunk.verify(addr).unwrap());

        // incorrect version
        let chunk = db.get_chunk(&sc, i as u32, 0).unwrap();
        assert!(chunk.is_none());

        // incorrect version
        let chunk = db.get_chunk(&sc, i as u32, 2).unwrap();
        assert!(chunk.is_none());

        let slot_metadata = db.get_slot_metadata(&sc, i as u32).unwrap().unwrap();
        assert!(slot_metadata.verify(addr).unwrap());
    }

    let versions = db.get_slot_versions(&sc).unwrap();
    assert_eq!(versions, vec![1; 10]);

    let timestamps = db.get_slot_write_timestamps(&sc).unwrap();
    for ts in timestamps {
        assert!(ts > 0);
    }
}

/// Verify that we can reconfigure the database by changing its slots
#[test]
fn test_reconfigure_stackerdb() {
    let path = "/tmp/test_stackerdb_reconfigure.sqlite";
    setup_test_path(path);

    let sc = QualifiedContractIdentifier::new(
        StacksAddress {
            version: 0x01,
            bytes: Hash160([0x01; 20]),
        }
        .into(),
        ContractName::try_from("db1").unwrap(),
    );

    let mut db = StackerDBs::connect(path, true).unwrap();

    let mut db_config = StackerDBConfig::noop();
    db_config.max_writes = 3;
    db_config.write_freq = 120;

    let tx = db.tx_begin(db_config.clone()).unwrap();

    let pks: Vec<_> = (0..10).map(|_| StacksPrivateKey::new()).collect();
    let addrs: Vec<_> = pks
        .iter()
        .map(|pk| {
            StacksAddress::from_public_keys(
                C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
                &AddressHashMode::SerializeP2PKH,
                1,
                &vec![StacksPublicKey::from_private(pk)],
            )
            .unwrap()
        })
        .collect();

    tx.create_stackerdb(
        &sc,
        &addrs
            .clone()
            .into_iter()
            .map(|addr| (addr, 1))
            .collect::<Vec<_>>(),
    )
    .unwrap();

    // store some data
    let mut initial_metadata = vec![];
    for (i, pk) in pks.iter().enumerate() {
        let mut chunk_data = StackerDBChunkData {
            slot_id: i as u32,
            slot_version: 1,
            sig: MessageSignature::empty(),
            data: vec![i as u8; 128],
        };

        chunk_data.sign(pk).unwrap();

        let slot_metadata = tx.get_slot_metadata(&sc, i as u32).unwrap().unwrap();
        assert_eq!(slot_metadata.slot_id, i as u32);
        assert_eq!(slot_metadata.slot_version, 0);
        assert_eq!(slot_metadata.data_hash, Sha512Trunc256Sum([0x00; 32]));
        assert_eq!(slot_metadata.signature, MessageSignature::empty());

        // should succeed
        tx.try_replace_chunk(&sc, &chunk_data.get_slot_metadata(), &chunk_data.data)
            .unwrap();

        let slot_metadata = tx.get_slot_metadata(&sc, i as u32).unwrap().unwrap();
        assert_eq!(slot_metadata.slot_id, i as u32);
        assert_eq!(slot_metadata.slot_version, chunk_data.slot_version);
        assert_eq!(slot_metadata.data_hash, chunk_data.data_hash());
        assert_eq!(slot_metadata.signature, chunk_data.sig);

        initial_metadata.push((slot_metadata, chunk_data));
    }

    tx.commit().unwrap();

    let db_slot_metadata = db.get_db_slot_metadata(&sc).unwrap();
    assert_eq!(db_slot_metadata.len(), pks.len());
    for (i, slot_md) in db_slot_metadata.iter().enumerate() {
        let slot_metadata = db.get_slot_metadata(&sc, i as u32).unwrap().unwrap();
        assert_eq!(slot_metadata, *slot_md);
    }

    let tx = db.tx_begin(StackerDBConfig::noop()).unwrap();
    let new_pks: Vec<_> = (0..10).map(|_| StacksPrivateKey::new()).collect();
    let reconfigured_pks = vec![
        // first five slots are unchanged
        pks[0], pks[1], pks[2], pks[3], pks[4],
        // next five slots are different, so their contents will be dropped and versions and write
        // timestamps reset
        new_pks[0], new_pks[1], new_pks[2], new_pks[3], new_pks[4],
        // next five slots are now, so they'll be uninitialized
        new_pks[5], new_pks[6], new_pks[7], new_pks[8], new_pks[9],
    ];
    let reconfigured_addrs: Vec<_> = reconfigured_pks
        .iter()
        .map(|pk| {
            StacksAddress::from_public_keys(
                C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
                &AddressHashMode::SerializeP2PKH,
                1,
                &vec![StacksPublicKey::from_private(pk)],
            )
            .unwrap()
        })
        .collect();

    // reconfigure
    tx.reconfigure_stackerdb(
        &sc,
        &reconfigured_addrs
            .clone()
            .into_iter()
            .map(|addr| (addr, 1))
            .collect::<Vec<_>>(),
    )
    .unwrap();

    tx.commit().unwrap();

    for (i, pk) in new_pks.iter().enumerate() {
        if i < 5 {
            // first five are unchanged
            let chunk_data = StackerDBChunkData {
                slot_id: i as u32,
                slot_version: 1,
                sig: MessageSignature::empty(),
                data: vec![i as u8; 128],
            };

            let slot_metadata = db.get_slot_metadata(&sc, i as u32).unwrap().unwrap();
            let chunk = db.get_latest_chunk(&sc, i as u32).unwrap().unwrap();

            assert_eq!(initial_metadata[i].0, slot_metadata);
            assert_eq!(initial_metadata[i].1.data, chunk);
        } else if i < 10 {
            // next five are wiped
            let slot_metadata = db.get_slot_metadata(&sc, i as u32).unwrap().unwrap();
            assert_eq!(slot_metadata.slot_id, i as u32);
            assert_eq!(slot_metadata.slot_version, 0);
            assert_eq!(slot_metadata.data_hash, Sha512Trunc256Sum([0x00; 32]));
            assert_eq!(slot_metadata.signature, MessageSignature::empty());

            let chunk = db.get_latest_chunk(&sc, i as u32).unwrap().unwrap();
            assert_eq!(chunk.len(), 0);
        } else {
            // final five are new
            let slot_metadata = db.get_slot_metadata(&sc, i as u32).unwrap().unwrap();
            assert_eq!(slot_metadata.slot_id, i as u32);
            assert_eq!(slot_metadata.slot_version, 0);
            assert_eq!(slot_metadata.data_hash, Sha512Trunc256Sum([0x00; 32]));
            assert_eq!(slot_metadata.signature, MessageSignature::empty());

            let chunk = db.get_latest_chunk(&sc, i as u32).unwrap().unwrap();
            assert_eq!(chunk.len(), 0);
        }
    }

    let db_slot_metadata = db.get_db_slot_metadata(&sc).unwrap();
    assert_eq!(db_slot_metadata.len(), reconfigured_pks.len());
    for (i, slot_md) in db_slot_metadata.iter().enumerate() {
        let slot_metadata = db.get_slot_metadata(&sc, i as u32).unwrap().unwrap();
        assert_eq!(slot_metadata, *slot_md);
    }

    // reconfigure with fewer slots
    let new_pks: Vec<_> = (0..10).map(|_| StacksPrivateKey::new()).collect();
    let reconfigured_pks = vec![
        // first five slots are unchanged
        pks[0], pks[1], pks[2], pks[3], pks[4],
        // next five slots are different, so their contents will be dropped and versions and write
        // timestamps reset
        new_pks[0], new_pks[1], new_pks[2], new_pks[3],
        new_pks[4],
        // slots 10-15 will disappear
    ];
    let reconfigured_addrs: Vec<_> = reconfigured_pks
        .iter()
        .map(|pk| {
            StacksAddress::from_public_keys(
                C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
                &AddressHashMode::SerializeP2PKH,
                1,
                &vec![StacksPublicKey::from_private(pk)],
            )
            .unwrap()
        })
        .collect();

    let tx = db.tx_begin(StackerDBConfig::noop()).unwrap();

    // reconfigure
    tx.reconfigure_stackerdb(
        &sc,
        &reconfigured_addrs
            .clone()
            .into_iter()
            .map(|addr| (addr, 1))
            .collect::<Vec<_>>(),
    )
    .unwrap();

    tx.commit().unwrap();

    for (i, pk) in new_pks.iter().enumerate() {
        if i < 5 {
            // first five are unchanged
            let chunk_data = StackerDBChunkData {
                slot_id: i as u32,
                slot_version: 1,
                sig: MessageSignature::empty(),
                data: vec![i as u8; 128],
            };

            let slot_metadata = db.get_slot_metadata(&sc, i as u32).unwrap().unwrap();
            let chunk = db.get_latest_chunk(&sc, i as u32).unwrap().unwrap();

            assert_eq!(initial_metadata[i].0, slot_metadata);
            assert_eq!(initial_metadata[i].1.data, chunk);
        } else if i < 10 {
            // next five are wiped
            let slot_metadata = db.get_slot_metadata(&sc, i as u32).unwrap().unwrap();
            assert_eq!(slot_metadata.slot_id, i as u32);
            assert_eq!(slot_metadata.slot_version, 0);
            assert_eq!(slot_metadata.data_hash, Sha512Trunc256Sum([0x00; 32]));
            assert_eq!(slot_metadata.signature, MessageSignature::empty());

            let chunk = db.get_latest_chunk(&sc, i as u32).unwrap().unwrap();
            assert_eq!(chunk.len(), 0);
        } else {
            // final five are gone
            let slot_metadata_opt = db.get_slot_metadata(&sc, i as u32).unwrap();
            assert!(slot_metadata_opt.is_none());
        }
    }

    let db_slot_metadata = db.get_db_slot_metadata(&sc).unwrap();
    assert_eq!(db_slot_metadata.len(), reconfigured_pks.len());
    for (i, slot_md) in db_slot_metadata.iter().enumerate() {
        let slot_metadata = db.get_slot_metadata(&sc, i as u32).unwrap().unwrap();
        assert_eq!(slot_metadata, *slot_md);
    }
}

// TODO: max chunk size
