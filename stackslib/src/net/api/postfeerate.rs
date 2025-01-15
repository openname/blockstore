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
use stacks_common::codec::{StacksMessageCodec, MAX_PAYLOAD_LEN};
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
use crate::chainstate::stacks::TransactionPayload;
use crate::core::mempool::MemPoolDB;
use crate::core::StacksEpoch;
use crate::cost_estimates::metrics::CostMetric;
use crate::cost_estimates::{CostEstimator, FeeEstimator, FeeRateEstimate};
use crate::net::http::{
    parse_json, Error, HttpBadRequest, HttpContentType, HttpNotFound, HttpRequest,
    HttpRequestContents, HttpRequestPreamble, HttpResponse, HttpResponseContents,
    HttpResponsePayload, HttpResponsePreamble, HttpServerError,
};
use crate::net::httpcore::{
    HttpPreambleExtensions, RPCRequestHandler, StacksHttpRequest, StacksHttpResponse,
};
use crate::net::p2p::PeerNetwork;
use crate::net::{Error as NetError, StacksNodeState};

#[derive(Serialize, Deserialize)]
pub struct FeeRateEstimateRequestBody {
    #[serde(default)]
    pub estimated_len: Option<u64>,
    pub transaction_payload: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RPCFeeEstimate {
    pub fee_rate: f64,
    pub fee: u64,
}

impl RPCFeeEstimate {
    pub fn estimate_fees(scalar: u64, fee_rates: FeeRateEstimate) -> Vec<RPCFeeEstimate> {
        let estimated_fees_f64 = fee_rates.clone() * (scalar as f64);
        vec![
            RPCFeeEstimate {
                fee: estimated_fees_f64.low as u64,
                fee_rate: fee_rates.low,
            },
            RPCFeeEstimate {
                fee: estimated_fees_f64.middle as u64,
                fee_rate: fee_rates.middle,
            },
            RPCFeeEstimate {
                fee: estimated_fees_f64.high as u64,
                fee_rate: fee_rates.high,
            },
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RPCFeeEstimateResponse {
    pub estimated_cost: ExecutionCost,
    pub estimated_cost_scalar: u64,
    pub estimations: Vec<RPCFeeEstimate>,
    pub cost_scalar_change_by_byte: f64,
}

#[derive(Clone)]
pub struct RPCPostFeeRateRequestHandler {
    pub estimated_len: Option<u64>,
    pub transaction_payload: Option<TransactionPayload>,
}

impl RPCPostFeeRateRequestHandler {
    pub fn new() -> Self {
        Self {
            estimated_len: None,
            transaction_payload: None,
        }
    }

    /// Estimate a transaction fee, given its execution cost estimation and length estimation
    /// and cost estimators.
    /// Returns Ok(fee structure) on success
    /// Returns Err(HTTP response) on error
    pub fn estimate_tx_fee_from_cost_and_length(
        preamble: &HttpRequestPreamble,
        fee_estimator: &dyn FeeEstimator,
        metric: &dyn CostMetric,
        estimated_cost: ExecutionCost,
        estimated_len: u64,
        stacks_epoch: StacksEpoch,
    ) -> Result<RPCFeeEstimateResponse, StacksHttpResponse> {
        let scalar_cost =
            metric.from_cost_and_len(&estimated_cost, &stacks_epoch.block_limit, estimated_len);
        let fee_rates = fee_estimator.get_rate_estimates().map_err(|e| {
            StacksHttpResponse::new_error(
                preamble,
                &HttpBadRequest::new(format!(
                    "Estimator RPC endpoint failed to estimate fees for tx: {:?}",
                    &e
                )),
            )
        })?;

        let mut estimations = RPCFeeEstimate::estimate_fees(scalar_cost, fee_rates).to_vec();

        let minimum_fee = estimated_len * MINIMUM_TX_FEE_RATE_PER_BYTE;

        for estimate in estimations.iter_mut() {
            if estimate.fee < minimum_fee {
                estimate.fee = minimum_fee;
            }
        }

        Ok(RPCFeeEstimateResponse {
            estimated_cost,
            estimations,
            estimated_cost_scalar: scalar_cost,
            cost_scalar_change_by_byte: metric.change_per_byte(),
        })
    }
}

/// Decode the HTTP request
impl HttpRequest for RPCPostFeeRateRequestHandler {
    fn verb(&self) -> &'static str {
        "POST"
    }

    fn path_regex(&self) -> Regex {
        Regex::new(r#"^/v2/fees/transaction$"#).unwrap()
    }

    fn metrics_identifier(&self) -> &str {
        "/v2/fees/transaction"
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
        let content_len = preamble.get_content_length();
        if !(content_len > 0 && content_len < MAX_PAYLOAD_LEN) {
            return Err(Error::DecodeError(format!(
                "Invalid Http request: invalid body length for FeeRateEstimate ({})",
                content_len
            )));
        }

        if preamble.content_type != Some(HttpContentType::JSON) {
            return Err(Error::DecodeError(
                "Invalid content-type: expected application/json".to_string(),
            ));
        }

        let body: FeeRateEstimateRequestBody = serde_json::from_slice(body)
            .map_err(|e| Error::DecodeError(format!("Failed to parse JSON body: {}", e)))?;

        let payload_hex = if body.transaction_payload.starts_with("0x") {
            &body.transaction_payload[2..]
        } else {
            &body.transaction_payload
        };

        let payload_data = hex_bytes(payload_hex).map_err(|_e| {
            Error::DecodeError("Bad hex string supplied for transaction payload".into())
        })?;

        let tx = TransactionPayload::consensus_deserialize(&mut payload_data.as_slice())?;
        let estimated_len =
            std::cmp::max(body.estimated_len.unwrap_or(0), payload_data.len() as u64);

        self.transaction_payload = Some(tx);
        self.estimated_len = Some(estimated_len);
        Ok(HttpRequestContents::new().query_string(query))
    }
}

impl RPCRequestHandler for RPCPostFeeRateRequestHandler {
    /// Reset internal state
    fn restart(&mut self) {
        self.estimated_len = None;
        self.transaction_payload = None;
    }

    /// Make the response
    /// TODO: accurately estimate the cost/length fee for token transfers, based on mempool
    /// pressure.
    fn try_handle_request(
        &mut self,
        preamble: HttpRequestPreamble,
        _contents: HttpRequestContents,
        node: &mut StacksNodeState,
    ) -> Result<(HttpResponsePreamble, HttpResponseContents), NetError> {
        let estimated_len = self
            .estimated_len
            .take()
            .ok_or(NetError::SendError("`estimated_len` not set".into()))?;
        let tx = self
            .transaction_payload
            .take()
            .ok_or(NetError::SendError("`transaction_payload` not set".into()))?;

        let data_resp =
            node.with_node_state(|_network, sortdb, _chainstate, _mempool, rpc_args| {
                let tip = self.get_canonical_burn_chain_tip(&preamble, sortdb)?;
                let stacks_epoch = self.get_stacks_epoch(&preamble, sortdb, tip.block_height)?;

                if let Some((cost_estimator, fee_estimator, metric)) = rpc_args.get_estimators_ref()
                {
                    let estimated_cost = cost_estimator
                        .estimate_cost(&tx, &stacks_epoch.epoch_id)
                        .map_err(|e| {
                            StacksHttpResponse::new_error(
                                &preamble,
                                &HttpBadRequest::new(format!(
                                    "Estimator RPC endpoint failed to estimate tx {}: {:?}",
                                    &tx.name(),
                                    &e
                                )),
                            )
                        })?;

                    Self::estimate_tx_fee_from_cost_and_length(
                        &preamble,
                        fee_estimator,
                        metric,
                        estimated_cost,
                        estimated_len,
                        stacks_epoch,
                    )
                } else {
                    debug!("Fee and cost estimation not configured on this stacks node");
                    Err(StacksHttpResponse::new_error(
                        &preamble,
                        &HttpBadRequest::new(
                            "Fee estimation not supported on this node".to_string(),
                        ),
                    ))
                }
            });

        let data_resp = match data_resp {
            Ok(data) => data,
            Err(response) => {
                return response.try_into_contents().map_err(NetError::from);
            }
        };

        let mut preamble = HttpResponsePreamble::ok_json(&preamble);
        preamble.set_canonical_stacks_tip_height(Some(node.canonical_stacks_tip_height()));
        let body = HttpResponseContents::try_from_json(&data_resp)?;
        Ok((preamble, body))
    }
}

/// Decode the HTTP response
impl HttpResponse for RPCPostFeeRateRequestHandler {
    fn try_parse_response(
        &self,
        preamble: &HttpResponsePreamble,
        body: &[u8],
    ) -> Result<HttpResponsePayload, Error> {
        let fee: RPCFeeEstimateResponse = parse_json(preamble, body)?;
        Ok(HttpResponsePayload::try_from_json(fee)?)
    }
}

impl StacksHttpResponse {
    pub fn decode_fee_estimate(self) -> Result<RPCFeeEstimateResponse, NetError> {
        let contents = self.get_http_payload_ok()?;
        let response_json: serde_json::Value = contents.try_into()?;
        let fee: RPCFeeEstimateResponse = serde_json::from_value(response_json)
            .map_err(|_e| Error::DecodeError("Failed to decode JSON".to_string()))?;
        Ok(fee)
    }
}

impl StacksHttpRequest {
    pub fn new_post_fee_rate(
        host: PeerHost,
        fee_request: FeeRateEstimateRequestBody,
    ) -> StacksHttpRequest {
        StacksHttpRequest::new_for_peer(
            host,
            "POST".into(),
            "/v2/fees/transaction".into(),
            HttpRequestContents::new().payload_json(
                serde_json::to_value(fee_request)
                    .expect("FATAL: failed to encode fee rate request to JSON"),
            ),
        )
        .expect("FATAL: failed to construct request from infallible data")
    }
}
