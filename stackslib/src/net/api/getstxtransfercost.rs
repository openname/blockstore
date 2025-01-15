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
use stacks_common::types::chainstate::{
    BlockHeaderHash, ConsensusHash, StacksBlockId, StacksPublicKey,
};
use stacks_common::types::net::PeerHost;
use stacks_common::types::StacksPublicKeyBuffer;
use stacks_common::util::hash::{Hash160, Sha256Sum};
use url::form_urlencoded;

use crate::burnchains::affirmation::AffirmationMap;
use crate::burnchains::Txid;
use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::stacks::db::blocks::MINIMUM_TX_FEE_RATE_PER_BYTE;
use crate::chainstate::stacks::db::StacksChainState;
use crate::core::mempool::MemPoolDB;
use crate::net::api::postfeerate::RPCPostFeeRateRequestHandler;
use crate::net::http::{
    parse_json, Error, HttpBadRequest, HttpRequest, HttpRequestContents, HttpRequestPreamble,
    HttpResponse, HttpResponseContents, HttpResponsePayload, HttpResponsePreamble,
};
use crate::net::httpcore::{
    HttpPreambleExtensions, RPCRequestHandler, StacksHttpRequest, StacksHttpResponse,
};
use crate::net::p2p::PeerNetwork;
use crate::net::{Error as NetError, HttpServerError, StacksNodeState};

pub(crate) const SINGLESIG_TX_TRANSFER_LEN: u64 = 180;

#[derive(Clone)]
pub struct RPCGetStxTransferCostRequestHandler {}

impl RPCGetStxTransferCostRequestHandler {
    pub fn new() -> Self {
        Self {}
    }
}

/// Decode the HTTP request
impl HttpRequest for RPCGetStxTransferCostRequestHandler {
    fn verb(&self) -> &'static str {
        "GET"
    }

    fn path_regex(&self) -> Regex {
        Regex::new(r#"^/v2/fees/transfer$"#).unwrap()
    }

    fn metrics_identifier(&self) -> &str {
        "/v2/fees/transfer"
    }

    /// Try to decode this request.
    /// There's nothing to load here, so just make sure the request is well-formed.
    fn try_parse_request(
        &mut self,
        preamble: &HttpRequestPreamble,
        _captures: &Captures,
        query: Option<&str>,
        _body: &[u8],
    ) -> Result<HttpRequestContents, Error> {
        if preamble.get_content_length() != 0 {
            return Err(Error::DecodeError(
                "Invalid Http request: expected 0-length body".to_string(),
            ));
        }
        Ok(HttpRequestContents::new().query_string(query))
    }
}

impl RPCRequestHandler for RPCGetStxTransferCostRequestHandler {
    /// Reset internal state
    fn restart(&mut self) {}

    /// Make the response
    fn try_handle_request(
        &mut self,
        preamble: HttpRequestPreamble,
        _contents: HttpRequestContents,
        node: &mut StacksNodeState,
    ) -> Result<(HttpResponsePreamble, HttpResponseContents), NetError> {
        // NOTE: The estimated length isn't needed per se because we're returning a fee rate, but
        // we do need an absolute length to use the estimator (so supply a common one).
        let estimated_len = SINGLESIG_TX_TRANSFER_LEN;

        let fee_resp = node.with_node_state(|_network, sortdb, _chainstate, _mempool, rpc_args| {
            let tip = self.get_canonical_burn_chain_tip(&preamble, sortdb)?;
            let stacks_epoch = self.get_stacks_epoch(&preamble, sortdb, tip.block_height)?;

            if let Some((_, fee_estimator, metric)) = rpc_args.get_estimators_ref() {
                // STX transfer transactions have zero runtime cost
                let estimated_cost = ExecutionCost::ZERO;
                let estimations =
                    RPCPostFeeRateRequestHandler::estimate_tx_fee_from_cost_and_length(
                        &preamble,
                        fee_estimator,
                        metric,
                        estimated_cost,
                        estimated_len,
                        stacks_epoch,
                    )?
                    .estimations;
                if estimations.len() != 3 {
                    // logic bug, but treat as runtime error
                    return Err(StacksHttpResponse::new_error(
                        &preamble,
                        &HttpServerError::new(
                            "Logic error in fee estimation: did not get three estimates".into(),
                        ),
                    ));
                }

                // safety -- checked estimations.len() == 3 above
                let median_estimation = &estimations[1];

                // NOTE: this returns the fee _rate_
                Ok(median_estimation.fee / estimated_len)
            } else {
                // unlike `POST /v2/fees/transaction`, this method can't fail due to the
                // unavailability of cost estimation, so just assume the minimum fee.
                debug!("Fee and cost estimation not configured on this stacks node");
                Ok(MINIMUM_TX_FEE_RATE_PER_BYTE)
            }
        });

        let fee = match fee_resp {
            Ok(fee) => fee,
            Err(response) => {
                return response.try_into_contents().map_err(NetError::from);
            }
        };

        let mut preamble = HttpResponsePreamble::ok_json(&preamble);
        preamble.set_canonical_stacks_tip_height(Some(node.canonical_stacks_tip_height()));
        let body = HttpResponseContents::try_from_json(&fee)?;
        Ok((preamble, body))
    }
}

/// Decode the HTTP response
impl HttpResponse for RPCGetStxTransferCostRequestHandler {
    fn try_parse_response(
        &self,
        preamble: &HttpResponsePreamble,
        body: &[u8],
    ) -> Result<HttpResponsePayload, Error> {
        let fee: u64 = parse_json(preamble, body)?;
        Ok(HttpResponsePayload::try_from_json(fee)?)
    }
}

impl StacksHttpRequest {
    pub fn new_get_stx_transfer_cost(host: PeerHost) -> StacksHttpRequest {
        let contents = HttpRequestContents::new();
        StacksHttpRequest::new_for_peer(host, "GET".into(), "/v2/fees/transfer".into(), contents)
            .expect("FATAL: failed to construct request from infallible data")
    }
}

impl StacksHttpResponse {
    pub fn decode_stx_transfer_fee(self) -> Result<u64, NetError> {
        let contents = self.get_http_payload_ok()?;
        let response_json: serde_json::Value = contents.try_into()?;
        let fee: u64 = serde_json::from_value(response_json)
            .map_err(|_e| Error::DecodeError("Failed to decode JSON".to_string()))?;
        Ok(fee)
    }
}
