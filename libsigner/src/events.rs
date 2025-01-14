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

use std::fmt::Debug;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::SystemTime;

use blockstack_lib::chainstate::nakamoto::NakamotoBlock;
use blockstack_lib::chainstate::stacks::boot::{MINERS_NAME, SIGNERS_NAME};
use blockstack_lib::chainstate::stacks::events::StackerDBChunksEvent;
use blockstack_lib::chainstate::stacks::StacksTransaction;
use blockstack_lib::net::api::postblock_proposal::{
    BlockValidateReject, BlockValidateResponse, ValidateRejectCode,
};
use blockstack_lib::net::stackerdb::MINER_SLOT_COUNT;
use blockstack_lib::util_lib::boot::boot_code_id;
use clarity::vm::types::serialization::SerializationError;
use clarity::vm::types::QualifiedContractIdentifier;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stacks_common::codec::{
    read_next, read_next_at_most, read_next_exact, write_next, Error as CodecError,
    StacksMessageCodec,
};
pub use stacks_common::consts::SIGNER_SLOTS_PER_USER;
use stacks_common::types::chainstate::{
    BlockHeaderHash, BurnchainHeaderHash, ConsensusHash, SortitionId, StacksPublicKey,
};
use stacks_common::util::hash::{Hash160, Sha512Trunc256Sum};
use stacks_common::util::HexError;
use tiny_http::{
    Method as HttpMethod, Request as HttpRequest, Response as HttpResponse, Server as HttpServer,
};

use crate::http::{decode_http_body, decode_http_request};
use crate::EventError;

/// Define the trait for the event processor
pub trait SignerEventTrait<T: StacksMessageCodec + Clone + Debug + Send = Self>:
    StacksMessageCodec + Clone + Debug + Send
{
}

impl<T: StacksMessageCodec + Clone + Debug + Send> SignerEventTrait for T {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// BlockProposal sent to signers
pub struct BlockProposal {
    /// The block itself
    pub block: NakamotoBlock,
    /// The burn height the block is mined during
    pub burn_height: u64,
    /// The reward cycle the block is mined during
    pub reward_cycle: u64,
}

impl StacksMessageCodec for BlockProposal {
    fn consensus_serialize<W: Write>(&self, fd: &mut W) -> Result<(), CodecError> {
        self.block.consensus_serialize(fd)?;
        self.burn_height.consensus_serialize(fd)?;
        self.reward_cycle.consensus_serialize(fd)?;
        Ok(())
    }

    fn consensus_deserialize<R: Read>(fd: &mut R) -> Result<Self, CodecError> {
        let block = NakamotoBlock::consensus_deserialize(fd)?;
        let burn_height = u64::consensus_deserialize(fd)?;
        let reward_cycle = u64::consensus_deserialize(fd)?;
        Ok(BlockProposal {
            block,
            burn_height,
            reward_cycle,
        })
    }
}

/// Event enum for newly-arrived signer subscribed events
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SignerEvent<T: SignerEventTrait> {
    /// A miner sent a message over .miners
    /// The `Vec<T>` will contain any signer messages made by the miner.
    /// The `StacksPublicKey` is the message sender's public key.
    MinerMessages(Vec<T>, StacksPublicKey),
    /// The signer messages for other signers and miners to observe
    /// The u32 is the signer set to which the message belongs (either 0 or 1)
    SignerMessages(u32, Vec<T>),
    /// A new block proposal validation response from the node
    BlockValidationResponse(BlockValidateResponse),
    /// Status endpoint request
    StatusCheck,
    /// A new burn block event was received with the given burnchain block height
    NewBurnBlock {
        /// the burn height for the newly processed burn block
        burn_height: u64,
        /// the burn hash for the newly processed burn block
        burn_header_hash: BurnchainHeaderHash,
        /// the time at which this event was received by the signer's event processor
        received_time: SystemTime,
    },
    /// A new processed Stacks block was received from the node with the given block hash
    NewBlock {
        /// The block header hash for the newly processed stacks block
        block_hash: Sha512Trunc256Sum,
        /// The block height for the newly processed stacks block
        block_height: u64,
    },
}

/// Trait to implement a stop-signaler for the event receiver thread.
/// The caller calls `send()` and the event receiver loop (which lives in a separate thread) will
/// terminate.
pub trait EventStopSignaler {
    /// Send the stop signal
    fn send(&mut self);
}

/// Trait to implement to handle signer specific events sent by the Stacks node
pub trait EventReceiver<T: SignerEventTrait> {
    /// The implementation of ST will ensure that a call to ST::send() will cause
    /// the call to `is_stopped()` below to return true.
    type ST: EventStopSignaler + Send + Sync;

    /// Open a server socket to the given socket address.
    fn bind(&mut self, listener: SocketAddr) -> Result<SocketAddr, EventError>;
    /// Return the next event
    fn next_event(&mut self) -> Result<SignerEvent<T>, EventError>;
    /// Add a downstream event consumer
    fn add_consumer(&mut self, event_out: Sender<SignerEvent<T>>);
    /// Forward the event to downstream consumers
    fn forward_event(&mut self, ev: SignerEvent<T>) -> bool;
    /// Determine if the receiver should hang up
    fn is_stopped(&self) -> bool;
    /// Get a stop signal instance that, when sent, will cause this receiver to stop accepting new
    /// events.  Called after `bind()`.
    fn get_stop_signaler(&mut self) -> Result<Self::ST, EventError>;

    /// Main loop for the receiver.
    /// Typically, this is started in a separate thread.
    fn main_loop(&mut self) {
        loop {
            if self.is_stopped() {
                info!("Event receiver stopped");
                break;
            }
            let next_event = match self.next_event() {
                Ok(event) => event,
                Err(EventError::UnrecognizedEvent(..)) => {
                    // got an event that we don't care about (not a problem)
                    continue;
                }
                Err(EventError::Terminated) => {
                    // we're done
                    info!("Caught termination signal");
                    break;
                }
                Err(e) => {
                    warn!("Failed to receive next event: {:?}", &e);
                    continue;
                }
            };
            if !self.forward_event(next_event) {
                info!("Failed to forward event");
                break;
            }
        }
        info!("Event receiver main loop exit");
    }
}

/// Event receiver for Signer events
pub struct SignerEventReceiver<T: SignerEventTrait> {
    /// Address we bind to
    local_addr: Option<SocketAddr>,
    /// server socket that listens for HTTP POSTs from the node
    http_server: Option<HttpServer>,
    /// channel into which to write newly-discovered data
    out_channels: Vec<Sender<SignerEvent<T>>>,
    /// inter-thread stop variable -- if set to true, then the `main_loop` will exit
    stop_signal: Arc<AtomicBool>,
    /// Whether the receiver is running on mainnet
    is_mainnet: bool,
}

impl<T: SignerEventTrait> SignerEventReceiver<T> {
    /// Make a new Signer event receiver, and return both the receiver and the read end of a
    /// channel into which node-received data can be obtained.
    pub fn new(is_mainnet: bool) -> SignerEventReceiver<T> {
        SignerEventReceiver {
            http_server: None,
            local_addr: None,
            out_channels: vec![],
            stop_signal: Arc::new(AtomicBool::new(false)),
            is_mainnet,
        }
    }

    /// Do something with the socket
    pub fn with_server<F, R>(&mut self, todo: F) -> Result<R, EventError>
    where
        F: FnOnce(&SignerEventReceiver<T>, &mut HttpServer, bool) -> R,
    {
        let mut server = if let Some(s) = self.http_server.take() {
            s
        } else {
            return Err(EventError::NotBound);
        };

        let res = todo(self, &mut server, self.is_mainnet);

        self.http_server = Some(server);
        Ok(res)
    }
}

/// Stop signaler implementation
pub struct SignerStopSignaler {
    stop_signal: Arc<AtomicBool>,
    local_addr: SocketAddr,
}

impl SignerStopSignaler {
    /// Make a new stop signaler
    pub fn new(sig: Arc<AtomicBool>, local_addr: SocketAddr) -> SignerStopSignaler {
        SignerStopSignaler {
            stop_signal: sig,
            local_addr,
        }
    }
}

impl EventStopSignaler for SignerStopSignaler {
    #[cfg_attr(test, mutants::skip)]
    fn send(&mut self) {
        self.stop_signal.store(true, Ordering::SeqCst);
        // wake up the thread so the atomicbool can be checked
        // This makes me sad...but for now...it works.
        if let Ok(mut stream) = TcpStream::connect(self.local_addr) {
            // We need to send actual data to trigger the event receiver
            let body = "Yo. Shut this shit down!".to_string();
            let req = format!(
                "POST /shutdown HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n{}",
                self.local_addr,
                body.len(),
                body
            );
            if let Err(e) = stream.write_all(req.as_bytes()) {
                error!("Failed to send shutdown request: {}", e);
            }
        }
    }
}

impl<T: SignerEventTrait> EventReceiver<T> for SignerEventReceiver<T> {
    type ST = SignerStopSignaler;

    /// Start listening on the given socket address.
    /// Returns the address that was bound.
    /// Errors out if bind(2) fails
    fn bind(&mut self, listener: SocketAddr) -> Result<SocketAddr, EventError> {
        self.http_server = Some(HttpServer::http(listener).expect("failed to start HttpServer"));
        self.local_addr = Some(listener);
        Ok(listener)
    }

    /// Wait for the node to post something, and then return it.
    /// Errors are recoverable -- the caller should call this method again even if it returns an
    /// error.
    fn next_event(&mut self) -> Result<SignerEvent<T>, EventError> {
        self.with_server(|event_receiver, http_server, _is_mainnet| {
            // were we asked to terminate?
            if event_receiver.is_stopped() {
                return Err(EventError::Terminated);
            }
            debug!("Request handling");
            let request = http_server.recv()?;
            debug!("Got request"; "method" => %request.method(), "path" => request.url());

            if request.url() == "/status" {
                request
                .respond(HttpResponse::from_string("OK"))
                .expect("response failed");
                return Ok(SignerEvent::StatusCheck);
            }

            if request.method() != &HttpMethod::Post {
                return Err(EventError::MalformedRequest(format!(
                    "Unrecognized method '{}'",
                    &request.method(),
                )));
            }
            debug!("Processing {} event", request.url());
            if request.url() == "/stackerdb_chunks" {
                process_event::<T, StackerDBChunksEvent>(request)
            } else if request.url() == "/proposal_response" {
                process_event::<T, BlockValidateResponse>(request)
            } else if request.url() == "/new_burn_block" {
                process_event::<T, BurnBlockEvent>(request)
            } else if request.url() == "/shutdown" {
                event_receiver.stop_signal.store(true, Ordering::SeqCst);
                Err(EventError::Terminated)
            } else if request.url() == "/new_block" {
                process_event::<T, BlockEvent>(request)
            } else {
                let url = request.url().to_string();
                debug!(
                    "[{:?}] next_event got request with unexpected url {}, return OK so other side doesn't keep sending this",
                    event_receiver.local_addr,
                    url
                );
                ack_dispatcher(request);
                Err(EventError::UnrecognizedEvent(url))
            }
        })?
    }

    /// Determine if the receiver is hung up
    fn is_stopped(&self) -> bool {
        self.stop_signal.load(Ordering::SeqCst)
    }

    /// Forward an event
    /// Return true on success; false on error.
    /// Returning false terminates the event receiver.
    fn forward_event(&mut self, ev: SignerEvent<T>) -> bool {
        if self.out_channels.is_empty() {
            // nothing to do
            error!("No channels connected to event receiver");
            false
        } else if self.out_channels.len() == 1 {
            // avoid a clone
            if let Err(e) = self.out_channels[0].send(ev) {
                error!("Failed to send to signer runloop: {:?}", &e);
                return false;
            }
            true
        } else {
            for (i, out_channel) in self.out_channels.iter().enumerate() {
                if let Err(e) = out_channel.send(ev.clone()) {
                    error!("Failed to send to signer runloop #{}: {:?}", i, &e);
                    return false;
                }
            }
            true
        }
    }

    /// Add an event consumer.  A received event will be forwarded to this Sender.
    fn add_consumer(&mut self, out_channel: Sender<SignerEvent<T>>) {
        self.out_channels.push(out_channel);
    }

    /// Get a stopped signaler.  The caller can then use it to terminate the event receiver loop,
    /// even if it's in a different thread.
    fn get_stop_signaler(&mut self) -> Result<SignerStopSignaler, EventError> {
        if let Some(local_addr) = self.local_addr {
            Ok(SignerStopSignaler::new(
                self.stop_signal.clone(),
                local_addr,
            ))
        } else {
            Err(EventError::NotBound)
        }
    }
}

fn ack_dispatcher(request: HttpRequest) {
    if let Err(e) = request.respond(HttpResponse::empty(200u16)) {
        error!("Failed to respond to request: {:?}", &e);
    };
}

// TODO: add tests from mutation testing results #4835
#[cfg_attr(test, mutants::skip)]
fn process_event<T, E>(mut request: HttpRequest) -> Result<SignerEvent<T>, EventError>
where
    T: SignerEventTrait,
    E: serde::de::DeserializeOwned + TryInto<SignerEvent<T>, Error = EventError>,
{
    let mut body = String::new();

    if let Err(e) = request.as_reader().read_to_string(&mut body) {
        error!("Failed to read body: {:?}", &e);
        ack_dispatcher(request);
        return Err(EventError::MalformedRequest(format!(
            "Failed to read body: {:?}",
            &e
        )));
    }
    // Regardless of whether we successfully deserialize, we should ack the dispatcher so they don't keep resending it
    ack_dispatcher(request);
    let json_event: E = serde_json::from_slice(body.as_bytes())
        .map_err(|e| EventError::Deserialize(format!("Could not decode body to JSON: {:?}", &e)))?;

    let signer_event: SignerEvent<T> = json_event.try_into()?;

    Ok(signer_event)
}

impl<T: SignerEventTrait> TryFrom<StackerDBChunksEvent> for SignerEvent<T> {
    type Error = EventError;

    fn try_from(event: StackerDBChunksEvent) -> Result<Self, Self::Error> {
        let signer_event = if event.contract_id.name.as_str() == MINERS_NAME
            && event.contract_id.is_boot()
        {
            let mut messages = vec![];
            let mut miner_pk = None;
            for chunk in event.modified_slots {
                let Ok(msg) = T::consensus_deserialize(&mut chunk.data.as_slice()) else {
                    continue;
                };

                miner_pk = Some(chunk.recover_pk().map_err(|e| {
                    EventError::MalformedRequest(format!(
                        "Failed to recover PK from StackerDB chunk: {e}"
                    ))
                })?);
                messages.push(msg);
            }
            SignerEvent::MinerMessages(messages, miner_pk.ok_or(EventError::EmptyChunksEvent)?)
        } else if event.contract_id.name.starts_with(SIGNERS_NAME) && event.contract_id.is_boot() {
            let Some((signer_set, _)) =
                get_signers_db_signer_set_message_id(event.contract_id.name.as_str())
            else {
                return Err(EventError::UnrecognizedStackerDBContract(event.contract_id));
            };
            // signer-XXX-YYY boot contract
            let signer_messages: Vec<T> = event
                .modified_slots
                .iter()
                .filter_map(|chunk| read_next::<T, _>(&mut &chunk.data[..]).ok())
                .collect();
            SignerEvent::SignerMessages(signer_set, signer_messages)
        } else {
            return Err(EventError::UnrecognizedStackerDBContract(event.contract_id));
        };
        Ok(signer_event)
    }
}

impl<T: SignerEventTrait> TryFrom<BlockValidateResponse> for SignerEvent<T> {
    type Error = EventError;

    fn try_from(block_validate_response: BlockValidateResponse) -> Result<Self, Self::Error> {
        Ok(SignerEvent::BlockValidationResponse(
            block_validate_response,
        ))
    }
}

#[derive(Debug, Deserialize)]
struct BurnBlockEvent {
    burn_block_hash: String,
    burn_block_height: u64,
    reward_recipients: Vec<serde_json::Value>,
    reward_slot_holders: Vec<String>,
    burn_amount: u64,
}

impl<T: SignerEventTrait> TryFrom<BurnBlockEvent> for SignerEvent<T> {
    type Error = EventError;

    fn try_from(burn_block_event: BurnBlockEvent) -> Result<Self, Self::Error> {
        let burn_header_hash = burn_block_event
            .burn_block_hash
            .get(2..)
            .ok_or_else(|| EventError::Deserialize("Hex string should be 0x prefixed".into()))
            .and_then(|hex| {
                BurnchainHeaderHash::from_hex(hex)
                    .map_err(|e| EventError::Deserialize(format!("Invalid hex string: {e}")))
            })?;

        Ok(SignerEvent::NewBurnBlock {
            burn_height: burn_block_event.burn_block_height,
            received_time: SystemTime::now(),
            burn_header_hash,
        })
    }
}

#[derive(Debug, Deserialize)]
struct BlockEvent {
    block_hash: String,
    block_height: u64,
}

impl<T: SignerEventTrait> TryFrom<BlockEvent> for SignerEvent<T> {
    type Error = EventError;

    fn try_from(block_event: BlockEvent) -> Result<Self, Self::Error> {
        let block_hash: Sha512Trunc256Sum = block_event
            .block_hash
            .get(2..)
            .ok_or_else(|| EventError::Deserialize("Hex string should be 0x prefixed".into()))
            .and_then(|hex| {
                Sha512Trunc256Sum::from_hex(hex)
                    .map_err(|e| EventError::Deserialize(format!("Invalid hex string: {e}")))
            })?;
        Ok(SignerEvent::NewBlock {
            block_hash,
            block_height: block_event.block_height,
        })
    }
}

pub fn get_signers_db_signer_set_message_id(name: &str) -> Option<(u32, u32)> {
    // Splitting the string by '-'
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    // Extracting message ID and slot ID
    let signer_set = parts[1].parse::<u32>().ok()?;
    let message_id = parts[2].parse::<u32>().ok()?;
    Some((signer_set, message_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_signers_db_signer_set_message_id() {
        let name = "signer-1-1";
        let (signer_set, message_id) = get_signers_db_signer_set_message_id(name).unwrap();
        assert_eq!(signer_set, 1);
        assert_eq!(message_id, 1);

        let name = "signer-0-2";
        let (signer_set, message_id) = get_signers_db_signer_set_message_id(name).unwrap();
        assert_eq!(signer_set, 0);
        assert_eq!(message_id, 2);

        let name = "signer--2";
        assert!(get_signers_db_signer_set_message_id(name).is_none());
    }
}
