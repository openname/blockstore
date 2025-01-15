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

use regex::{Captures, Regex};
use serde::de::Error as de_Error;
use stacks_common::codec::{read_next, StacksMessageCodec, MAX_MESSAGE_LEN};
use stacks_common::types::chainstate::{BlockHeaderHash, StacksBlockId};
use stacks_common::types::net::PeerHost;
use stacks_common::util::hash::to_hex;
use {serde, serde_json};

use crate::chainstate::stacks::db::StacksChainState;
use crate::chainstate::stacks::{Error as ChainError, StacksBlockHeader, StacksMicroblock};
use crate::net::http::{
    parse_bytes, Error, HttpBadRequest, HttpChunkGenerator, HttpContentType, HttpNotFound,
    HttpRequest, HttpRequestContents, HttpRequestPreamble, HttpResponse, HttpResponseContents,
    HttpResponsePayload, HttpResponsePreamble, HttpServerError, HttpVersion,
};
use crate::net::httpcore::{
    request, HttpRequestContentsExtensions, RPCRequestHandler, StacksHttp, StacksHttpRequest,
    StacksHttpResponse,
};
use crate::net::{Error as NetError, StacksNodeState, TipRequest, MAX_HEADERS};
use crate::util_lib::db::{DBConn, Error as DBError};

#[derive(Clone)]
pub struct RPCMicroblocksIndexedRequestHandler {
    pub tail_microblock_id: Option<StacksBlockId>,
}
impl RPCMicroblocksIndexedRequestHandler {
    pub fn new() -> Self {
        Self {
            tail_microblock_id: None,
        }
    }
}

#[derive(Debug)]
pub struct StacksIndexedMicroblockStream {
    /// length prefix
    pub num_items_buf: [u8; 4],
    pub num_items_ptr: usize,

    /// microblock pointer
    pub microblock_hash: BlockHeaderHash,
    pub parent_index_block_hash: StacksBlockId,

    /// connection to the chain state
    chainstate_db: DBConn,
}

impl StacksIndexedMicroblockStream {
    pub fn new(
        chainstate: &StacksChainState,
        tail_index_microblock_hash: &StacksBlockId,
    ) -> Result<Self, ChainError> {
        // look up parent
        let mblock_info = StacksChainState::load_staging_microblock_info_indexed(
            chainstate.db(),
            tail_index_microblock_hash,
        )?
        .ok_or(ChainError::NoSuchBlockError)?;

        let parent_index_block_hash = StacksBlockHeader::make_index_block_hash(
            &mblock_info.consensus_hash,
            &mblock_info.anchored_block_hash,
        );

        // need to send out the consensus_serialize()'ed array length before sending microblocks.
        // this is exactly what seq tells us, though.
        test_debug!(
            "Will stream {} microblocks back from {}",
            mblock_info.sequence,
            &tail_index_microblock_hash
        );
        let num_items_buf = ((mblock_info.sequence as u32) + 1).to_be_bytes();

        Ok(StacksIndexedMicroblockStream {
            microblock_hash: mblock_info.microblock_hash,
            parent_index_block_hash,
            num_items_buf,
            num_items_ptr: 0,
            chainstate_db: chainstate.reopen_db()?,
        })
    }
}

/// Decode the HTTP request
impl HttpRequest for RPCMicroblocksIndexedRequestHandler {
    fn verb(&self) -> &'static str {
        "GET"
    }

    fn path_regex(&self) -> Regex {
        Regex::new(r#"^/v2/microblocks/(?P<tail_microblock_id>[0-9a-f]{64})$"#).unwrap()
    }

    fn metrics_identifier(&self) -> &str {
        "/v2/microblocks/:microblock_id"
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

        let tail_microblock_id = request::get_block_hash(captures, "tail_microblock_id")?;

        self.tail_microblock_id = Some(tail_microblock_id);

        Ok(HttpRequestContents::new().query_string(query))
    }
}

impl RPCRequestHandler for RPCMicroblocksIndexedRequestHandler {
    /// Reset internal state
    fn restart(&mut self) {
        self.tail_microblock_id = None;
    }

    /// Make the response
    fn try_handle_request(
        &mut self,
        preamble: HttpRequestPreamble,
        _contents: HttpRequestContents,
        node: &mut StacksNodeState,
    ) -> Result<(HttpResponsePreamble, HttpResponseContents), NetError> {
        let tail_microblock_id = self
            .tail_microblock_id
            .take()
            .ok_or(NetError::SendError("`tail_microblock_id` not set".into()))?;
        let stream_res =
            node.with_node_state(|_network, _sortdb, chainstate, _mempool, _rpc_args| {
                StacksIndexedMicroblockStream::new(chainstate, &tail_microblock_id)
            });

        // start loading up the microblocks
        let stream = match stream_res {
            Ok(stream) => stream,
            Err(ChainError::NoSuchBlockError) => {
                return StacksHttpResponse::new_error(
                    &preamble,
                    &HttpNotFound::new(format!("No such microblock {:?}\n", &tail_microblock_id)),
                )
                .try_into_contents()
                .map_err(NetError::from)
            }
            Err(e) => {
                // nope -- error trying to check
                let msg = format!("Failed to load microblock: {:?}\n", &e);
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
impl HttpResponse for RPCMicroblocksIndexedRequestHandler {
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

/// Stream implementation for HeaderStreamData
impl HttpChunkGenerator for StacksIndexedMicroblockStream {
    #[cfg(not(test))]
    fn hint_chunk_size(&self) -> usize {
        4096
    }

    #[cfg(test)]
    fn hint_chunk_size(&self) -> usize {
        // make this hurt
        32
    }

    /// Stream back microblock chunks.
    /// The first chunk is a 4-byte length prefix
    /// Subsequent chunks are microblocks
    #[cfg_attr(test, mutants::skip)]
    fn generate_next_chunk(&mut self) -> Result<Vec<u8>, String> {
        if self.num_items_ptr == 0 {
            // send length prefix
            self.num_items_ptr += self.num_items_buf.len();
            return Ok(self.num_items_buf.to_vec());
        }

        // load next microblock
        let mblock_info_opt = StacksChainState::load_staging_microblock_indexed(
            &self.chainstate_db,
            &self.parent_index_block_hash,
            &self.microblock_hash,
        ).map_err(|e| {
            warn!("Failed to load microblock"; "microblock" => %self.microblock_hash, "parent anchored block" => %self.parent_index_block_hash, "error" => %e);
            let msg = format!("Failed to load microblock {}-{}: {:?}", &self.parent_index_block_hash, &self.microblock_hash, &e);
            msg
        })?;

        let mblock_info = if let Some(x) = mblock_info_opt {
            x
        } else {
            // out of microblocks
            debug!(
                "Out of microblocks to stream";
                "last microblock" => %self.microblock_hash,
                "parent anchored block" => %self.parent_index_block_hash
            );
            return Ok(vec![]);
        };

        let buf = mblock_info.block_data;

        self.microblock_hash = mblock_info.parent_hash;
        return Ok(buf);
    }
}

impl StacksHttpRequest {
    pub fn new_getmicroblocks_indexed(
        host: PeerHost,
        index_microblock_hash: StacksBlockId,
    ) -> StacksHttpRequest {
        StacksHttpRequest::new_for_peer(
            host,
            "GET".into(),
            format!("/v2/microblocks/{}", &index_microblock_hash),
            HttpRequestContents::new(),
        )
        .expect("FATAL: failed to construct request from infallible data")
    }
}

impl StacksHttpResponse {
    #[cfg(test)]
    pub fn new_getmicroblocks_indexed(
        mblocks: Vec<StacksMicroblock>,
        with_content_length: bool,
    ) -> StacksHttpResponse {
        let value = mblocks.serialize_to_vec();
        let length = value.len();
        let preamble = HttpResponsePreamble::new(
            HttpVersion::Http11,
            200,
            "OK".to_string(),
            if with_content_length {
                Some(length as u32)
            } else {
                None
            },
            HttpContentType::Bytes,
            true,
        );
        let body = HttpResponsePayload::Bytes(value);
        StacksHttpResponse::new(preamble, body)
    }

    /// Decode an HTTP response into a microblock stream
    /// If it fails, return Self::Error(..)
    pub fn decode_microblocks(self) -> Result<Vec<StacksMicroblock>, NetError> {
        let contents = self.get_http_payload_ok()?;

        // contents will be a SIP-003 bytestream
        let mblock_bytes: Vec<u8> = contents.try_into()?;
        let microblocks: Vec<StacksMicroblock> = read_next(&mut &mblock_bytes[..])?;

        Ok(microblocks)
    }
}
