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

use std::io::{Read, Seek, SeekFrom, Write};
use std::{fs, io};

use regex::{Captures, Regex};
use serde::de::Error as de_Error;
use stacks_common::codec::{StacksMessageCodec, MAX_PAYLOAD_LEN};
use stacks_common::types::chainstate::{ConsensusHash, StacksBlockId};
use stacks_common::types::net::PeerHost;
use stacks_common::util::hash::to_hex;
use {serde, serde_json};

use crate::chainstate::nakamoto::{NakamotoBlock, NakamotoChainState, NakamotoStagingBlocksConn};
use crate::chainstate::stacks::db::StacksChainState;
use crate::chainstate::stacks::Error as ChainError;
use crate::net::api::getblock_v3::NakamotoBlockStream;
use crate::net::http::{
    parse_bytes, Error, HttpBadRequest, HttpChunkGenerator, HttpContentType, HttpNotFound,
    HttpRequest, HttpRequestContents, HttpRequestPreamble, HttpResponse, HttpResponseContents,
    HttpResponsePayload, HttpResponsePreamble, HttpServerError, HttpVersion,
};
use crate::net::httpcore::{
    HttpRequestContentsExtensions, RPCRequestHandler, StacksHttp, StacksHttpRequest,
    StacksHttpResponse,
};
use crate::net::{Error as NetError, StacksNodeState, TipRequest, MAX_HEADERS};
use crate::util_lib::db::{DBConn, Error as DBError};

#[derive(Clone)]
pub struct RPCNakamotoTenureRequestHandler {
    /// Block to start streaming from. It and its ancestors will be incrementally streamed until one of
    /// hte following happens:
    /// * we reach the first block in the tenure
    /// * we would exceed MAX_PAYLOAD_LEN bytes transmitted if we started sending the next block
    pub block_id: Option<StacksBlockId>,
    /// What's the final block ID to stream from?
    /// Passed as `stop=` query parameter
    pub last_block_id: Option<StacksBlockId>,
}

impl RPCNakamotoTenureRequestHandler {
    pub fn new() -> Self {
        Self {
            block_id: None,
            last_block_id: None,
        }
    }
}

pub struct NakamotoTenureStream {
    /// stream for the current block
    pub block_stream: NakamotoBlockStream,
    /// connection to the headers DB
    pub headers_conn: DBConn,
    /// total bytess sent so far
    pub total_sent: u64,
    /// stop streaming if we reach this block
    pub last_block_id: Option<StacksBlockId>,
}

impl NakamotoTenureStream {
    pub fn new(
        chainstate: &StacksChainState,
        block_id: StacksBlockId,
        consensus_hash: ConsensusHash,
        parent_block_id: StacksBlockId,
        last_block_id: Option<StacksBlockId>,
    ) -> Result<Self, ChainError> {
        let block_stream =
            NakamotoBlockStream::new(chainstate, block_id, consensus_hash, parent_block_id)?;
        let headers_conn = chainstate.reopen_db()?;
        Ok(NakamotoTenureStream {
            block_stream,
            headers_conn,
            total_sent: 0,
            last_block_id,
        })
    }

    /// Start streaming the next block (i.e. the parent of the block we last streamed).
    /// Return Ok(true) if we can fit the block into the stream.
    /// Return Ok(false) if not. The caller will need to call this RPC method again with the block
    /// ID of the last block it received.
    /// Return Err(..) on DB error
    pub fn next_block(&mut self) -> Result<bool, ChainError> {
        let parent_header = NakamotoChainState::get_block_header(
            &self.headers_conn,
            &self.block_stream.parent_block_id,
        )?
        .ok_or(ChainError::NoSuchBlockError)?;

        // stop sending if the parent is an epoch2 block
        let Some(parent_nakamoto_header) = parent_header.anchored_header.as_stacks_nakamoto()
        else {
            return Ok(false);
        };

        if let Some(last_block_id) = self.last_block_id.as_ref() {
            if &parent_nakamoto_header.block_id() == last_block_id {
                // asked to stop
                return Ok(false);
            }
        }

        // stop sending if the parent is in a different tenure
        if parent_nakamoto_header.consensus_hash != self.block_stream.consensus_hash {
            return Ok(false);
        }

        let parent_size = self
            .block_stream
            .staging_db_conn
            .conn()
            .get_nakamoto_block_size(&self.block_stream.parent_block_id)?
            .ok_or(ChainError::NoSuchBlockError)?;

        self.total_sent = self
            .total_sent
            .saturating_add(self.block_stream.total_bytes);
        if self.total_sent.saturating_add(parent_size) > MAX_PAYLOAD_LEN.into() {
            // out of space to send this
            return Ok(false);
        }

        self.block_stream.reset(
            parent_nakamoto_header.block_id(),
            parent_nakamoto_header.parent_block_id.clone(),
        )?;
        Ok(true)
    }
}

/// Decode the HTTP request
impl HttpRequest for RPCNakamotoTenureRequestHandler {
    fn verb(&self) -> &'static str {
        "GET"
    }

    fn path_regex(&self) -> Regex {
        Regex::new(r#"^/v3/tenures/(?P<block_id>[0-9a-f]{64})$"#).unwrap()
    }

    fn metrics_identifier(&self) -> &str {
        "/v3/tenures/:block_id"
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
                "Invalid Http request: expected 0-length body".to_string(),
            ));
        }

        let block_id_str = captures
            .name("block_id")
            .ok_or_else(|| {
                Error::DecodeError("Failed to match path to block ID group".to_string())
            })?
            .as_str();

        let block_id = StacksBlockId::from_hex(block_id_str).map_err(|_| {
            Error::DecodeError("Invalid path: unparseable consensus hash".to_string())
        })?;

        let req_contents = HttpRequestContents::new().query_string(query);
        let last_block_id = req_contents
            .get_query_arg("stop")
            .map(|last_block_id_hex| StacksBlockId::from_hex(last_block_id_hex))
            .transpose()
            .map_err(|e| {
                Error::DecodeError(format!("Failed to parse stop= query parameter: {e:?}"))
            })?;

        self.last_block_id = last_block_id;
        self.block_id = Some(block_id);

        Ok(req_contents)
    }
}

impl RPCRequestHandler for RPCNakamotoTenureRequestHandler {
    /// Reset internal state
    fn restart(&mut self) {
        self.block_id = None;
        self.last_block_id = None;
    }

    /// Make the response
    fn try_handle_request(
        &mut self,
        preamble: HttpRequestPreamble,
        _contents: HttpRequestContents,
        node: &mut StacksNodeState,
    ) -> Result<(HttpResponsePreamble, HttpResponseContents), NetError> {
        let block_id = self
            .block_id
            .take()
            .ok_or(NetError::SendError("Missing `block_id`".into()))?;

        let stream_res =
            node.with_node_state(|_network, _sortdb, chainstate, _mempool, _rpc_args| {
                let Some(header) =
                    NakamotoChainState::get_block_header_nakamoto(chainstate.db(), &block_id)?
                else {
                    return Err(ChainError::NoSuchBlockError);
                };
                let Some(nakamoto_header) = header.anchored_header.as_stacks_nakamoto() else {
                    return Err(ChainError::NoSuchBlockError);
                };
                NakamotoTenureStream::new(
                    chainstate,
                    block_id,
                    nakamoto_header.consensus_hash.clone(),
                    nakamoto_header.parent_block_id.clone(),
                    self.last_block_id.clone(),
                )
            });

        // start loading up the block
        let stream = match stream_res {
            Ok(stream) => stream,
            Err(ChainError::NoSuchBlockError) => {
                return StacksHttpResponse::new_error(
                    &preamble,
                    &HttpNotFound::new(format!("No such block {:?}\n", &block_id)),
                )
                .try_into_contents()
                .map_err(NetError::from)
            }
            Err(e) => {
                // nope -- error trying to check
                let msg = format!("Failed to load block {}: {:?}\n", &block_id, &e);
                warn!("{}", &msg);
                return StacksHttpResponse::new_error(&preamble, &HttpServerError::new(msg))
                    .try_into_contents()
                    .map_err(NetError::from);
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
impl HttpResponse for RPCNakamotoTenureRequestHandler {
    /// Decode this response from a byte stream.  This is called by the client to decode this
    /// message
    fn try_parse_response(
        &self,
        preamble: &HttpResponsePreamble,
        body: &[u8],
    ) -> Result<HttpResponsePayload, Error> {
        let bytes = parse_bytes(preamble, body, MAX_PAYLOAD_LEN.into())?;
        Ok(HttpResponsePayload::Bytes(bytes))
    }
}

/// Stream implementation for a Nakamoto block
impl HttpChunkGenerator for NakamotoTenureStream {
    #[cfg(test)]
    fn hint_chunk_size(&self) -> usize {
        // make this hurt
        32
    }

    #[cfg(not(test))]
    fn hint_chunk_size(&self) -> usize {
        4096
    }

    fn generate_next_chunk(&mut self) -> Result<Vec<u8>, String> {
        let next_block_chunk = self.block_stream.generate_next_chunk()?;
        if !next_block_chunk.is_empty() {
            // have block data to send
            return Ok(next_block_chunk);
        }

        // load up next block
        let send_more = self.next_block().map_err(|e| {
            let msg = format!("Failed to load next block in this tenure: {:?}", &e);
            warn!("{}", &msg);
            msg
        })?;

        if !send_more {
            return Ok(vec![]);
        }

        self.block_stream.generate_next_chunk()
    }
}

impl StacksHttpRequest {
    pub fn new_get_nakamoto_tenure(
        host: PeerHost,
        block_id: StacksBlockId,
        last_block_id: Option<StacksBlockId>,
    ) -> StacksHttpRequest {
        StacksHttpRequest::new_for_peer(
            host,
            "GET".into(),
            format!(
                "/v3/tenures/{}{}",
                &block_id,
                last_block_id
                    .map(|block_id| format!("?stop={}", &block_id))
                    .unwrap_or("".to_string())
            ),
            HttpRequestContents::new(),
        )
        .expect("FATAL: failed to construct request from infallible data")
    }
}

impl StacksHttpResponse {
    /// Decode an HTTP response into a tenure.
    /// The bytes are a concatenation of Nakamoto blocks, with no length prefix.
    /// If it fails, return Self::Error(..)
    pub fn decode_nakamoto_tenure(self) -> Result<Vec<NakamotoBlock>, NetError> {
        let contents = self.get_http_payload_ok()?;

        // contents will be raw bytes
        let tenure_bytes: Vec<u8> = contents.try_into()?;
        let ptr = &mut tenure_bytes.as_slice();

        let mut blocks = vec![];
        while !ptr.is_empty() {
            let block = NakamotoBlock::consensus_deserialize(ptr)?;
            blocks.push(block);
        }

        Ok(blocks)
    }
}
