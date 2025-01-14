// Copyright (C) 2024 Stacks Open Internet Foundation
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
use regex::{Captures, Regex};
use serde_json::json;
use stacks_common::types::chainstate::StacksBlockId;
use stacks_common::types::net::PeerHost;
use stacks_common::util::hash::Sha256Sum;

use crate::burnchains::Burnchain;
use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::coordinator::OnChainRewardSetProvider;
use crate::chainstate::stacks::boot::{
    PoxVersions, RewardSet, POX_1_NAME, POX_2_NAME, POX_3_NAME, POX_4_NAME,
};
use crate::chainstate::stacks::db::StacksChainState;
use crate::chainstate::stacks::Error as ChainError;
use crate::core::mempool::MemPoolDB;
use crate::net::http::{
    parse_json, Error, HttpBadRequest, HttpNotFound, HttpRequest, HttpRequestContents,
    HttpRequestPreamble, HttpResponse, HttpResponseContents, HttpResponsePayload,
    HttpResponsePreamble, HttpServerError,
};
use crate::net::httpcore::{
    HttpPreambleExtensions, HttpRequestContentsExtensions, RPCRequestHandler, StacksHttp,
    StacksHttpRequest, StacksHttpResponse,
};
use crate::net::p2p::PeerNetwork;
use crate::net::{Error as NetError, StacksNodeState, TipRequest};
use crate::util_lib::boot::boot_code_id;
use crate::util_lib::db::Error as DBError;

#[derive(Clone, Default)]
pub struct GetStackersRequestHandler {
    cycle_number: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GetStackersResponse {
    pub stacker_set: RewardSet,
}

pub enum GetStackersErrors {
    NotAvailableYet(crate::chainstate::coordinator::Error),
    Other(String),
}

impl GetStackersErrors {
    pub const NOT_AVAILABLE_ERR_TYPE: &str = "not_available_try_again";
    pub const OTHER_ERR_TYPE: &str = "other";

    pub fn error_type_string(&self) -> &'static str {
        match self {
            Self::NotAvailableYet(_) => Self::NOT_AVAILABLE_ERR_TYPE,
            Self::Other(_) => Self::OTHER_ERR_TYPE,
        }
    }
}

impl From<&str> for GetStackersErrors {
    fn from(value: &str) -> Self {
        GetStackersErrors::Other(value.into())
    }
}

impl std::fmt::Display for GetStackersErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GetStackersErrors::NotAvailableYet(e) => write!(f, "Could not read reward set. Prepare phase may not have started for this cycle yet. Err = {e:?}"),
            GetStackersErrors::Other(msg) => write!(f, "{msg}")
        }
    }
}

impl GetStackersResponse {
    pub fn load(
        sortdb: &SortitionDB,
        chainstate: &mut StacksChainState,
        tip: &StacksBlockId,
        burnchain: &Burnchain,
        cycle_number: u64,
    ) -> Result<Self, GetStackersErrors> {
        let cycle_start_height = burnchain.reward_cycle_to_block_height(cycle_number);
        let pox_contract_name = burnchain
            .pox_constants
            .active_pox_contract(cycle_start_height);
        let pox_version = PoxVersions::lookup_by_name(pox_contract_name)
            .ok_or("Failed to lookup PoX contract version at tip")?;
        if !matches!(pox_version, PoxVersions::Pox4) {
            return Err(
                "Active PoX contract version at tip is Pre-PoX-4, the signer set is not fetchable"
                    .into(),
            );
        }

        let provider = OnChainRewardSetProvider::new();
        let stacker_set = provider
            .read_reward_set_nakamoto(chainstate, cycle_number, sortdb, tip, true)
            .map_err(GetStackersErrors::NotAvailableYet)?;

        Ok(Self { stacker_set })
    }
}

/// Decode the HTTP request
impl HttpRequest for GetStackersRequestHandler {
    fn verb(&self) -> &'static str {
        "GET"
    }

    fn path_regex(&self) -> Regex {
        Regex::new(r#"^/v3/stacker_set/(?P<cycle_num>[0-9]{1,10})$"#).unwrap()
    }

    fn metrics_identifier(&self) -> &str {
        "/v3/stacker_set/:cycle_num"
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
                "Invalid Http request: expected 0-length body".into(),
            ));
        }

        let Some(cycle_num_str) = captures.name("cycle_num") else {
            return Err(Error::DecodeError(
                "Missing in request path: `cycle_num`".into(),
            ));
        };
        let cycle_num = u64::from_str_radix(cycle_num_str.into(), 10)
            .map_err(|e| Error::DecodeError(format!("Failed to parse cycle number: {e}")))?;

        self.cycle_number = Some(cycle_num);

        Ok(HttpRequestContents::new().query_string(query))
    }
}

impl RPCRequestHandler for GetStackersRequestHandler {
    /// Reset internal state
    fn restart(&mut self) {
        self.cycle_number = None;
    }

    /// Make the response
    fn try_handle_request(
        &mut self,
        preamble: HttpRequestPreamble,
        contents: HttpRequestContents,
        node: &mut StacksNodeState,
    ) -> Result<(HttpResponsePreamble, HttpResponseContents), NetError> {
        let tip = match node.load_stacks_chain_tip(&preamble, &contents) {
            Ok(tip) => tip,
            Err(error_resp) => {
                return error_resp.try_into_contents().map_err(NetError::from);
            }
        };
        let Some(cycle_number) = self.cycle_number.clone() else {
            return StacksHttpResponse::new_error(
                    &preamble,
                    &HttpBadRequest::new_json(json!({"response": "error", "err_msg": "Failed to read cycle number in request"}))
                )
                    .try_into_contents()
                    .map_err(NetError::from);
        };

        let stacker_response =
            node.with_node_state(|network, sortdb, chainstate, _mempool, _rpc_args| {
                GetStackersResponse::load(
                    sortdb,
                    chainstate,
                    &tip,
                    network.get_burnchain(),
                    cycle_number,
                )
            });

        let response = match stacker_response {
            Ok(response) => response,
            Err(error) => {
                return StacksHttpResponse::new_error(
                    &preamble,
                    &HttpBadRequest::new_json(json!({
                        "response": "error",
                        "err_type": error.error_type_string(),
                        "err_msg": error.to_string()})),
                )
                .try_into_contents()
                .map_err(NetError::from)
            }
        };

        let mut preamble = HttpResponsePreamble::ok_json(&preamble);
        preamble.set_canonical_stacks_tip_height(Some(node.canonical_stacks_tip_height()));
        let body = HttpResponseContents::try_from_json(&response)?;
        Ok((preamble, body))
    }
}

impl HttpResponse for GetStackersRequestHandler {
    fn try_parse_response(
        &self,
        preamble: &HttpResponsePreamble,
        body: &[u8],
    ) -> Result<HttpResponsePayload, Error> {
        let response: GetStackersResponse = parse_json(preamble, body)?;
        Ok(HttpResponsePayload::try_from_json(response)?)
    }
}

impl StacksHttpRequest {
    /// Make a new getinfo request to this endpoint
    pub fn new_getstackers(
        host: PeerHost,
        cycle_num: u64,
        tip_req: TipRequest,
    ) -> StacksHttpRequest {
        StacksHttpRequest::new_for_peer(
            host,
            "GET".into(),
            format!("/v3/stacker_set/{cycle_num}"),
            HttpRequestContents::new().for_tip(tip_req),
        )
        .expect("FATAL: failed to construct request from infallible data")
    }
}

impl StacksHttpResponse {
    pub fn decode_stacker_set(self) -> Result<GetStackersResponse, NetError> {
        let contents = self.get_http_payload_ok()?;
        let response_json: serde_json::Value = contents.try_into()?;
        let response: GetStackersResponse = serde_json::from_value(response_json)
            .map_err(|_e| Error::DecodeError("Failed to decode JSON".to_string()))?;
        Ok(response)
    }
}

#[cfg(test)]
mod test {
    use super::GetStackersErrors;

    #[test]
    // Test the formatting and error type strings of GetStackersErrors
    fn get_stackers_errors() {
        let not_available_err = GetStackersErrors::NotAvailableYet(
            crate::chainstate::coordinator::Error::PoXNotProcessedYet,
        );
        let other_err = GetStackersErrors::Other("foo".into());

        assert_eq!(
            not_available_err.error_type_string(),
            GetStackersErrors::NOT_AVAILABLE_ERR_TYPE
        );
        assert_eq!(
            other_err.error_type_string(),
            GetStackersErrors::OTHER_ERR_TYPE
        );

        assert!(not_available_err
            .to_string()
            .starts_with("Could not read reward set"));
        assert_eq!(other_err.to_string(), "foo".to_string());
    }
}
