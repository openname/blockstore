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

use std::io::{Read, Write};

use clarity::vm::costs::ExecutionCost;
use regex::{Captures, Regex};
use stacks_common::codec::{Error as CodecError, StacksMessageCodec, MAX_PAYLOAD_LEN};
use stacks_common::types::chainstate::{
    BlockHeaderHash, ConsensusHash, StacksBlockId, StacksPublicKey,
};
use stacks_common::types::net::PeerHost;
use stacks_common::types::StacksPublicKeyBuffer;
use stacks_common::util::hash::{hex_bytes, Hash160, Sha256Sum};
use stacks_common::util::retry::BoundReader;

use crate::burnchains::affirmation::AffirmationMap;
use crate::burnchains::Txid;
use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::stacks::db::blocks::MINIMUM_TX_FEE_RATE_PER_BYTE;
use crate::chainstate::stacks::db::StacksChainState;
use crate::chainstate::stacks::{
    Error as ChainError, StacksBlockHeader, StacksMicroblock, StacksTransaction, TransactionPayload,
};
use crate::core::mempool::MemPoolDB;
use crate::cost_estimates::FeeRateEstimate;
use crate::net::http::{
    parse_json, Error, HttpBadRequest, HttpContentType, HttpNotFound, HttpRequest,
    HttpRequestContents, HttpRequestPreamble, HttpResponse, HttpResponseContents,
    HttpResponsePayload, HttpResponsePreamble, HttpServerError,
};
use crate::net::httpcore::{
    HttpPreambleExtensions, HttpRequestContentsExtensions, RPCRequestHandler, StacksHttpRequest,
    StacksHttpResponse,
};
use crate::net::p2p::PeerNetwork;
use crate::net::relay::Relayer;
use crate::net::{
    Attachment, Error as NetError, MicroblocksData, StacksMessageType, StacksNodeState, TipRequest,
};

#[derive(Clone)]
pub struct RPCPostMicroblockRequestHandler {
    pub microblock: Option<StacksMicroblock>,
}

impl RPCPostMicroblockRequestHandler {
    pub fn new() -> Self {
        Self { microblock: None }
    }

    /// Decode a bare block from the body
    fn parse_postmicroblock_octets(mut body: &[u8]) -> Result<StacksMicroblock, Error> {
        let mblock = StacksMicroblock::consensus_deserialize(&mut body).map_err(|e| {
            if let CodecError::DeserializeError(msg) = e {
                Error::DecodeError(format!("Failed to deserialize posted microblock: {}", msg))
            } else {
                e.into()
            }
        })?;
        Ok(mblock)
    }
}

/// Decode the HTTP request
impl HttpRequest for RPCPostMicroblockRequestHandler {
    fn verb(&self) -> &'static str {
        "POST"
    }

    fn path_regex(&self) -> Regex {
        Regex::new(r#"^/v2/microblocks$"#).unwrap()
    }

    fn metrics_identifier(&self) -> &str {
        "/v2/microblocks"
    }

    /// Try to decode this request.
    /// There's nothing to load here, so just make sure the request is well-formed.
    fn try_parse_request(
        &mut self,
        preamble: &HttpRequestPreamble,
        _captures: &Captures,
        query: Option<&str>,
        body: &[u8],
    ) -> Result<HttpRequestContents, Error> {
        if preamble.get_content_length() == 0 {
            return Err(Error::DecodeError(
                "Invalid Http request: expected non-zero-length body for PostMicroblock"
                    .to_string(),
            ));
        }

        if preamble.get_content_length() > MAX_PAYLOAD_LEN {
            return Err(Error::DecodeError(
                "Invalid Http request: PostMicroblock body is too big".to_string(),
            ));
        }

        if Some(HttpContentType::Bytes) != preamble.content_type || preamble.content_type.is_none()
        {
            return Err(Error::DecodeError(
                "Invalid Http request: PostMicroblock takes application/octet-stream".to_string(),
            ));
        }

        let microblock = Self::parse_postmicroblock_octets(body)?;
        self.microblock = Some(microblock);

        Ok(HttpRequestContents::new().query_string(query))
    }
}

impl RPCRequestHandler for RPCPostMicroblockRequestHandler {
    /// Reset internal state
    fn restart(&mut self) {
        self.microblock = None;
    }

    /// Make the response
    fn try_handle_request(
        &mut self,
        preamble: HttpRequestPreamble,
        contents: HttpRequestContents,
        node: &mut StacksNodeState,
    ) -> Result<(HttpResponsePreamble, HttpResponseContents), NetError> {
        let microblock = self
            .microblock
            .take()
            .ok_or(NetError::SendError("`microblock` not set".into()))?;
        let tip = match node.load_stacks_chain_tip(&preamble, &contents) {
            Ok(tip) => tip,
            Err(error_resp) => {
                return error_resp.try_into_contents().map_err(NetError::from);
            }
        };
        let data_resp = node.with_node_state(|_network, sortdb, chainstate, _mempool, _rpc_args| {
            let stacks_tip = match StacksChainState::load_staging_block_info(chainstate.db(), &tip) {
                Ok(Some(tip_info)) => tip_info,
                Ok(None) => {
                    return Err(StacksHttpResponse::new_error(&preamble, &HttpNotFound::new("No such stacks tip".into())));
                },
                Err(e) => {
                    return Err(StacksHttpResponse::new_error(&preamble, &HttpServerError::new(format!("Failed to load chain tip: {:?}", &e))));
                }
            };

            let consensus_hash = &stacks_tip.consensus_hash;
            let block_hash = &stacks_tip.anchored_block_hash;

            // make sure we can accept this
            let ch_sn = match SortitionDB::get_block_snapshot_consensus(sortdb.conn(), consensus_hash) {
                Ok(Some(sn)) => sn,
                Ok(None) => {
                    return Err(StacksHttpResponse::new_error(&preamble, &HttpNotFound::new("No such snapshot for Stacks tip consensus hash".to_string())));
                }
                Err(e) => {
                    debug!("No block snapshot for consensus hash {}", &consensus_hash);
                    return Err(StacksHttpResponse::new_error(&preamble, &HttpBadRequest::new_json(ChainError::DBError(e).into_json())));
                }
            };

            let sort_handle = sortdb.index_handle(&ch_sn.sortition_id);
            let parent_block_snapshot = Relayer::get_parent_stacks_block_snapshot(&sort_handle, consensus_hash, block_hash)
                .map_err(|e| StacksHttpResponse::new_error(&preamble, &HttpServerError::new(format!("Failed to load parent block for Stacks tip: {:?}", &e))))?;

            let ast_rules = SortitionDB::get_ast_rules(&sort_handle, parent_block_snapshot.block_height)
                .map_err(|e| StacksHttpResponse::new_error(&preamble, &HttpServerError::new(format!("Failed to load AST rules for Bitcoin block height {}: {:?}", parent_block_snapshot.block_height, &e))))?;

            let epoch_id = self.get_stacks_epoch(&preamble, sortdb, parent_block_snapshot.block_height)?.epoch_id;

            if !Relayer::static_check_problematic_relayed_microblock(
                chainstate.mainnet,
                epoch_id,
                &microblock,
                ast_rules,
            ) {
                info!("Microblock {} from {}/{} is problematic; will not store or relay it, nor its descendants", &microblock.block_hash(), consensus_hash, &block_hash);

                // NOTE: txid is ignored in chainstate error .into_json()
                return Err(StacksHttpResponse::new_error(&preamble, &HttpBadRequest::new_json(ChainError::ProblematicTransaction(Txid([0x00; 32])).into_json())));
            }

            match chainstate.preprocess_streamed_microblock(consensus_hash, block_hash, &microblock) {
                Ok(accepted) => {
                    debug!("{} uploaded microblock {}/{}-{}",
                           if accepted { "Accepted" } else { "Did not accept" },
                           consensus_hash,
                           block_hash,
                           &microblock.block_hash()
                    );
                    return Ok((accepted, StacksBlockHeader::make_index_block_hash(consensus_hash, block_hash)));
                },
                Err(e) => {
                    debug!("Failed to process microblock {}/{}-{}: {:?}", &consensus_hash, &block_hash, &microblock.block_hash(), &e);
                    return Err(StacksHttpResponse::new_error(&preamble, &HttpBadRequest::new_json(e.into_json())));
                }
            }
        });

        let (accepted, parent_block_id, data_resp) = match data_resp {
            Ok((accepted, parent_block_id)) => (accepted, parent_block_id, microblock.block_hash()),
            Err(response) => {
                return response.try_into_contents().map_err(NetError::from);
            }
        };

        // don't forget to forward this to the p2p network!
        if accepted {
            node.set_relay_message(StacksMessageType::Microblocks(MicroblocksData {
                index_anchor_block: parent_block_id,
                microblocks: vec![microblock],
            }));
        }

        let mut preamble = HttpResponsePreamble::ok_json(&preamble);
        preamble.set_canonical_stacks_tip_height(Some(node.canonical_stacks_tip_height()));
        let body = HttpResponseContents::try_from_json(&data_resp)?;
        Ok((preamble, body))
    }
}

/// Decode the HTTP response
impl HttpResponse for RPCPostMicroblockRequestHandler {
    fn try_parse_response(
        &self,
        preamble: &HttpResponsePreamble,
        body: &[u8],
    ) -> Result<HttpResponsePayload, Error> {
        let mblock_hash: BlockHeaderHash = parse_json(preamble, body)?;
        Ok(HttpResponsePayload::try_from_json(mblock_hash)?)
    }
}

impl StacksHttpRequest {
    /// Make a new post-microblock request
    pub fn new_post_microblock(
        host: PeerHost,
        mblock: StacksMicroblock,
        tip_req: TipRequest,
    ) -> StacksHttpRequest {
        StacksHttpRequest::new_for_peer(
            host,
            "POST".into(),
            "/v2/microblocks".into(),
            HttpRequestContents::new()
                .payload_stacks(&mblock)
                .for_tip(tip_req),
        )
        .expect("FATAL: failed to construct request from infallible data")
    }
}

impl StacksHttpResponse {
    pub fn decode_stacks_microblock_response(self) -> Result<BlockHeaderHash, NetError> {
        let contents = self.get_http_payload_ok()?;
        let response_json: serde_json::Value = contents.try_into()?;
        let result: BlockHeaderHash = serde_json::from_value(response_json)
            .map_err(|_e| NetError::DeserializeError("Failed to load from JSON".to_string()))?;
        Ok(result)
    }
}
