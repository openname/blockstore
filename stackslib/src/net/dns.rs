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
use std::hash::{Hash, Hasher};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{
    sync_channel, Receiver, RecvError, RecvTimeoutError, SyncSender, TryRecvError, TrySendError,
};

use stacks_common::types::net::PeerAddress;
use stacks_common::util::hash::to_hex;
use stacks_common::util::{get_epoch_time_ms, get_epoch_time_secs, log, sleep_ms};

use crate::net::asn::ASEntry4;
use crate::net::codec::*;
use crate::net::{Error as net_error, Neighbor, NeighborKey, *};
use crate::util_lib::db::Error as db_error;

/// In Rust, there's no easy way to do non-blocking DNS lookups (I blame getaddrinfo), so do it in
/// a separate thread, and implement a way for the block downloader to periodically poll for
/// resolved names.
#[derive(Debug, Clone, Eq)]
pub struct DNSRequest {
    pub host: String,
    pub port: u16,
    pub timeout: u128, // in millis
}

impl DNSRequest {
    pub fn new(host: String, port: u16, timeout: u128) -> DNSRequest {
        DNSRequest {
            host,
            port,
            timeout,
        }
    }

    pub fn is_timed_out(&self) -> bool {
        let now = get_epoch_time_ms();
        now > self.timeout
    }
}

impl Hash for DNSRequest {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.host.hash(state);
        self.port.hash(state);
    }
}

impl PartialEq for DNSRequest {
    fn eq(&self, other: &DNSRequest) -> bool {
        self.host == other.host && self.port == other.port
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct DNSResponse {
    pub request: DNSRequest,
    pub result: Result<Vec<SocketAddr>, String>,
}

impl DNSResponse {
    pub fn new(request: DNSRequest, result: Result<Vec<SocketAddr>, String>) -> DNSResponse {
        DNSResponse { request, result }
    }

    pub fn error(request: DNSRequest, errstr: String) -> DNSResponse {
        DNSResponse {
            request,
            result: Err(errstr),
        }
    }
}

/// The DNSResolver runs as a background thread in the node. In a loop, it collects inbound requests,
/// then tries to resolve the valid requests.
#[derive(Debug)]
pub struct DNSResolver {
    queries: VecDeque<DNSRequest>,
    inbound: Receiver<DNSRequest>,
    outbound: SyncSender<DNSResponse>,
    max_inflight: u64,

    // used mainly for testing
    hardcoded: HashMap<(String, u16), Vec<SocketAddr>>,
}

/// The DNSClient provides an API to send DNS requests and poll DNS results. The client forwards
/// requests and receives results through "channels" to the DNSResolver.  
#[derive(Debug)]
pub struct DNSClient {
    requests: HashMap<DNSRequest, Option<DNSResponse>>,
    requests_tx: SyncSender<DNSRequest>,
    requests_rx: Receiver<DNSResponse>,
}

impl DNSResolver {
    pub fn new(max_inflight: u64) -> (DNSResolver, DNSClient) {
        let (dns_chan_tx, dns_chan_rx) = sync_channel(1024);
        let (socket_chan_tx, socket_chan_rx) = sync_channel(1024);

        let client = DNSClient::new(socket_chan_tx, dns_chan_rx);
        let resolver = DNSResolver {
            queries: VecDeque::new(),
            inbound: socket_chan_rx,
            outbound: dns_chan_tx,
            max_inflight,
            hardcoded: HashMap::new(),
        };
        (resolver, client)
    }

    pub fn add_hardcoded(&mut self, host: &str, port: u16, addrs: Vec<SocketAddr>) {
        self.hardcoded.insert((host.to_string(), port), addrs);
    }

    pub fn resolve(&self, req: DNSRequest) -> DNSResponse {
        if let Some(ref addrs) = self.hardcoded.get(&(req.host.clone(), req.port)) {
            return DNSResponse::new(req, Ok(addrs.to_vec()));
        }

        // TODO: this is a blocking operation, but there's not really a good solution here other
        // than to just do this in a separate thread :shrug:
        test_debug!("Resolve {}:{}", &req.host, req.port);
        let addrs: Vec<SocketAddr> = match (req.host.as_str(), req.port).to_socket_addrs() {
            Ok(iter) => {
                let mut list = vec![];
                for addr in iter {
                    list.push(addr);
                }
                list
            }
            Err(ioe) => {
                return DNSResponse::error(req, format!("DNS resolve error: {:?}", &ioe));
            }
        };

        if addrs.is_empty() {
            return DNSResponse::error(req, "DNS resolve error: got zero addresses".to_string());
        }
        test_debug!("{}:{} resolved to {:?}", &req.host, req.port, &addrs);
        DNSResponse::new(req, Ok(addrs))
    }

    /// Drain inbound DNS requests.
    /// Handles overflows (too many requests) and timeouts.
    fn drain_inbox(&mut self) -> Result<usize, net_error> {
        let mut received = 0;
        for _ in 0..self.max_inflight {
            match self.inbound.try_recv() {
                Ok(req) => {
                    if req.is_timed_out() {
                        let resp = DNSResponse::error(req, "DNS request timed out".to_string());
                        if let Err(TrySendError::Disconnected(_)) = self.outbound.try_send(resp) {
                            test_debug!(
                                "DNS client inbox disconnected -- could not issue timeout error"
                            );
                            return Err(net_error::ConnectionBroken);
                        }
                    } else if (self.queries.len() as u64) < self.max_inflight {
                        test_debug!("Queued {:?}", &req);
                        self.queries.push_back(req);
                        received += 1;
                    } else {
                        let resp =
                            DNSResponse::error(req, "Too many DNS requests in-flight".to_string());
                        if let Err(TrySendError::Disconnected(_)) = self.outbound.try_send(resp) {
                            test_debug!("DNS client inbox disconnected -- could not issue too-many-requests error");
                            return Err(net_error::ConnectionBroken);
                        }
                    }
                }
                Err(TryRecvError::Empty) => {
                    break;
                }
                Err(_e) => {
                    test_debug!("Failed to receive DNS inbox: {:?}", &_e);
                    return Err(net_error::ConnectionBroken);
                }
            }
        }
        return Ok(received);
    }

    pub fn handle_query(&mut self) -> Option<DNSResponse> {
        let req = match self.queries.pop_front() {
            Some(r) => r,
            None => {
                return None;
            }
        };

        if req.is_timed_out() {
            return Some(DNSResponse::error(req, "DNS request timed out".to_string()));
        }

        let resp = self.resolve(req);
        Some(resp)
    }

    pub fn thread_main(&mut self) {
        test_debug!("DNS start");
        loop {
            // prime the pump, or die if the inbound channel is broken
            match self.drain_inbox() {
                Ok(count) => {
                    if count == 0 {
                        sleep_ms(100);
                    }
                }
                Err(_e) => {
                    test_debug!("Failed to drain DNS inbox; exiting");
                    break;
                }
            }

            for _ in 0..self.max_inflight {
                let resp = match self.handle_query() {
                    Some(r) => r,
                    None => {
                        // out of requests
                        break;
                    }
                };

                if let Err(TrySendError::Disconnected(_)) = self.outbound.try_send(resp) {
                    test_debug!("DNS client disconnected; exiting");
                    break;
                }
            }
        }
        test_debug!("DNS join");
    }
}

impl DNSClient {
    pub fn new(inbound: SyncSender<DNSRequest>, outbound: Receiver<DNSResponse>) -> DNSClient {
        DNSClient {
            requests_tx: inbound,
            requests_rx: outbound,
            requests: HashMap::new(),
        }
    }

    pub fn queue_lookup(&mut self, host: &str, port: u16, timeout: u128) -> Result<(), net_error> {
        let req = DNSRequest::new(host.to_string(), port, timeout);
        self.requests_tx
            .send(req.clone())
            .map_err(|_se| net_error::LookupError("Failed to queue DNS query".to_string()))?;
        self.requests.insert(req, None);
        Ok(())
    }

    fn clear_timeouts(&mut self) {
        let mut to_remove = vec![];
        for req in self.requests.keys() {
            if req.is_timed_out() {
                debug!("Lookup {}:{} timed out", &req.host, req.port);
                to_remove.push(req.clone());
            }
        }
        for req in to_remove.into_iter() {
            self.requests.insert(
                req.clone(),
                Some(DNSResponse::error(req, "DNS lookup timed out".to_string())),
            );
        }
    }

    pub fn try_recv(&mut self) -> Result<usize, net_error> {
        self.clear_timeouts();

        let mut num_recved = 0;
        loop {
            match self.requests_rx.try_recv() {
                Ok(resp) => {
                    if self.requests.contains_key(&resp.request) {
                        if !resp.request.is_timed_out() {
                            self.requests.insert(resp.request.clone(), Some(resp));
                            num_recved += 1;
                        } else {
                            self.requests.insert(
                                resp.request.clone(),
                                Some(DNSResponse::error(
                                    resp.request,
                                    "DNS lookup timed out".to_string(),
                                )),
                            );
                        }
                    }
                }
                Err(TryRecvError::Empty) => {
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    if num_recved == 0 {
                        return Err(net_error::RecvError("Disconnected".to_string()));
                    } else {
                        break;
                    }
                }
            }
        }
        Ok(num_recved)
    }

    pub fn poll_lookup(&mut self, host: &str, port: u16) -> Result<Option<DNSResponse>, net_error> {
        let req = DNSRequest::new(host.to_string(), port, 0);
        if !self.requests.contains_key(&req) {
            return Err(net_error::LookupError(format!(
                "No such pending lookup: {}:{}",
                host, port
            )));
        }

        let _ = match self.requests.get(&req) {
            Some(None) => {
                return Ok(None);
            }
            Some(Some(resp)) => resp,
            None => {
                unreachable!();
            }
        };

        let resp = self
            .requests
            .remove(&req)
            .expect("BUG: had key but then didn't")
            .expect("BUG: had response but then didn't");

        Ok(Some(resp))
    }

    pub fn clear_all_requests(&mut self) {
        self.requests.clear()
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;
    use std::error::Error;

    use stacks_common::util::*;

    use crate::net::test::*;

    #[test]
    fn dns_start_stop() {
        let (client, thread_handle) = dns_thread_start(100);
        drop(client);
        thread_handle.join().unwrap();
    }

    #[test]
    fn dns_resolve_one_name() {
        let (mut client, thread_handle) = dns_thread_start(100);
        client
            .queue_lookup("www.google.com", 80, get_epoch_time_ms() + 120_000)
            .unwrap();
        let mut resolved_addrs = None;
        loop {
            client.try_recv().unwrap();
            match client.poll_lookup("www.google.com", 80).unwrap() {
                Some(addrs) => {
                    test_debug!("addrs: {:?}", &addrs);
                    resolved_addrs = Some(addrs);
                    break;
                }
                None => {}
            }
            sleep_ms(100);
        }
        test_debug!("www.google.com:80 is {:?}", &resolved_addrs);
        assert!(resolved_addrs.is_some());

        dns_thread_shutdown(client, thread_handle);
    }

    #[test]
    fn dns_resolve_10_names() {
        let (mut client, thread_handle) = dns_thread_start(100);
        let names = [
            "www.google.com",
            "www.facebook.com",
            "www.twitter.com",
            "www.blockstack.org",
            "www.reddit.com",
            "www.slashdot.org",
            "www.coinmarketcap.com",
            "core.blockstack.org",
            "news.ycombinator.com",
            "lobste.rs",
        ];

        for name in names.iter() {
            client
                .queue_lookup(name, 80, get_epoch_time_ms() + 120_000)
                .unwrap();
        }

        let mut resolved_addrs = HashMap::new();
        loop {
            client.try_recv().unwrap();

            for name in names.iter() {
                if resolved_addrs.contains_key(&name.to_string()) {
                    continue;
                }
                match client.poll_lookup(name, 80).unwrap() {
                    Some(addrs) => {
                        test_debug!("name {} addrs: {:?}", name, &addrs);
                        resolved_addrs.insert(name.to_string(), addrs);
                        break;
                    }
                    None => {}
                }
            }

            if resolved_addrs.len() == names.len() {
                break;
            }
            sleep_ms(100);
        }
        assert_eq!(resolved_addrs.len(), names.len());

        dns_thread_shutdown(client, thread_handle);
    }

    #[test]
    fn dns_resolve_invalid_name() {
        let (mut client, thread_handle) = dns_thread_start(100);
        client
            .queue_lookup("asdfjkl;", 80, get_epoch_time_ms() + 120_000_000)
            .unwrap();
        let mut resolved_error = None;
        loop {
            client.try_recv().unwrap();
            match client.poll_lookup("asdfjkl;", 80).unwrap() {
                Some(resp) => {
                    test_debug!("addrs: {:?}", &resp);
                    resolved_error = Some(resp);
                    break;
                }
                None => {}
            }
            sleep_ms(100);
        }
        test_debug!("asdfjkl;:80 is {:?}", &resolved_error);
        assert!(resolved_error.is_some());
        assert!(resolved_error
            .unwrap()
            .result
            .unwrap_err()
            .find("DNS resolve error")
            .is_some());

        dns_thread_shutdown(client, thread_handle);
    }

    #[test]
    fn dns_resolve_no_such_name() {
        let (mut client, thread_handle) = dns_thread_start(100);
        client
            .queue_lookup("www.google.com", 80, get_epoch_time_ms() + 120_000_000)
            .unwrap();
        let mut resolved_err = None;
        loop {
            client.try_recv().unwrap();
            match client.poll_lookup("www.facebook.com", 80) {
                Ok(_) => {}
                Err(e) => {
                    resolved_err = Some(e);
                    break;
                }
            }
            sleep_ms(100);
        }
        assert!(resolved_err.is_some());
        assert!(format!("{:?}", &resolved_err.unwrap())
            .find("No such pending lookup")
            .is_some());
        dns_thread_shutdown(client, thread_handle);
    }

    #[test]
    fn dns_resolve_timeout() {
        let (mut client, thread_handle) = dns_thread_start(100);
        client
            .queue_lookup("www.google.com", 80, get_epoch_time_ms() + 100)
            .unwrap();
        sleep_ms(200);
        let mut resolved_err = None;
        loop {
            client.try_recv().unwrap();
            match client.poll_lookup("www.google.com", 80) {
                Ok(res) => {
                    resolved_err = Some(res);
                    break;
                }
                Err(e) => {
                    eprintln!("err: {:?}", &e);
                    assert!(false);
                }
            }
            sleep_ms(100);
        }
        assert!(resolved_err.is_some());
        eprintln!("{:?}", &resolved_err);
        assert!(format!("{:?}", &resolved_err.unwrap())
            .find("timed out")
            .is_some());
        dns_thread_shutdown(client, thread_handle);
    }
}
