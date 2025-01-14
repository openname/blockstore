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

use std::collections::{HashMap, HashSet};
use std::io::{Error as io_error, ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr};
use std::time::Duration;
use std::{io, net, time};

use mio::{net as mio_net, PollOpt, Ready, Token};
use rand::RngCore;
use stacks_common::types::net::PeerAddress;
use stacks_common::util::{log, sleep_ms};
use {mio, rand};

use crate::net::{Error as net_error, Neighbor, NeighborKey};
use crate::util_lib::db::{DBConn, Error as db_error};

const SERVER: Token = mio::Token(0);

pub struct NetworkPollState {
    pub new: HashMap<usize, mio_net::TcpStream>,
    pub ready: Vec<usize>,
}

impl NetworkPollState {
    pub fn new() -> NetworkPollState {
        NetworkPollState {
            new: HashMap::new(),
            ready: vec![],
        }
    }
}

// state for a single network server
#[derive(Debug)]
pub struct NetworkServerState {
    addr: SocketAddr,
    server_socket: mio_net::TcpListener,
    server_event: mio::Token,
}

// state for the entire network
#[derive(Debug)]
pub struct NetworkState {
    poll: mio::Poll,
    events: mio::Events,
    event_capacity: usize,
    servers: Vec<NetworkServerState>,
    count: usize,
    event_map: HashMap<usize, usize>, // map socket events to their registered server socket (including server sockets)
}

impl NetworkState {
    pub fn new(event_capacity: usize) -> Result<NetworkState, net_error> {
        let poll = mio::Poll::new().map_err(|e| {
            error!("Failed to initialize poller: {:?}", e);
            net_error::BindError
        })?;

        let events = mio::Events::with_capacity(event_capacity);

        Ok(NetworkState {
            poll,
            events,
            event_capacity,
            servers: vec![],
            count: 1,
            event_map: HashMap::new(),
        })
    }

    #[cfg_attr(test, mutants::skip)]
    pub fn num_events(&self) -> usize {
        self.event_map.len()
    }

    fn bind_address(addr: &SocketAddr) -> Result<mio_net::TcpListener, net_error> {
        if !cfg!(test) && !cfg!(feature = "testing") {
            mio_net::TcpListener::bind(addr).map_err(|e| {
                error!("Failed to bind to {:?}: {:?}", addr, e);
                net_error::BindError
            })
        } else {
            let mut backoff = 1000;
            let mut rng = rand::thread_rng();
            let mut count = 1000;
            loop {
                match mio_net::TcpListener::bind(addr) {
                    Ok(server) => {
                        return Ok(server);
                    }
                    Err(e) => match e.kind() {
                        io::ErrorKind::AddrInUse => {
                            debug!(
                                "Waiting {} millis and trying to bind {:?} again",
                                backoff, addr
                            );
                            sleep_ms(backoff);
                            backoff = count + (rng.next_u64() % count);
                            count += count;
                            continue;
                        }
                        _ => {
                            debug!("Failed to bind {:?}: {:?}", addr, &e);
                            return Err(net_error::BindError);
                        }
                    },
                }
            }
        }
    }

    /// Bind to the given socket address.
    /// Returns the handle to the poll state and the bound address, used to key network poll events.
    pub fn bind(&mut self, addr: &SocketAddr) -> Result<(usize, SocketAddr), net_error> {
        let server = NetworkState::bind_address(addr)?;
        let next_server_event = self.next_event_id()?;

        self.poll
            .register(
                &server,
                mio::Token(next_server_event),
                Ready::all(),
                PollOpt::edge(),
            )
            .map_err(|e| {
                error!("Failed to register server socket: {:?}", &e);
                net_error::BindError
            })?;

        // N.B. the port for `addr` might be 0, in which case, `local_addr` may not be equal to
        // `addr` since the port will be system-assigned.  Use `local_adddr`.
        let local_addr = server.local_addr().map_err(|e| {
            error!("Failed to get local address for server: {:?}", &e);
            net_error::BindError
        })?;

        let network_server = NetworkServerState {
            addr: local_addr.clone(),
            server_socket: server,
            server_event: mio::Token(next_server_event),
        };

        assert!(
            !self.event_map.contains_key(&next_server_event),
            "BUG: failed to generate an unused server event ID"
        );

        self.servers.push(network_server);
        self.event_map.insert(next_server_event, 0); // server events always mapped to 0

        Ok((next_server_event, local_addr))
    }

    /// Register a socket for read/write notifications with this poller.
    /// Try to use the given hint_event_id value, but generate a different event ID if it's been
    /// taken.
    /// Return the actual event ID used (it may be different than hint_event_id)
    pub fn register(
        &mut self,
        server_event_id: usize,
        hint_event_id: usize,
        sock: &mio_net::TcpStream,
    ) -> Result<usize, net_error> {
        let hint_event_id = hint_event_id % (self.event_capacity + self.servers.len());
        if let Some(x) = self.event_map.get(&server_event_id) {
            if x != &0 {
                // not a server event
                error!(
                    "Server event ID {} not mapped to a server token, but to {}",
                    &server_event_id, x
                );
                return Err(net_error::RegisterError);
            }
        } else {
            // not a server event
            panic!("Not a server event ID: {}", &server_event_id);
        }

        // if the event ID is in use, then find another one
        let event_id = if self.event_map.contains_key(&hint_event_id) {
            self.next_event_id()?
        } else {
            hint_event_id
        };

        assert!(
            self.event_map.len() <= self.event_capacity + self.servers.len(),
            "BUG: event map exceeded event capacity ({} > {} + {})",
            self.event_map.len(),
            self.event_capacity,
            self.servers.len()
        );

        self.poll
            .register(sock, mio::Token(event_id), Ready::all(), PollOpt::edge())
            .map_err(|e| {
                error!(
                    "Failed to register socket on server {} event ID {} ({}): {:?}",
                    server_event_id, event_id, hint_event_id, &e
                );
                net_error::RegisterError
            })?;

        self.event_map.insert(event_id, server_event_id);

        debug!(
            "Socket registered: {}, hint {}, {:?} on server {} (Events total: {}, max: {})",
            event_id,
            hint_event_id,
            sock,
            server_event_id,
            self.event_map.len(),
            self.event_capacity
        );
        Ok(event_id)
    }

    /// Deregister a socket event
    pub fn deregister(
        &mut self,
        event_id: usize,
        sock: &mio_net::TcpStream,
    ) -> Result<(), net_error> {
        assert!(
            self.event_map.contains_key(&event_id),
            "BUG: no such socket {}",
            event_id
        );
        self.event_map.remove(&event_id);

        if let Err(e) = self.poll.deregister(sock) {
            warn!("Failed to deregister socket {}: {:?}", event_id, &e);
        };

        debug!(
            "Socket deregistered: {}, {:?} (Events total: {}, max: {})",
            event_id,
            sock,
            self.event_map.len(),
            self.event_capacity
        );

        if let Err(e) = sock.shutdown(Shutdown::Both) {
            debug!("Failed to shut down socket {}: {:?}", event_id, &e);
        }

        Ok(())
    }

    fn make_next_event_id(&self, cur_count: usize, in_use: &HashSet<usize>) -> Option<usize> {
        let mut ret = cur_count;

        let mut in_use_count = 0;
        let mut event_map_count = 0;

        for _ in 0..(self.event_capacity + self.servers.len()) {
            if self.event_map.contains_key(&ret) || in_use.contains(&ret) {
                ret = (ret + 1) % (self.event_capacity + self.servers.len());

                if in_use.contains(&ret) {
                    in_use_count += 1;
                } else {
                    event_map_count += 1;
                }
            } else {
                return Some(ret);
            }
        }

        debug!(
            "Too many peers (events: {}, in_use: {}, max: {})",
            event_map_count, in_use_count, self.event_capacity
        );
        None
    }

    /// next event ID
    pub fn next_event_id(&mut self) -> Result<usize, net_error> {
        let ret = self
            .make_next_event_id(self.count, &HashSet::new())
            .ok_or(net_error::TooManyPeers)?;
        self.count = (ret + 1) % (self.event_capacity + self.servers.len());
        Ok(ret)
    }

    /// Connect to a remote peer, but don't register it with the poll handle.
    /// The underlying connect(2) is _asynchronous_, so the caller will need to register it with a
    /// poll handle and wait for it to be connected.
    pub fn connect(
        addr: &SocketAddr,
        socket_send_buffer: u32,
        socket_recv_buffer: u32,
    ) -> Result<mio_net::TcpStream, net_error> {
        let stream = mio_net::TcpStream::connect(addr).map_err(|_e| {
            test_debug!("Failed to convert to mio stream: {:?}", &_e);
            net_error::ConnectionError
        })?;

        // set some helpful defaults
        // Don't go crazy on TIME_WAIT states; have them all die after 5 seconds
        stream
            .set_linger(Some(time::Duration::from_millis(5000)))
            .map_err(|e| {
                warn!("Failed to set SO_LINGER: {:?}", &e);
                net_error::ConnectionError
            })?;

        // Disable Nagle algorithm
        stream.set_nodelay(true).map_err(|_e| {
            warn!("Failed to set TCP_NODELAY: {:?}", &_e);
            net_error::ConnectionError
        })?;

        // Make sure keep-alive is on, since at least in p2p messages, we keep sockets around
        // for a while.  Linux default is 7200 seconds, so make sure we keep it here.
        stream
            .set_keepalive(Some(time::Duration::from_millis(7200 * 1000)))
            .map_err(|e| {
                warn!("Failed to set TCP_KEEPALIVE and/or SO_KEEPALIVE: {:?}", &e);
                net_error::ConnectionError
            })?;

        if cfg!(test) {
            if std::env::var("STACKS_TEST_DISABLE_EDGE_TRIGGER_TEST") != Ok("1".to_string()) {
                // edge-trigger torture test
                stream.set_send_buffer_size(32).unwrap();
                stream.set_recv_buffer_size(32).unwrap();
            }
        } else {
            stream
                .set_send_buffer_size(socket_send_buffer as usize)
                .map_err(|e| {
                    warn!(
                        "Failed to set socket write buffer size to {}: {:?}",
                        socket_send_buffer, &e
                    );
                    net_error::ConnectionError
                })?;

            stream
                .set_recv_buffer_size(socket_recv_buffer as usize)
                .map_err(|e| {
                    warn!(
                        "Failed to set socket read buffer size to {}: {:?}",
                        socket_send_buffer, &e
                    );
                    net_error::ConnectionError
                })?;
        }

        test_debug!("New socket connected to {:?}: {:?}", addr, &stream);
        Ok(stream)
    }

    /// Poll all server sockets.
    /// Returns a map between network server handles (returned by bind()) and their new polling state
    pub fn poll(&mut self, timeout: u64) -> Result<HashMap<usize, NetworkPollState>, net_error> {
        self.events.clear();
        self.poll
            .poll(&mut self.events, Some(Duration::from_millis(timeout)))
            .map_err(|e| {
                error!("Failed to poll: {:?}", &e);
                net_error::PollError
            })?;

        let mut poll_states = HashMap::new();
        for server in self.servers.iter() {
            // pre-populate with server tokens
            let server_event_id = usize::from(server.server_event);
            poll_states.insert(server_event_id, NetworkPollState::new());
        }

        let mut new_events = HashSet::new();

        for event in &self.events {
            let token = event.token();
            let mut is_server_event = false;

            for server in self.servers.iter() {
                // server token?
                if token == server.server_event {
                    // new inbound connection(s)
                    let poll_state =
                        poll_states.get_mut(&usize::from(token)).unwrap_or_else(|| {
                            panic!(
                                "BUG: FATAL: no poll state registered for server {}",
                                usize::from(token)
                            )
                        });

                    loop {
                        let (client_sock, client_addr) = match server.server_socket.accept() {
                            Ok((client_sock, client_addr)) => (client_sock, client_addr),
                            Err(e) => match e.kind() {
                                ErrorKind::WouldBlock => {
                                    break;
                                }
                                _ => {
                                    error!("Network error: {}", e);
                                    return Err(net_error::AcceptError);
                                }
                            },
                        };

                        // this does the same thing as next_event_id(), but we can't borrow self
                        // mutably here (so we'll just do the increment-mod directly).
                        let next_event_id = match self.make_next_event_id(self.count, &new_events) {
                            Some(eid) => eid,
                            None => {
                                // no poll slots available. Close the socket and carry on.
                                info!("Too many peers on {:?}, closing {:?} (events: {}, in-flight: {}, capacity: {})", &server.server_socket, &client_sock, self.event_map.len(), new_events.len(), self.event_capacity);
                                let _ = client_sock.shutdown(Shutdown::Both);
                                continue;
                            }
                        };

                        self.count =
                            (next_event_id + 1) % (self.event_capacity + self.servers.len());

                        new_events.insert(next_event_id);

                        debug!(
                            "New socket event: {}, {:?} addr={:?} (Events total: {}, max: {}) on server {:?}",
                            next_event_id,
                            &client_sock,
                            &client_addr,
                            self.event_map.len(),
                            self.event_capacity,
                            &server.server_socket
                        );

                        poll_state.new.insert(next_event_id, client_sock);
                    }

                    is_server_event = true;
                    break;
                }
            }

            if is_server_event {
                continue;
            }

            // event for a client of one of our servers.  which one?
            let event_id = usize::from(token);
            match self.event_map.get(&event_id) {
                Some(server_event_id) => {
                    if let Some(poll_state) = poll_states.get_mut(server_event_id) {
                        test_debug!(
                            "Wakeup socket event {} on server {}",
                            event_id,
                            server_event_id
                        );
                        poll_state.ready.push(event_id);
                    } else {
                        warn!("Unknown server event ID {}", server_event_id);
                    }
                }
                None => {
                    warn!("Surreptitious readiness event {}", event_id);
                }
            }
        }

        Ok(poll_states)
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;

    use mio;
    use mio::{net as mio_net, PollOpt, Ready, Token};

    use super::*;

    #[test]
    fn test_bind() {
        let mut ns = NetworkState::new(100).unwrap();
        let mut server_events = HashSet::new();
        for _ in 0..10 {
            let addr = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
            let (event_id, _local_addr) = ns.bind(&addr).unwrap();
            assert!(!server_events.contains(&event_id));
            server_events.insert(event_id);
        }
    }

    #[test]
    fn test_register_deregister() {
        let mut ns = NetworkState::new(100).unwrap();
        let mut server_events = vec![];
        let mut event_ids = HashSet::new();
        let mut ports = vec![];
        for _ in 0..10 {
            let addr = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
            let (event_id, local_addr) = ns.bind(&addr).unwrap();
            server_events.push(event_id);
            event_ids.insert(event_id);

            ports.push(local_addr.port());
        }

        let mut client_events = vec![];
        for (i, port) in ports.iter().enumerate() {
            let addr = format!("127.0.0.1:{}", &port)
                .parse::<SocketAddr>()
                .unwrap();
            let sock = NetworkState::connect(&addr, 4096, 4096).unwrap();

            let event_id = ns.register(server_events[i], 1, &sock).unwrap();
            assert!(event_id != 0);
            assert!(!event_ids.contains(&event_id));
            ns.deregister(event_id, &sock).unwrap();

            let event_id = ns.register(server_events[i], 101, &sock).unwrap();
            assert!(event_id != 0);
            assert!(!event_ids.contains(&event_id));
            ns.deregister(event_id, &sock).unwrap();

            let event_id = ns
                .register(server_events[i], server_events[i], &sock)
                .unwrap();
            assert!(event_id != 0);
            assert!(!event_ids.contains(&event_id));
            ns.deregister(event_id, &sock).unwrap();

            let event_id = ns.register(server_events[i], 11, &sock).unwrap();
            assert!(!event_ids.contains(&event_id));

            event_ids.insert(event_id);
            client_events.push(event_id);
        }

        test_debug!("=====");
        for (i, port) in ports.iter().enumerate() {
            let addr = format!("127.0.0.1:{}", &port)
                .parse::<SocketAddr>()
                .unwrap();
            let sock = NetworkState::connect(&addr, 4096, 4096).unwrap();

            // can't use non-server events
            assert_eq!(
                Err(net_error::RegisterError),
                ns.register(client_events[i], i + 1, &sock)
            );
        }
    }

    #[test]
    fn test_register_too_many_peers() {
        let mut ns = NetworkState::new(10).unwrap();
        let mut event_ids = HashSet::new();
        let addr = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
        let (server_event_id, local_addr) = ns.bind(&addr).unwrap();
        let port = local_addr.port();
        for _ in 0..10 {
            let addr = format!("127.0.0.1:{}", &port)
                .parse::<SocketAddr>()
                .unwrap();
            event_ids.insert(server_event_id);

            let sock = NetworkState::connect(&addr, 4096, 4096).unwrap();

            // register 10 client events
            let event_id = ns.register(server_event_id, 11, &sock).unwrap();
            assert!(!event_ids.contains(&event_id));
        }

        // the 11th socket should fail
        let addr = format!("127.0.0.1:{}", port).parse::<SocketAddr>().unwrap();
        let sock = NetworkState::connect(&addr, 4096, 4096).unwrap();
        let res = ns.register(server_event_id, 11, &sock);
        assert_eq!(Err(net_error::TooManyPeers), res);
    }

    #[test]
    fn test_register_deregister_stress() {
        let mut ns = NetworkState::new(20).unwrap();
        let count = 0;
        let mut in_use = HashSet::new();
        let mut events_in = vec![];

        for _ in 0..20 {
            let next_eid = ns.make_next_event_id(count, &in_use).unwrap();
            ns.event_map.insert(next_eid, 0);
            events_in.push(next_eid);
        }

        assert_eq!(ns.event_map.len(), 20);

        for _ in 0..20 {
            assert!(ns.make_next_event_id(count, &in_use).is_none());
        }

        for eid in events_in.iter() {
            ns.event_map.remove(eid);
        }

        events_in.clear();

        for _ in 0..20 {
            let next_eid = ns.make_next_event_id(count, &in_use).unwrap();
            events_in.push(next_eid);
            in_use.insert(next_eid);
        }

        assert_eq!(ns.event_map.len(), 0);

        for _ in 0..20 {
            assert!(ns.make_next_event_id(count, &in_use).is_none());
        }
    }
}
