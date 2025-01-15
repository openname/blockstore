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
use stacks_common::util::hash::Hash160;
use stacks_common::util::{get_epoch_time_secs, log};

use crate::burnchains::{Address, Burnchain, BurnchainView};
use crate::net::db::PeerDB;
use crate::net::neighbors::{NeighborWalkResult, NEIGHBOR_MINIMUM_CONTACT_INTERVAL, NUM_NEIGHBORS};
use crate::net::p2p::PeerNetwork;
use crate::net::{
    Error as net_error, HandshakeAcceptData, HandshakeData, Neighbor, NeighborAddress, NeighborKey,
    Preamble, StackerDBHandshakeData, StacksMessage,
};
use crate::util_lib::db::{DBConn, DBTx};

/// Capture replacement state
#[derive(Debug, Clone, PartialEq)]
pub struct NeighborReplacements {
    /// neighbors to be replaced
    replacements: HashMap<NeighborAddress, Neighbor>,
    /// slots in the peer DB into which a neighbor will be stored if it must be replaced
    replaced_neighbors: HashMap<NeighborAddress, u32>,
}

impl NeighborReplacements {
    pub fn new() -> NeighborReplacements {
        NeighborReplacements {
            replacements: HashMap::new(),
            replaced_neighbors: HashMap::new(),
        }
    }

    pub fn add_neighbor(&mut self, naddr: NeighborAddress, neighbor: Neighbor, slot: u32) {
        self.replacements.insert(naddr.clone(), neighbor);
        self.replaced_neighbors.insert(naddr, slot);
    }

    pub fn get_slot(&self, naddr: &NeighborAddress) -> Option<u32> {
        self.replaced_neighbors.get(naddr).copied()
    }

    pub fn get_neighbor(&self, naddr: &NeighborAddress) -> Option<&Neighbor> {
        self.replacements.get(naddr)
    }

    pub fn iter_slots(&self) -> impl Iterator<Item = (&NeighborAddress, &u32)> {
        self.replaced_neighbors.iter()
    }

    pub fn clear(&mut self) {
        self.replacements.clear();
        self.replaced_neighbors.clear();
    }

    pub fn has_neighbor(&self, naddr: &NeighborAddress) -> bool {
        self.replacements.contains_key(naddr)
    }

    pub fn remove(&mut self, naddr: &NeighborAddress) {
        self.replacements.remove(naddr);
        self.replaced_neighbors.remove(naddr);
    }
}

/// Trait that captures all of the DB I/O that the neighbor walk state machine needs to do
pub trait NeighborWalkDB {
    /// Gets a list of random neighbors to crawl for the purposes of continuing a random neighbor
    /// walk that have been contacted no earlier than the given `last_contact_time`.
    ///
    /// Returns a list of one or more neighbors on success.
    /// Returns NoSuchNeighbor if there are no known neighbors
    /// Returns DBError if there's a problem reading the DB
    fn get_fresh_random_neighbors(
        &self,
        network: &PeerNetwork,
        num_neighbors: u64,
    ) -> Result<Vec<Neighbor>, net_error>;

    /// Get the initial peers for a walk, depending on whether or not we're in IBD.
    /// If we're in IBD, then we have to use the bootstrap nodes.
    fn get_initial_walk_neighbors(
        &self,
        network: &PeerNetwork,
        ibd: bool,
    ) -> Result<Vec<Neighbor>, net_error>;

    /// Find the neighbor addresses and neighbor state that we need to resolve to neighbors,
    /// and find out the neighbor addresses that we already have fresh neighbor data for.
    /// If we know of a neighbor, and contacted it recently, then consider it resolved _even if_
    /// the reported NeighborAddress public key hash doesn't match our records.
    fn lookup_stale_neighbors(
        &self,
        network: &PeerNetwork,
        addrs: &[NeighborAddress],
    ) -> Result<(HashMap<NeighborAddress, Neighbor>, Vec<NeighborAddress>), net_error>;

    /// Add a neighbor to the DB, or if there's no slot available for it, schedule it to be
    /// replaced.  The neighbor info is identified by the handshake message components
    /// (captured in `preamble`, `handshake`, and `db_data`).  If there is no space in the DB for
    /// this neighbor, an _existing_ neighbor is loaded from the DB and added to `replacements`.
    ///
    /// Returns (was-new?, neighbor-record)
    fn add_or_schedule_replace_neighbor(
        &self,
        network: &mut PeerNetwork,
        preamble: &Preamble,
        handshake: &HandshakeData,
        db_data: Option<&StackerDBHandshakeData>,
        replacements: &mut NeighborReplacements,
    ) -> Result<(bool, Neighbor), net_error>;

    /// Is a peer denied?
    fn check_neighbor_denied(
        &self,
        network: &PeerNetwork,
        nk: &NeighborKey,
    ) -> Result<(), net_error>;

    /// Replace a set of neighbors in the peer DB with the given set.
    /// The network result will be updated with a list of replaced neighbors.
    fn replace_neighbors(
        &self,
        network: &mut PeerNetwork,
        replacements: &NeighborReplacements,
        result: &mut NeighborWalkResult,
    ) -> Result<(), net_error>;

    /// Get a neighbor record from a handshake.
    /// If any data for this neighbor exists in the DB already, then load that in as well.
    fn neighbor_from_handshake(
        &self,
        network: &PeerNetwork,
        preamble: &Preamble,
        data: &HandshakeAcceptData,
    ) -> Result<Neighbor, net_error>;

    /// Save a new neighbor to the DB from its handshake data.
    /// Returns the neighbor the handshake data represents.
    fn save_neighbor_from_handshake(
        &self,
        network: &mut PeerNetwork,
        preamble: &Preamble,
        data: &HandshakeAcceptData,
        db_data: Option<&StackerDBHandshakeData>,
    ) -> Result<Neighbor, net_error>;

    /// Update the given neighbor with optional new handshake state and save it to the DB.
    /// Returns the updated neighbor.
    fn update_neighbor(
        &self,
        network: &mut PeerNetwork,
        cur_neighbor: Neighbor,
        new_data: Option<&HandshakeAcceptData>,
        new_db_data: Option<&StackerDBHandshakeData>,
    ) -> Result<Neighbor, net_error>;

    /// Get the number of peers in a given AS
    fn get_asn_count(&self, network: &PeerNetwork, asn: u32) -> u64;

    /// Pick neighbors with a minimum age for a walk.
    /// If there are none, then fall back to seed nodes.
    fn pick_walk_neighbors(
        network: &PeerNetwork,
        num_neighbors: u64,
        min_age: u64,
    ) -> Result<Vec<Neighbor>, net_error> {
        let block_height = network.get_chain_view().burn_block_height;
        let cur_epoch = network.get_current_epoch();
        let neighbors = PeerDB::get_random_walk_neighbors(
            &network.peerdb_conn(),
            network.get_local_peer().network_id,
            cur_epoch.network_epoch,
            min_age,
            num_neighbors as u32,
            block_height,
        )
        .map_err(net_error::DBError)?;

        if neighbors.is_empty() {
            debug!(
                "{:?}: No neighbors available in the peer DB newer than {}!",
                network.get_local_peer(),
                min_age
            );
            let seed_nodes = PeerDB::get_bootstrap_peers(
                &network.peerdb_conn(),
                network.get_local_peer().network_id,
            )?;
            if seed_nodes.is_empty() {
                return Err(net_error::NoSuchNeighbor);
            }
            return Ok(seed_nodes);
        }
        Ok(neighbors)
    }

    /// Get a random starting neighbor for an ongoing walk.
    /// Older but still fresh neighbors will be preferred -- a neighbor from the first 50th
    /// percentile of neighbors (by last contact time) will be selected at random.
    /// Returns the random neighbor on success
    /// Returns NoSuchNeighbor if there are no candidates
    fn get_next_walk_neighbor(&self, network: &PeerNetwork) -> Result<Neighbor, net_error> {
        // pick a random neighbor as a walking point.
        // favor neighbors with older last-contact times
        let next_neighbors_res = self
            .get_fresh_random_neighbors(network, (NUM_NEIGHBORS as u64) * 2)
            .map_err(|e| {
                debug!(
                    "{:?}: Failed to load fresh initial walk neighbors: {:?}",
                    network.get_local_peer(),
                    &e
                );
                e
            });

        let db_neighbors = if let Ok(neighbors) = next_neighbors_res {
            neighbors
        } else {
            let any_neighbors = Self::pick_walk_neighbors(network, (NUM_NEIGHBORS as u64) * 2, 0)
                .map_err(|e| {
                info!(
                    "{:?}: Failed to load any initial walk neighbors: {:?}",
                    network.get_local_peer(),
                    &e
                );
                e
            })?;

            any_neighbors
        };

        let mut next_neighbors: Vec<_> = db_neighbors
            .into_iter()
            .filter_map(|neighbor| {
                if !network.get_connection_opts().private_neighbors
                    && neighbor.addr.addrbytes.is_in_private_range()
                {
                    None
                } else {
                    Some(neighbor)
                }
            })
            .collect();

        if next_neighbors.is_empty() {
            return Err(net_error::NoSuchNeighbor);
        }

        next_neighbors.sort_by(|n1, n2| n1.last_contact_time.cmp(&n2.last_contact_time));
        let median_neighbor_idx = next_neighbors.len() / 2;
        let random_neighbor_idx = if median_neighbor_idx > 0 {
            thread_rng().gen::<usize>() % median_neighbor_idx
        } else {
            0
        };

        Ok(next_neighbors[random_neighbor_idx].clone())
    }
}

/// Production database I/O implementation that uses PeerDB
pub struct PeerDBNeighborWalk {}

/// Database I/O helpers for the NeighborWalkDB implementation
impl PeerDBNeighborWalk {
    pub fn new() -> Self {
        Self {}
    }

    /// Given a neighbor we tried to insert into the peer database, find one of the existing
    /// neighbors it collided with.  Return its slot in the peer db.
    fn find_replaced_neighbor_slot(
        conn: &DBConn,
        nk: &NeighborKey,
    ) -> Result<Option<u32>, net_error> {
        let mut slots = PeerDB::peer_slots(conn, nk.network_id, &nk.addrbytes, nk.port)
            .map_err(net_error::DBError)?;

        if slots.is_empty() {
            // not present
            return Ok(None);
        }

        let mut rng = thread_rng();
        slots.shuffle(&mut rng);
        Ok(Some(slots[0]))
    }
}

impl NeighborWalkDB for PeerDBNeighborWalk {
    fn get_fresh_random_neighbors(
        &self,
        network: &PeerNetwork,
        num_neighbors: u64,
    ) -> Result<Vec<Neighbor>, net_error> {
        let min_age =
            get_epoch_time_secs().saturating_sub(network.connection_opts.max_neighbor_age);
        Self::pick_walk_neighbors(network, num_neighbors, min_age)
    }

    fn lookup_stale_neighbors(
        &self,
        network: &PeerNetwork,
        addrs: &[NeighborAddress],
    ) -> Result<(HashMap<NeighborAddress, Neighbor>, Vec<NeighborAddress>), net_error> {
        let network_id = network.bound_neighbor_key().network_id;
        let block_height = network.get_chain_view().burn_block_height;
        let dbconn = network.peerdb_conn();
        let mut to_resolve = vec![];
        let mut resolved: HashMap<NeighborAddress, Neighbor> = HashMap::new();
        for naddr in addrs {
            let neighbor_opt = Neighbor::load_by_address(dbconn, network_id, block_height, naddr)?;

            if let Some(neighbor) = neighbor_opt {
                // already know about this neighbor, so look at its last contact time
                if neighbor.last_contact_time + NEIGHBOR_MINIMUM_CONTACT_INTERVAL
                    < get_epoch_time_secs()
                {
                    // stale
                    to_resolve.push((*naddr).clone());
                } else {
                    // our copy is still fresh
                    resolved.insert(naddr.clone(), neighbor);
                }
                continue;
            }

            // need to resolve this one, but don't talk to it if we did so recently (even
            // if we have stale information for it -- the remote node could be trying to trick
            // us into DDoS'ing this node).
            let peer_opt = PeerDB::get_peer(dbconn, network_id, &naddr.addrbytes, naddr.port)
                .map_err(net_error::DBError)?;

            if let Some(n) = peer_opt {
                // we know about this neighbor, but its key didn't match the
                // neighboraddress.  Only try to re-connect with it if we haven't done
                // so recently, so a rogue neighbor can't force us to DDoS another
                // peer.
                if n.last_contact_time + NEIGHBOR_MINIMUM_CONTACT_INTERVAL < get_epoch_time_secs() {
                    to_resolve.push((*naddr).clone());
                } else {
                    // recently contacted
                    resolved.insert(naddr.clone(), n);
                }
            } else {
                // okay, we really don't know about this neighbor
                to_resolve.push((*naddr).clone());
            }
        }
        Ok((resolved, to_resolve))
    }

    fn add_or_schedule_replace_neighbor(
        &self,
        network: &mut PeerNetwork,
        preamble: &Preamble,
        handshake: &HandshakeData,
        db_data: Option<&StackerDBHandshakeData>,
        replacements: &mut NeighborReplacements,
    ) -> Result<(bool, Neighbor), net_error> {
        let local_peer_str = format!("{:?}", network.get_local_peer());
        let tx = network.peerdb_tx_begin()?;
        let (mut neighbor_from_handshake, was_present) =
            Neighbor::load_and_update(&tx, preamble.peer_version, preamble.network_id, handshake)?;

        if was_present {
            test_debug!(
                "{}: already know about neighbor {:?}",
                &local_peer_str,
                &neighbor_from_handshake.addr
            );
            neighbor_from_handshake
                .save_update(&tx, db_data.map(|x| x.smart_contracts.as_slice()))?;
            tx.commit()?;

            // seen this neighbor before
            return Ok((false, neighbor_from_handshake));
        }

        debug!(
            "{}: new neighbor {:?}",
            &local_peer_str, &neighbor_from_handshake.addr
        );

        // didn't know about this neighbor yet. Try to add it.
        let added =
            neighbor_from_handshake.save(&tx, db_data.map(|x| x.smart_contracts.as_slice()))?;

        if added {
            // neighbor was new, and we had space to add it.
            tx.commit()?;
            return Ok((true, neighbor_from_handshake));
        }

        // neighbor was new, but we don't have space to insert it.
        // find and record a neighbor it would replace.
        let replaced_neighbor_slot_opt =
            Self::find_replaced_neighbor_slot(&tx, &neighbor_from_handshake.addr)?;
        if let Some(slot) = replaced_neighbor_slot_opt {
            replacements.add_neighbor(
                NeighborAddress::from_neighbor(&neighbor_from_handshake),
                neighbor_from_handshake.clone(),
                slot,
            );
        }

        tx.commit()?;

        // neighbor was new
        Ok((true, neighbor_from_handshake))
    }

    fn get_initial_walk_neighbors(
        &self,
        network: &PeerNetwork,
        ibd: bool,
    ) -> Result<Vec<Neighbor>, net_error> {
        let allowed_peers = if ibd {
            // only get bootstrap peers (will be randomized)
            PeerDB::get_bootstrap_peers(
                &network.peerdb_conn(),
                network.get_local_peer().network_id,
            )?
        } else {
            // can be any peer marked 'always-allowed' (will be randomized)
            PeerDB::get_always_allowed_peers(
                network.peerdb_conn(),
                network.get_local_peer().network_id,
            )?
        };
        Ok(allowed_peers)
    }

    fn check_neighbor_denied(
        &self,
        network: &PeerNetwork,
        nk: &NeighborKey,
    ) -> Result<(), net_error> {
        // don't proceed if denied
        if PeerDB::is_peer_denied(
            &network.peerdb_conn(),
            nk.network_id,
            &nk.addrbytes,
            nk.port,
        )? {
            debug!(
                "{:?}: neighbor {:?} is denied",
                network.get_local_peer(),
                nk
            );
            return Err(net_error::Denied);
        }
        Ok(())
    }

    fn replace_neighbors(
        &self,
        network: &mut PeerNetwork,
        replacements: &NeighborReplacements,
        result: &mut NeighborWalkResult,
    ) -> Result<(), net_error> {
        let network_id = network.bound_neighbor_key().network_id;
        let local_peer_str = format!("{:?}", network.get_local_peer());

        let tx = network.peerdb_tx_begin()?;
        for (replaceable_naddr, slot) in replacements.iter_slots() {
            let replacement = match replacements.get_neighbor(replaceable_naddr) {
                Some(n) => n,
                None => {
                    continue;
                }
            };

            let replaced_opt = PeerDB::get_peer_at(&tx, network_id, *slot)?;
            if let Some(replaced) = replaced_opt {
                if PeerDB::is_address_denied(&tx, &replacement.addr.addrbytes)? {
                    debug!(
                        "{:?}: Will not replace {:?} with {:?} -- is denied",
                        local_peer_str, &replaced.addr, &replacement.addr
                    );
                    continue;
                }
                debug!(
                    "{:?}: Replace {:?} with {:?}",
                    local_peer_str, &replaced.addr, &replacement.addr
                );

                PeerDB::insert_or_replace_peer(&tx, &replacement, *slot)?;
                result.add_replaced(replaced.addr.clone());
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn neighbor_from_handshake(
        &self,
        network: &PeerNetwork,
        preamble: &Preamble,
        data: &HandshakeAcceptData,
    ) -> Result<Neighbor, net_error> {
        Neighbor::load_and_update(
            &network.peerdb_conn(),
            preamble.peer_version,
            preamble.network_id,
            &data.handshake,
        )
        .map(|(neighbor, _)| neighbor)
    }

    fn save_neighbor_from_handshake(
        &self,
        network: &mut PeerNetwork,
        preamble: &Preamble,
        data: &HandshakeAcceptData,
        db_data: Option<&StackerDBHandshakeData>,
    ) -> Result<Neighbor, net_error> {
        let tx = network.peerdb_tx_begin()?;
        let (mut neighbor_from_handshake, _) = Neighbor::load_and_update(
            &tx,
            preamble.peer_version,
            preamble.network_id,
            &data.handshake,
        )?;
        neighbor_from_handshake.save_update(&tx, db_data.map(|x| x.smart_contracts.as_slice()))?;
        tx.commit()?;
        Ok(neighbor_from_handshake)
    }

    fn update_neighbor(
        &self,
        network: &mut PeerNetwork,
        mut cur_neighbor: Neighbor,
        new_data: Option<&HandshakeAcceptData>,
        new_db_data: Option<&StackerDBHandshakeData>,
    ) -> Result<Neighbor, net_error> {
        let tx = network.peerdb_tx_begin()?;

        if let Some(data) = new_data {
            cur_neighbor.handshake_update(&tx, &data.handshake)?;
        }

        if let Some(db_data) = new_db_data {
            cur_neighbor.save_update(&tx, Some(db_data.smart_contracts.as_slice()))?;
        } else {
            cur_neighbor.save_update(&tx, None)?;
        }

        tx.commit()?;
        Ok(cur_neighbor)
    }

    fn get_asn_count(&self, network: &PeerNetwork, asn: u32) -> u64 {
        PeerDB::asn_count(network.peerdb_conn(), asn).unwrap_or(1)
    }
}
