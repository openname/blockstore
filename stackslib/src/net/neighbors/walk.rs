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
use std::{cmp, mem};

use rand::prelude::*;
use rand::thread_rng;
use stacks_common::types::chainstate::StacksPublicKey;
use stacks_common::util::hash::Hash160;
use stacks_common::util::secp256k1::Secp256k1PublicKey;
use stacks_common::util::{get_epoch_time_secs, log};

use crate::burnchains::{Address, Burnchain, BurnchainView, PublicKey};
use crate::net::connection::{ConnectionOptions, ReplyHandleP2P};
use crate::net::db::{LocalPeer, PeerDB};
use crate::net::neighbors::{
    NeighborComms, NeighborReplacements, NeighborWalkDB, ToNeighborKey, MAX_NEIGHBOR_BLOCK_DELAY,
    NEIGHBOR_MINIMUM_CONTACT_INTERVAL,
};
use crate::net::p2p::PeerNetwork;
use crate::net::{
    Error as net_error, HandshakeAcceptData, HandshakeData, MessageSequence, Neighbor,
    NeighborAddress, NeighborKey, PeerAddress, Preamble, StackerDBHandshakeData, StacksMessage,
    StacksMessageType, NUM_NEIGHBORS,
};

/// This struct records information from an inbound peer that has authenticated to this node.  As
/// new remote nodes connect, this node will remember this state for them so that the neighbor walk
/// logic can try to ask them for neighbors.  This enables a public peer to ask a NAT'ed peer for
/// its neighbors.
#[derive(Debug, PartialEq, Clone)]
pub struct NeighborPingback {
    pub ts: u64,                 // when we discovered this neighbor to ping back
    pub peer_version: u32,       // peer version of neighbor to ping back
    pub network_id: u32,         // network ID of neighbor to ping back
    pub pubkey: StacksPublicKey, // public key of neighbor to ping back
}

/// Struct for capturing the results of a walk.
/// -- reports newly-connected neighbors
/// -- reports neighbors we had trouble talking to.
/// The peer network will use this struct to clean out dead neighbors, and to keep the number of
/// _outgoing_ connections limited to NUM_NEIGHBORS.
#[derive(Clone, Debug)]
pub struct NeighborWalkResult {
    /// Newly-added node neighbors
    pub new_connections: HashSet<NeighborKey>,
    /// Dead connections discovered (so we can close their sockets)
    pub dead_connections: HashSet<NeighborKey>,
    /// Connections to misbehaving peers (so we can close their sockets and ban them)
    pub broken_connections: HashSet<NeighborKey>,
    /// Neighbors who got replaced in the PeerDB because they were offline, but mapped to a new
    /// peer that was online and had the same slot locations
    pub replaced_neighbors: HashSet<NeighborKey>,
}

impl NeighborWalkResult {
    pub fn new() -> NeighborWalkResult {
        NeighborWalkResult {
            new_connections: HashSet::new(),
            dead_connections: HashSet::new(),
            broken_connections: HashSet::new(),
            replaced_neighbors: HashSet::new(),
        }
    }

    pub fn add_new(&mut self, nk: NeighborKey) {
        self.new_connections.insert(nk);
    }

    pub fn add_broken(&mut self, nk: NeighborKey) {
        self.broken_connections.insert(nk);
    }

    pub fn add_dead(&mut self, nk: NeighborKey) {
        self.dead_connections.insert(nk);
    }

    pub fn add_replaced(&mut self, nk: NeighborKey) {
        self.replaced_neighbors.insert(nk);
    }

    pub fn clear(&mut self) {
        self.new_connections.clear();
        self.dead_connections.clear();
        self.broken_connections.clear();
        self.replaced_neighbors.clear();
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum NeighborWalkState {
    HandshakeBegin,
    HandshakeFinish,
    GetNeighborsBegin,
    GetNeighborsFinish,
    GetHandshakesBegin,
    GetHandshakesFinish,
    GetNeighborsNeighborsBegin,
    GetNeighborsNeighborsFinish,
    PingbackHandshakesBegin,
    PingbackHandshakesFinish,
    ReplacedNeighborsPingBegin,
    ReplacedNeighborsPingFinish,
    Finished,
}

// TODO: NeighborWalkState should be refactored so that its members simply contain the relevant
// parts of this struct.  This struct, as well as walk statistics kept in PeerNetwork, should live
// entirely within this struct.
/// A struct representing the ongoing state of the neighbor walk.  The peer node continuously
/// attempts to connect to peers in its frontier at random (including inbound peers) to discover
/// other peers it can reach.  The walk uses a variation of Metropolis-Hastings random graph walk
/// in order to calculate a random subset of the total set of peers.  The high-level steps are:
///
/// 1. Handshake with the current neighbor (if there is no current neighbor, then pick one at random)
/// 2. Ask it for its neighbors
/// 3. Handshake with each neighbor
/// 4. Ask each neighbor for its neighbors
/// 5. Calculate the ratio of in-degree to out-degree for each neighbor, and then flip a coin.  If
///    heads, then keep the current neighbor as-is.  If tails, then
#[derive(Debug)]
pub struct NeighborWalk<DB: NeighborWalkDB, NC: NeighborComms> {
    /// Current state of the walk
    pub state: NeighborWalkState,

    /// Addresses of neighbors resolved by GetNeighborsBegin/GetNeighborsFinish
    pending_neighbor_addrs: Option<Vec<NeighborAddress>>,

    /// First-ever neighbor queried
    first_neighbor: Neighbor,
    /// Last neighbor visited
    prev_neighbor: Option<Neighbor>,
    /// Current neighbor we're querying
    pub(crate) cur_neighbor: Neighbor,
    /// Next neighbor we're going to query
    next_neighbor: Option<Neighbor>,

    /// Whether or not the next walk should start with an outbound peer or inbound peer
    next_walk_outbound: bool,
    /// Whether or not we have an outbound connection to cur_neighbor
    walk_outbound: bool,

    /// This is the value of cur_neighbor as returned by its HandshakeAccept.
    /// It might be different than our value.
    neighbor_from_handshake: NeighborKey,

    /// current neighbor's frontier, built up when querying `cur_neighbor`'s neighbors
    pub frontier: HashMap<NeighborKey, Neighbor>,
    /// newly-discovered neighbors-of-neighbors of `cur_neighbor`
    new_frontier: HashMap<NeighborKey, Neighbor>,

    /// GetHandshakesBegin / GetHandshakesFinish: outstanding requests to handshake with our cur_neighbor's neighbors.
    resolved_handshake_neighbors: HashMap<NeighborAddress, Neighbor>,
    handshake_neighbor_addrs: Vec<NeighborAddress>,

    /// GetNeighborsNeighborsBegin / GetNeighborsNeighborsFinish:
    /// outstanding requests to get the neighbors of our cur_neighbor's neighbors
    resolved_getneighbors_neighbors: HashMap<NeighborAddress, Vec<NeighborAddress>>,

    /// ReplacedNeighborsPingBegin / ReplacedNeighborsPingFinish:
    /// outstanding requests to ping existing neighbors to be replaced in the frontier
    neighbor_replacements: NeighborReplacements,

    /// PingbackHandshakesBegin / PingbackHandshakesFinish:
    /// outstanding requests to new inbound peers
    network_pingbacks: HashMap<NeighborAddress, NeighborPingback>, // taken from the network at instantiation.  Maps address to (peer version, network ID, timestamp)

    /// neighbor walk result we build up incrementally
    pub result: NeighborWalkResult,

    /// time that we started/finished the last walk
    walk_start_time: u64,
    walk_end_time: u64,

    /// walk random-restart parameters
    pub(crate) walk_step_count: u64, // how many times we've taken a step
    pub(crate) walk_min_duration: u64, // minimum steps we have to take before reset
    pub(crate) walk_max_duration: u64, // maximum steps we have to take before reset
    pub(crate) walk_reset_prob: f64, // probability that we do a reset once the minimum duration is met
    pub(crate) walk_instantiation_time: u64,
    pub(crate) walk_reset_interval: u64, // how long a walk can last, in wall-clock time
    pub(crate) walk_state_time: u64,     // when the walk entered this state
    pub(crate) walk_state_timeout: u64,  // how long the walk can remain in this state

    /// Link to the underlying neighbor DB
    neighbor_db: DB,

    /// Link to the underlying p2p netwrk
    comms: NC,
}

/// Constructors and state-machine mechanics.
/// No direct access to I/O is allowed here.
impl<DB: NeighborWalkDB, NC: NeighborComms> NeighborWalk<DB, NC> {
    pub fn new(
        db: DB,
        comms: NC,
        neighbor: &Neighbor,
        outbound: bool,
        pingbacks: HashMap<NeighborAddress, NeighborPingback>,
        connection_opts: &ConnectionOptions,
    ) -> NeighborWalk<DB, NC> {
        NeighborWalk {
            state: NeighborWalkState::HandshakeBegin,

            pending_neighbor_addrs: None,

            first_neighbor: neighbor.clone(),
            prev_neighbor: None,
            cur_neighbor: neighbor.clone(),
            next_neighbor: None,
            next_walk_outbound: true,
            walk_outbound: outbound,
            neighbor_from_handshake: NeighborKey::empty(),

            frontier: HashMap::new(),
            new_frontier: HashMap::new(),

            resolved_handshake_neighbors: HashMap::new(),
            handshake_neighbor_addrs: vec![],

            resolved_getneighbors_neighbors: HashMap::new(),

            neighbor_replacements: NeighborReplacements::new(),

            network_pingbacks: pingbacks,

            result: NeighborWalkResult::new(),

            walk_start_time: get_epoch_time_secs(),
            walk_end_time: 0,

            walk_step_count: 0,
            walk_min_duration: connection_opts.walk_min_duration,
            walk_max_duration: connection_opts.walk_max_duration,
            walk_reset_prob: connection_opts.walk_reset_prob,
            walk_instantiation_time: get_epoch_time_secs(),
            walk_reset_interval: connection_opts.walk_reset_interval,
            walk_state_time: get_epoch_time_secs(),
            walk_state_timeout: connection_opts.walk_state_timeout,

            neighbor_db: db,
            comms,
        }
    }

    /// Instantiate the neighbor walk from a neighbor routable from us.
    /// Returns the walk on success.
    /// Returns NoSuchNeighbor if there's no neighbors available
    /// Returns DBError if we can't read the peer DB
    pub(crate) fn instantiate_walk(
        db: DB,
        comms: NC,
        network: &PeerNetwork,
    ) -> Result<NeighborWalk<DB, NC>, net_error> {
        let first_neighbor = db.get_next_walk_neighbor(network)?;
        let w = NeighborWalk::new(
            db,
            comms,
            &first_neighbor,
            true,
            network.get_walk_pingbacks().clone(),
            network.get_connection_opts(),
        );

        debug!(
            "{:?}: instantiated neighbor walk to outbound peer {:?}",
            network.get_local_peer(),
            &first_neighbor
        );

        Ok(w)
    }

    /// Instantiate the neighbor walk to an always-allowed node.
    /// If we're in the initial block download, then this must also be a *bootstrap* peer.
    /// Returns the neighbor walk on success
    /// Returns NotFoundError if no always-allwed neighbors are in the DB.
    /// Returns DBError if there's a problem querying the DB
    pub(crate) fn instantiate_walk_to_always_allowed(
        db: DB,
        comms: NC,
        network: &PeerNetwork,
        ibd: bool,
    ) -> Result<NeighborWalk<DB, NC>, net_error> {
        let allowed_peers = db.get_initial_walk_neighbors(network, ibd)?;
        let allowed_peer_opt = allowed_peers.into_iter().find_map(|peer| {
            if peer.public_key
                == Secp256k1PublicKey::from_private(&network.get_local_peer().private_key)
            {
                None
            } else {
                Some(peer)
            }
        });

        let allowed_peer = if let Some(peer) = allowed_peer_opt {
            peer
        } else {
            // no allowed peers in DB that aren't us. Try a different strategy
            debug!(
                "{:?}: No allowed peers in the DB that aren't us",
                network.get_local_peer()
            );
            return Err(net_error::NotFoundError);
        };

        let w = NeighborWalk::new(
            db,
            comms,
            &allowed_peer,
            true,
            network.get_walk_pingbacks().clone(),
            network.get_connection_opts(),
        );

        debug!(
            "{:?}: instantiated neighbor walk to always-allowed peer {:?}",
            network.get_local_peer(),
            &allowed_peer
        );
        Ok(w)
    }

    /// Instantiate a neighbor walk, but use an inbound neighbor instead of a neighbor from our
    /// peer DB.  This helps a public node discover other public nodes, by asking a private node
    /// for its neighbors (which can include other public nodes).
    /// If an inbound connection is found, then return the walk to it.
    /// Otherwise, return NoSuchNeighbor
    pub(crate) fn instantiate_walk_from_inbound(
        db: DB,
        comms: NC,
        network: &PeerNetwork,
    ) -> Result<NeighborWalk<DB, NC>, net_error> {
        let event_ids: Vec<_> = network.iter_peer_event_ids().collect();
        if event_ids.is_empty() {
            debug!(
                "{:?}: failed to begin inbound neighbor walk: no one's connected to us",
                network.get_local_peer()
            );
            return Err(net_error::NoSuchNeighbor);
        }

        // pick a random search index
        let mut idx = thread_rng().gen::<usize>() % event_ids.len();

        debug!(
            "{:?}: instantiate inbound walk: try inbound neighbors -- sample out of {}. idx = {}",
            network.get_local_peer(),
            network.get_num_p2p_convos(),
            idx
        );

        // find an inbound connection
        for _ in 0..event_ids.len() {
            let event_id = event_ids[idx];
            idx = (idx + 1) % event_ids.len();

            let convo = network
                .get_p2p_convo(*event_id)
                .expect("BUG: no conversation for event ID key");

            if convo.is_outbound() || !convo.is_authenticated() {
                test_debug!(
                    "{:?}: skip outbound and/or unauthenticated neighbor {}",
                    network.get_local_peer(),
                    &convo.to_neighbor_key()
                );
                continue;
            }

            // found!
            let pubkey = convo
                .get_public_key()
                .expect("BUG: authenticated conversation without public key");

            let nk = convo.to_neighbor_key();
            let empty_neighbor = Neighbor::empty(&nk, &pubkey, 0);
            let w = NeighborWalk::new(
                db,
                comms,
                &empty_neighbor,
                false,
                network.get_walk_pingbacks().clone(),
                network.get_connection_opts(),
            );

            debug!(
                "{:?}: instantiated neighbor walk to inbound peer {}",
                network.get_local_peer(),
                &nk
            );

            return Ok(w);
        }

        // no inbound peers
        debug!(
            "{:?}: no inbound peers to talk to",
            network.get_local_peer()
        );
        return Err(net_error::NoSuchNeighbor);
    }

    /// Instantiate a neighbor walk, but go straight to the pingback logic (i.e. we don't have any
    /// immediate neighbors).  That is, try to connect and step to a node that connected to us.
    /// The returned neighbor walk will be in the PingbackHandshakesBegin state.
    ///
    /// Returns the new walk, if we have any pingbacks to connect to.
    /// Returns NoSuchNeighbor if there are no pingbacks to choose from
    /// Return Denied if the chosen pingback peer is blocked
    pub(crate) fn instantiate_walk_from_pingback(
        db: DB,
        comms: NC,
        network: &PeerNetwork,
    ) -> Result<NeighborWalk<DB, NC>, net_error> {
        if network.get_walk_pingbacks().is_empty() {
            debug!("{:?}: no walk pingbacks", network.get_local_peer());
            return Err(net_error::NoSuchNeighbor);
        }

        // random search
        let idx = thread_rng().gen::<usize>() % network.get_walk_pingbacks().len();

        debug!(
            "{:?}: try pingback candidates -- sample out of {}. idx = {}",
            network.get_local_peer(),
            network.get_walk_pingbacks().len(),
            idx
        );

        let (addr, pingback_peer) = match network.get_walk_pingbacks().iter().skip(idx).next() {
            Some((addr, pingback_peer)) => (addr, pingback_peer),
            None => {
                return Err(net_error::NoSuchNeighbor);
            }
        };

        let nk = NeighborKey::from_neighbor_address(
            pingback_peer.peer_version,
            pingback_peer.network_id,
            addr,
        );

        // don't proceed if denied
        db.check_neighbor_denied(network, &nk)?;

        // (this will be ignored by the neighbor walk)
        let empty_neighbor = Neighbor::empty(&nk, &pingback_peer.pubkey, 0);

        let mut w = NeighborWalk::new(
            db,
            comms,
            &empty_neighbor,
            false,
            network.get_walk_pingbacks().clone(),
            network.get_connection_opts(),
        );

        debug!(
            "{:?}: instantiated neighbor walk to {} for pingback only",
            network.get_local_peer(),
            &nk
        );

        w.set_state(
            network.get_local_peer(),
            NeighborWalkState::PingbackHandshakesBegin,
        )?;
        Ok(w)
    }

    /// Reset the walk with a new neighbor.
    /// Give back a report of the walk.
    /// Resets neighbor pointer.
    /// Clears out connections, but preserves state (frontier, result, etc.).
    fn reset(
        &mut self,
        local_peer: &LocalPeer,
        next_neighbor: Neighbor,
        next_neighbor_outbound: bool,
    ) -> NeighborWalkResult {
        debug!(
            "{:?}: Walk reset to {} neighbor {:?}",
            local_peer,
            if self.next_walk_outbound {
                "outbound"
            } else {
                "inbound"
            },
            &next_neighbor.addr
        );
        self.state = NeighborWalkState::HandshakeBegin;
        self.walk_state_time = get_epoch_time_secs();

        if self.cur_neighbor != next_neighbor {
            // moving on -- clear frontier
            self.frontier.clear();
        }

        self.prev_neighbor = Some(self.cur_neighbor.clone());
        self.cur_neighbor = next_neighbor;
        self.walk_outbound = next_neighbor_outbound;
        self.next_neighbor = None;

        self.clear_connections(local_peer);
        self.new_frontier.clear();

        let result = self.result.clone();

        self.walk_end_time = get_epoch_time_secs();

        // leave self.frontier and self.result alone until the next walk.
        // (makes it so that at the end of the walk, we can query the result and frontier, which
        // get built up over successive passes of the state-machine)
        result
    }

    /// Clear the walk's connection state
    fn clear_connections(&mut self, _local_peer: &LocalPeer) {
        test_debug!("{:?}: Walk clear connections", _local_peer);
        self.pending_neighbor_addrs = None;
        self.comms.reset();

        self.resolved_handshake_neighbors.clear();
        self.handshake_neighbor_addrs.clear();
        self.resolved_getneighbors_neighbors.clear();
        self.neighbor_replacements.clear();
        self.network_pingbacks.clear();
    }

    /// Update the state of the walk.
    /// If the code spent too much time in one state, then the walk will fail with StepTimeout
    fn set_state(
        &mut self,
        _local_peer: &LocalPeer,
        new_state: NeighborWalkState,
    ) -> Result<(), net_error> {
        if self.walk_state_time + self.walk_state_timeout < get_epoch_time_secs() {
            return Err(net_error::StepTimeout);
        }

        test_debug!(
            "{:?}: Advance walk state: {:?} --> {:?} (after {} seconds)",
            _local_peer,
            &self.state,
            &new_state,
            get_epoch_time_secs().saturating_sub(self.walk_state_time)
        );

        // save dead/broken neighbors
        let dead_neighbors = self.comms.take_dead_neighbors();
        let broken_neighbors = self.comms.take_broken_neighbors();
        for dead in dead_neighbors.into_iter() {
            self.result.add_dead(dead);
        }
        for broken in broken_neighbors.into_iter() {
            self.result.add_broken(broken);
        }

        self.state = new_state;
        self.walk_state_time = get_epoch_time_secs();
        Ok(())
    }

    /// facade to self.comms.get_pinned_connections() to the pruner can have at the events we're using
    pub fn get_pinned_connections(&self) -> &HashSet<usize> {
        self.comms.get_pinned_connections()
    }

    /// Select neighbors that are routable, and ignore ones that are not.
    fn filter_sensible_neighbors(
        mut neighbors: Vec<NeighborAddress>,
        private_neighbors: bool,
    ) -> Vec<NeighborAddress> {
        neighbors.retain(|neighbor| !neighbor.addrbytes.is_anynet());
        if !private_neighbors {
            neighbors.retain(|neighbor| !neighbor.addrbytes.is_in_private_range());
        }
        neighbors
    }

    /// Begin handshaking with our current neighbor.
    /// On success, return Ok(true) and transition to HandshakeFinish
    /// On failure, return an Err(...)
    /// If we're not yet connected, return Ok(false).  The caller should try again.
    pub fn handshake_begin(&mut self, network: &mut PeerNetwork) -> Result<bool, net_error> {
        if self.comms.count_inflight() > 0 {
            // in progress already
            return Ok(true);
        }

        // if cur_neighbor is _us_, then grab a different neighbor and try again.
        // Note that we compare the Hash160s here because `::from_node_public_key()` will return
        // the same data regardless of whether or not the public key argument is compressed.
        // `cur_neighbor.public_key` is always compressed.
        if Hash160::from_node_public_key(&self.cur_neighbor.public_key)
            == Hash160::from_node_public_key(&Secp256k1PublicKey::from_private(
                &network.get_local_peer().private_key,
            ))
        {
            test_debug!(
                "{:?}: Walk stepped to ourselves.  Will reset instead.",
                network.get_local_peer()
            );
            return Err(net_error::NoSuchNeighbor);
        }

        // if cur_neighbor is our bind address, then grab a different neighbor and try
        // again
        if network.is_bound(&self.cur_neighbor.addr) {
            debug!(
                "{:?}: Walk stepped to our bind address ({:?}).  Will reset instead.",
                network.get_local_peer(),
                &self.cur_neighbor.addr
            );
            return Err(net_error::NoSuchNeighbor);
        }

        // if cur_neighbor is an anynet address, then grab a different neighbor and try
        // again
        if self.cur_neighbor.addr.addrbytes.is_anynet() {
            debug!(
                "{:?}: Walk stepped to an any-network address ({:?}).  Will reset instead.",
                network.get_local_peer(),
                &self.cur_neighbor.addr
            );
            return Err(net_error::NoSuchNeighbor);
        }

        let cur_addr = self.cur_neighbor.addr.clone();
        self.new_frontier.clear();
        self.result.clear();

        if self
            .comms
            .neighbor_session_begin(network, &NeighborAddress::from_neighbor(&self.cur_neighbor))?
        {
            debug!(
                "{:?}: Handshake sent to {:?}",
                network.get_local_peer(),
                &cur_addr
            );
            self.set_state(network.get_local_peer(), NeighborWalkState::HandshakeFinish)?;
            Ok(true)
        } else {
            debug!(
                "{:?}: No Handshake sent (dest was {:?}); still connecting",
                network.get_local_peer(),
                &cur_addr,
            );
            Ok(false)
        }
    }

    /// Handle a HandshakeAcceptData.
    /// Update the PeerDB information from the handshake data, as well as `self.cur_neighbor`, if
    /// this neighbor was routable.  If it's not routable (i.e. we walked to an inbound neighbor),
    /// then do not update the DB.
    /// Add this neighbor to our newly-calculated frontier either way
    /// Returns the updated `self.cur_neighbor` on success.
    /// Returns Err(..) if we failed to validate the request or we have a DB error.
    fn handle_handshake_accept(
        &mut self,
        network: &mut PeerNetwork,
        preamble: &Preamble,
        data: &HandshakeAcceptData,
        db_data: Option<&StackerDBHandshakeData>,
    ) -> Result<Neighbor, net_error> {
        let local_peer_str = format!("{:?}", network.get_local_peer());

        let mut neighbor_from_handshake = self
            .neighbor_db
            .neighbor_from_handshake(network, preamble, data)?;

        // if the neighbor accidentally gave us a private IP address, then
        // just use the one we used to contact it.  This can happen if the
        // node is behind a load-balancer, or is doing port-forwarding,
        // etc. But do nothing if both cur_neighbor and its reported address are private.
        if (neighbor_from_handshake.addr.addrbytes.is_in_private_range()
            || neighbor_from_handshake.addr.addrbytes.is_anynet())
            && !self.cur_neighbor.addr.addrbytes.is_in_private_range()
        {
            debug!(
                "{}: outbound neighbor gave private IP address {:?}; assuming it meant {:?}",
                local_peer_str, &neighbor_from_handshake.addr, &self.cur_neighbor.addr
            );
            neighbor_from_handshake.addr.addrbytes = self.cur_neighbor.addr.addrbytes.clone();
            neighbor_from_handshake.addr.port = self.cur_neighbor.addr.port;
        }

        if self.walk_outbound && neighbor_from_handshake.addr != self.cur_neighbor.addr {
            // somehow, got a handshake from someone that _isn't_ cur_neighbor.
            // Note that this does not matter for inbound walks, because we don't always know the
            // real address anyway (since an inbound neighbor might be NAT'ed from us).
            debug!("{}: got unsolicited (or bootstrapping) HandshakeAccept from outbound {:?} (expected {:?})", 
                       local_peer_str,
                       &neighbor_from_handshake.addr,
                       &self.cur_neighbor.addr);

            return Err(net_error::PeerNotConnected);
        };

        debug!(
            "{}: Connected with {:?}",
            local_peer_str, &self.cur_neighbor.addr
        );

        // update our view of `cur_neighbor`, but only if `cur_neighbor` is routable for us.
        // That is not guaranteed to be the case in one instance: this is an inbound walk, and
        // `cur_neighbor` is the very first neighbor we're querying.
        if self.walk_outbound || self.first_neighbor.addr != self.cur_neighbor.addr {
            let cur_neighbor = self.cur_neighbor.clone();
            let new_cur_neighbor =
                self.neighbor_db
                    .update_neighbor(network, cur_neighbor, Some(data), db_data)?;
            self.cur_neighbor = new_cur_neighbor;
        }
        self.new_frontier
            .insert(self.cur_neighbor.addr.clone(), self.cur_neighbor.clone());
        self.neighbor_from_handshake = neighbor_from_handshake.addr;

        Ok(self.cur_neighbor.clone())
    }

    /// Finish handshaking with our current neighbor, thereby ensuring that it is connected
    /// Returns true if we finished talking to the neighbor
    /// Returns false if not
    pub fn handshake_try_finish(&mut self, network: &mut PeerNetwork) -> Result<bool, net_error> {
        assert!(self.state == NeighborWalkState::HandshakeFinish);
        if self.comms.count_inflight() == 0 {
            // can't proceed
            debug!("{:?}: No messages inflight", network.get_local_peer());
            return Err(net_error::PeerNotConnected);
        }

        let message = if let Some((_, message)) = self.comms.collect_replies(network).pop() {
            message
        } else {
            // try again later
            return Ok(false);
        };

        let (data, db_data) = match message.payload {
            StacksMessageType::HandshakeAccept(ref data) => (data, None),
            StacksMessageType::StackerDBHandshakeAccept(ref data, ref db_data) => {
                (data, Some(db_data))
            }
            StacksMessageType::HandshakeReject => {
                // told to bugger off
                return Err(net_error::PeerNotConnected);
            }
            StacksMessageType::Nack(_) => {
                // something's wrong on our end (we're using a new key that they don't yet
                // know about, or something)
                return Err(net_error::PeerNotConnected);
            }
            _ => {
                // invalid message
                debug!(
                    "{:?}: Got out-of-sequence message from {:?}",
                    network.get_local_peer(),
                    &self.cur_neighbor.addr
                );
                self.comms
                    .add_broken(network, &self.cur_neighbor.addr.clone());
                return Err(net_error::InvalidMessage);
            }
        };

        debug!(
            "Received HandshakeAccept.";
            "local_peer" => ?network.get_local_peer(),
            "walk_type" => if self.walk_outbound { "outbound" } else { "inbound"},
            "neighbor" => ?message.to_neighbor_key(&data.handshake.addrbytes, data.handshake.port),
            "handshake_data" => ?data.handshake,
            "stackerdb_data" => ?db_data
        );

        self.handle_handshake_accept(network, &message.preamble, data, db_data)?;

        // proceed to ask this neighbor for its neighbors.
        self.set_state(
            network.get_local_peer(),
            NeighborWalkState::GetNeighborsBegin,
        )?;
        Ok(true)
    }

    /// Begin refreshing our knowledge of peer in/out degrees.
    /// Ask self.cur_neighbor for its neighbors
    pub fn getneighbors_begin(&mut self, network: &mut PeerNetwork) -> Result<bool, net_error> {
        assert!(self.state == NeighborWalkState::GetNeighborsBegin);

        if self.comms.count_inflight() > 0 {
            // already in-flight
            return Ok(true);
        }
        debug!(
            "{:?}: send GetNeighbors to {:?}",
            network.get_local_peer(),
            &self.cur_neighbor.addr
        );

        self.comms.neighbor_send(
            network,
            &NeighborAddress::from_neighbor(&self.cur_neighbor),
            StacksMessageType::GetNeighbors,
        )?;
        self.set_state(
            network.get_local_peer(),
            NeighborWalkState::GetNeighborsFinish,
        )?;
        Ok(true)
    }

    /// Try to finish the getneighbors request to cur_neighbor
    /// Returns true if we succeed
    /// Returns false if we're still waiting
    pub fn getneighbors_try_finish(
        &mut self,
        network: &mut PeerNetwork,
    ) -> Result<bool, net_error> {
        assert!(self.state == NeighborWalkState::GetNeighborsFinish);

        if self.comms.count_inflight() == 0 {
            // can't proceed
            debug!("{:?}: No messages inflight", network.get_local_peer());
            return Err(net_error::PeerNotConnected);
        }

        let message = if let Some((_, message)) = self.comms.collect_replies(network).pop() {
            message
        } else {
            // try again later
            return Ok(false);
        };

        let mut neighbor_addrs_to_resolve = match message.payload {
            StacksMessageType::Neighbors(ref data) => {
                debug!(
                    "{:?}: Got Neighbors from {:?}: {:?}",
                    network.get_local_peer(),
                    &self.cur_neighbor.addr,
                    data.neighbors
                );
                let neighbors = Self::filter_sensible_neighbors(
                    data.neighbors.clone(),
                    network.get_connection_opts().private_neighbors,
                );
                let (mut found, to_resolve) = self
                    .neighbor_db
                    .lookup_stale_neighbors(network, &neighbors)?;

                // add neighbors we already know about to the frontier of `cur_neighbor`
                for (_naddr, neighbor) in found.drain() {
                    self.new_frontier
                        .insert(neighbor.addr.clone(), neighbor.clone());
                    self.frontier
                        .insert(neighbor.addr.clone(), neighbor.clone());
                }

                to_resolve
            }
            StacksMessageType::Nack(ref data) => {
                debug!(
                    "{:?}: Neighbor {:?} NACK'ed GetNeighbors with code {:?}",
                    network.get_local_peer(),
                    &self.cur_neighbor.addr,
                    data.error_code
                );
                self.comms
                    .add_broken(network, &self.cur_neighbor.addr.clone());
                return Err(net_error::ConnectionBroken);
            }
            _ => {
                // invalid message
                debug!(
                    "{:?}: Got out-of-sequence message from {:?}",
                    network.get_local_peer(),
                    &self.cur_neighbor.addr
                );
                self.comms
                    .add_broken(network, &self.cur_neighbor.addr.clone());
                return Err(net_error::InvalidMessage);
            }
        };

        // prune the list to a reasonable size in case cur_neighbor gave us too many for our
        // configuration
        if neighbor_addrs_to_resolve.len() as u64
            > network.get_connection_opts().max_neighbors_of_neighbor
        {
            debug!(
                "{:?}: will handshake with {} neighbors out of {} reported by {:?}",
                network.get_local_peer(),
                network.get_connection_opts().max_neighbors_of_neighbor,
                neighbor_addrs_to_resolve.len(),
                &self.cur_neighbor.addr
            );
            neighbor_addrs_to_resolve.shuffle(&mut thread_rng());
            neighbor_addrs_to_resolve
                .truncate(network.get_connection_opts().max_neighbors_of_neighbor as usize);
        }

        // proceed to handshake with them.
        // also, try to handshake with the current neighbor's advertized IP address (it might be
        // different than the one we use)
        test_debug!("{:?}: will try to handshake with inbound neighbor {:?}'s advertized address {:?} as well", network.get_local_peer(), &self.cur_neighbor.addr, &self.neighbor_from_handshake);
        let cur_neighbor_pubkey_hash = Hash160::from_node_public_key(&self.cur_neighbor.public_key);
        neighbor_addrs_to_resolve.push(NeighborAddress::from_neighbor_key(
            self.neighbor_from_handshake.clone(),
            cur_neighbor_pubkey_hash,
        ));

        test_debug!(
            "{:?}: received Neighbors from {} {:?}: {:?}",
            network.get_local_peer(),
            if self.walk_outbound {
                "outbound"
            } else {
                "inbound"
            },
            &self.cur_neighbor.addr,
            &neighbor_addrs_to_resolve
        );

        // now go and try and connect to these neighbors
        self.pending_neighbor_addrs = Some(neighbor_addrs_to_resolve);
        self.set_state(
            network.get_local_peer(),
            NeighborWalkState::GetHandshakesBegin,
        )?;
        Ok(true)
    }

    /// Begin getting the neighors of cur_neighbor's neighbors.
    /// ReplyHandleP2Ps should be reply handles for Handshake requests.
    pub fn neighbor_handshakes_begin(
        &mut self,
        network: &mut PeerNetwork,
    ) -> Result<bool, net_error> {
        assert!(self.state == NeighborWalkState::GetHandshakesBegin);

        let my_pubkey_hash = Hash160::from_node_public_key(&Secp256k1PublicKey::from_private(
            &network.get_local_peer().private_key,
        ));

        let pending_neighbor_addrs = self
            .pending_neighbor_addrs
            .take()
            .expect("FATAL: no result from GetNeighbors");

        // got neighbors -- proceed to ask each one for *its* neighbors so we can
        // estimate cur_neighbor's in-degree and grow our frontier.
        debug!(
            "{:?}: will try to connect to {} neighbors of {:?}",
            network.get_local_peer(),
            pending_neighbor_addrs.len(),
            &self.cur_neighbor.addr
        );

        let mut still_pending = vec![];
        for na in pending_neighbor_addrs.into_iter() {
            // don't talk to myself if we're listed as a neighbor of this
            // remote peer.
            if na.public_key_hash == my_pubkey_hash {
                test_debug!(
                    "{:?}: skip handshaking with myself",
                    network.get_local_peer()
                );
                continue;
            }

            // don't handshake with cur_neighbor if we already know its public IP
            // address (we may not know this if the neighbor is inbound)
            if na.addrbytes == self.cur_neighbor.addr.addrbytes
                && na.port == self.cur_neighbor.addr.port
            {
                test_debug!(
                    "{:?}: skip handshaking with cur_neighbor {:?}",
                    network.get_local_peer(),
                    &self.cur_neighbor.addr
                );
                continue;
            }

            let nk = na.to_neighbor_key(network);

            // don't talk to a neighbor if it's unroutable anyway
            if network.is_bound(&nk) || nk.addrbytes.is_anynet() {
                test_debug!(
                    "{:?}: will not connect to bind / anynet address {:?}",
                    network.get_local_peer(),
                    &nk
                );
                continue;
            }

            // start a session with this neighbor
            match self.comms.neighbor_session_begin(network, &na) {
                Ok(true) => {
                    debug!(
                        "{:?}: will Handshake with neighbor-of-neighbor {:?} ({})",
                        network.get_local_peer(),
                        &nk,
                        &na.public_key_hash
                    );
                }
                Ok(false) => {
                    test_debug!(
                        "{:?}: already connecting to {:?}",
                        network.get_local_peer(),
                        &nk
                    );
                    still_pending.push(na);
                    continue;
                }
                Err(e) => {
                    debug!(
                        "{:?}: Failed to connect to {:?}: {:?}",
                        network.get_local_peer(),
                        &nk,
                        &e
                    );
                    continue;
                }
            }
        }

        if !still_pending.is_empty() {
            // try again
            self.pending_neighbor_addrs = Some(still_pending);
            return Ok(false);
        }

        // everybody connected! next state
        test_debug!(
            "{:?}: connected to {} neighbors-of-neighbors of {:?}",
            network.get_local_peer(),
            self.comms.count_inflight(),
            &self.cur_neighbor.addr
        );
        self.set_state(
            network.get_local_peer(),
            NeighborWalkState::GetHandshakesFinish,
        )?;
        Ok(true)
    }

    /// Handle a handshake accept from a neighbor as part of our neighbor-handshake step
    fn handle_neighbor_handshake_accept(
        &mut self,
        network: &mut PeerNetwork,
        naddr: NeighborAddress,
        preamble: &Preamble,
        data: &HandshakeAcceptData,
        db_data: Option<&StackerDBHandshakeData>,
    ) -> Result<(), net_error> {
        // NOTE: even if cur_neighbor is an inbound neighbor, the neighbors
        // of cur_neighbor that we could handshake with are necessarily
        // outbound connections.  So, save them all.
        // Do we know about this peer already?
        let (new, neighbor) = self.neighbor_db.add_or_schedule_replace_neighbor(
            network,
            preamble,
            &data.handshake,
            db_data,
            &mut self.neighbor_replacements,
        )?;

        if new {
            // neighbor was new
            self.new_frontier
                .insert(neighbor.addr.clone(), neighbor.clone());
        } else {
            // frontier maintenance
            self.frontier
                .insert(neighbor.addr.clone(), neighbor.clone());
        }

        self.resolved_handshake_neighbors.insert(naddr, neighbor);
        Ok(())
    }

    /// Try to finish getting handshakes from cur_neighbors' neighbors.
    /// As a side-effect of handshaking with all these peers, our PeerDB instance will be expanded
    /// with the addresses, public keys, public key expiries of these neighbors -- i.e. this method grows
    /// our frontier.
    /// Returns Ok(true) if all outstanding requests completed.
    /// Returns Ok(false) if there are still pending requests
    /// Returns Err(..) on DB errors
    pub fn neighbor_handshakes_try_finish(
        &mut self,
        network: &mut PeerNetwork,
    ) -> Result<bool, net_error> {
        assert!(self.state == NeighborWalkState::GetHandshakesFinish);

        // see if we got any replies
        test_debug!(
            "{:?}: Try to finish {} in-flight handshakes with neighbors-of-neighbor {:?}",
            network.get_local_peer(),
            self.comms.count_inflight(),
            &self.cur_neighbor.addr
        );

        for (naddr, message) in self.comms.collect_replies(network).into_iter() {
            let nkey = naddr.to_neighbor_key(network);
            let (data, db_data) = match message.payload {
                StacksMessageType::HandshakeAccept(ref data) => (data, None),
                StacksMessageType::StackerDBHandshakeAccept(ref data, ref db_data) => {
                    (data, Some(db_data))
                }
                StacksMessageType::HandshakeReject => {
                    // remote peer doesn't want to talk to us
                    debug!(
                        "{:?}: Neighbor {:?} rejected our handshake",
                        network.get_local_peer(),
                        &nkey
                    );
                    self.comms.add_dead(
                        network,
                        &NeighborKey::from_neighbor_address(
                            message.preamble.peer_version,
                            message.preamble.network_id,
                            &naddr,
                        ),
                    );
                    continue;
                }
                StacksMessageType::Nack(ref data) => {
                    // remote peer nope'd us
                    debug!(
                        "{:?}: Neighbor {:?} NACK'ed our handshake with error code {:?}",
                        network.get_local_peer(),
                        &nkey,
                        data.error_code
                    );
                    self.comms.add_dead(
                        network,
                        &NeighborKey::from_neighbor_address(
                            message.preamble.peer_version,
                            message.preamble.network_id,
                            &naddr,
                        ),
                    );
                    continue;
                }
                _ => {
                    // protocol violation
                    debug!(
                        "{:?}: Neighbor {:?} replied an out-of-sequence message",
                        network.get_local_peer(),
                        &naddr
                    );
                    self.comms.add_broken(
                        network,
                        &NeighborKey::from_neighbor_address(
                            message.preamble.peer_version,
                            message.preamble.network_id,
                            &naddr,
                        ),
                    );
                    continue;
                }
            };
            debug!(
                "{:?}: Got HandshakeAccept from {:?}",
                network.get_local_peer(),
                &nkey;
                "handshake_data" => ?data,
                "stackerdb_data" => ?db_data
            );

            self.handle_neighbor_handshake_accept(
                network,
                naddr,
                &message.preamble,
                data,
                db_data,
            )?;
        }

        if self.comms.count_inflight() > 0 {
            // still handshaking
            return Ok(false);
        }

        // finished handshaking!  find neighbors that accepted
        let mut neighbor_addrs = vec![];

        // update our frontier knowledge
        for (nkey, new_neighbor) in self.new_frontier.drain() {
            debug!(
                "{:?}: Add to frontier of {:?}: {:?}",
                network.get_local_peer(),
                &self.cur_neighbor.addr,
                &nkey
            );

            if nkey.addrbytes != self.cur_neighbor.addr.addrbytes
                || nkey.port != self.cur_neighbor.addr.port
            {
                neighbor_addrs.push(NeighborAddress::from_neighbor(&new_neighbor));
            }

            self.frontier.insert(nkey.clone(), new_neighbor);
        }

        debug!("{:?}: Adding to frontier of {:?}", &network.get_local_peer(), &self.cur_neighbor.addr;
               "new_frontier_entries" => ?self.new_frontier);

        self.new_frontier.clear();

        self.handshake_neighbor_addrs.clear();
        self.handshake_neighbor_addrs.append(&mut neighbor_addrs);

        // advance state!
        self.set_state(
            network.get_local_peer(),
            NeighborWalkState::GetNeighborsNeighborsBegin,
        )?;
        Ok(true)
    }

    /// Begin asking remote neighbors for their neighbors in order to estimate cur_neighbor's
    /// in-degree.  We should be connected to all of them, so don't worry about establishing
    /// connections to them.
    pub fn getneighbors_neighbors_begin(
        &mut self,
        network: &mut PeerNetwork,
    ) -> Result<bool, net_error> {
        assert!(self.state == NeighborWalkState::GetNeighborsNeighborsBegin);

        let handshake_neighbor_addrs = mem::replace(&mut self.handshake_neighbor_addrs, vec![]);
        for naddr in handshake_neighbor_addrs.into_iter() {
            let nk = naddr.to_neighbor_key(network);
            if !network.is_registered(&nk) {
                // not connected to this neighbor -- can't ask for neighbors
                debug!("{:?}: Not connected to {:?}", network.get_local_peer(), &nk);
                continue;
            }
            debug!(
                "{:?}: send GetNeighbors to {:?}",
                network.get_local_peer(),
                &nk
            );
            if let Err(e) =
                self.comms
                    .neighbor_send(network, &naddr, StacksMessageType::GetNeighbors)
            {
                debug!(
                    "{:?}: Could not send to {:?}: {:?}",
                    network.get_local_peer(),
                    &nk,
                    &e
                );
                continue;
            }
        }

        // advance state!
        self.set_state(
            network.get_local_peer(),
            NeighborWalkState::GetNeighborsNeighborsFinish,
        )?;
        Ok(true)
    }

    /// Try to finish getting the neighbors from cur_neighbors' neighbors.
    /// Once finished, update `cur_neighbor` and `prev_neighbor` to walk to the next random neighbor based
    /// on what we have discovered, and if this neighbor we were considering was an outbound
    /// neighbor, then also update its in/out-degree estimates in the peers DB.
    /// Returns Ok(true) if we're done
    /// Returns Ok(false) if we're still waiting
    /// Returns Err(..) on irrecoverable error
    pub fn getneighbors_neighbors_try_finish(
        &mut self,
        network: &mut PeerNetwork,
    ) -> Result<bool, net_error> {
        assert!(self.state == NeighborWalkState::GetNeighborsNeighborsFinish);

        // see if we got any replies
        for (naddr, message) in self.comms.collect_replies(network).into_iter() {
            let nkey = naddr.to_neighbor_key(network);
            match message.payload {
                StacksMessageType::Neighbors(ref data) => {
                    debug!(
                        "{:?}: Got Neighbors from {:?}: {:?}",
                        network.get_local_peer(),
                        &nkey,
                        &data.neighbors
                    );
                    let neighbors = Self::filter_sensible_neighbors(
                        data.neighbors.clone(),
                        network.get_connection_opts().private_neighbors,
                    );
                    self.resolved_getneighbors_neighbors
                        .insert(naddr, neighbors);
                }
                StacksMessageType::Nack(ref data) => {
                    // not broken; likely because it hasn't gotten to processing our
                    // handshake yet.  We'll just ignore it.
                    debug!(
                        "{:?}: Neighbor {:?} NACKed with code {:?}",
                        network.get_local_peer(),
                        &nkey,
                        data.error_code
                    );
                }
                _ => {
                    // unexpected reply
                    debug!("{:?}: Neighbor {:?} replied an out-of-sequence message (type {}); assuming broken", network.get_local_peer(), &nkey, message.get_message_name());
                    self.comms.add_broken(network, &nkey);
                }
            }
        }

        if self.comms.count_inflight() > 0 {
            // not done yet
            debug!(
                "{:?}: still waiting for {} Neighbors replies",
                network.get_local_peer(),
                self.comms.count_inflight()
            );
            return Ok(false);
        }

        // finished!  build up frontier's in-degree estimation, plus ourselves
        self.cur_neighbor.in_degree = 1;
        self.cur_neighbor.out_degree = self.frontier.len() as u32;

        for (_, neighbor_list) in self.resolved_getneighbors_neighbors.iter() {
            let cur_neighbor_in_list = neighbor_list.iter().find(|na| {
                na.addrbytes == self.cur_neighbor.addr.addrbytes
                    && na.port == self.cur_neighbor.addr.port
            });
            if cur_neighbor_in_list.is_some() {
                self.cur_neighbor.in_degree += 1;
            }
        }

        // remember this peer's in/out degree estimates if it's guaranteed to be routable from us
        if self.walk_outbound || self.first_neighbor.addr != self.cur_neighbor.addr {
            debug!(
                "{:?}: In/Out degree of current neighbor {:?} is {}/{}",
                network.get_local_peer(),
                &self.cur_neighbor.addr,
                self.cur_neighbor.in_degree,
                self.cur_neighbor.out_degree
            );

            let cur_neighbor = self.cur_neighbor.clone();
            let new_cur_neighbor =
                self.neighbor_db
                    .update_neighbor(network, cur_neighbor, None, None)?;
            self.cur_neighbor = new_cur_neighbor;
        }

        // perform the MHRWDA step to update the cur_neighbor cursor to potentially point to a
        // new neighbor, so we can do this all again!
        self.step(network);

        // advance state
        self.set_state(
            network.get_local_peer(),
            NeighborWalkState::PingbackHandshakesBegin,
        )?;
        Ok(true)
    }

    /// Pick a random neighbor from a given list of neighbors, excluding an optional given neighbor
    fn pick_random_neighbor(
        frontier: &HashMap<NeighborKey, Neighbor>,
        exclude: Option<&Neighbor>,
    ) -> Option<Neighbor> {
        let mut rnd = thread_rng();
        if frontier.is_empty() || (exclude.is_some() && frontier.len() == 1) {
            return None;
        }
        // select a random neighbor index, if exclude is set, and matches this
        //  neighbor, then use the next index (modulo the frontier length).
        let mut neighbor_index = rnd.gen_range(0..frontier.len());
        for _ in 0..2 {
            // two attempts, in case our first attempt lands on `exclude`
            for (cnt, (nk, n)) in frontier.iter().enumerate() {
                if cnt < neighbor_index {
                    continue;
                }

                let exclude_addr = match exclude {
                    None => return Some(n.clone()),
                    Some(ref e) => e,
                };
                if exclude_addr.addr == *nk {
                    // hit `exclude`. try again with an index we know will work
                    neighbor_index = (neighbor_index + 1) % frontier.len();
                    break;
                } else {
                    return Some(n.clone());
                }
            }
        }
        None
    }

    /// Calculate the "degree ratio" between two neighbors, used to determine the probability of
    /// stepping to a neighbor in MHRWDA.  We estimate each neighbor's undirected degree, and then
    /// measure how represented each neighbor's AS is in the peer graph.  We *bias* the sample so
    /// that peers in under-represented ASs are more likely to be walked to than they otherwise
    /// would be if considering only neighbor degrees.
    fn degree_ratio(&self, network: &PeerNetwork, n1: &Neighbor, n2: &Neighbor) -> f64 {
        let d1 = n1.degree() as f64;
        let d2 = n2.degree() as f64;
        let as_d1 = self.neighbor_db.get_asn_count(network, n1.asn) as f64;
        let as_d2 = self.neighbor_db.get_asn_count(network, n2.asn) as f64;
        (d1 * as_d2) / (d2 * as_d1)
    }

    /// Do the MHRWDA step -- try to step from our cur_neighbor to an immediate neighbor, if there
    /// is any neighbor to step to.  Return the new cur_neighbor, if we were able to step.
    /// The caller should call reset() after this, optionally with a newly-selected frontier
    /// neighbor if we were unable to take a step.
    ///
    /// This is a slightly modified MHRWDA algorithm.  The following differences are described:
    /// * The Stacks peer network is a _directed_ graph, whereas MHRWDA is desigend to operate
    /// on _undirected_ graphs.  As such, we calculate a separate peer graph with undirected edges
    /// with the same peers.  We estimate a peer's undirected degree with Neighbor::degree().
    /// * The probability of transitioning to a new peer is proportional not only to the ratio of
    /// the current peer's degree to the new peer's degree, but also to the ratio of the new
    /// peer's AS's node count to the current peer's AS's node count.
    ///
    /// This method updates self.next_neighbor with a new neighbor to step to, or None to restart.
    pub fn step(&mut self, network: &PeerNetwork) {
        test_debug!(
            "{:?}: execute neighbor step from {:?}",
            network.get_local_peer(),
            &self.cur_neighbor.addr
        );

        let mut rnd = thread_rng();

        // step to a node in cur_neighbor's frontier, per MHRWDA
        let next_neighbor_opt = if self.frontier.is_empty() {
            // stay here for now -- we don't yet know this neighbor's
            // frontier
            if self.walk_outbound {
                Some(self.cur_neighbor.clone())
            } else {
                None
            }
        } else {
            // continuing the walk
            let next_neighbor =
                Self::pick_random_neighbor(&self.frontier, None).expect("BUG: empty frontier size"); // won't panic since !self.frontier.is_empty()
            let walk_prob: f64 = rnd.gen();
            if walk_prob
                < self
                    .degree_ratio(network, &self.cur_neighbor, &next_neighbor)
                    .min(1.0)
            {
                // won the coin toss; will take a step.
                // take care not to step back to the neighbor from which we
                // stepped previously
                if let Some(prev_neighbor) = self.prev_neighbor.as_ref() {
                    if prev_neighbor.addr == next_neighbor.addr {
                        // oops, backtracked.  Try to pick a different neighbor, if possible.
                        if self.frontier.len() == 1 {
                            // no other choices. will need to reset this walk.
                            None
                        } else {
                            // have alternative choices, so instead of backtracking, we'll delay
                            // acceptance by probabilistically deciding to step to an alternative
                            // instead of backtracking.
                            let alt_next_neighbor =
                                Self::pick_random_neighbor(&self.frontier, Some(prev_neighbor))
                                    .expect("BUG: empty frontier size");
                            let alt_prob: f64 = rnd.gen();

                            let cur_to_alt =
                                self.degree_ratio(network, &self.cur_neighbor, &alt_next_neighbor);
                            let prev_to_cur =
                                self.degree_ratio(network, prev_neighbor, &self.cur_neighbor);
                            let trans_prob = fmin!(
                                fmin!(1.0, cur_to_alt * cur_to_alt),
                                fmax!(1.0, prev_to_cur * prev_to_cur)
                            );

                            if alt_prob < fmin!(1.0, trans_prob) {
                                // go to alt peer instead
                                Some(alt_next_neighbor)
                            } else {
                                // backtrack.
                                Some(next_neighbor)
                            }
                        }
                    } else {
                        // not backtracking.  Take a step.
                        Some(next_neighbor)
                    }
                } else {
                    // not backtracking.  Take a step.
                    Some(next_neighbor)
                }
            } else {
                // lost the coin toss. will not take a step
                Some(self.cur_neighbor.clone())
            }
        };

        if let Some(ref neighbor) = next_neighbor_opt {
            debug!(
                "{:?}: Walk will step to {:?}",
                network.get_local_peer(),
                &neighbor.addr
            );
        } else {
            debug!(
                "{:?}: Walk will not step to a new neighbor (stay at {})",
                network.get_local_peer(),
                &self.cur_neighbor.addr
            );
        }

        self.next_neighbor = next_neighbor_opt;
        if self.next_neighbor.is_some() {
            self.next_walk_outbound = self.walk_outbound;
        } else {
            // will reset using a peer routable from us
            self.next_walk_outbound = true;
        }
    }

    /// Start to connect to newly-discovered inbound peers
    pub fn pingback_handshakes_begin(
        &mut self,
        network: &mut PeerNetwork,
    ) -> Result<bool, net_error> {
        // caller will have already populated the pending_pingback_handshakes hashmap
        assert!(self.state == NeighborWalkState::PingbackHandshakesBegin);

        let network_pingbacks = mem::replace(&mut self.network_pingbacks, HashMap::new());
        let mut still_pending: HashMap<NeighborAddress, _> = HashMap::new();

        for (naddr, pingback) in network_pingbacks.into_iter() {
            // pingback hint is stale? (or we tried to connect and timed out?)
            if pingback.ts + network.get_connection_opts().pingback_timeout < get_epoch_time_secs()
            {
                continue;
            }

            let nk = NeighborKey::from_neighbor_address(
                pingback.peer_version,
                pingback.network_id,
                &naddr,
            );

            // start a session with this neighbor
            match self.comms.neighbor_session_begin(network, &naddr) {
                Ok(true) => {
                    debug!(
                        "{:?}: Sent pingback handshake to {:?}",
                        network.get_local_peer(),
                        &nk
                    );
                }
                Ok(false) => {
                    debug!(
                        "{:?}: No pingback handshake sent to {:?}; still connecting",
                        network.get_local_peer(),
                        &nk
                    );

                    // try again
                    still_pending.insert(naddr, pingback);
                    continue;
                }
                Err(e) => {
                    debug!(
                        "{:?}: Failed to connect to pingback {:?}: {:?}",
                        network.get_local_peer(),
                        &nk,
                        &e
                    );
                    continue;
                }
            }
        }

        self.network_pingbacks = still_pending;
        if !self.network_pingbacks.is_empty() {
            // still connecting
            debug!(
                "{:?}: Still trying to pingback-handshake with {} neighbors",
                network.get_local_peer(),
                self.network_pingbacks.len()
            );
            return Ok(false);
        }

        // good to go!
        self.set_state(
            network.get_local_peer(),
            NeighborWalkState::PingbackHandshakesFinish,
        )?;
        Ok(true)
    }

    /// Does a given handshakedata represent an expected public key hash?
    fn check_handshake_pubkey_hash(
        nk: &NeighborKey,
        data: &HandshakeAcceptData,
        naddr: &NeighborAddress,
    ) -> bool {
        let neighbor_pubkey_hash =
            Hash160::from_node_public_key_buffer(&data.handshake.node_public_key);
        if neighbor_pubkey_hash != naddr.public_key_hash {
            debug!(
                "Neighbor {:?} had an unexpected pubkey hash: expected {:?} != {:?}",
                nk, &naddr.public_key_hash, &neighbor_pubkey_hash
            );
            return false;
        }

        true
    }

    /// Finish up connecting to newly-discovered inbound peers
    pub fn pingback_handshakes_try_finish(
        &mut self,
        network: &mut PeerNetwork,
    ) -> Result<bool, net_error> {
        assert!(self.state == NeighborWalkState::PingbackHandshakesFinish);

        // see if we got any replies
        for (naddr, message) in self.comms.collect_replies(network).into_iter() {
            // if we got back a HandshakeAccept, and it's on the same chain as us, we're good!
            let (data, db_data) = match message.payload {
                StacksMessageType::HandshakeAccept(ref data) => {
                    debug!("{:?}: received HandshakeAccept from peer {:?}; now known to be routable from us", network.get_local_peer(), &message.to_neighbor_key(&data.handshake.addrbytes, data.handshake.port));
                    (data, None)
                }
                StacksMessageType::StackerDBHandshakeAccept(ref data, ref db_data) => {
                    debug!("{:?}: received StackerDBHandshakeAccept from peer {:?}; now known to be routable from us", network.get_local_peer(), &message.to_neighbor_key(&data.handshake.addrbytes, data.handshake.port));
                    (data, Some(db_data))
                }
                _ => {
                    let nkey = naddr.to_neighbor_key(network);
                    debug!(
                        "{:?}: Neighbor {:?} replied {:?} instead of pingback handshake",
                        network.get_local_peer(),
                        &nkey,
                        &message.get_message_name()
                    );
                    continue;
                }
            };

            let peer_nk = message.to_neighbor_key(&data.handshake.addrbytes, data.handshake.port);
            if !Self::check_handshake_pubkey_hash(&peer_nk, data, &naddr) {
                continue;
            }

            self.neighbor_db.add_or_schedule_replace_neighbor(
                network,
                &message.preamble,
                &data.handshake,
                db_data,
                &mut self.neighbor_replacements,
            )?;
        }

        if self.comms.count_inflight() > 0 {
            debug!(
                "{:?}: Still waiting for pingback-handshake response from {} neighbors",
                network.get_local_peer(),
                self.comms.count_inflight()
            );
            return Ok(false);
        }

        // done!
        self.set_state(
            network.get_local_peer(),
            NeighborWalkState::ReplacedNeighborsPingBegin,
        )?;
        Ok(true)
    }

    /// Ping existing neighbors that would be replaced by the discovery of new neighbors (i.e.
    /// through getting the neighbors of our neighbor, or though pingbacks)
    pub fn ping_existing_neighbors_begin(
        &mut self,
        network: &mut PeerNetwork,
    ) -> Result<bool, net_error> {
        assert!(self.state == NeighborWalkState::ReplacedNeighborsPingBegin);

        let replaced_neighbors =
            mem::replace(&mut self.neighbor_replacements, NeighborReplacements::new());
        for (naddr, _slot) in replaced_neighbors.iter_slots() {
            let nk = naddr.to_neighbor_key(network);
            test_debug!(
                "{:?}: send Handshake to replaceable neighbor {:?}",
                network.get_local_peer(),
                nk
            );

            if let Err(e) = self.comms.neighbor_send(
                network,
                naddr,
                StacksMessageType::Handshake(HandshakeData::from_local_peer(
                    network.get_local_peer(),
                )),
            ) {
                debug!(
                    "{:?}: Not connected to {:?}: ({:?}",
                    network.get_local_peer(),
                    &nk,
                    &e
                );
            }
        }

        // advance state!
        self.set_state(
            network.get_local_peer(),
            NeighborWalkState::ReplacedNeighborsPingFinish,
        )?;
        Ok(true)
    }

    /// Handle a handshake accept for a pinged neighbor.
    /// If it was a StackerDBHandshakeAccept, then also handle the newly-announced DBs
    fn handle_handshake_accept_from_ping(
        &mut self,
        network: &mut PeerNetwork,
        preamble: &Preamble,
        data: &HandshakeAcceptData,
        db_data: Option<&StackerDBHandshakeData>,
    ) -> Result<(), net_error> {
        let neighbor_from_handshake = self
            .neighbor_db
            .save_neighbor_from_handshake(network, preamble, data, db_data)?;
        let naddr = NeighborAddress::from_neighbor(&neighbor_from_handshake);

        // not going to replace
        if self.neighbor_replacements.has_neighbor(&naddr) {
            test_debug!(
                "{:?}: will NOT replace {:?}",
                network.get_local_peer(),
                &neighbor_from_handshake.addr
            );
            self.neighbor_replacements.remove(&naddr);
        }

        Ok(())
    }

    /// try to finish pinging/handshaking all exisitng neighbors.
    /// if the remote neighbor does _not_ respond to our ping, then replace it.
    ///
    /// This is the final step in the state-machine.  It returns the walk result.
    ///
    /// Returns Ok(Some(walk_result)) if the task is completed.
    /// Returns Ok(None) if we're still waiting for network replies
    /// Returns Err(..) on unrecoverable error
    pub fn ping_existing_neighbors_try_finish(
        &mut self,
        network: &mut PeerNetwork,
    ) -> Result<Option<NeighborWalkResult>, net_error> {
        assert!(self.state == NeighborWalkState::ReplacedNeighborsPingFinish);

        for (nkey, message) in self.comms.collect_replies(network).into_iter() {
            let (data, db_data) = match message.payload {
                StacksMessageType::HandshakeAccept(ref data) => {
                    // this peer is still alive -- will not replace it
                    // save knowledge to the peer DB (NOTE: the neighbor should already be in
                    // the DB, since it's cur_neighbor)
                    (data, None)
                }
                StacksMessageType::StackerDBHandshakeAccept(ref data, ref db_data) => {
                    // this peer is still alive -- will not replace it
                    // save knowledge to the peer DB (NOTE: the neighbor should already be in
                    // the DB, since it's cur_neighbor)
                    (data, Some(db_data))
                }
                StacksMessageType::Nack(ref data) => {
                    // evict
                    debug!(
                        "{:?}: Neighbor {:?} NACK'ed Handshake with code {:?}; will evict",
                        network.get_local_peer(),
                        nkey,
                        data.error_code
                    );
                    self.comms.add_broken(network, &nkey);
                    continue;
                }
                _ => {
                    // unexpected reply -- this peer is misbehaving and should be replaced
                    debug!("{:?}: Neighbor {:?} replied an out-of-sequence message (type {}); will replace", network.get_local_peer(), &nkey, message.get_message_name());
                    self.comms.add_broken(network, &nkey);
                    continue;
                }
            };

            debug!(
                "{:?}: Got HandshakeAccept on pingback from {:?}",
                network.get_local_peer(),
                &nkey;
                "handshake_data" => ?data,
                "stackerdb_data" => ?db_data
            );

            self.handle_handshake_accept_from_ping(network, &message.preamble, data, db_data)?;
        }

        if self.comms.count_inflight() > 0 {
            // still pending
            return Ok(None);
        }

        // done getting pings.  do our replacements
        self.neighbor_db.replace_neighbors(
            network,
            &self.neighbor_replacements,
            &mut self.result,
        )?;

        // advance state!
        self.set_state(network.get_local_peer(), NeighborWalkState::Finished)?;

        // calculate the walk result
        if let Some(next_neighbor) = self.next_neighbor.take() {
            // did something useful! return the result
            return Ok(Some(self.reset(
                network.get_local_peer(),
                next_neighbor,
                self.next_walk_outbound,
            )));
        }

        // walk stopped.
        // need to select a random new neighbor (will be outbound)
        // NOTE: this will fail if this peer only has inbound neighbors,
        // and force the walk to restart.
        let next_neighbor = self.neighbor_db.get_next_walk_neighbor(network)?;

        test_debug!(
            "{:?}: Did not step to any neighbor; resetting walk to {:?}",
            network.get_local_peer(),
            &next_neighbor.addr
        );
        Ok(Some(self.reset(
            network.get_local_peer(),
            next_neighbor,
            true,
        )))
    }

    /// Top-level state transition.
    /// Returns Some(walk result) when the state machine completes
    /// Returns None if there's still more work to do
    /// Returns Err(..) if the walk failed and ought to be terminated
    pub fn run(
        &mut self,
        network: &mut PeerNetwork,
    ) -> Result<Option<NeighborWalkResult>, net_error> {
        // synchronize local peer state, in case we learn e.g. the public IP address in the mean
        // time
        let mut can_continue = true;
        while can_continue {
            // a walk times out if it stays in one state for too long
            if self.walk_state_time + self.walk_state_timeout < get_epoch_time_secs() {
                debug!(
                    "{:?}: walk has timed out: stayed in state {:?} for more than {} seconds",
                    network.get_local_peer(),
                    &self.state,
                    self.walk_state_timeout
                );
                return Err(net_error::StepTimeout);
            }

            can_continue = match self.state {
                NeighborWalkState::HandshakeBegin => self.handshake_begin(network)?,
                NeighborWalkState::HandshakeFinish => self.handshake_try_finish(network)?,
                NeighborWalkState::GetNeighborsBegin => self.getneighbors_begin(network)?,
                NeighborWalkState::GetNeighborsFinish => self.getneighbors_try_finish(network)?,
                NeighborWalkState::GetHandshakesBegin => self.neighbor_handshakes_begin(network)?,
                NeighborWalkState::GetHandshakesFinish => {
                    self.neighbor_handshakes_try_finish(network)?
                }
                NeighborWalkState::GetNeighborsNeighborsBegin => {
                    self.getneighbors_neighbors_begin(network)?
                }
                NeighborWalkState::GetNeighborsNeighborsFinish => {
                    self.getneighbors_neighbors_try_finish(network)?
                }
                NeighborWalkState::PingbackHandshakesBegin => {
                    self.pingback_handshakes_begin(network)?
                }
                NeighborWalkState::PingbackHandshakesFinish => {
                    self.pingback_handshakes_try_finish(network)?
                }
                NeighborWalkState::ReplacedNeighborsPingBegin => {
                    self.ping_existing_neighbors_begin(network)?
                }
                NeighborWalkState::ReplacedNeighborsPingFinish => {
                    let walk_result_opt = self.ping_existing_neighbors_try_finish(network)?;
                    if walk_result_opt.is_some() {
                        // did one pass of the state-machine
                        self.walk_step_count += 1;
                        return Ok(walk_result_opt);
                    }

                    // blocked; waiting for more replies
                    false
                }
                NeighborWalkState::Finished => false,
            };
        }

        Ok(None)
    }
}
