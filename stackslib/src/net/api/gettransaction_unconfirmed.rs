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

use regex::{Captures, Regex};
use stacks_common::codec::StacksMessageCodec;
use stacks_common::types::chainstate::{
    BlockHeaderHash, ConsensusHash, StacksBlockId, StacksPublicKey,
};
use stacks_common::types::net::PeerHost;
use stacks_common::types::StacksPublicKeyBuffer;
use stacks_common::util::hash::{to_hex, Hash160, Sha256Sum};

use crate::burnchains::affirmation::AffirmationMap;
use crate::burnchains::Txid;
use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::stacks::db::StacksChainState;
use crate::core::mempool::MemPoolDB;
use crate::net::http::{
    parse_json, Error, HttpNotFound, HttpRequest, HttpRequestContents, HttpRequestPreamble,
    HttpResponse, HttpResponseContents, HttpResponsePayload, HttpResponsePreamble, HttpServerError,
};
use crate::net::httpcore::{
    request, HttpPreambleExtensions, RPCRequestHandler, StacksHttpRequest, StacksHttpResponse,
};
use crate::net::p2p::PeerNetwork;
use crate::net::{Error as NetError, StacksNodeState};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UnconfirmedTransactionStatus {
    Microblock {
        block_hash: BlockHeaderHash,
        seq: u16,
    },
    Mempool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnconfirmedTransactionResponse {
    pub tx: String,
    pub status: UnconfirmedTransactionStatus,
}

#[derive(Clone)]
pub struct RPCGetTransactionUnconfirmedRequestHandler {
    pub txid: Option<Txid>,
}
impl RPCGetTransactionUnconfirmedRequestHandler {
    pub fn new() -> Self {
        Self { txid: None }
    }
}

/// Decode the HTTP request
impl HttpRequest for RPCGetTransactionUnconfirmedRequestHandler {
    fn verb(&self) -> &'static str {
        "GET"
    }

    fn path_regex(&self) -> Regex {
        Regex::new(r#"^/v2/transactions/unconfirmed/(?P<txid>[0-9a-f]{64})$"#).unwrap()
    }

    fn metrics_identifier(&self) -> &str {
        "/v2/transactions/unconfirmed/:txid"
    }

    /// Try to decode this request.
    /// There's nothing to load here, so just make sure the request is well-formed.
    fn try_parse_request(
        &mut self,
        preamble: &HttpRequestPreamble,
        captures: &Captures,
        query: Option<&str>,
        _body: &[u8],
    ) -> Result<HttpRequestContents, Error> {
        if preamble.get_content_length() != 0 {
            return Err(Error::DecodeError(
                "Invalid Http request: expected 0-length body for GetTransactionUnconfirmed"
                    .to_string(),
            ));
        }

        let txid = request::get_txid(captures, "txid")?;
        self.txid = Some(txid);

        Ok(HttpRequestContents::new().query_string(query))
    }
}

impl RPCRequestHandler for RPCGetTransactionUnconfirmedRequestHandler {
    /// Reset internal state
    fn restart(&mut self) {
        self.txid = None;
    }

    /// Make the response
    fn try_handle_request(
        &mut self,
        preamble: HttpRequestPreamble,
        _contents: HttpRequestContents,
        node: &mut StacksNodeState,
    ) -> Result<(HttpResponsePreamble, HttpResponseContents), NetError> {
        let txid = self
            .txid
            .take()
            .ok_or(NetError::SendError("`txid` no set".into()))?;

        let txinfo_res =
            node.with_node_state(|_network, _sortdb, chainstate, mempool, _rpc_args| {
                // present in the unconfirmed state?
                if let Some(unconfirmed) = chainstate.unconfirmed_state.as_ref() {
                    if let Some((transaction, mblock_hash, seq)) =
                        unconfirmed.get_unconfirmed_transaction(&txid)
                    {
                        return Ok(UnconfirmedTransactionResponse {
                            status: UnconfirmedTransactionStatus::Microblock {
                                block_hash: mblock_hash,
                                seq,
                            },
                            tx: to_hex(&transaction.serialize_to_vec()),
                        });
                    }
                }

                // present in the mempool?
                if let Some(txinfo) = MemPoolDB::get_tx(mempool.conn(), &txid)? {
                    return Ok(UnconfirmedTransactionResponse {
                        status: UnconfirmedTransactionStatus::Mempool,
                        tx: to_hex(&txinfo.tx.serialize_to_vec()),
                    });
                }

                return Err(NetError::NotFoundError);
            });

        let txinfo = match txinfo_res {
            Ok(txinfo) => txinfo,
            Err(NetError::NotFoundError) => {
                return StacksHttpResponse::new_error(
                    &preamble,
                    &HttpNotFound::new(format!(
                        "Transaction {} not found in mempool or unconfirmed microblock stream",
                        &txid
                    )),
                )
                .try_into_contents()
                .map_err(NetError::from);
            }
            Err(e) => {
                return StacksHttpResponse::new_error(
                    &preamble,
                    &HttpServerError::new(format!(
                        "Failed to query transaction {}: {:?}",
                        &txid, &e
                    )),
                )
                .try_into_contents()
                .map_err(NetError::from);
            }
        };

        let mut preamble = HttpResponsePreamble::ok_json(&preamble);
        preamble.set_canonical_stacks_tip_height(Some(node.canonical_stacks_tip_height()));
        let body = HttpResponseContents::try_from_json(&txinfo)?;
        Ok((preamble, body))
    }
}

/// Decode the HTTP response
impl HttpResponse for RPCGetTransactionUnconfirmedRequestHandler {
    fn try_parse_response(
        &self,
        preamble: &HttpResponsePreamble,
        body: &[u8],
    ) -> Result<HttpResponsePayload, Error> {
        let txinfo: UnconfirmedTransactionResponse = parse_json(preamble, body)?;
        Ok(HttpResponsePayload::try_from_json(txinfo)?)
    }
}

impl StacksHttpRequest {
    /// Make a new get-unconfirmed-tx request
    pub fn new_gettransaction_unconfirmed(host: PeerHost, txid: Txid) -> StacksHttpRequest {
        StacksHttpRequest::new_for_peer(
            host,
            "GET".into(),
            format!("/v2/transactions/unconfirmed/{}", &txid),
            HttpRequestContents::new(),
        )
        .expect("FATAL: failed to construct request from infallible data")
    }
}

impl StacksHttpResponse {
    pub fn decode_gettransaction_unconfirmed(
        self,
    ) -> Result<UnconfirmedTransactionResponse, NetError> {
        let contents = self.get_http_payload_ok()?;
        let response_json: serde_json::Value = contents.try_into()?;
        let txinfo: UnconfirmedTransactionResponse = serde_json::from_value(response_json)
            .map_err(|_e| Error::DecodeError("Failed to decode JSON".to_string()))?;
        Ok(txinfo)
    }
}
