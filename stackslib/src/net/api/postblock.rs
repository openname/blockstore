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
    StacksBlock, StacksBlockHeader, StacksTransaction, TransactionPayload,
};
use crate::core::mempool::MemPoolDB;
use crate::cost_estimates::FeeRateEstimate;
use crate::net::http::{
    parse_json, Error, HttpBadRequest, HttpContentType, HttpNotFound, HttpRequest,
    HttpRequestContents, HttpRequestPreamble, HttpResponse, HttpResponseContents,
    HttpResponsePayload, HttpResponsePreamble, HttpServerError,
};
use crate::net::httpcore::{
    request, HttpPreambleExtensions, HttpRequestContentsExtensions, RPCRequestHandler,
    StacksHttpRequest, StacksHttpResponse,
};
use crate::net::p2p::PeerNetwork;
use crate::net::relay::{BlockAcceptResponse, Relayer};
use crate::net::{
    Attachment, BlocksData, BlocksDatum, Error as NetError, StacksMessageType, StacksNodeState,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StacksBlockAcceptedData {
    pub stacks_block_id: StacksBlockId,
    pub accepted: bool,
}

#[derive(Clone)]
pub struct RPCPostBlockRequestHandler {
    pub block: Option<StacksBlock>,
    pub consensus_hash: Option<ConsensusHash>,
}

impl RPCPostBlockRequestHandler {
    pub fn new() -> Self {
        Self {
            block: None,
            consensus_hash: None,
        }
    }

    /// Decode a bare block from the body
    fn parse_postblock_octets(mut body: &[u8]) -> Result<StacksBlock, Error> {
        let block = StacksBlock::consensus_deserialize(&mut body).map_err(|e| {
            if let CodecError::DeserializeError(msg) = e {
                Error::DecodeError(format!("Failed to deserialize posted transaction: {}", msg))
            } else {
                e.into()
            }
        })?;
        Ok(block)
    }
}

/// Decode the HTTP request
impl HttpRequest for RPCPostBlockRequestHandler {
    fn verb(&self) -> &'static str {
        "POST"
    }

    fn path_regex(&self) -> Regex {
        Regex::new(r#"^/v2/blocks/upload/(?P<consensus_hash>[0-9a-f]{40})$"#).unwrap()
    }

    fn metrics_identifier(&self) -> &str {
        "/v2/blocks/upload/:block"
    }

    /// Try to decode this request.
    /// There's nothing to load here, so just make sure the request is well-formed.
    fn try_parse_request(
        &mut self,
        preamble: &HttpRequestPreamble,
        captures: &Captures,
        query: Option<&str>,
        body: &[u8],
    ) -> Result<HttpRequestContents, Error> {
        if preamble.get_content_length() == 0 {
            return Err(Error::DecodeError(
                "Invalid Http request: expected non-zero-length body for PostBlock".to_string(),
            ));
        }

        if preamble.get_content_length() > MAX_PAYLOAD_LEN {
            return Err(Error::DecodeError(
                "Invalid Http request: PostBlock body is too big".to_string(),
            ));
        }

        if Some(HttpContentType::Bytes) != preamble.content_type || preamble.content_type.is_none()
        {
            return Err(Error::DecodeError(
                "Invalid Http request: PostBlock takes application/octet-stream".to_string(),
            ));
        }

        let consensus_hash = request::get_consensus_hash(captures, "consensus_hash")?;
        let block = Self::parse_postblock_octets(body)?;

        self.consensus_hash = Some(consensus_hash);
        self.block = Some(block);
        Ok(HttpRequestContents::new().query_string(query))
    }
}

impl RPCRequestHandler for RPCPostBlockRequestHandler {
    /// Reset internal state
    fn restart(&mut self) {
        self.consensus_hash = None;
        self.block = None;
    }

    /// Make the response
    fn try_handle_request(
        &mut self,
        preamble: HttpRequestPreamble,
        _contents: HttpRequestContents,
        node: &mut StacksNodeState,
    ) -> Result<(HttpResponsePreamble, HttpResponseContents), NetError> {
        // get out the request body
        let block = self
            .block
            .take()
            .ok_or(NetError::SendError("`block` not set".into()))?;
        let consensus_hash = self
            .consensus_hash
            .take()
            .ok_or(NetError::SendError("`consensus_hash` not set".into()))?;

        let block_hash = block.block_hash();

        let data_resp =
            node.with_node_state(|_network, sortdb, chainstate, _mempool, _rpc_args| {
                match SortitionDB::get_sortition_id_by_consensus(sortdb.conn(), &consensus_hash) {
                    Ok(Some(_)) => {
                        // we recognize this consensus hash
                        let ic = sortdb.index_conn();
                        match Relayer::process_new_anchored_block(
                            &ic,
                            chainstate,
                            &consensus_hash,
                            &block,
                            0,
                        ) {
                            Ok(accepted) => {
                                debug!(
                                    "Received POSTed Stacks block {}/{}: {:?}",
                                    &consensus_hash, &block_hash, &accepted
                                );
                                return Ok(BlockAcceptResponse::Accepted == accepted);
                            }
                            Err(e) => {
                                let msg = format!(
                                    "Failed to process anchored block {}/{}: {:?}",
                                    consensus_hash,
                                    &block.block_hash(),
                                    &e
                                );
                                error!("{}", &msg);
                                return Err(StacksHttpResponse::new_error(
                                    &preamble,
                                    &HttpServerError::new(msg),
                                ));
                            }
                        }
                    }
                    Ok(None) => {
                        let msg = format!(
                            "Unrecognized consensus hash {} for block {}",
                            consensus_hash,
                            &block.block_hash()
                        );
                        debug!("{}", &msg);
                        return Err(StacksHttpResponse::new_error(
                            &preamble,
                            &HttpNotFound::new(msg),
                        ));
                    }
                    Err(e) => {
                        let msg = format!(
                            "Failed to query sortition ID by consensus '{}': {:?}",
                            consensus_hash, &e
                        );
                        error!("{}", &msg);
                        return Err(StacksHttpResponse::new_error(
                            &preamble,
                            &HttpServerError::new(msg),
                        ));
                    }
                }
            });

        let data_resp = match data_resp {
            Ok(accepted) => StacksBlockAcceptedData {
                accepted,
                stacks_block_id: StacksBlockHeader::make_index_block_hash(
                    &consensus_hash,
                    &block_hash,
                ),
            },
            Err(response) => {
                return response.try_into_contents().map_err(NetError::from);
            }
        };

        // don't forget to forward this to the p2p network!
        if data_resp.accepted {
            node.set_relay_message(StacksMessageType::Blocks(BlocksData {
                blocks: vec![BlocksDatum(consensus_hash, block)],
            }));
        }

        let mut preamble = HttpResponsePreamble::ok_json(&preamble);
        preamble.set_canonical_stacks_tip_height(Some(node.canonical_stacks_tip_height()));
        let body = HttpResponseContents::try_from_json(&data_resp)?;
        Ok((preamble, body))
    }
}

/// Decode the HTTP response
impl HttpResponse for RPCPostBlockRequestHandler {
    fn try_parse_response(
        &self,
        preamble: &HttpResponsePreamble,
        body: &[u8],
    ) -> Result<HttpResponsePayload, Error> {
        let accepted: StacksBlockAcceptedData = parse_json(preamble, body)?;
        Ok(HttpResponsePayload::try_from_json(accepted)?)
    }
}

impl StacksHttpRequest {
    /// Make a new post-block request
    pub fn new_post_block(
        host: PeerHost,
        ch: ConsensusHash,
        block: StacksBlock,
    ) -> StacksHttpRequest {
        StacksHttpRequest::new_for_peer(
            host,
            "POST".into(),
            format!("/v2/blocks/upload/{}", &ch),
            HttpRequestContents::new().payload_stacks(&block),
        )
        .expect("FATAL: failed to construct request from infallible data")
    }
}

impl StacksHttpResponse {
    pub fn decode_stacks_block_accepted(self) -> Result<StacksBlockAcceptedData, NetError> {
        let contents = self.get_http_payload_ok()?;
        let response_json: serde_json::Value = contents.try_into()?;
        let result: StacksBlockAcceptedData = serde_json::from_value(response_json)
            .map_err(|_e| NetError::DeserializeError("Failed to load from JSON".to_string()))?;
        Ok(result)
    }
}
