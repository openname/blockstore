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

use std::collections::{HashMap, VecDeque};
use std::io::{Error as io_error, ErrorKind, Read, Write};
use std::sync::mpsc::{sync_channel, Receiver, RecvError, SendError, SyncSender, TryRecvError};

use mio::net as mio_net;
use stacks_common::types::net::{PeerAddress, PeerHost};
use stacks_common::types::StacksEpochId;
use stacks_common::util::get_epoch_time_secs;

use crate::burnchains::{Burnchain, BurnchainView};
use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::stacks::db::StacksChainState;
use crate::core::mempool::*;
use crate::net::atlas::AtlasDB;
use crate::net::connection::*;
use crate::net::db::*;
use crate::net::http::*;
use crate::net::httpcore::*;
use crate::net::p2p::{PeerMap, PeerNetwork};
use crate::net::poll::*;
use crate::net::rpc::*;
use crate::net::{Error as net_error, *};

#[derive(Debug)]
pub struct HttpPeer {
    /// ongoing http conversations (either they reached out to us, or we to them)
    pub peers: HashMap<usize, ConversationHttp>,
    pub sockets: HashMap<usize, mio_net::TcpStream>,

    /// outbound connections that are pending connection
    pub connecting: HashMap<
        usize,
        (
            mio_net::TcpStream,
            Option<UrlString>,
            Option<StacksHttpRequest>,
            u64,
        ),
    >,

    /// server network handle
    pub http_server_handle: usize,

    /// server socket address
    pub http_server_addr: SocketAddr,

    /// connection options
    pub connection_opts: ConnectionOptions,
}

impl HttpPeer {
    pub fn new(
        conn_opts: ConnectionOptions,
        server_handle: usize,
        server_addr: SocketAddr,
    ) -> HttpPeer {
        HttpPeer {
            peers: HashMap::new(),
            sockets: HashMap::new(),

            connecting: HashMap::new(),
            http_server_handle: server_handle,
            http_server_addr: server_addr,

            connection_opts: conn_opts,
        }
    }

    pub fn set_server_handle(&mut self, h: usize, addr: SocketAddr) {
        self.http_server_handle = h;
        self.http_server_addr = addr;
    }

    /// Is there a HTTP conversation open to this data_url that is not in progress?
    #[cfg_attr(test, mutants::skip)]
    pub fn find_free_conversation(&self, data_url: &UrlString) -> Option<usize> {
        for (event_id, convo) in self.peers.iter() {
            if let Some(ref url) = convo.get_url() {
                if *url == data_url && !convo.is_request_inflight() {
                    return Some(*event_id);
                }
            }
        }
        None
    }

    /// Get a mut ref to a conversation
    #[cfg_attr(test, mutants::skip)]
    pub fn get_conversation(&mut self, event_id: usize) -> Option<&mut ConversationHttp> {
        self.peers.get_mut(&event_id)
    }

    /// Get a mut ref to a conversation and its socket
    pub fn get_conversation_and_socket(
        &mut self,
        event_id: usize,
    ) -> (
        Option<&mut ConversationHttp>,
        Option<&mut mio::net::TcpStream>,
    ) {
        (
            self.peers.get_mut(&event_id),
            self.sockets.get_mut(&event_id),
        )
    }

    /// Connect to a new remote HTTP endpoint, given the data URL and a (resolved) socket address to
    /// its origin.  Once connected, optionally send the given request.
    /// Idempotent -- will not re-connect if already connected and there is a free conversation channel open
    /// (will return Error::AlreadyConnected with the event ID)
    pub fn connect_http(
        &mut self,
        network_state: &mut NetworkState,
        network: &PeerNetwork,
        data_url: UrlString,
        addr: SocketAddr,
        request: Option<StacksHttpRequest>,
    ) -> Result<usize, net_error> {
        if let Some(event_id) = self.find_free_conversation(&data_url) {
            let http_nk = NeighborKey {
                peer_version: network.burnchain.peer_version,
                network_id: network.local_peer.network_id,
                addrbytes: PeerAddress::from_socketaddr(&addr),
                port: addr.port(),
            };
            return Err(net_error::AlreadyConnected(event_id, http_nk));
        }

        let sock = NetworkState::connect(
            &addr,
            network.connection_opts.socket_send_buffer_size,
            network.connection_opts.socket_recv_buffer_size,
        )?;
        let hint_event_id = network_state.next_event_id()?;
        let next_event_id =
            network_state.register(self.http_server_handle, hint_event_id, &sock)?;

        self.connecting.insert(
            next_event_id,
            (sock, Some(data_url), request, get_epoch_time_secs()),
        );
        Ok(next_event_id)
    }

    /// How many conversations are connected from this IP address?
    fn count_inbound_ip_addrs(&self, peer_addr: &SocketAddr) -> u64 {
        let mut count = 0;
        for (_, convo) in self.peers.iter() {
            if convo.get_url().is_none() && convo.get_peer_addr().ip() == peer_addr.ip() {
                count += 1;
            }
        }
        count
    }

    /// Can we register this socket?
    #[cfg_attr(test, mutants::skip)]
    fn can_register_http(
        &self,
        peer_addr: &SocketAddr,
        outbound_url: Option<&UrlString>,
    ) -> Result<(), net_error> {
        if outbound_url.is_none()
            && (self.peers.len() as u64) + 1 > self.connection_opts.max_http_clients
        {
            // inbound
            debug!(
                "HTTP: too many inbound peers total (max is {})",
                self.connection_opts.max_http_clients
            );
            return Err(net_error::TooManyPeers);
        }

        // how many other conversations are connected?
        let num_inbound = self.count_inbound_ip_addrs(peer_addr);
        if num_inbound > self.connection_opts.max_http_clients {
            // too many
            debug!(
                "HTTP: too many inbound HTTP peers from {:?} ({} > {})",
                peer_addr, num_inbound, self.connection_opts.max_http_clients
            );
            return Err(net_error::TooManyPeers);
        }

        debug!(
            "HTTP: Have {} peers now (max {}) inbound={}, including {} from host of {:?}",
            self.peers.len(),
            self.connection_opts.max_http_clients,
            outbound_url.is_none(),
            num_inbound,
            peer_addr
        );
        Ok(())
    }

    /// Low-level method to register a socket/event pair on the p2p network interface.
    /// Call only once the socket is connected (called once the socket triggers ready).
    /// Will destroy the socket if we can't register for whatever reason.
    #[cfg_attr(test, mutants::skip)]
    fn register_http(
        &mut self,
        network_state: &mut NetworkState,
        node_state: &mut StacksNodeState,
        event_id: usize,
        mut socket: mio_net::TcpStream,
        outbound_url: Option<UrlString>,
        initial_request: Option<StacksHttpRequest>,
    ) -> Result<(), net_error> {
        let send_buffer_size = node_state
            .with_node_state(|network, _, _, _, _| network.connection_opts.socket_send_buffer_size);

        let client_addr = match socket.peer_addr() {
            Ok(addr) => addr,
            Err(e) => {
                warn!("Failed to get peer address of {:?}: {:?}", &socket, &e);
                let _ = network_state.deregister(event_id, &socket);
                return Err(net_error::SocketError);
            }
        };

        match self.can_register_http(&client_addr, outbound_url.as_ref()) {
            Ok(_) => {}
            Err(e) => {
                let _ = network_state.deregister(event_id, &socket);
                return Err(e);
            }
        }

        let peer_host = match outbound_url {
            Some(ref url_str) => {
                PeerHost::try_from_url(url_str).unwrap_or(PeerHost::from_socketaddr(&client_addr))
            }
            None => PeerHost::from_socketaddr(&client_addr),
        };

        let mut new_convo = ConversationHttp::new(
            client_addr.clone(),
            outbound_url.clone(),
            peer_host,
            &self.connection_opts,
            event_id,
            send_buffer_size,
        );

        debug!(
            "Registered HTTP {:?} as event {} (outbound={:?})",
            &socket, event_id, &outbound_url
        );

        if let Some(request) = initial_request {
            test_debug!("Sending initial HTTP request to {:?}", &socket);
            match new_convo.send_request(request) {
                Ok(_) => {}
                Err(e) => {
                    let _ = network_state.deregister(event_id, &socket);
                    return Err(e);
                }
            }

            // prime the socket
            if let Err(e) = HttpPeer::saturate_http_socket(&mut socket, &mut new_convo) {
                let _ = network_state.deregister(event_id, &socket);
                return Err(e);
            }
        }

        self.sockets.insert(event_id, socket);
        self.peers.insert(event_id, new_convo);
        Ok(())
    }

    /// Deregister a socket/event pair
    #[cfg_attr(test, mutants::skip)]
    pub fn deregister_http(&mut self, network_state: &mut NetworkState, event_id: usize) {
        test_debug!("Remove HTTP event {}", event_id);
        self.peers.remove(&event_id);

        match self.sockets.remove(&event_id) {
            None => {}
            Some(sock) => {
                let _ = network_state.deregister(event_id, &sock);
            }
        }
        match self.connecting.remove(&event_id) {
            None => {}
            Some((sock, ..)) => {
                let _ = network_state.deregister(event_id, &sock);
            }
        }
    }

    /// Remove slow/unresponsive peers
    fn disconnect_unresponsive(&mut self, network_state: &mut NetworkState) {
        let now = get_epoch_time_secs();
        let mut to_remove = vec![];
        for (event_id, (socket, _, _, ts)) in self.connecting.iter() {
            if ts + self.connection_opts.connect_timeout < now {
                debug!("Disconnect connecting HTTP peer {:?}", &socket);
                to_remove.push(*event_id);
            }
        }

        for (event_id, convo) in self.peers.iter() {
            let mut last_request_time = convo.get_last_request_time();
            if last_request_time == 0 {
                // never got a request
                last_request_time = convo.get_connection_time();
            }

            let mut last_response_time = convo.get_last_response_time();
            if last_response_time == 0 {
                // never sent a response
                last_response_time = convo.get_connection_time();
            }

            if last_request_time + self.connection_opts.timeout < now
                && last_response_time + self.connection_opts.idle_timeout < now
            {
                // it's been too long
                debug!("Removing idle HTTP conversation {:?}", convo);
                to_remove.push(*event_id);
            }
        }

        for event_id in to_remove.into_iter() {
            self.deregister_http(network_state, event_id);
        }
    }

    /// Saturate a conversation's socket -- either sends the whole request, or fills the socket
    /// buffer.
    pub fn saturate_http_socket(
        client_sock: &mut mio::net::TcpStream,
        convo: &mut ConversationHttp,
    ) -> Result<(), net_error> {
        // saturate the socket
        loop {
            let send_res = convo.send(client_sock);
            match send_res {
                Err(e) => {
                    debug!("Failed to send data to socket {:?}: {:?}", &client_sock, &e);
                    return Err(e);
                }
                Ok(sz) => {
                    if sz == 0 {
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    /// Process new inbound HTTP connections we just accepted.
    /// Returns the event IDs of sockets we need to register
    fn process_new_sockets(
        &mut self,
        network_state: &mut NetworkState,
        node_state: &mut StacksNodeState,
        poll_state: &mut NetworkPollState,
    ) -> Vec<usize> {
        let mut registered = vec![];

        for (hint_event_id, client_sock) in poll_state.new.drain() {
            let event_id = match network_state.register(
                self.http_server_handle,
                hint_event_id,
                &client_sock,
            ) {
                Ok(event_id) => event_id,
                Err(e) => {
                    warn!(
                        "Failed to register HTTP connection {:?}: {:?}",
                        &client_sock, &e
                    );
                    continue;
                }
            };

            // event ID already used?
            if self.peers.contains_key(&event_id) {
                warn!(
                    "Already have an event {}: {:?}",
                    event_id,
                    self.peers.get(&event_id)
                );
                let _ = network_state.deregister(event_id, &client_sock);
                continue;
            }

            if let Err(_e) =
                self.register_http(network_state, node_state, event_id, client_sock, None, None)
            {
                // NOTE: register_http will deregister the socket for us
                continue;
            }
            registered.push(event_id);
        }

        registered
    }

    /// Process network traffic on a HTTP conversation.
    /// Returns whether or not the convo is still alive, as well as any message(s) that need to be
    /// forwarded to the peer network.
    fn process_http_conversation(
        node_state: &mut StacksNodeState,
        event_id: usize,
        client_sock: &mut mio_net::TcpStream,
        convo: &mut ConversationHttp,
    ) -> Result<(bool, Vec<StacksMessageType>), net_error> {
        // get incoming bytes and update the state of this conversation.
        let mut convo_dead = false;
        let recv_res = convo.recv(client_sock);
        match recv_res {
            Err(e) => {
                match e {
                    net_error::PermanentlyDrained => {
                        // socket got closed, but we might still have pending unsolicited messages
                        debug!(
                            "Remote HTTP peer disconnected event {} (socket {:?})",
                            event_id, &client_sock
                        );
                        convo_dead = true;
                    }
                    net_error::InvalidMessage => {
                        // got sent bad data.  If this was an inbound conversation, send it a HTTP
                        // 400 and close the socket.
                        debug!("Got a bad HTTP message on socket {:?}", &client_sock);
                        match convo.reply_error(StacksHttpResponse::new_empty_error(
                            &HttpBadRequest::new(
                                "Received an HTTP message that the node could not decode"
                                    .to_string(),
                            ),
                        )) {
                            Ok(_) => {
                                // prime the socket
                                if let Err(e) = HttpPeer::saturate_http_socket(client_sock, convo) {
                                    debug!(
                                        "Failed to flush HTTP 400 to socket {:?}: {:?}",
                                        &client_sock, &e
                                    );
                                    // convo_dead = true;
                                }
                            }
                            Err(e) => {
                                debug!(
                                    "Failed to reply HTTP 400 to socket {:?}: {:?}",
                                    &client_sock, &e
                                );
                                convo_dead = true;
                            }
                        }
                    }
                    _ => {
                        debug!(
                            "Failed to receive HTTP data on event {} (socket {:?}): {:?}",
                            event_id, &client_sock, &e
                        );
                        convo_dead = true;
                    }
                }
            }
            Ok(_) => {}
        }

        // react to inbound messages -- do we need to send something out, or fulfill requests
        // to other threads?  Try to chat even if the recv() failed, since we'll want to at
        // least drain the conversation inbox.
        let msgs = match convo.chat(node_state) {
            Ok(msgs) => msgs,
            Err(e) => {
                debug!(
                    "Failed to converse HTTP on event {} (socket {:?}): {:?}",
                    event_id, &client_sock, &e
                );
                convo_dead = true;
                vec![]
            }
        };

        if !convo_dead {
            // (continue) sending out data in this conversation, if the conversation is still
            // ongoing
            if let Err(e) = HttpPeer::saturate_http_socket(client_sock, convo) {
                debug!(
                    "Failed to send HTTP data to event {} (socket {:?}): {:?}",
                    event_id, &client_sock, &e
                );
                convo_dead = true;
            }
        }

        Ok((!convo_dead, msgs))
    }

    /// Is an event in the process of connecting?
    pub fn is_connecting(&self, event_id: usize) -> bool {
        self.connecting.contains_key(&event_id)
    }

    /// Process newly-connected sockets
    fn process_connecting_sockets(
        &mut self,
        network_state: &mut NetworkState,
        node_state: &mut StacksNodeState,
        poll_state: &mut NetworkPollState,
    ) {
        for event_id in poll_state.ready.iter() {
            if self.connecting.contains_key(event_id) {
                let (socket, data_url, initial_request_opt, _) =
                    self.connecting.remove(event_id).unwrap();

                debug!("HTTP event {} connected ({:?})", event_id, &data_url);

                if let Err(_e) = self.register_http(
                    network_state,
                    node_state,
                    *event_id,
                    socket,
                    data_url.clone(),
                    initial_request_opt,
                ) {
                    debug!(
                        "Failed to register HTTP connection ({}, {:?})",
                        event_id, data_url
                    );
                }
            }
        }
    }

    /// Process sockets that are ready, but specifically inbound or outbound only.
    /// Advance the state of all such conversations with remote peers.
    /// Return the list of events that correspond to failed conversations, as well as the list of
    /// peer network messages we'll need to forward
    #[cfg_attr(test, mutants::skip)]
    fn process_ready_sockets(
        &mut self,
        poll_state: &mut NetworkPollState,
        node_state: &mut StacksNodeState,
    ) -> (Vec<StacksMessageType>, Vec<usize>) {
        let mut to_remove = vec![];
        let mut msgs = vec![];
        for event_id in &poll_state.ready {
            let Some(client_sock) = self.sockets.get_mut(&event_id) else {
                debug!("Rogue socket event {}", event_id);
                to_remove.push(*event_id);
                continue;
            };

            match self.peers.get_mut(event_id) {
                Some(ref mut convo) => {
                    // activity on a http socket
                    debug!("Process HTTP data from {:?}", convo);
                    match HttpPeer::process_http_conversation(
                        node_state,
                        *event_id,
                        client_sock,
                        convo,
                    ) {
                        Ok((alive, mut new_msgs)) => {
                            if !alive {
                                debug!("HTTP convo {:?} is no longer alive", &convo);
                                to_remove.push(*event_id);
                            }
                            msgs.append(&mut new_msgs);
                        }
                        Err(e) => {
                            debug!("Failed to process HTTP convo {:?}: {:?}", &convo, &e);
                            to_remove.push(*event_id);
                            continue;
                        }
                    };
                }
                None => {
                    warn!("Rogue event {} for socket {:?}", event_id, &client_sock);
                    to_remove.push(*event_id);
                }
            }
        }

        (msgs, to_remove)
    }

    /// Flush outgoing replies, but don't block.
    /// Drop broken handles.
    /// Return the list of conversation event IDs to close (i.e. they're broken, or the request is done)
    #[cfg_attr(test, mutants::skip)]
    fn flush_conversations(&mut self) -> Vec<usize> {
        let mut close = vec![];

        // flush each outgoing conversation
        for (event_id, ref mut convo) in self.peers.iter_mut() {
            if let Err(e) = convo.try_flush() {
                info!("Broken HTTP connection {:?}: {:?}", convo, &e);
                close.push(*event_id);
            }
            if convo.is_drained() && !convo.is_keep_alive() {
                // did some work, but nothing more to do and we're not keep-alive
                debug!("Close drained HTTP connection {:?}", convo);
                close.push(*event_id);
            }
        }

        close
    }

    /// Update HTTP server state
    /// -- accept new connections
    /// -- send data on ready sockets
    /// -- receive data on ready sockets
    /// -- clear out timed-out requests
    /// Returns the list of messages to forward along to the peer network.
    #[cfg_attr(test, mutants::skip)]
    pub fn run(
        &mut self,
        network_state: &mut NetworkState,
        node_state: &mut StacksNodeState,
        mut poll_state: NetworkPollState,
    ) -> Vec<StacksMessageType> {
        // set up new inbound conversations
        self.process_new_sockets(network_state, node_state, &mut poll_state);

        // set up connected sockets
        self.process_connecting_sockets(network_state, node_state, &mut poll_state);

        // run existing conversations, clear out broken ones, and get back messages forwarded to us
        let (stacks_msgs, error_events) = self.process_ready_sockets(&mut poll_state, node_state);
        for error_event in error_events {
            debug!("Failed HTTP connection on event {}", error_event);
            self.deregister_http(network_state, error_event);
        }

        // move conversations along
        let close_events = self.flush_conversations();
        for close_event in close_events {
            debug!("Close HTTP connection on event {}", close_event);
            self.deregister_http(network_state, close_event);
        }

        // remove timed-out requests
        for (_, convo) in self.peers.iter_mut() {
            convo.clear_timeouts();
        }

        // clear out slow or non-responsive peers
        self.disconnect_unresponsive(network_state);

        stacks_msgs
    }
}

#[cfg(test)]
mod test {
    use std::cell::RefCell;
    use std::net::{SocketAddr, TcpStream};
    use std::sync::mpsc::{sync_channel, Receiver, RecvError, SendError, SyncSender, TryRecvError};
    use std::thread;

    use clarity::vm::contracts::Contract;
    use clarity::vm::representations::{ClarityName, ContractName};
    use clarity::vm::types::*;
    use stacks_common::codec::MAX_MESSAGE_LEN;
    use stacks_common::types::chainstate::{BlockHeaderHash, BurnchainHeaderHash};
    use stacks_common::util::pipe::*;
    use stacks_common::util::{get_epoch_time_secs, sleep_ms};

    use super::*;
    use crate::burnchains::{Burnchain, BurnchainView, *};
    use crate::chainstate::burn::ConsensusHash;
    use crate::chainstate::stacks::db::blocks::test::*;
    use crate::chainstate::stacks::db::StacksChainState;
    use crate::chainstate::stacks::test::*;
    use crate::chainstate::stacks::{Error as chain_error, StacksBlockHeader, *, *};
    use crate::net::codec::*;
    use crate::net::http::*;
    use crate::net::httpcore::*;
    use crate::net::rpc::*;
    use crate::net::test::*;
    use crate::net::*;

    fn test_http_server<F, C>(
        test_name: &str,
        peer_p2p: u16,
        peer_http: u16,
        conn_opts: ConnectionOptions,
        num_clients: usize,
        client_sleep: u64,
        mut make_request: F,
        check_result: C,
    ) -> usize
    where
        F: FnMut(usize, &mut StacksChainState) -> Vec<u8>,
        C: Fn(usize, Result<Vec<u8>, net_error>) -> bool,
    {
        let mut peer_config = TestPeerConfig::new(test_name, peer_p2p, peer_http);
        peer_config.connection_opts = conn_opts;

        let mut peer = TestPeer::new(peer_config);
        let view = peer.get_burnchain_view().unwrap();
        let (http_sx, http_rx) = sync_channel(1);

        let network_id = peer.config.network_id;
        let chainstate_path = peer.chainstate_path.clone();

        let (num_events_sx, num_events_rx) = sync_channel(1);
        let http_thread = thread::spawn(move || {
            let view = peer.get_burnchain_view().unwrap();
            loop {
                test_debug!("http wakeup");

                peer.step().unwrap();

                // asked to yield?
                match http_rx.try_recv() {
                    Ok(_) => {
                        break;
                    }
                    Err(_) => {}
                }
            }

            test_debug!("http server joined");
            let num_events = peer.network.network.as_ref().unwrap().num_events();
            let _ = num_events_sx.send(num_events);
        });

        let mut client_requests = vec![];
        let mut client_threads = vec![];
        let mut client_handles = vec![];
        let (mut chainstate, _) =
            StacksChainState::open(false, network_id, &chainstate_path, None).unwrap();
        for i in 0..num_clients {
            let request = make_request(i, &mut chainstate);
            client_requests.push(request);
        }

        for (i, request) in client_requests.into_iter().enumerate() {
            let (client_sx, client_rx) = sync_channel(1);
            let client = thread::spawn(move || {
                let mut sock = TcpStream::connect(
                    &format!("127.0.0.1:{}", peer_http)
                        .parse::<SocketAddr>()
                        .unwrap(),
                )
                .unwrap();

                if client_sleep > 0 {
                    sleep_ms(client_sleep * 1000);
                }

                match sock.write_all(&request) {
                    Ok(_) => {}
                    Err(e) => {
                        test_debug!("Client {} failed to write: {:?}", i, &e);
                        client_sx.send(Err(net_error::WriteError(e))).unwrap();
                        return;
                    }
                }

                let mut resp = vec![];
                match sock.read_to_end(&mut resp) {
                    Ok(_) => {
                        if resp.is_empty() {
                            test_debug!("Client {} did not receive any data", i);
                            client_sx.send(Err(net_error::PermanentlyDrained)).unwrap();
                            return;
                        }
                    }
                    Err(e) => {
                        test_debug!("Client {} failed to read: {:?}", i, &e);
                        client_sx.send(Err(net_error::ReadError(e))).unwrap();
                        return;
                    }
                }

                test_debug!("Client {} received {} bytes", i, resp.len());
                client_sx.send(Ok(resp)).unwrap();
            });
            client_threads.push(client);
            client_handles.push(client_rx);
        }

        for (i, client_thread) in client_threads.into_iter().enumerate() {
            test_debug!("Client join {}", i);
            client_thread.join().unwrap();
            let resp = client_handles[i].recv().unwrap();
            assert!(check_result(i, resp));
        }

        http_sx.send(true).unwrap();
        let num_events = num_events_rx.recv().unwrap();
        http_thread.join().unwrap();
        num_events
    }

    #[test]
    fn test_http_getinfo() {
        test_http_server(
            function_name!(),
            51000,
            51001,
            ConnectionOptions::default(),
            1,
            0,
            |client_id, _| {
                let mut request = StacksHttpRequest::new_for_peer(
                    PeerHost::from_host_port("127.0.0.1".to_string(), 51001),
                    "GET".to_string(),
                    "/v2/info".to_string(),
                    HttpRequestContents::new(),
                )
                .unwrap();
                request.preamble_mut().keep_alive = false;

                let request_bytes = request.try_serialize().unwrap();
                request_bytes
            },
            |client_id, http_response_bytes_res| {
                // should be a PeerInfo
                let http_response_bytes = http_response_bytes_res.unwrap();
                let response =
                    StacksHttp::parse_response("GET", "/v2/info", &http_response_bytes).unwrap();
                true
            },
        );
    }

    #[test]
    #[ignore]
    fn test_http_10_threads_getinfo() {
        test_http_server(
            function_name!(),
            51010,
            51011,
            ConnectionOptions::default(),
            10,
            0,
            |client_id, _| {
                let mut request = StacksHttpRequest::new_for_peer(
                    PeerHost::from_host_port("127.0.0.1".to_string(), 51011),
                    "GET".to_string(),
                    "/v2/info".to_string(),
                    HttpRequestContents::new(),
                )
                .unwrap();
                request.preamble_mut().keep_alive = false;

                let request_bytes = request.try_serialize().unwrap();
                request_bytes
            },
            |client_id, http_response_bytes_res| {
                // should be a PeerInfo
                let http_response_bytes = http_response_bytes_res.unwrap();
                let response =
                    StacksHttp::parse_response("GET", "/v2/info", &http_response_bytes).unwrap();
                true
            },
        );
    }

    #[test]
    fn test_http_getblock() {
        test_http_server(
            function_name!(),
            51020,
            51021,
            ConnectionOptions::default(),
            1,
            0,
            |client_id, ref mut chainstate| {
                let peer_server_block = make_codec_test_block(25, StacksEpochId::Epoch25);
                let peer_server_consensus_hash = ConsensusHash([(client_id + 1) as u8; 20]);
                let index_block_hash = StacksBlockHeader::make_index_block_hash(
                    &peer_server_consensus_hash,
                    &peer_server_block.block_hash(),
                );

                test_debug!("Store peer server index block {:?}", &index_block_hash);
                store_staging_block(
                    chainstate,
                    &peer_server_consensus_hash,
                    &peer_server_block,
                    &ConsensusHash([client_id as u8; 20]),
                    456,
                    123,
                );

                let mut request = StacksHttpRequest::new_for_peer(
                    PeerHost::from_host_port("127.0.0.1".to_string(), 51021),
                    "GET".to_string(),
                    format!("/v2/blocks/{}", &index_block_hash),
                    HttpRequestContents::new(),
                )
                .unwrap();
                request.preamble_mut().keep_alive = false;

                let request_bytes = request.try_serialize().unwrap();
                request_bytes
            },
            |client_id, http_response_bytes_res| {
                // should be a Block
                let http_response_bytes = http_response_bytes_res.unwrap();

                let peer_server_block = make_codec_test_block(25, StacksEpochId::Epoch25);
                let peer_server_consensus_hash = ConsensusHash([(client_id + 1) as u8; 20]);
                let index_block_hash = StacksBlockHeader::make_index_block_hash(
                    &peer_server_consensus_hash,
                    &peer_server_block.block_hash(),
                );

                let request_path = format!("/v2/blocks/{}", &index_block_hash);
                let response =
                    StacksHttp::parse_response("GET", &request_path, &http_response_bytes).unwrap();
                match response {
                    StacksHttpMessage::Response(stacks_http_response) => {
                        if let Ok(block) = StacksHttpResponse::decode_block(stacks_http_response) {
                            block == peer_server_block
                        } else {
                            false
                        }
                    }
                    _ => false,
                }
            },
        );
    }

    #[test]
    #[ignore]
    fn test_http_10_threads_getblock() {
        test_http_server(
            function_name!(),
            51030,
            51031,
            ConnectionOptions::default(),
            10,
            0,
            |client_id, ref mut chainstate| {
                let peer_server_block = make_codec_test_block(25, StacksEpochId::latest());
                let peer_server_consensus_hash = ConsensusHash([(client_id + 1) as u8; 20]);
                let index_block_hash = StacksBlockHeader::make_index_block_hash(
                    &peer_server_consensus_hash,
                    &peer_server_block.block_hash(),
                );

                test_debug!("Store peer server index block {:?}", &index_block_hash);
                store_staging_block(
                    chainstate,
                    &peer_server_consensus_hash,
                    &peer_server_block,
                    &ConsensusHash([client_id as u8; 20]),
                    456,
                    123,
                );

                let mut request = StacksHttpRequest::new_for_peer(
                    PeerHost::from_host_port("127.0.0.1".to_string(), 51031),
                    "GET".to_string(),
                    format!("/v2/blocks/{}", &index_block_hash),
                    HttpRequestContents::new(),
                )
                .unwrap();
                request.preamble_mut().keep_alive = false;

                let request_bytes = request.try_serialize().unwrap();
                request_bytes
            },
            |client_id, http_response_bytes_res| {
                // should be a Block
                let http_response_bytes = http_response_bytes_res.unwrap();

                let peer_server_block = make_codec_test_block(25, StacksEpochId::latest());
                let peer_server_consensus_hash = ConsensusHash([(client_id + 1) as u8; 20]);
                let index_block_hash = StacksBlockHeader::make_index_block_hash(
                    &peer_server_consensus_hash,
                    &peer_server_block.block_hash(),
                );

                let request_path = format!("/v2/blocks/{}", &index_block_hash);
                let response =
                    StacksHttp::parse_response("GET", &request_path, &http_response_bytes).unwrap();
                match response {
                    StacksHttpMessage::Response(stacks_http_response) => {
                        if let Ok(block) = StacksHttpResponse::decode_block(stacks_http_response) {
                            block == peer_server_block
                        } else {
                            false
                        }
                    }
                    _ => false,
                }
            },
        );
    }

    #[test]
    #[ignore]
    fn test_http_too_many_clients() {
        let mut conn_opts = ConnectionOptions::default();
        conn_opts.num_clients = 1;
        conn_opts.max_http_clients = 1;

        let have_success = RefCell::new(false);
        let have_error = RefCell::new(false);

        test_http_server(
            function_name!(),
            51040,
            51041,
            conn_opts,
            10,
            0,
            |client_id, _| {
                let mut request = StacksHttpRequest::new_for_peer(
                    PeerHost::from_host_port("127.0.0.1".to_string(), 51041),
                    "GET".to_string(),
                    "/v2/info".to_string(),
                    HttpRequestContents::new(),
                )
                .unwrap();
                request.preamble_mut().keep_alive = false;

                let request_bytes = request.try_serialize().unwrap();
                request_bytes
            },
            |client_id, http_response_bytes_res| {
                match http_response_bytes_res {
                    Ok(http_response_bytes) => {
                        // should be a PeerInfo
                        let response = match StacksHttp::parse_response(
                            "GET",
                            "/v2/info",
                            &http_response_bytes,
                        ) {
                            Ok(res) => res,
                            Err(e) => {
                                eprintln!(
                                    "Failed to parse /v2/info response from:\n{:?}\n{:?}",
                                    &http_response_bytes, &e
                                );
                                assert!(false);
                                unreachable!();
                            }
                        };
                        *have_success.borrow_mut() = true;
                        true
                    }
                    Err(err) => {
                        // should have failed
                        eprintln!("{:?}", &err);
                        *have_error.borrow_mut() = true;
                        true
                    }
                }
            },
        );

        assert!(*have_success.borrow());
        assert!(*have_error.borrow());
    }

    #[test]
    #[ignore]
    fn test_http_slow_client() {
        let mut conn_opts = ConnectionOptions::default();
        conn_opts.timeout = 3; // kill a connection after 3 seconds of idling

        test_http_server(
            function_name!(),
            51050,
            51051,
            conn_opts,
            1,
            30,
            |client_id, _| {
                let mut request = StacksHttpRequest::new_for_peer(
                    PeerHost::from_host_port("127.0.0.1".to_string(), 51051),
                    "GET".to_string(),
                    "/v2/info".to_string(),
                    HttpRequestContents::new(),
                )
                .unwrap();
                request.preamble_mut().keep_alive = false;

                let request_bytes = request.try_serialize().unwrap();
                request_bytes
            },
            |client_id, http_response_bytes_res| {
                match http_response_bytes_res {
                    Ok(bytes) => bytes.is_empty(), // should not have gotten any data
                    Err(net_error::PermanentlyDrained) => true,
                    Err(err) => {
                        // should have failed
                        eprintln!("{:?}", &err);
                        false
                    }
                }
            },
        );
    }

    #[test]
    fn test_http_endless_data_client() {
        let conn_opts = ConnectionOptions::default();
        test_http_server(
            function_name!(),
            51060,
            51061,
            conn_opts,
            1,
            0,
            |client_id, ref mut chainstate| {
                // make a gigantic transaction
                let mut big_contract_parts = vec![];
                let mut total_len = 0;
                while total_len < MAX_MESSAGE_LEN {
                    let next_line = format!(
                        "(define-constant meaningless-data-{} {})\n",
                        total_len, total_len
                    );
                    total_len += next_line.len() as u32;
                    big_contract_parts.push(next_line);
                }

                let big_contract = big_contract_parts.join("");

                let privk_origin = StacksPrivateKey::from_hex(
                    "027682d2f7b05c3801fe4467883ab4cff0568b5e36412b5289e83ea5b519de8a01",
                )
                .unwrap();
                let auth_origin = TransactionAuth::from_p2pkh(&privk_origin).unwrap();
                let mut tx_contract = StacksTransaction::new(
                    TransactionVersion::Testnet,
                    auth_origin.clone(),
                    TransactionPayload::new_smart_contract(
                        &"hello-world".to_string(),
                        &big_contract.to_string(),
                        None,
                    )
                    .unwrap(),
                );

                tx_contract.chain_id = chainstate.config().chain_id;
                tx_contract.set_tx_fee(0);

                let mut signer = StacksTransactionSigner::new(&tx_contract);
                signer.sign_origin(&privk_origin).unwrap();

                let signed_contract_tx = signer.get_tx().unwrap();

                let mut request = StacksHttpRequest::new_for_peer(
                    PeerHost::from_host_port("127.0.0.1".to_string(), 51061),
                    "POST".to_string(),
                    "/v2/transactions".to_string(),
                    HttpRequestContents::new().payload_stacks(&signed_contract_tx),
                )
                .unwrap();
                request.preamble_mut().keep_alive = false;

                let request_bytes = request.try_serialize().unwrap();
                request_bytes
            },
            |client_id, http_response_bytes_res| {
                match http_response_bytes_res {
                    Ok(bytes) => false,
                    Err(err) => {
                        // should have failed
                        eprintln!("{:?}", &err);
                        true
                    }
                }
            },
        );
    }

    #[test]
    fn test_http_400() {
        test_http_server(
            function_name!(),
            51070,
            51071,
            ConnectionOptions::default(),
            1,
            0,
            |client_id, _| {
                // live example -- should fail because we don't support `Connection:
                // upgrade`
                let request_txt = "GET /favicon.ico HTTP/1.1\r\nConnection: upgrade\r\nHost: crashy-stacky.zone117x.com\r\nX-Real-IP: 213.127.17.55\r\nX-Forwarded-For: 213.127.17.55\r\nX-Forwarded-Proto: http\r\nX-Forwarded-Host: crashy-stacky.zone117x.com\r\nX-Forwarded-Port: 9001\r\nUser-Agent: Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_3) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/81.0.4044.113 Safari/537.36\r\nAccept: image/webp,image/apng,image/*,*/*;q=0.8\r\nReferer: http://crashy-stacky.zone117x.com:9001/v2/info\r\nAccept-Encoding: gzip, deflate\r\nAccept-Language: en-US,en;q=0.9\r\n\r\n";
                request_txt.as_bytes().to_vec()
            },
            |client_id, http_response_bytes_res| {
                // should be a HTTP 400 error
                eprintln!("{:?}", &http_response_bytes_res);
                let http_response_bytes = http_response_bytes_res.unwrap();
                let http_response_str = String::from_utf8(http_response_bytes).unwrap();
                eprintln!("HTTP response\n{}", http_response_str);
                assert!(http_response_str.find("400 Bad Request").is_some());
                true
            },
        );
    }

    #[test]
    fn test_http_404() {
        test_http_server(
            function_name!(),
            51072,
            51073,
            ConnectionOptions::default(),
            1,
            0,
            |client_id, _| {
                // live example -- should fail because /favicon.ico doesn't exist.
                let request_txt = "GET /favicon.ico HTTP/1.1\r\nConnection: close\r\nHost: 127.0.0.1:20443\r\nuser-agent: Mozilla/5.0 (Macintosh; Intel Mac OS X 10_14_6) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/81.0.4044.138 Safari/537.36\r\nreferer: http://127.0.0.1:20443/v2/info\r\naccept: image/webp,image/apng,image/*,*/*;q=0.8\r\nsec-fetch-dest: empty\r\naccept-encoding: gzip, deflate, br\r\nsec-fetch-site: same-origin\r\naccept-language: en-US,en;q=0.9\r\ndnt: 1\r\nsec-fetch-mode: no-cors\r\n\r\n";
                request_txt.as_bytes().to_vec()
            },
            |client_id, http_response_bytes_res| {
                // should be a HTTP 404 error
                eprintln!("{:?}", &http_response_bytes_res);
                let http_response_bytes = http_response_bytes_res.unwrap();
                let http_response_str = String::from_utf8(http_response_bytes).unwrap();
                eprintln!("HTTP response\n{}", http_response_str);
                assert!(http_response_str.find("404 Not Found").is_some());
                true
            },
        );
    }

    #[test]
    fn test_http_no_connecting_event_id_leak() {
        use std::net::TcpListener;

        let mut conn_opts = ConnectionOptions::default();
        conn_opts.timeout = 10;
        conn_opts.connect_timeout = 10;

        let num_events = test_http_server(
            function_name!(),
            51082,
            51083,
            conn_opts,
            1,
            0,
            |client_id, _| {
                // open a socket and just sit there
                use std::net::TcpStream;
                let sock = TcpStream::connect("127.0.0.1:51083");

                sleep_ms(15_000);

                // send a different request
                let mut request = StacksHttpRequest::new_for_peer(
                    PeerHost::from_host_port("127.0.0.1".to_string(), 51083),
                    "GET".to_string(),
                    "/v2/info".to_string(),
                    HttpRequestContents::new(),
                )
                .unwrap();
                request.preamble_mut().keep_alive = false;

                let request_bytes = request.try_serialize().unwrap();
                request_bytes
            },
            |client_id, res| true,
        );

        assert_eq!(num_events, 2);
    }

    #[test]
    fn test_http_noop() {
        if std::env::var("BLOCKSTACK_HTTP_TEST") != Ok("1".to_string()) {
            eprintln!("Set BLOCKSTACK_HTTP_TEST=1 to use this test.");
            eprintln!("To test, run `curl http://localhost:51081/v2/blocks/a3b82874a8bf02b91613f61bff41580dab439ecc14f5e71c7288d89623499dfa` to download a block");
            return;
        }

        // doesn't do anything; just runs a server for 10 minutes
        let conn_opts = ConnectionOptions::default();
        test_http_server(
            function_name!(),
            51080,
            51081,
            conn_opts,
            1,
            600,
            |client_id, ref mut chainstate| {
                let peer_server_block = make_codec_test_block(25, StacksEpochId::latest());
                let peer_server_consensus_hash = ConsensusHash([(client_id + 1) as u8; 20]);
                let index_block_hash = StacksBlockHeader::make_index_block_hash(
                    &peer_server_consensus_hash,
                    &peer_server_block.block_hash(),
                );

                test_debug!("Store peer server index block {:?}", &index_block_hash);
                store_staging_block(
                    chainstate,
                    &peer_server_consensus_hash,
                    &peer_server_block,
                    &ConsensusHash([client_id as u8; 20]),
                    456,
                    123,
                );

                let mut request = StacksHttpRequest::new_for_peer(
                    PeerHost::from_host_port("127.0.0.1".to_string(), 51071),
                    "GET".to_string(),
                    format!("/v2/blocks/{}", index_block_hash),
                    HttpRequestContents::new(),
                )
                .unwrap();
                request.preamble_mut().keep_alive = false;

                let request_bytes = request.try_serialize().unwrap();
                request_bytes
            },
            |client_id, http_response_bytes_res| true,
        );
    }
}
