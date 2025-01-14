// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2024 Stacks Open Internet Foundation
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
use crate::chainstate::burn::db::sortdb::{SortitionDB, SortitionHandle};
use crate::chainstate::nakamoto::{NakamotoBlock, NakamotoChainState};
use crate::chainstate::stacks::db::blocks::test::*;
use crate::chainstate::stacks::db::StacksChainState;
use crate::chainstate::stacks::{
    Error as chainstate_error, StacksBlock, StacksBlockHeader, StacksMicroblock,
};
use crate::net::api::gettenure::NakamotoTenureStream;
use crate::net::api::*;
use crate::net::connection::ConnectionOptions;
use crate::net::http::HttpChunkGenerator;
use crate::net::httpcore::{
    HttpPreambleExtensions, HttpRequestContentsExtensions, RPCRequestHandler, StacksHttp,
    StacksHttpRequest,
};
use crate::net::test::TestEventObserver;
use crate::net::tests::inv::nakamoto::make_nakamoto_peer_from_invs;
use crate::net::{ProtocolFamily, TipRequest};
use crate::util_lib::db::DBConn;

#[test]
fn test_try_parse_request() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 33333);
    let mut http = StacksHttp::new(addr.clone(), &ConnectionOptions::default());

    let request =
        StacksHttpRequest::new_get_nakamoto_tenure(addr.into(), StacksBlockId([0x11; 32]), None);
    let bytes = request.try_serialize().unwrap();

    debug!("Request:\n{}\n", std::str::from_utf8(&bytes).unwrap());

    let (parsed_preamble, offset) = http.read_preamble(&bytes).unwrap();
    let mut handler = gettenure::RPCNakamotoTenureRequestHandler::new();
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
    assert_eq!(handler.block_id, Some(StacksBlockId([0x11; 32])));

    assert_eq!(&preamble, request.preamble());

    handler.restart();
    assert!(handler.block_id.is_none());
}

#[test]
fn test_try_make_response() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 33333);

    let test_observer = TestEventObserver::new();
    let rpc_test = TestRPC::setup_nakamoto(function_name!(), &test_observer);

    let nakamoto_chain_tip = rpc_test.canonical_tip.clone();
    let consensus_hash = rpc_test.consensus_hash.clone();

    let mut requests = vec![];

    // query existing tenure
    let request =
        StacksHttpRequest::new_get_nakamoto_tenure(addr.into(), nakamoto_chain_tip.clone(), None);
    requests.push(request);

    // TODO: mid-tenure?
    // TODO: just the start of the tenure?

    // query non-existant block
    let request =
        StacksHttpRequest::new_get_nakamoto_tenure(addr.into(), StacksBlockId([0x11; 32]), None);
    requests.push(request);

    let mut responses = rpc_test.run(requests);

    // got the block
    let response = responses.remove(0);
    let resp = response.decode_nakamoto_tenure().unwrap();

    info!("response: {:?}", &resp);
    assert_eq!(resp.len(), 10);
    assert_eq!(resp.first().unwrap().header.block_id(), nakamoto_chain_tip);

    // no block
    let response = responses.remove(0);
    let (preamble, body) = response.destruct();

    assert_eq!(preamble.status_code, 404);
}

#[test]
fn test_stream_nakamoto_tenure() {
    let test_observer = TestEventObserver::new();
    let bitvecs = vec![vec![
        true, true, true, true, true, true, true, true, true, true,
    ]];

    let mut peer =
        make_nakamoto_peer_from_invs(function_name!(), &test_observer, 10, 3, bitvecs.clone());

    // can't stream a nonexistant tenure
    assert!(NakamotoTenureStream::new(
        peer.chainstate(),
        StacksBlockId([0x11; 32]),
        ConsensusHash([0x22; 20]),
        StacksBlockId([0x33; 32]),
        None
    )
    .is_err());

    let nakamoto_tip = {
        let sortdb = peer.sortdb.take().unwrap();
        let tip = SortitionDB::get_canonical_burn_chain_tip(sortdb.conn()).unwrap();
        let ih = sortdb.index_handle(&tip.sortition_id);
        let nakamoto_tip = ih.get_nakamoto_tip().unwrap().unwrap();
        peer.sortdb = Some(sortdb);
        nakamoto_tip
    };

    let nakamoto_tip_block_id = StacksBlockId::new(&nakamoto_tip.0, &nakamoto_tip.1);
    let nakamoto_header = {
        let header_info = NakamotoChainState::get_block_header_nakamoto(
            peer.chainstate().db(),
            &nakamoto_tip_block_id,
        )
        .unwrap()
        .unwrap();
        header_info
            .anchored_header
            .as_stacks_nakamoto()
            .cloned()
            .unwrap()
    };

    let mut stream = NakamotoTenureStream::new(
        peer.chainstate(),
        nakamoto_tip_block_id.clone(),
        nakamoto_header.consensus_hash.clone(),
        nakamoto_header.parent_block_id.clone(),
        None,
    )
    .unwrap();
    let mut all_block_bytes = vec![];

    loop {
        let mut next_bytes = stream.generate_next_chunk().unwrap();
        if next_bytes.is_empty() {
            break;
        }
        test_debug!(
            "Got {} more bytes from staging; add to {} total",
            next_bytes.len(),
            all_block_bytes.len()
        );
        all_block_bytes.append(&mut next_bytes);
    }

    let ptr = &mut all_block_bytes.as_slice();
    let mut blocks = vec![];
    while !ptr.is_empty() {
        let block = NakamotoBlock::consensus_deserialize(ptr).unwrap();
        blocks.push(block);
    }

    info!("blocks = {:?}", &blocks);
    assert_eq!(blocks.len(), 10);
    assert_eq!(
        blocks.first().unwrap().header.block_id(),
        nakamoto_tip_block_id
    );
}
