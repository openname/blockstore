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

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use clarity::vm::types::{QualifiedContractIdentifier, StacksAddressExtensions};
use clarity::vm::{ClarityName, ContractName};
use stacks_common::codec::StacksMessageCodec;
use stacks_common::types::chainstate::{
    ConsensusHash, StacksAddress, StacksBlockId, StacksPrivateKey,
};
use stacks_common::types::net::PeerHost;
use stacks_common::types::Address;

use super::TestRPC;
use crate::chainstate::stacks::db::blocks::test::*;
use crate::chainstate::stacks::db::test::instantiate_chainstate;
use crate::chainstate::stacks::db::{ExtendedStacksHeader, StacksChainState};
use crate::chainstate::stacks::{
    Error as chainstate_error, StacksBlock, StacksBlockHeader, StacksMicroblock,
};
use crate::net::api::getheaders::StacksHeaderStream;
use crate::net::api::*;
use crate::net::connection::ConnectionOptions;
use crate::net::http::HttpChunkGenerator;
use crate::net::httpcore::{
    HttpPreambleExtensions, HttpRequestContentsExtensions, RPCRequestHandler, StacksHttp,
    StacksHttpRequest,
};
use crate::net::{ProtocolFamily, TipRequest};
use crate::util_lib::db::DBConn;

#[test]
fn test_try_parse_request() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 33333);
    let mut http = StacksHttp::new(addr.clone(), &ConnectionOptions::default());

    let request = StacksHttpRequest::new_getheaders(
        addr.into(),
        2100,
        TipRequest::SpecificTip(StacksBlockId([0x22; 32])),
    );
    assert_eq!(
        request.contents().tip_request(),
        TipRequest::SpecificTip(StacksBlockId([0x22; 32]))
    );

    let bytes = request.try_serialize().unwrap();

    debug!("Request:\n{}\n", std::str::from_utf8(&bytes).unwrap());

    let (parsed_preamble, offset) = http.read_preamble(&bytes).unwrap();
    let mut handler = getheaders::RPCHeadersRequestHandler::new();
    let mut parsed_request = http
        .handle_try_parse_request(
            &mut handler,
            &parsed_preamble.expect_request(),
            &bytes[offset..],
        )
        .unwrap();

    // parsed request consumes headers that would not be in a constructed reqeuest
    parsed_request.clear_headers();
    let (preamble, contents) = parsed_request.destruct();

    // consumed path args
    assert_eq!(handler.quantity, Some(2100));
    assert_eq!(&preamble, request.preamble());

    handler.restart();
    assert!(handler.quantity.is_none());
}

#[test]
fn test_try_make_response() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 33333);

    let rpc_test = TestRPC::setup(function_name!());
    let stacks_chain_tip = rpc_test.canonical_tip.clone();
    let consensus_hash = rpc_test.consensus_hash.clone();

    let mut requests = vec![];

    // query existing headers
    let request =
        StacksHttpRequest::new_getheaders(addr.into(), 2100, TipRequest::UseLatestAnchoredTip);
    requests.push(request);

    // this fails if we use a microblock tip
    let request =
        StacksHttpRequest::new_getheaders(addr.into(), 2100, TipRequest::UseLatestUnconfirmedTip);
    requests.push(request);

    // query existing headers
    let request = StacksHttpRequest::new_getheaders(
        addr.into(),
        2100,
        TipRequest::SpecificTip(stacks_chain_tip.clone()),
    );
    requests.push(request);

    // query non-existant headers
    let request = StacksHttpRequest::new_getheaders(
        addr.into(),
        2100,
        TipRequest::SpecificTip(StacksBlockId([0x11; 32])),
    );
    requests.push(request);

    let mut responses = rpc_test.run(requests);

    // got the headers
    let response = responses.remove(0);
    debug!(
        "Response:\n{}\n",
        std::str::from_utf8(&response.try_serialize().unwrap()).unwrap()
    );

    let resp = response.decode_stacks_headers().unwrap();

    assert_eq!(resp.len(), 1);

    // fails on microblock tip
    let response = responses.remove(0);
    debug!(
        "Response:\n{}\n",
        std::str::from_utf8(&response.try_serialize().unwrap()).unwrap()
    );

    let (preamble, body) = response.destruct();

    assert_eq!(preamble.status_code, 404);

    // got the headers
    let response = responses.remove(0);
    debug!(
        "Response:\n{}\n",
        std::str::from_utf8(&response.try_serialize().unwrap()).unwrap()
    );

    let resp = response.decode_stacks_headers().unwrap();

    assert_eq!(resp.len(), 1);

    // no headers
    let response = responses.remove(0);
    debug!(
        "Response:\n{}\n",
        std::str::from_utf8(&response.try_serialize().unwrap()).unwrap()
    );

    let (preamble, body) = response.destruct();

    assert_eq!(preamble.status_code, 404);
}

fn stream_headers_to_vec(stream: &mut StacksHeaderStream) -> Vec<u8> {
    let mut header_bytes = vec![];
    loop {
        let mut next_bytes = stream.generate_next_chunk().unwrap();
        if next_bytes.is_empty() {
            break;
        }
        header_bytes.append(&mut next_bytes);
    }
    header_bytes
}

#[test]
fn test_stream_getheaders() {
    let mut chainstate = instantiate_chainstate(false, 0x80000000, function_name!());
    let privk = StacksPrivateKey::from_hex(
        "eb05c83546fdd2c79f10f5ad5434a90dd28f7e3acb7c092157aa1bc3656b012c01",
    )
    .unwrap();

    let mut blocks: Vec<StacksBlock> = vec![];
    let mut blocks_index_hashes: Vec<StacksBlockId> = vec![];

    // make a linear stream
    for i in 0..32 {
        let mut block = make_empty_coinbase_block(&privk);

        if i == 0 {
            block.header.total_work.work = 1;
            block.header.total_work.burn = 1;
        }
        if i > 0 {
            block.header.parent_block = blocks.get(i - 1).unwrap().block_hash();
            block.header.total_work.work = blocks.get(i - 1).unwrap().header.total_work.work + 1;
            block.header.total_work.burn = blocks.get(i - 1).unwrap().header.total_work.burn + 1;
        }

        let consensus_hash = ConsensusHash([((i + 1) as u8); 20]);
        let parent_consensus_hash = ConsensusHash([(i as u8); 20]);

        store_staging_block(
            &mut chainstate,
            &consensus_hash,
            &block,
            &parent_consensus_hash,
            i as u64,
            i as u64,
        );

        blocks_index_hashes.push(StacksBlockHeader::make_index_block_hash(
            &consensus_hash,
            &block.block_hash(),
        ));
        blocks.push(block);
    }

    let mut blocks_fork = blocks[0..16].to_vec();
    let mut blocks_fork_index_hashes = blocks_index_hashes[0..16].to_vec();

    // make a stream that branches off
    for i in 16..32 {
        let mut block = make_empty_coinbase_block(&privk);

        if i == 16 {
            block.header.parent_block = blocks.get(i - 1).unwrap().block_hash();
            block.header.total_work.work = blocks.get(i - 1).unwrap().header.total_work.work + 1;
            block.header.total_work.burn = blocks.get(i - 1).unwrap().header.total_work.burn + 2;
        } else {
            block.header.parent_block = blocks_fork.get(i - 1).unwrap().block_hash();
            block.header.total_work.work =
                blocks_fork.get(i - 1).unwrap().header.total_work.work + 1;
            block.header.total_work.burn =
                blocks_fork.get(i - 1).unwrap().header.total_work.burn + 2;
        }

        let consensus_hash = ConsensusHash([((i + 1) as u8) | 0x80; 20]);
        let parent_consensus_hash = if i == 16 {
            ConsensusHash([(i as u8); 20])
        } else {
            ConsensusHash([(i as u8) | 0x80; 20])
        };

        store_staging_block(
            &mut chainstate,
            &consensus_hash,
            &block,
            &parent_consensus_hash,
            i as u64,
            i as u64,
        );

        blocks_fork_index_hashes.push(StacksBlockHeader::make_index_block_hash(
            &consensus_hash,
            &block.block_hash(),
        ));
        blocks_fork.push(block);
    }

    // can't stream a non-existant header
    assert!(StacksHeaderStream::new(&chainstate, &StacksBlockId([0x11; 32]), 1).is_err());

    // stream back individual headers
    for i in 0..blocks.len() {
        let mut stream = StacksHeaderStream::new(&chainstate, &blocks_index_hashes[i], 1).unwrap();
        let next_header_bytes = stream_headers_to_vec(&mut stream);

        test_debug!("Got {} total bytes", next_header_bytes.len());
        test_debug!(
            "bytes: '{}'",
            std::str::from_utf8(&next_header_bytes).unwrap()
        );
        let header: Vec<ExtendedStacksHeader> =
            serde_json::from_reader(&mut &next_header_bytes[..]).unwrap();

        assert_eq!(header.len(), 1);
        let header = header[0].clone();
        assert_eq!(header.consensus_hash, ConsensusHash([(i + 1) as u8; 20]));
        assert_eq!(header.header, blocks[i].header);

        if i > 0 {
            assert_eq!(header.parent_block_id, blocks_index_hashes[i - 1]);
        }
    }

    // stream back a run of headers
    let block_expected_headers: Vec<StacksBlockHeader> =
        blocks.iter().rev().map(|blk| blk.header.clone()).collect();

    let block_expected_index_hashes: Vec<StacksBlockId> =
        blocks_index_hashes.iter().rev().copied().collect();

    let block_fork_expected_headers: Vec<StacksBlockHeader> = blocks_fork
        .iter()
        .rev()
        .map(|blk| blk.header.clone())
        .collect();

    let block_fork_expected_index_hashes: Vec<StacksBlockId> =
        blocks_fork_index_hashes.iter().rev().copied().collect();

    // get them all -- ask for more than there is
    let mut stream =
        StacksHeaderStream::new(&chainstate, blocks_index_hashes.last().unwrap(), 4096).unwrap();
    let header_bytes = stream_headers_to_vec(&mut stream);

    eprintln!(
        "headers: {}",
        String::from_utf8(header_bytes.clone()).unwrap()
    );
    let headers: Vec<ExtendedStacksHeader> =
        serde_json::from_reader(&mut &header_bytes[..]).unwrap();

    assert_eq!(headers.len(), block_expected_headers.len());
    for ((i, h), eh) in headers
        .iter()
        .enumerate()
        .zip(block_expected_headers.iter())
    {
        assert_eq!(h.header, *eh);
        assert_eq!(h.consensus_hash, ConsensusHash([(32 - i) as u8; 20]));
        if i + 1 < block_expected_index_hashes.len() {
            assert_eq!(h.parent_block_id, block_expected_index_hashes[i + 1]);
        }
    }

    let mut stream =
        StacksHeaderStream::new(&chainstate, blocks_fork_index_hashes.last().unwrap(), 4096)
            .unwrap();
    let header_bytes = stream_headers_to_vec(&mut stream);
    let fork_headers: Vec<ExtendedStacksHeader> =
        serde_json::from_reader(&mut &header_bytes[..]).unwrap();

    assert_eq!(fork_headers.len(), block_fork_expected_headers.len());
    for ((i, h), eh) in fork_headers
        .iter()
        .enumerate()
        .zip(block_fork_expected_headers.iter())
    {
        let consensus_hash = if i >= 16 {
            ConsensusHash([((32 - i) as u8); 20])
        } else {
            ConsensusHash([((32 - i) as u8) | 0x80; 20])
        };

        assert_eq!(h.header, *eh);
        assert_eq!(h.consensus_hash, consensus_hash);
        if i + 1 < block_fork_expected_index_hashes.len() {
            assert_eq!(h.parent_block_id, block_fork_expected_index_hashes[i + 1]);
        }
    }

    assert_eq!(fork_headers[16..32], headers[16..32]);

    // ask for only a few
    let mut stream =
        StacksHeaderStream::new(&chainstate, blocks_index_hashes.last().unwrap(), 10).unwrap();
    let header_bytes = stream_headers_to_vec(&mut stream);
    eprintln!(
        "header bytes: {}",
        String::from_utf8(header_bytes.clone()).unwrap()
    );

    let headers: Vec<ExtendedStacksHeader> =
        serde_json::from_reader(&mut &header_bytes[..]).unwrap();

    assert_eq!(headers.len(), 10);
    for (i, hdr) in headers.iter().enumerate() {
        assert_eq!(hdr.header, block_expected_headers[i]);
        assert_eq!(hdr.parent_block_id, block_expected_index_hashes[i + 1]);
    }

    // ask for only a few
    let mut stream =
        StacksHeaderStream::new(&chainstate, &blocks_fork_index_hashes.last().unwrap(), 10)
            .unwrap();
    let header_bytes = stream_headers_to_vec(&mut stream);
    let headers: Vec<ExtendedStacksHeader> =
        serde_json::from_reader(&mut &header_bytes[..]).unwrap();

    assert_eq!(headers.len(), 10);
    for (i, hdr) in headers.iter().enumerate() {
        assert_eq!(hdr.header, block_fork_expected_headers[i]);
        assert_eq!(hdr.parent_block_id, block_fork_expected_index_hashes[i + 1]);
    }
}
