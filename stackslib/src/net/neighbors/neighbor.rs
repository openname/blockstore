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

use std::cmp;

use clarity::vm::types::QualifiedContractIdentifier;
use rand::prelude::*;
use rand::thread_rng;
use stacks_common::types::chainstate::StacksPublicKey;
use stacks_common::util::hash::Hash160;
use stacks_common::util::secp256k1::Secp256k1PublicKey;
use stacks_common::util::{get_epoch_time_secs, log};

use crate::burnchains::{Address, PublicKey};
use crate::net::db::PeerDB;
use crate::net::{Error as net_error, Neighbor, NeighborAddress, NeighborKey};
use crate::util_lib::db::{DBConn, DBTx};

/// Walk-specific helper functions for neighbors
impl Neighbor {
    pub fn empty(key: &NeighborKey, pubk: &Secp256k1PublicKey, expire_block: u64) -> Neighbor {
        Neighbor {
            addr: key.clone(),
            public_key: pubk.clone(),
            expire_block,
            last_contact_time: 0,
            allowed: 0,
            denied: 0,
            asn: 0,
            org: 0,
            in_degree: 1,
            out_degree: 1,
        }
    }

    /// Update this peer in the DB.
    /// If there's no DB entry for this peer, then do nothing.
    /// Updates last-contact-time to now, since this is only called when we get back a Handshake
    pub fn save_update(
        &mut self,
        tx: &DBTx<'_>,
        stacker_dbs: Option<&[QualifiedContractIdentifier]>,
    ) -> Result<(), net_error> {
        self.last_contact_time = get_epoch_time_secs();
        PeerDB::update_peer(tx, self).map_err(net_error::DBError)?;
        if let Some(stacker_dbs) = stacker_dbs {
            PeerDB::update_peer_stacker_dbs(tx, self, stacker_dbs).map_err(net_error::DBError)?;
        }
        Ok(())
    }

    /// Save to the peer DB, inserting it if it isn't already there.
    /// Updates last-contact-time to now, since this is only called when we get back a Handshake
    /// Return true if saved.
    /// Return false if not saved -- i.e. the frontier is full and we should try evicting neighbors.
    pub fn save(
        &mut self,
        tx: &DBTx<'_>,
        stacker_dbs: Option<&[QualifiedContractIdentifier]>,
    ) -> Result<bool, net_error> {
        self.last_contact_time = get_epoch_time_secs();
        PeerDB::try_insert_peer(tx, self, stacker_dbs.unwrap_or(&[])).map_err(net_error::DBError)
    }

    /// Attempt to load a neighbor from our peer DB, given its NeighborAddress reported by another
    /// peer.  Returns a neighbor in the peer DB if it matches the neighbor address and has a fresh public key
    /// (where "fresh" means "the public key hash matches the neighbor address")  If the neighbor
    /// is not present in the peer DB, then None will be returned.
    pub fn load_by_address(
        conn: &DBConn,
        network_id: u32,
        block_height: u64,
        neighbor_address: &NeighborAddress,
    ) -> Result<Option<Neighbor>, net_error> {
        let peer_opt = PeerDB::get_peer(
            conn,
            network_id,
            &neighbor_address.addrbytes,
            neighbor_address.port,
        )
        .map_err(net_error::DBError)?;

        match peer_opt {
            None => {
                Ok(None) // unkonwn
            }
            Some(peer) => {
                // expired public key?
                if peer.expire_block < block_height {
                    Ok(None)
                } else {
                    let pubkey_160 = Hash160::from_node_public_key(&peer.public_key);
                    if pubkey_160 == neighbor_address.public_key_hash {
                        // we know this neighbor's key
                        Ok(Some(peer))
                    } else {
                        // this neighbor's key may be stale
                        Ok(None)
                    }
                }
            }
        }
    }

    /// Weighted _undirected_ degree estimate.  This is a random sample between
    /// min(in-degree, out-degree) and max(in-degree, out-degree).
    ///
    /// The reason it's a random sample is as follows.  Any two routable nodes are just as likely
    /// to be inbound neighbors as outbound neighbors, because when one queries the other, the
    /// other queries it back.  The only time when there's a very large, permanent discrepancy
    /// between in-degree and out-degree is when a node has a lot of NAT'ed clients.  In this case,
    /// our node would be given a list of neighbor nodes that it cannot connect to.
    ///
    /// If this were an undirected peer graph, the lower bound of a peer's degree would be
    /// min(in-degree, out-degree), and the upper bound would be max(in-degree, out-degree).
    /// Considering that "P1 points to P2" is just as likely as "P2 points to P1", this means that
    /// Pr["P1 points to P2" | "P2 points to P1"] == Pr["P2 points to P1" | "P1 points to P2"].
    /// So, we can estimate the undirected degree as being a random value between the lower and
    /// upper bound.
    pub fn degree(&self) -> u64 {
        let mut rng = thread_rng();
        let min = cmp::min(self.in_degree, self.out_degree);
        let max = cmp::max(self.in_degree, self.out_degree);
        let res = rng.gen_range(min..(max + 1)) as u64;
        if res == 0 {
            1
        } else {
            res
        }
    }
}
