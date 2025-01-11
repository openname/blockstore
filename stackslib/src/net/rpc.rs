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

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::prelude::*;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::time::Instant;
use std::{fmt, io};

use clarity::vm::analysis::errors::CheckErrors;
use clarity::vm::ast::ASTRules;
use clarity::vm::costs::{ExecutionCost, LimitedCostTracker};
use clarity::vm::database::clarity_store::{make_contract_hash_key, ContractCommitment};
use clarity::vm::database::{
    BurnStateDB, ClarityDatabase, ClaritySerializable, STXBalance, StoreType,
};
use clarity::vm::errors::Error::Unchecked;
use clarity::vm::errors::{Error as ClarityRuntimeError, InterpreterError};
use clarity::vm::types::{
    PrincipalData, QualifiedContractIdentifier, StandardPrincipalData, TraitIdentifier,
};
use clarity::vm::{ClarityName, ClarityVersion, ContractName, SymbolicExpression, Value};
use libstackerdb::{StackerDBChunkAckData, StackerDBChunkData};
use rand::prelude::*;
use rand::thread_rng;
use rusqlite::DatabaseName;
use stacks_common::codec::StacksMessageCodec;
use stacks_common::types::chainstate::{
    BlockHeaderHash, BurnchainHeaderHash, StacksAddress, StacksBlockId,
};
use stacks_common::types::net::{PeerAddress, PeerHost};
use stacks_common::types::sqlite::NO_PARAMS;
use stacks_common::types::StacksPublicKeyBuffer;
use stacks_common::util::chunked_encoding::*;
use stacks_common::util::get_epoch_time_secs;
use stacks_common::util::hash::{hex_bytes, to_hex, Hash160, Sha256Sum};
use stacks_common::util::secp256k1::MessageSignature;
use stacks_common::{types, util};

use crate::burnchains::affirmation::AffirmationMap;
use crate::burnchains::{Burnchain, BurnchainView, *};
use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::burn::operations::leader_block_commit::OUTPUTS_PER_COMMIT;
use crate::chainstate::burn::ConsensusHash;
use crate::chainstate::stacks::db::blocks::{CheckError, MINIMUM_TX_FEE_RATE_PER_BYTE};
use crate::chainstate::stacks::db::StacksChainState;
use crate::chainstate::stacks::{Error as chain_error, StacksBlockHeader, *};
use crate::clarity_vm::clarity::{ClarityConnection, Error as clarity_error};
use crate::clarity_vm::database::marf::MarfedKV;
use crate::core::mempool::*;
use crate::cost_estimates::metrics::CostMetric;
use crate::cost_estimates::{CostEstimator, FeeEstimator};
use crate::net::atlas::{AtlasDB, Attachment, MAX_ATTACHMENT_INV_PAGES_PER_REQUEST};
use crate::net::connection::{ConnectionHttp, ConnectionOptions, ReplyHandleHttp};
use crate::net::db::PeerDB;
use crate::net::http::{HttpRequestContents, HttpResponseContents};
use crate::net::httpcore::{
    StacksHttp, StacksHttpMessage, StacksHttpRequest, StacksHttpResponse, HTTP_REQUEST_ID_RESERVED,
};
use crate::net::p2p::{PeerMap, PeerNetwork};
use crate::net::relay::Relayer;
use crate::net::stackerdb::{StackerDBTx, StackerDBs};
use crate::net::{Error as net_error, StacksMessageType, StacksNodeState};
use crate::util_lib::boot::boot_code_id;
use crate::util_lib::db::{DBConn, Error as db_error};
use crate::util_lib::strings::UrlString;
use crate::{monitoring, version_string};

pub const STREAM_CHUNK_SIZE: u64 = 4096;

pub struct ConversationHttp {
    /// send/receive buffering state-machine for interfacing with a non-blocking socket
    connection: ConnectionHttp,
    /// poll ID for this struct's associated socket
    conn_id: usize,
    /// time (in seconds) for how long an attempt to connect to a peer is allowed to take
    timeout: u64,
    /// remote host's identifier (DNS or IP).  Goes into the `Host:` header
    peer_host: PeerHost,
    /// URL of the remote peer's data, if given
    outbound_url: Option<UrlString>,
    /// remote host's IP address
    peer_addr: SocketAddr,
    /// remote host's keep-alive setting
    keep_alive: bool,
    /// number of messages consumed
    total_request_count: u64,
    /// number of messages sent
    total_reply_count: u64,
    /// absolute timestamp of the last time we recieved at least 1 byte
    last_request_timestamp: u64,
    /// absolute timestamp of the last time we sent at least 1 byte
    last_response_timestamp: u64,
    /// absolute time when this conversation was instantiated
    connection_time: u64,
    /// stacks canonical chain tip that this peer reported
    canonical_stacks_tip_height: Option<u32>,
    /// Ongoing replies
    reply_streams: VecDeque<(ReplyHandleHttp, HttpResponseContents, bool)>,
    /// outstanding request
    pending_request: Option<ReplyHandleHttp>,
    /// outstanding response
    pending_response: Option<StacksHttpResponse>,
    /// how much data to buffer (i.e. the socket's send buffer size)
    socket_send_buffer_size: u32,
}

impl fmt::Display for ConversationHttp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "http:id={},request={:?},peer={:?}",
            self.conn_id,
            self.pending_request.is_some(),
            &self.peer_addr
        )
    }
}

impl fmt::Debug for ConversationHttp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "http:id={},request={:?},peer={:?}",
            self.conn_id,
            self.pending_request.is_some(),
            &self.peer_addr
        )
    }
}

impl ConversationHttp {
    pub fn new(
        peer_addr: SocketAddr,
        outbound_url: Option<UrlString>,
        peer_host: PeerHost,
        conn_opts: &ConnectionOptions,
        conn_id: usize,
        socket_send_buffer_size: u32,
    ) -> ConversationHttp {
        let stacks_http = StacksHttp::new(peer_addr.clone(), conn_opts);
        ConversationHttp {
            connection: ConnectionHttp::new(stacks_http, conn_opts, None),
            conn_id,
            timeout: conn_opts.timeout,
            reply_streams: VecDeque::new(),
            peer_addr,
            outbound_url,
            peer_host,
            canonical_stacks_tip_height: None,
            pending_request: None,
            pending_response: None,
            keep_alive: true,
            total_request_count: 0,
            total_reply_count: 0,
            last_request_timestamp: 0,
            last_response_timestamp: 0,
            socket_send_buffer_size,
            connection_time: get_epoch_time_secs(),
        }
    }

    /// How many ongoing requests do we have on this conversation?
    pub fn num_pending_outbound(&self) -> usize {
        self.reply_streams.len()
    }

    /// What's our outbound URL?
    pub fn get_url(&self) -> Option<&UrlString> {
        self.outbound_url.as_ref()
    }

    /// What's our peer IP address?
    pub fn get_peer_addr(&self) -> &SocketAddr {
        &self.peer_addr
    }

    /// Is a request in-progress?
    pub fn is_request_inflight(&self) -> bool {
        self.pending_request.is_some() || self.pending_response.is_some()
    }

    /// Start a HTTP request from this peer, and expect a response.
    /// Returns the request handle; does not set the handle into this connection.
    fn start_request(&mut self, req: StacksHttpRequest) -> Result<ReplyHandleHttp, net_error> {
        test_debug!(
            "{:?},id={}: Start HTTP request {:?}",
            &self.peer_host,
            self.conn_id,
            &req
        );
        let mut handle = self.connection.make_request_handle(
            HTTP_REQUEST_ID_RESERVED,
            get_epoch_time_secs() + self.timeout,
            self.conn_id,
        )?;
        let stacks_msg = StacksHttpMessage::Request(req);
        self.connection.send_message(&mut handle, &stacks_msg)?;
        Ok(handle)
    }

    /// Start a HTTP request from this peer, and expect a response.
    /// Non-blocking.
    /// Only one request in-flight is allowed.
    pub fn send_request(&mut self, req: StacksHttpRequest) -> Result<(), net_error> {
        if self.is_request_inflight() {
            test_debug!(
                "{:?},id={}: Request in progress still",
                &self.peer_host,
                self.conn_id
            );
            return Err(net_error::InProgress);
        }
        let handle = self.start_request(req)?;

        self.pending_request = Some(handle);
        self.pending_response = None;
        Ok(())
    }

    /// Send a HTTP error response.
    /// Discontinues and disables sending a non-error response.
    pub fn reply_error(&mut self, res: StacksHttpResponse) -> Result<(), net_error> {
        if self.is_request_inflight() || self.pending_response.is_some() {
            test_debug!(
                "{:?},id={}: Request or response is already in progress",
                &self.peer_host,
                self.conn_id
            );
            return Err(net_error::InProgress);
        }
        let (mut preamble, body_contents) = res.try_into_contents()?;
        preamble.content_length = body_contents.content_length();
        preamble.keep_alive = false;

        // account for the request
        self.total_request_count += 1;

        // make the relay handle. There may not have been a valid request in the first place, so
        // we'll use a relay handle (not a reply handle) to push out the error.
        let mut reply = self.connection.make_relay_handle(self.conn_id)?;

        // queue up the HTTP headers, and then stream back the body.
        preamble.consensus_serialize(&mut reply)?;
        self.reply_streams.push_back((reply, body_contents, false));
        Ok(())
    }

    /// Handle an external HTTP request.
    /// Returns a StacksMessageType option -- it's Some(...) if we need to forward a message to the
    /// peer network (like a transaction or a block or microblock)
    pub fn handle_request(
        &mut self,
        req: StacksHttpRequest,
        node: &mut StacksNodeState,
    ) -> Result<Option<StacksMessageType>, net_error> {
        // NOTE: This may set node.relay_message
        let keep_alive = req.preamble().keep_alive;
        let (mut response_preamble, response_body) =
            self.connection.protocol.try_handle_request(req, node)?;

        let mut reply = self.connection.make_relay_handle(self.conn_id)?;
        let relay_msg_opt = node.take_relay_message();

        // make sure content-length is properly set, based on how we're about to stream data back
        response_preamble.content_length = response_body.content_length();

        // buffer up response headers into the reply handle
        response_preamble.consensus_serialize(&mut reply)?;
        self.reply_streams
            .push_back((reply, response_body, keep_alive));
        Ok(relay_msg_opt)
    }

    /// Make progress on outbound requests.
    fn send_outbound_responses(&mut self) -> Result<(), net_error> {
        // send out streamed responses in the order they were requested
        let mut drained_handle = false;
        let mut drained_stream = false;
        let mut broken = false;
        let mut do_keep_alive = true;

        test_debug!(
            "{:?}: {} HTTP replies pending",
            &self,
            self.reply_streams.len()
        );
        let _self_str = format!("{}", &self);

        if let Some((ref mut reply, ref mut http_response, ref keep_alive)) =
            self.reply_streams.front_mut()
        {
            do_keep_alive = *keep_alive;

            while !drained_stream {
                // write out the last-generated data into the write-end of the reply handle's pipe
                if let Some(pipe_fd) = reply.inner_pipe_out() {
                    let num_written = http_response.pipe_out(pipe_fd)?;
                    if num_written == 0 {
                        // no more chunks
                        drained_stream = true;
                    }
                    test_debug!("{}: Wrote {} bytes", &_self_str, num_written);
                    if (pipe_fd.pending() as u32) >= self.socket_send_buffer_size {
                        // we've written more data than can be dumped into the socket buffer, so
                        // we're good to go for now -- we'll get an edge trigger next time the data
                        // drains from this socket.
                        break;
                    }
                } else {
                    test_debug!("{}: No inner pipe", &_self_str);
                    drained_stream = true;
                }
            }

            if !drained_stream {
                // Consume data from the read-end of the reply-handle's pipe and try to drain it into
                // the socket.  Note that this merely fills the socket buffer; the read-end may still
                // have pending data after this call (which will need to be drained into the
                // socket by a subsequent call to `try_flush()` -- i.e. on the next pass of the
                // event loop).
                //
                // The `false` parameter means that the handle should be able to continue to receive
                // more data from the write-end (i.e. the request handler's streamer instance) even if
                // all data gets drained to the socket buffer on flush.
                match reply.try_flush_ex(false) {
                    Ok(res) => {
                        test_debug!("{}: Streamed reply is drained?: {}", &_self_str, res);
                        drained_handle = res;
                    }
                    Err(e) => {
                        // dead
                        warn!("{}: Broken HTTP connection: {:?}", &_self_str, &e);
                        broken = true;
                    }
                }
            } else {
                // If we're actually done sending data, then try to flush the reply handle without
                // expecting more data to be written to the write-end of this reply handle's pipe.
                // Then, once all bufferred data gets drained to the socket, we can drop this request.
                match reply.try_flush() {
                    Ok(res) => {
                        test_debug!("{}: Streamed reply is drained?: {}", &_self_str, res);
                        drained_handle = res;
                    }
                    Err(e) => {
                        // dead
                        warn!("{}: Broken HTTP connection: {:?}", &_self_str, &e);
                        broken = true;
                    }
                }
            }
        }

        test_debug!(
            "broken = {}, drained_handle = {}, drained_stream = {}",
            broken,
            drained_handle,
            drained_stream
        );
        if broken || (drained_handle && drained_stream) {
            // done with this stream
            test_debug!(
                "{:?}: done with stream (broken={}, drained_handle={}, drained_stream={}, do_keep_alive={})",
                &self,
                broken,
                drained_handle,
                drained_stream,
                do_keep_alive,
            );
            self.total_reply_count += 1;
            self.reply_streams.pop_front();

            if !do_keep_alive {
                // encountered "Connection: close"
                self.keep_alive = false;
            }
        }
        Ok(())
    }

    /// Try to move pending bytes into and out of the reply handle.
    /// If we finish doing so, then extract the StacksHttpResponse
    /// If we are not done yet, then return Ok(reply-handle) if we can try again, or net_error if
    /// we cannot.
    fn try_send_recv_response(
        req: ReplyHandleHttp,
    ) -> Result<StacksHttpResponse, Result<ReplyHandleHttp, net_error>> {
        match req.try_send_recv() {
            Ok(message) => match message {
                StacksHttpMessage::Request(_) => {
                    warn!("Received response: not a HTTP response");
                    return Err(Err(net_error::InvalidMessage));
                }
                StacksHttpMessage::Response(http_response) => Ok(http_response),
                StacksHttpMessage::Error(_, http_response) => Ok(http_response),
            },
            Err(res) => Err(res),
        }
    }

    /// Make progress on our request/response
    fn recv_inbound_response(&mut self) -> Result<(), net_error> {
        // make progress on our pending request (if it exists).
        let in_progress = self.pending_request.is_some();
        let is_pending = self.pending_response.is_none();

        let pending_request = self.pending_request.take();
        let response = match pending_request {
            None => Ok(self.pending_response.take()),
            Some(req) => match Self::try_send_recv_response(req) {
                Ok(response) => Ok(Some(response)),
                Err(res) => match res {
                    Ok(handle) => {
                        // try again
                        self.pending_request = Some(handle);
                        Ok(self.pending_response.take())
                    }
                    Err(e) => Err(e),
                },
            },
        }?;

        self.pending_response = response;

        if in_progress && self.pending_request.is_none() {
            test_debug!(
                "{:?},id={}: HTTP request finished",
                &self.peer_host,
                self.conn_id
            );
        }

        if is_pending && self.pending_response.is_some() {
            test_debug!(
                "{:?},id={}: HTTP response finished",
                &self.peer_host,
                self.conn_id
            );
        }

        Ok(())
    }

    /// Try to get our response
    pub fn try_get_response(&mut self) -> Option<StacksHttpResponse> {
        self.pending_response.take()
    }

    /// Make progress on in-flight messages.
    pub fn try_flush(&mut self) -> Result<(), net_error> {
        self.send_outbound_responses()?;
        self.recv_inbound_response()?;
        Ok(())
    }

    /// Is the connection idle?
    pub fn is_idle(&self) -> bool {
        test_debug!(
            "{:?} is_idle? {},{},{},{}",
            self,
            self.pending_response.is_none(),
            self.connection.inbox_len(),
            self.connection.outbox_len(),
            self.reply_streams.len()
        );
        self.pending_response.is_none()
            && self.connection.inbox_len() == 0
            && self.connection.outbox_len() == 0
            && self.reply_streams.is_empty()
    }

    /// Is the conversation out of pending data?
    /// Don't consider it drained if we haven't received anything yet
    pub fn is_drained(&self) -> bool {
        test_debug!(
            "{:?} is_drained? {},{}",
            self,
            self.total_request_count,
            self.total_reply_count
        );
        self.total_request_count > 0 && self.total_reply_count > 0 && self.is_idle()
    }

    /// Should the connection be kept alive even if drained?
    pub fn is_keep_alive(&self) -> bool {
        self.keep_alive
    }

    /// When was the last time we got an inbound request?
    pub fn get_last_request_time(&self) -> u64 {
        self.last_request_timestamp
    }

    /// When was the last time we sent data as part of an outbound response?
    pub fn get_last_response_time(&self) -> u64 {
        self.last_response_timestamp
    }

    /// When was this converation conencted?
    #[cfg_attr(test, mutants::skip)]
    pub fn get_connection_time(&self) -> u64 {
        self.connection_time
    }

    /// Make progress on in-flight requests and replies.
    /// Returns the list of messages we'll need to forward to the peer network
    pub fn chat(
        &mut self,
        node: &mut StacksNodeState,
    ) -> Result<Vec<StacksMessageType>, net_error> {
        // handle in-bound HTTP request(s)
        let num_inbound = self.connection.inbox_len();
        let mut ret = vec![];
        test_debug!("{:?}: {} HTTP requests pending", &self, num_inbound);

        for _i in 0..num_inbound {
            let Some(msg) = self.connection.next_inbox_message() else {
                continue;
            };

            match msg {
                StacksHttpMessage::Request(req) => {
                    // new request that we can handle
                    self.total_request_count += 1;
                    self.last_request_timestamp = get_epoch_time_secs();
                    let latency = req.duration_ms();
                    let start_time = Instant::now();
                    let verb = req.verb().to_string();
                    let request_path = req.request_path().to_string();
                    let msg_opt = monitoring::instrument_http_request_handler(
                        self,
                        req,
                        |conv_http, req| conv_http.handle_request(req, node),
                    )?;

                    info!("Handled StacksHTTPRequest";
                          "verb" => %verb,
                          "path" => %request_path,
                          "processing_time_ms" => start_time.elapsed().as_millis(),
                          "latency_ms" => latency,
                          "conn_id" => self.conn_id,
                          "peer_addr" => &self.peer_addr,
                          "p2p_msg" => ?msg_opt);

                    if let Some(msg) = msg_opt {
                        ret.push(msg);
                    }
                }
                StacksHttpMessage::Error(path, resp) => {
                    // new request, but resulted in an error when parsing it
                    self.last_request_timestamp = get_epoch_time_secs();
                    let start_time = Instant::now();
                    self.reply_error(resp)?;

                    info!("Handled StacksHTTPRequest Error"; "path" => %path, "processing_time_ms" => start_time.elapsed().as_millis(), "conn_id" => self.conn_id, "peer_addr" => &self.peer_addr);
                }
                StacksHttpMessage::Response(resp) => {
                    // Is there someone else waiting for this message?  If so, pass it along.
                    // (this _should_ be our pending_request handle)
                    match self
                        .connection
                        .fulfill_request(StacksHttpMessage::Response(resp))
                    {
                        None => {
                            test_debug!("{:?}: Fulfilled pending HTTP request", &self);
                        }
                        Some(_msg) => {
                            // unsolicited; discard
                            test_debug!("{:?}: Dropping unsolicited HTTP response", &self);
                        }
                    }
                }
            }
        }

        Ok(ret)
    }

    /// Remove all timed-out messages, and ding the remote peer as unhealthy
    #[cfg_attr(test, mutants::skip)]
    pub fn clear_timeouts(&mut self) {
        self.connection.drain_timeouts();
    }

    /// Load data into our HTTP connection
    pub fn recv<R: Read>(&mut self, r: &mut R) -> Result<usize, net_error> {
        let mut total_recv = 0;
        loop {
            let nrecv = match self.connection.recv_data(r) {
                Ok(nr) => nr,
                Err(e) => {
                    debug!("{:?}: failed to recv: {:?}", self, &e);
                    return Err(e);
                }
            };

            total_recv += nrecv;
            if nrecv > 0 {
                self.last_request_timestamp = get_epoch_time_secs();
            } else {
                break;
            }
        }
        monitoring::update_inbound_rpc_bandwidth(total_recv as i64);
        Ok(total_recv)
    }

    /// Write data out of our HTTP connection.  Write as much as we can
    #[cfg_attr(test, mutants::skip)]
    pub fn send<W: Write>(&mut self, w: &mut W) -> Result<usize, net_error> {
        let mut total_sz = 0;
        loop {
            test_debug!("{:?}: Try to send bytes (total {})", self, total_sz);

            // fill the reply handles in self.connection with data
            self.try_flush()?;

            // dump reply handle state into `w`
            let sz = match self.connection.send_data(w) {
                Ok(sz) => sz,
                Err(e) => {
                    info!("{:?}: failed to send on HTTP conversation: {:?}", self, &e);
                    return Err(e);
                }
            };
            test_debug!("{:?}: Sent {} bytes (total {})", self, sz, total_sz);

            total_sz += sz;
            if sz > 0 {
                self.last_response_timestamp = get_epoch_time_secs();
            } else {
                break;
            }
        }
        monitoring::update_inbound_rpc_bandwidth(total_sz as i64);
        Ok(total_sz)
    }

    pub fn get_peer_host(&self) -> PeerHost {
        self.peer_host.clone()
    }

    pub fn metrics_identifier(&self, req: &mut StacksHttpRequest) -> &str {
        self.connection.protocol.metrics_identifier(req)
    }
}
