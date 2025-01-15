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
use stacks_common::codec::{read_next, Error as CodecError, StacksMessageCodec, MAX_MESSAGE_LEN};
use stacks_common::types::chainstate::{BlockHeaderHash, StacksBlockId};
use stacks_common::types::net::PeerHost;
use stacks_common::util::hash::to_hex;
use stacks_common::util::retry::BoundReader;
use {serde, serde_json};

use crate::chainstate::stacks::db::StacksChainState;
use crate::chainstate::stacks::{Error as ChainError, StacksBlockHeader, StacksMicroblock};
use crate::net::http::{
    parse_bytes, Error, HttpBadRequest, HttpChunkGenerator, HttpContentType, HttpNotFound,
    HttpRequest, HttpRequestContents, HttpRequestPreamble, HttpResponse, HttpResponseContents,
    HttpResponsePayload, HttpResponsePreamble, HttpServerError,
};
use crate::net::httpcore::{
    request, HttpRequestContentsExtensions, RPCRequestHandler, StacksHttp, StacksHttpRequest,
    StacksHttpResponse,
};
use crate::net::{
    Error as NetError, StacksNodeState, TipRequest, MAX_HEADERS, MAX_MICROBLOCKS_UNCONFIRMED,
};
use crate::util_lib::db::{DBConn, Error as DBError};

#[derive(Clone)]
pub struct RPCMicroblocksUnconfirmedRequestHandler {
    pub parent_block_id: Option<StacksBlockId>,
    pub start_sequence: Option<u16>,
}
impl RPCMicroblocksUnconfirmedRequestHandler {
    pub fn new() -> Self {
        Self {
            parent_block_id: None,
            start_sequence: None,
        }
    }
}

#[derive(Debug)]
pub struct StacksUnconfirmedMicroblockStream {
    /// microblock pointer
    pub microblock_hash: BlockHeaderHash,
    pub parent_index_block_hash: StacksBlockId,
    pub seq: u16,
    pub finished: bool,
    pub next_microblock: StacksMicroblock,

    /// connection to the chain state
    chainstate_db: DBConn,
}

impl StacksUnconfirmedMicroblockStream {
    pub fn new(
        chainstate: &StacksChainState,
        parent_block_id: &StacksBlockId,
        seq: u16,
    ) -> Result<Self, ChainError> {
        let mblock_info = StacksChainState::load_next_descendant_microblock(
            chainstate.db(),
            parent_block_id,
            seq,
        )?
        .ok_or(ChainError::NoSuchBlockError)?;

        // need to send out the consensus_serialize()'ed array length before sending microblocks.
        // this is exactly what seq tells us, though.
        Ok(StacksUnconfirmedMicroblockStream {
            microblock_hash: mblock_info.block_hash(),
            parent_index_block_hash: parent_block_id.clone(),
            seq,
            finished: false,
            next_microblock: mblock_info,
            chainstate_db: chainstate.reopen_db()?,
        })
    }
}

/// Decode the HTTP request
impl HttpRequest for RPCMicroblocksUnconfirmedRequestHandler {
    fn verb(&self) -> &'static str {
        "GET"
    }

    fn path_regex(&self) -> Regex {
        Regex::new(r#"^/v2/microblocks/unconfirmed/(?P<parent_block_id>[0-9a-f]{64})/(?P<start_sequence>[0-9]{1,6})$"#).unwrap()
    }

    fn metrics_identifier(&self) -> &str {
        "/v2/microblocks/unconfirmed/:block_id/:seq"
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
                "Invalid Http request: expected 0-length body for GetInfo".to_string(),
            ));
        }

        let parent_block_id = request::get_block_hash(captures, "parent_block_id")?;
        let start_sequence_u32 = request::get_u32(captures, "start_sequence")?;

        if start_sequence_u32 > u16::MAX.into() {
            return Err(Error::DecodeError("`start_sequence` is too big".into()));
        }

        let start_sequence = start_sequence_u32 as u16;

        self.parent_block_id = Some(parent_block_id);
        self.start_sequence = Some(start_sequence);

        Ok(HttpRequestContents::new().query_string(query))
    }
}

impl RPCRequestHandler for RPCMicroblocksUnconfirmedRequestHandler {
    /// Reset internal state
    fn restart(&mut self) {
        self.parent_block_id = None;
        self.start_sequence = None;
    }

    /// Make the response
    fn try_handle_request(
        &mut self,
        preamble: HttpRequestPreamble,
        _contents: HttpRequestContents,
        node: &mut StacksNodeState,
    ) -> Result<(HttpResponsePreamble, HttpResponseContents), NetError> {
        let block_id = self
            .parent_block_id
            .take()
            .ok_or(NetError::SendError("`parent_block_id` not set".into()))?;
        let start_seq = self
            .start_sequence
            .take()
            .ok_or(NetError::SendError("`start_seq` not set".into()))?;

        let stream_res =
            node.with_node_state(|_network, _sortdb, chainstate, _mempool, _rpc_args| {
                StacksUnconfirmedMicroblockStream::new(chainstate, &block_id, start_seq)
            });

        // start loading up the microblocks
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
impl HttpResponse for RPCMicroblocksUnconfirmedRequestHandler {
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
impl HttpChunkGenerator for StacksUnconfirmedMicroblockStream {
    fn hint_chunk_size(&self) -> usize {
        4096
    }

    /// Stream back microblock chunks.
    /// The first chunk is a 4-byte length prefix
    /// Subsequent chunks are microblocks
    fn generate_next_chunk(&mut self) -> Result<Vec<u8>, String> {
        if self.finished {
            // no more to load
            return Ok(vec![]);
        }

        // advance streamer to next microblock in the sequence
        let next_seq = match self.seq {
            u16::MAX => {
                return Err("No more microblocks; exceeded maximum sequence number".to_string());
            }
            x => x + 1,
        };

        let next_mblock_opt = StacksChainState::load_next_descendant_microblock(
            &self.chainstate_db,
            &self.parent_index_block_hash,
            next_seq,
        ).map_err(|e| {
            warn!("Failed to query for next descendant microblock"; "parent anchored block" => %self.parent_index_block_hash, "next_seq" => %next_seq);
            let msg = format!("Failed to query for next descendant microblock of {} at {}: {:?}", &self.parent_index_block_hash, next_seq, &e);
            msg
        })?;

        let buf = self.next_microblock.serialize_to_vec();
        if let Some(mblock) = next_mblock_opt {
            test_debug!(
                "Switch to {}-{} ({})",
                &self.parent_index_block_hash,
                &mblock.block_hash(),
                next_seq
            );
            self.microblock_hash = mblock.block_hash();
            self.seq = next_seq;
            self.next_microblock = mblock;
        } else {
            // we're EOF
            self.finished = true;
        }

        return Ok(buf);
    }
}

impl StacksHttpRequest {
    pub fn new_getmicroblocks_unconfirmed(
        host: PeerHost,
        parent_block_id: StacksBlockId,
        seq: u16,
    ) -> StacksHttpRequest {
        StacksHttpRequest::new_for_peer(
            host,
            "GET".into(),
            format!("/v2/microblocks/unconfirmed/{}/{}", &parent_block_id, seq),
            HttpRequestContents::new(),
        )
        .expect("FATAL: failed to construct request from infallible data")
    }
}

impl StacksHttpResponse {
    /// Decode an HTTP response into an unconfirmed microblock stream
    pub fn decode_microblocks_unconfirmed(self) -> Result<Vec<StacksMicroblock>, NetError> {
        let contents = self.get_http_payload_ok()?;
        let mblock_bytes: Vec<u8> = contents.try_into()?;
        let mut mblock_bytes_ptr = mblock_bytes.as_slice();

        let mut microblocks = vec![];
        let mut bound_reader =
            BoundReader::from_reader(&mut mblock_bytes_ptr, MAX_MESSAGE_LEN.into());
        loop {
            let mblock: StacksMicroblock = match read_next(&mut bound_reader) {
                Ok(mblock) => Ok(mblock),
                Err(e) => match e {
                    CodecError::ReadError(ref ioe) => match ioe.kind() {
                        io::ErrorKind::UnexpectedEof => {
                            // end of stream -- this is fine
                            break;
                        }
                        _ => Err(e),
                    },
                    _ => Err(e),
                },
            }?;

            microblocks.push(mblock);
            if microblocks.len() == MAX_MICROBLOCKS_UNCONFIRMED {
                break;
            }
        }

        Ok(microblocks)
    }
}
