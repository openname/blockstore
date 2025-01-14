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

use std::collections::HashSet;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use clarity::vm::types::{PrincipalData, QualifiedContractIdentifier, StacksAddressExtensions};
use clarity::vm::{ClarityName, ContractName, Value};
use stacks_common::codec::{Error as CodecError, StacksMessageCodec};
use stacks_common::types::chainstate::{
    BlockHeaderHash, ConsensusHash, StacksAddress, StacksPrivateKey,
};
use stacks_common::types::net::PeerHost;
use stacks_common::types::Address;
use stacks_common::util::hash::{to_hex, Hash160};

use super::TestRPC;
use crate::burnchains::Txid;
use crate::chainstate::stacks::db::blocks::test::*;
use crate::chainstate::stacks::db::test::{chainstate_path, instantiate_chainstate};
use crate::chainstate::stacks::db::{ExtendedStacksHeader, StacksChainState};
use crate::chainstate::stacks::{
    Error as chainstate_error, StacksTransaction, TokenTransferMemo, TransactionAnchorMode,
    TransactionAuth, TransactionPayload, TransactionPostConditionMode, TransactionVersion,
};
use crate::core::mempool::{decode_tx_stream, MemPoolSyncData, TxTag, MAX_BLOOM_COUNTER_TXS};
use crate::core::{MemPoolDB, BLOCK_LIMIT_MAINNET_21};
use crate::net::api::postmempoolquery::StacksMemPoolStream;
use crate::net::api::*;
use crate::net::connection::ConnectionOptions;
use crate::net::http::HttpChunkGenerator;
use crate::net::httpcore::{
    HttpRequestContentsExtensions, RPCRequestHandler, StacksHttp, StacksHttpRequest,
};
use crate::net::{Error as NetError, ProtocolFamily, TipRequest};
use crate::util_lib::db::DBConn;

#[test]
fn test_try_parse_request() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 33333);
    let mut http = StacksHttp::new(addr.clone(), &ConnectionOptions::default());

    let request = StacksHttpRequest::new_mempool_query(
        addr.into(),
        MemPoolSyncData::TxTags([0x11; 32], vec![TxTag([0x22; 8])]),
        Some(Txid([0x33; 32])),
    );
    let bytes = request.try_serialize().unwrap();

    debug!("Request:\n{}\n", std::str::from_utf8(&bytes).unwrap());

    let (parsed_preamble, offset) = http.read_preamble(&bytes).unwrap();
    let mut handler = postmempoolquery::RPCMempoolQueryRequestHandler::new();
    let mut parsed_request = http
        .handle_try_parse_request(
            &mut handler,
            &parsed_preamble.expect_request(),
            &bytes[offset..],
        )
        .unwrap();

    assert_eq!(handler.page_id, Some(Txid([0x33; 32])));
    assert_eq!(
        handler.mempool_query,
        Some(MemPoolSyncData::TxTags([0x11; 32], vec![TxTag([0x22; 8])]))
    );

    // parsed request consumes headers that would not be in a constructed reqeuest
    parsed_request.clear_headers();
    let (preamble, contents) = parsed_request.destruct();

    assert_eq!(&preamble, request.preamble());

    handler.restart();
    assert!(handler.page_id.is_none());
    assert!(handler.mempool_query.is_none());
}

#[test]
fn test_try_make_response() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 33333);

    let test_rpc = TestRPC::setup(function_name!());
    let mempool_txids = test_rpc.mempool_txids.clone();
    let mempool_txids: HashSet<_> = mempool_txids.iter().map(|txid| txid.clone()).collect();

    let sync_data = test_rpc
        .peer_1
        .mempool
        .as_ref()
        .unwrap()
        .make_mempool_sync_data()
        .unwrap();

    let mut requests = vec![];
    let request = StacksHttpRequest::new_mempool_query(
        addr.into(),
        MemPoolSyncData::TxTags([0x00; 32], vec![]),
        Some(Txid([0x00; 32])),
    );
    requests.push(request);

    let mut responses = test_rpc.run(requests);

    let response = responses.remove(0);

    let (txs, page) = response.decode_mempool_txs_page().unwrap();
    let received_txids: HashSet<_> = txs.iter().map(|tx| tx.txid()).collect();

    assert_eq!(received_txids, mempool_txids);
    assert!(page.is_none());
}

#[test]
fn test_stream_mempool_txs() {
    let mut chainstate = instantiate_chainstate(false, 0x80000000, function_name!());
    let chainstate_path = chainstate_path(function_name!());
    let mut mempool = MemPoolDB::open_test(false, 0x80000000, &chainstate_path).unwrap();

    let addr = StacksAddress {
        version: 1,
        bytes: Hash160([0xff; 20]),
    };
    let mut txs = vec![];
    let block_height = 10;
    let mut total_len = 0;

    let mut mempool_tx = mempool.tx_begin().unwrap();
    for i in 0..10 {
        let pk = StacksPrivateKey::new();
        let mut tx = StacksTransaction {
            version: TransactionVersion::Testnet,
            chain_id: 0x80000000,
            auth: TransactionAuth::from_p2pkh(&pk).unwrap(),
            anchor_mode: TransactionAnchorMode::Any,
            post_condition_mode: TransactionPostConditionMode::Allow,
            post_conditions: vec![],
            payload: TransactionPayload::TokenTransfer(
                addr.to_account_principal(),
                123,
                TokenTransferMemo([0u8; 34]),
            ),
        };
        tx.set_tx_fee(1000);
        tx.set_origin_nonce(0);

        let txid = tx.txid();
        let tx_bytes = tx.serialize_to_vec();
        let origin_addr = tx.origin_address();
        let origin_nonce = tx.get_origin_nonce();
        let sponsor_addr = tx.sponsor_address().unwrap_or(origin_addr.clone());
        let sponsor_nonce = tx.get_sponsor_nonce().unwrap_or(origin_nonce);
        let tx_fee = tx.get_tx_fee();

        total_len += tx_bytes.len();

        // should succeed
        MemPoolDB::try_add_tx(
            &mut mempool_tx,
            &mut chainstate,
            &ConsensusHash([0x1 + (block_height as u8); 20]),
            &BlockHeaderHash([0x2 + (block_height as u8); 32]),
            false, // don't resolve the above chain tip since it doesn't exist
            txid.clone(),
            tx_bytes,
            tx_fee,
            block_height,
            &origin_addr,
            origin_nonce,
            &sponsor_addr,
            sponsor_nonce,
            None,
        )
        .unwrap();

        eprintln!("Added {} {}", i, &txid);
        txs.push(tx);
    }
    mempool_tx.commit().unwrap();

    let mut buf = vec![];
    let mut tx_stream_data = StacksMemPoolStream::new(
        mempool.reopen(false).unwrap(),
        MemPoolSyncData::TxTags([0u8; 32], vec![]),
        MAX_BLOOM_COUNTER_TXS.into(),
        block_height,
        Some(Txid([0u8; 32])),
    );

    loop {
        let chunk = tx_stream_data.generate_next_chunk().unwrap();
        if chunk.is_empty() {
            break;
        }
        buf.extend_from_slice(&chunk[..]);
    }

    eprintln!("Read {} bytes of tx data", buf.len());

    // buf decodes to the list of txs we have
    let mut decoded_txs = vec![];
    let mut ptr = &buf[..];
    loop {
        let tx: StacksTransaction = match read_next::<StacksTransaction, _>(&mut ptr) {
            Ok(tx) => tx,
            Err(e) => match e {
                CodecError::ReadError(ref ioe) => match ioe.kind() {
                    io::ErrorKind::UnexpectedEof => {
                        eprintln!("out of transactions");
                        break;
                    }
                    _ => {
                        panic!("IO error: {:?}", &e);
                    }
                },
                _ => {
                    panic!("other error: {:?}", &e);
                }
            },
        };
        decoded_txs.push(tx);
    }

    let mut tx_set = HashSet::new();
    for tx in txs.iter() {
        tx_set.insert(tx.txid());
    }

    // the order won't be preserved
    assert_eq!(tx_set.len(), decoded_txs.len());
    for tx in decoded_txs {
        assert!(tx_set.contains(&tx.txid()));
    }

    // verify that we can stream through pagination, with an empty tx tags
    let mut page_id = Txid([0u8; 32]);
    let mut decoded_txs = vec![];
    loop {
        let mut tx_stream_data = StacksMemPoolStream::new(
            mempool.reopen(false).unwrap(),
            MemPoolSyncData::TxTags([0u8; 32], vec![]),
            1,
            block_height,
            Some(page_id),
        );

        let mut buf = vec![];
        loop {
            let chunk = tx_stream_data.generate_next_chunk().unwrap();
            if chunk.is_empty() {
                break;
            }
            buf.extend_from_slice(&chunk[..]);
        }

        // buf decodes to the list of txs we have, plus page ids
        let mut ptr = &buf[..];
        test_debug!("Decode {}", to_hex(ptr));
        let (mut next_txs, next_page) = decode_tx_stream(&mut ptr).unwrap();

        decoded_txs.append(&mut next_txs);

        // for fun, use a page ID that is actually a well-formed prefix of a transaction
        if let Some(ref tx) = decoded_txs.last() {
            let mut evil_buf = tx.serialize_to_vec();
            let mut evil_page_id = [0u8; 32];
            evil_page_id.copy_from_slice(&evil_buf[0..32]);
            evil_buf.extend_from_slice(&evil_page_id);

            test_debug!("Decode evil buf {}", &to_hex(&evil_buf));

            let (evil_next_txs, evil_next_page) = decode_tx_stream(&mut &evil_buf[..]).unwrap();

            // should still work
            assert_eq!(evil_next_txs.len(), 1);
            assert_eq!(evil_next_txs[0].txid(), tx.txid());
            assert_eq!(evil_next_page.unwrap().0[0..32], evil_buf[0..32]);
        }

        if let Some(next_page) = next_page {
            page_id = next_page;
        } else {
            break;
        }
    }

    // make sure we got them all
    let mut tx_set = HashSet::new();
    for tx in txs.iter() {
        tx_set.insert(tx.txid());
    }

    // the order won't be preserved
    assert_eq!(tx_set.len(), decoded_txs.len());
    for tx in decoded_txs {
        assert!(tx_set.contains(&tx.txid()));
    }

    // verify that we can stream through pagination, with a full bloom filter
    let mut page_id = Txid([0u8; 32]);
    let all_txs_tags: Vec<_> = txs
        .iter()
        .map(|tx| TxTag::from(&[0u8; 32], &tx.txid()))
        .collect();
    loop {
        let mut tx_stream_data = StacksMemPoolStream::new(
            mempool.reopen(false).unwrap(),
            MemPoolSyncData::TxTags([0u8; 32], all_txs_tags.clone()),
            1,
            block_height,
            Some(page_id),
        );

        let mut buf = vec![];
        loop {
            let chunk = tx_stream_data.generate_next_chunk().unwrap();
            if chunk.is_empty() {
                break;
            }
            buf.extend_from_slice(&chunk[..]);
        }

        // buf decodes to an empty list of txs, plus page ID
        let mut ptr = &buf[..];
        test_debug!("Decode {}", to_hex(ptr));
        let (next_txs, next_page) = decode_tx_stream(&mut ptr).unwrap();

        assert_eq!(next_txs.len(), 0);

        if let Some(next_page) = next_page {
            page_id = next_page;
        } else {
            break;
        }
    }
}

#[test]
fn test_decode_tx_stream() {
    let addr = StacksAddress {
        version: 1,
        bytes: Hash160([0xff; 20]),
    };
    let mut txs = vec![];
    for _i in 0..10 {
        let pk = StacksPrivateKey::new();
        let mut tx = StacksTransaction {
            version: TransactionVersion::Testnet,
            chain_id: 0x80000000,
            auth: TransactionAuth::from_p2pkh(&pk).unwrap(),
            anchor_mode: TransactionAnchorMode::Any,
            post_condition_mode: TransactionPostConditionMode::Allow,
            post_conditions: vec![],
            payload: TransactionPayload::TokenTransfer(
                addr.to_account_principal(),
                123,
                TokenTransferMemo([0u8; 34]),
            ),
        };
        tx.set_tx_fee(1000);
        tx.set_origin_nonce(0);
        txs.push(tx);
    }

    // valid empty tx stream
    let empty_stream = [0x11u8; 32];
    let (next_txs, next_page) = decode_tx_stream(&mut empty_stream.as_ref()).unwrap();
    assert_eq!(next_txs.len(), 0);
    assert_eq!(next_page, Some(Txid([0x11; 32])));

    // valid tx stream with a page id at the end
    let mut tx_stream: Vec<u8> = vec![];
    for tx in txs.iter() {
        tx.consensus_serialize(&mut tx_stream).unwrap();
    }
    tx_stream.extend_from_slice(&[0x22; 32]);

    let (next_txs, next_page) = decode_tx_stream(&mut &tx_stream[..]).unwrap();
    assert_eq!(next_txs, txs);
    assert_eq!(next_page, Some(Txid([0x22; 32])));

    // valid tx stream with _no_ page id at the end
    let mut partial_stream: Vec<u8> = vec![];
    txs[0].consensus_serialize(&mut partial_stream).unwrap();
    let (next_txs, next_page) = decode_tx_stream(&mut &partial_stream[..]).unwrap();
    assert_eq!(next_txs.len(), 1);
    assert_eq!(next_txs[0], txs[0]);
    assert!(next_page.is_none());

    // garbage tx stream
    let garbage_stream = [0xff; 256];
    let err = decode_tx_stream(&mut garbage_stream.as_ref());
    match err {
        Err(NetError::ExpectedEndOfStream) => {}
        x => {
            error!("did not fail: {:?}", &x);
            panic!();
        }
    }

    // tx stream that is too short
    let short_stream = [0x33u8; 33];
    let err = decode_tx_stream(&mut short_stream.as_ref());
    match err {
        Err(NetError::ExpectedEndOfStream) => {}
        x => {
            error!("did not fail: {:?}", &x);
            panic!();
        }
    }

    // tx stream has a tx, a page ID, and then another tx
    let mut interrupted_stream = vec![];
    txs[0].consensus_serialize(&mut interrupted_stream).unwrap();
    interrupted_stream.extend_from_slice(&[0x00u8; 32]);
    txs[1].consensus_serialize(&mut interrupted_stream).unwrap();

    let err = decode_tx_stream(&mut &interrupted_stream[..]);
    match err {
        Err(NetError::ExpectedEndOfStream) => {}
        x => {
            error!("did not fail: {:?}", &x);
            panic!();
        }
    }
}
