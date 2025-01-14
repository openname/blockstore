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

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::{fs, io};

use rand::{thread_rng, Rng};
use regex::{Captures, Regex};
use serde::de::Error as de_Error;
use stacks_common::codec::{StacksMessageCodec, MAX_MESSAGE_LEN};
use stacks_common::types::chainstate::StacksBlockId;
use stacks_common::types::net::PeerHost;
use stacks_common::util::hash::to_hex;
use url::form_urlencoded;
use {serde, serde_json};

use crate::burnchains::Txid;
use crate::chainstate::nakamoto::NakamotoChainState;
use crate::chainstate::stacks::db::StacksChainState;
use crate::chainstate::stacks::{Error as ChainError, StacksTransaction};
use crate::core::mempool::{decode_tx_stream, MemPoolDB, MemPoolSyncData};
use crate::net::http::{
    parse_bytes, Error, HttpBadRequest, HttpChunkGenerator, HttpContentType, HttpNotFound,
    HttpRequest, HttpRequestContents, HttpRequestPreamble, HttpResponse, HttpResponseContents,
    HttpResponsePayload, HttpResponsePreamble, HttpServerError,
};
use crate::net::httpcore::{
    HttpRequestContentsExtensions, RPCRequestHandler, StacksHttp, StacksHttpRequest,
    StacksHttpResponse,
};
use crate::net::{Error as NetError, StacksNodeState, TipRequest, MAX_HEADERS};
use crate::util_lib::db::{DBConn, Error as DBError};

#[derive(Clone)]
pub struct RPCMempoolQueryRequestHandler {
    pub page_id: Option<Txid>,
    pub mempool_query: Option<MemPoolSyncData>,
}

impl RPCMempoolQueryRequestHandler {
    pub fn new() -> Self {
        Self {
            page_id: None,
            mempool_query: None,
        }
    }

    /// Obtain the mempool page_id query string, if it is present
    fn get_page_id_query(&self, query: Option<&str>) -> Option<Txid> {
        match query {
            Some(query_string) => {
                for (key, value) in form_urlencoded::parse(query_string.as_bytes()) {
                    if key != "page_id" {
                        continue;
                    }
                    if let Ok(page_id) = Txid::from_hex(&value) {
                        return Some(page_id);
                    }
                }
                return None;
            }
            None => {
                return None;
            }
        }
    }
}

#[derive(Debug)]
pub struct StacksMemPoolStream {
    /// Mempool sync data requested
    pub tx_query: MemPoolSyncData,
    /// last txid loaded
    pub last_randomized_txid: Txid,
    /// number of transactions visited in the DB so far
    pub num_txs: u64,
    /// maximum we can visit in the query
    pub max_txs: u64,
    /// coinbase height of the chain at time of query
    pub coinbase_height: u64,
    /// Are we done sending transactions, and are now in the process of sending the trailing page
    /// ID?
    pub corked: bool,
    /// Did we run out of transactions to send?
    pub finished: bool,
    /// link to the mempool DB
    mempool_db: DBConn,
}

impl StacksMemPoolStream {
    pub fn new(
        mempool_db: DBConn,
        tx_query: MemPoolSyncData,
        max_txs: u64,
        coinbase_height: u64,
        page_id_opt: Option<Txid>,
    ) -> Self {
        let last_randomized_txid = page_id_opt.unwrap_or_else(|| {
            let random_bytes = thread_rng().gen::<[u8; 32]>();
            Txid(random_bytes)
        });

        Self {
            tx_query,
            last_randomized_txid,
            num_txs: 0,
            max_txs,
            coinbase_height,
            corked: false,
            finished: false,
            mempool_db,
        }
    }
}

impl HttpChunkGenerator for StacksMemPoolStream {
    #[cfg_attr(test, mutants::skip)]
    fn hint_chunk_size(&self) -> usize {
        4096
    }

    #[cfg_attr(test, mutants::skip)]
    fn generate_next_chunk(&mut self) -> Result<Vec<u8>, String> {
        if self.corked {
            test_debug!(
                "Finished streaming txs; last page was {:?}",
                &self.last_randomized_txid
            );
            return Ok(vec![]);
        }

        if self.num_txs >= self.max_txs || self.finished {
            test_debug!(
                "Finished sending transactions after {:?}. Corking tx stream.",
                &self.last_randomized_txid
            );

            // cork the stream -- send the next page_id the requester should use to continue
            // streaming.
            self.corked = true;
            return Ok(self.last_randomized_txid.serialize_to_vec());
        }

        let remaining = self.max_txs.saturating_sub(self.num_txs);
        let (next_txs, next_last_randomized_txid_opt, num_rows_visited) =
            MemPoolDB::static_find_next_missing_transactions(
                &self.mempool_db,
                &self.tx_query,
                self.coinbase_height,
                &self.last_randomized_txid,
                1,
                remaining,
            )
            .map_err(|e| format!("Failed to find next missing transactions: {:?}", &e))?;

        debug!(
            "Streaming mempool propagation stepped";
            "rows_visited" => num_rows_visited,
            "last_rand_txid" => %self.last_randomized_txid,
            "num_txs" => self.num_txs,
            "max_txs" => self.max_txs
        );

        if !next_txs.is_empty() {
            // have another tx to send
            let chunk = next_txs[0].serialize_to_vec();
            if let Some(next_last_randomized_txid) = next_last_randomized_txid_opt {
                // we have more after this
                self.last_randomized_txid = next_last_randomized_txid;
            } else {
                // that was the last transaction.
                // next call will cork the stream
                self.finished = true;
            }
            self.num_txs += next_txs.len() as u64;
            return Ok(chunk);
        } else if let Some(next_txid) = next_last_randomized_txid_opt {
            // no more txs to send
            test_debug!(
                "No rows returned for {}; cork tx stream with next page {}",
                &self.last_randomized_txid,
                &next_txid
            );

            // send the page ID as the final chunk
            let chunk = next_txid.serialize_to_vec();
            self.finished = true;
            self.corked = true;
            return Ok(chunk);
        } else {
            test_debug!(
                "No more txs to send after {:?}; corking stream",
                &self.last_randomized_txid
            );

            // no more transactions, and none after this
            self.finished = true;
            self.corked = true;
            return Ok(vec![]);
        }
    }
}

/// Decode the HTTP request
impl HttpRequest for RPCMempoolQueryRequestHandler {
    fn verb(&self) -> &'static str {
        "POST"
    }

    fn path_regex(&self) -> Regex {
        Regex::new(r#"^/v2/mempool/query$"#).unwrap()
    }

    fn metrics_identifier(&self) -> &str {
        "/v2/mempool/query"
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
                "Invalid Http request: expected nonzero body length".to_string(),
            ));
        }

        let mut body_ptr = body;
        let mempool_body = MemPoolSyncData::consensus_deserialize(&mut body_ptr)?;

        self.mempool_query = Some(mempool_body);
        if let Some(page_id) = self.get_page_id_query(query) {
            self.page_id = Some(page_id);
        }
        Ok(HttpRequestContents::new().query_string(query))
    }
}

impl RPCRequestHandler for RPCMempoolQueryRequestHandler {
    /// Reset internal state
    fn restart(&mut self) {
        self.mempool_query = None;
        self.page_id = None;
    }

    /// Make the response
    fn try_handle_request(
        &mut self,
        preamble: HttpRequestPreamble,
        _contents: HttpRequestContents,
        node: &mut StacksNodeState,
    ) -> Result<(HttpResponsePreamble, HttpResponseContents), NetError> {
        let mempool_query = self
            .mempool_query
            .take()
            .ok_or(NetError::SendError("`mempool_query` not set".into()))?;
        let page_id = self.page_id.take();

        let stream_res = node.with_node_state(|network, _sortdb, _chainstate, mempool, _rpc_args| {
            let coinbase_height = network.stacks_tip.coinbase_height;
            let max_txs = network.connection_opts.mempool_max_tx_query;
            debug!(
                "Begin mempool query";
                "page_id" => %page_id.map(|txid| format!("{}", &txid)).unwrap_or("(none".to_string()),
                "coinbase_height" => coinbase_height,
                "max_txs" => max_txs
            );

            let mempool_db = match mempool.reopen(false) {
                Ok(db) => db,
                Err(e) => {
                    return Err(StacksHttpResponse::new_error(&preamble, &HttpServerError::new(format!("Failed to open mempool DB: {:?}", &e))));
                }
            };

            Ok(StacksMemPoolStream::new(mempool_db, mempool_query, max_txs, coinbase_height, page_id))
        });

        let stream = match stream_res {
            Ok(stream) => stream,
            Err(response) => {
                return response.try_into_contents().map_err(NetError::from);
            }
        };

        let resp_preamble = HttpResponsePreamble::from_http_request_preamble(
            &preamble,
            200,
            "OK",
            None,
            HttpContentType::Bytes,
        );

        Ok((
            resp_preamble,
            HttpResponseContents::from_stream(Box::new(stream)),
        ))
    }
}

/// Decode the HTTP response
impl HttpResponse for RPCMempoolQueryRequestHandler {
    /// Decode this response from a byte stream.  This is called by the client to decode this
    /// message
    fn try_parse_response(
        &self,
        preamble: &HttpResponsePreamble,
        body: &[u8],
    ) -> Result<HttpResponsePayload, Error> {
        let bytes = parse_bytes(preamble, body, MAX_MESSAGE_LEN.into())?;
        Ok(HttpResponsePayload::Bytes(bytes))
    }
}

impl StacksHttpRequest {
    pub fn new_mempool_query(
        host: PeerHost,
        query: MemPoolSyncData,
        page_id_opt: Option<Txid>,
    ) -> StacksHttpRequest {
        StacksHttpRequest::new_for_peer(
            host,
            "POST".into(),
            "/v2/mempool/query".into(),
            if let Some(page_id) = page_id_opt {
                HttpRequestContents::new()
                    .query_arg("page_id".into(), format!("{}", &page_id))
                    .payload_stacks(&query)
            } else {
                HttpRequestContents::new().payload_stacks(&query)
            },
        )
        .expect("FATAL: failed to construct request from infallible data")
    }
}

impl StacksHttpResponse {
    /// Decode an HTTP response body into the transactions and next-page ID returned from
    /// /v2/mempool/query.
    pub fn decode_mempool_txs_page(
        self,
    ) -> Result<(Vec<StacksTransaction>, Option<Txid>), NetError> {
        let contents = self.get_http_payload_ok()?;
        let raw_bytes: Vec<u8> = contents.try_into()?;
        let (txs, page_id_opt) = decode_tx_stream(&mut &raw_bytes[..])?;
        Ok((txs, page_id_opt))
    }
}
