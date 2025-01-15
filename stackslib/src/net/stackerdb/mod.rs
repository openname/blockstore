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

/// # The StackerDB System
///
/// A StackerDB is a best-effort replicated database controlled by a smart contract, which Stacks
/// node operators can opt-in to hosting.  Unlike a smart contract's data space, a StackerDB's
/// data is not consensus-critical -- nodes do not need to read its state to validate the
/// blockchain.  Instead, developers use StackerDBSet to host and replicate auxiliary smart contract
/// data for the purposes of some (off-chain) application in a best-effort manner.  In doing so,
/// Stacks-powered applications are able to leverage the Stacks peer-to-peer node network to host
/// and disseminate their data without incuring the cost and performance penalties of bundling it
/// within a transaction.
///
/// ## Data Model
///
/// Data within a StackerDB is eventually-consistent.  In the absence of writes and network
/// partitions, all replicas will receive the latest data in a finite number of protocol rounds,
/// with high probability.  Given that network partitions in the peer-to-peer network are assumed
/// to be temporary, we assume that all StackerDB instances will receive the latest state in finite time.
/// Beyond this, it makes no guarantees about how quickly a write will materialize on a given replica.
///
/// The StackerDB schema is chunk-oriented.  Each StackerDB contains a fixed number of bound-size
/// _slots_, each of which contain one _chunk_.  Slots are array-indexed, and a slot may have zero
/// or one chunk.
///
/// A `write` to a StackerDB is the act of replacing one slot's chunk with new data, and a
/// `read` on a StackerDB is the act of loading one slot's chunk from the node's local replica.  Reading
/// and writing a single slot on one node is atomic.  StackerDB replication proceeds in a
/// store-and-forward manner -- newly-discovered chunks are stored to the node's local replica and
/// broadcast to a subset of neighbors who also replicate the given StackerDB.
///
/// Each slot has an associated Lamport clock, and an associated public key hash used to
/// authenticate writes.  The Lamport clock is used to identify the latest version of a slot's
/// chunk -- a node will replace an existing but stale copy of a chunk with a new chunk if its
/// Lamport clock has a strictly higher value.  The slot's metadata -- its ID, Lamport clock, and
/// data hash -- must be signed by the slot's public key hash's associated private key in order to
/// be stored.  The chunks themselves are ordered byte sequences with no mandatory internal
/// structure.
///
/// StackerDB state is ephemeral.  Chunk eviction is controlled by the smart contract.  At every
/// Bitcoin block, the node queries the smart contract for a list of slots to clear.
///
/// ## Control Plane
///
/// The smart contract to which a StackerDB is bound controls how many slots the DB has, who can
/// write to which slots (identified by public key hash), how big a slot is, and how often a
/// slot can be written to (in wall-clock time).  This smart contract is queried once per reward cycle
/// in order to configure the database.
///
/// Applications that employ StackerDBSet would deploy one or more smart contracts that list out
/// which users can store data to the StackerDB replica, and how much space they get.
///
/// ## Replication Protocol
///
/// StackerDB replication proceeds in a three-part protocol: discovery, inventory query, and
/// chunk exchange.  The discovery protocol leverages the Stacks node's neighbor-walk algorithm to
/// discover which StackerDBSet other nodes claim to replicate.  On receipt of a `Handshake` message,
/// a StackerDB-aware node replies with a `StackerDBHandshakeAccept` message which encodes both the
/// contents of a `HandshakeAccept` message as well as a list of local StackerDBSet (identified by
/// their smart contracts' addresses).  Upon receipt of a `StackerDBHandshakeAccept`, the node
/// stores the list of smart contracts in its `PeerDB` as part of the network frontier state.  In
/// doing so, nodes eventually learn of all of the StackerDBSet replicated by all other nodes.  To
/// bound the size of this state, the protocol mandates that a node can only replicate up to 256
/// StackerDBSet.  The handshake-handling code happens in net::chat::handle_handshake().
///
/// When a node begins to replicate a StackerDB, it first queries the `PeerDB` for the set of nodes
/// that claim to have copies.  This set, called the "DB neighbors", is distinct from the set
/// of neighbors the node uses to replicate blocks and transactions.  It then connects
/// to these nodes with a `Handshake` / `StackerDBHandshakeAccept` exchange (if the neighbor walk
/// has not done so already), and proceeds to query each DB's inventories by sending them
/// `StackerDBGetChunkInData` messages.
///
/// The DB inventory (`StackerDBChunkInvData`) is simply a vector of all of the remote peers' slots' versions.
/// Once the node has received all DB inventories from its neighbors, it schedules them for
/// download by prioritizing them by newest-first, and then by rarest-first, in order to ensure
/// that the latest, least-replicated data is downloaded first.
///
/// Once the node has computed its download schedule, it queries its DB neighbors for chunks with
/// the given versions (via `StackerDBGetChunkData`).  Upon receipt of a chunk, the node verifies the signature on the chunk's
/// metadata (via `SlotMetadata`), verifies that the chunk data hashes to the metadata's indicated data hash, and stores
/// the chunk (via `StackerDBSet` and `StackerDBTx`).  It will then select neighbors to which to broadcast this chunk, inferring from the
/// download schedule which DB neighbors have yet to process this particular version of the chunk.
///
/// ## Comparison to other Stacks storage
///
/// StackerDBSet differ from AtlasDBs in that data chunks are not authenticated by the blockchain,
/// but instead are authenticated by public key hashes made available from a smart contract.  As
/// such, a node can begin replicating a StackerDB whenever its operator wants -- it does not need
/// to re-synchronize blockchain state to get the list of chunk hashes.  Furthermore, StackerDB
/// state can be written to as fast as the smart contract permits -- there is no need to wait for a
/// corresponding transaction to confirm.
///
/// StackerDBSet differ from Gaia in that Stacks nodes are the principal means of storing data.  Any
/// reachable Stacks node can fulfill requests for chunks.  It is up to the StackerDB maintainer to
/// convince node operators to replicate StackerDBSet on their behalf.  In addition, StackerDB state
/// is ephemeral -- its longevity in the system depends on application endpoints re-replicating the
/// state periodically (whereas Gaia stores data for as long as the back-end storage provider's SLA
/// indicates).

#[cfg(test)]
pub mod tests;

pub mod config;
pub mod db;
pub mod sync;

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use clarity::vm::types::QualifiedContractIdentifier;
use libstackerdb::{SlotMetadata, STACKERDB_MAX_CHUNK_SIZE};
use stacks_common::consts::SIGNER_SLOTS_PER_USER;
use stacks_common::types::chainstate::{ConsensusHash, StacksAddress};
use stacks_common::util::get_epoch_time_secs;
use stacks_common::util::hash::Sha512Trunc256Sum;
use stacks_common::util::secp256k1::MessageSignature;

use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::nakamoto::NakamotoChainState;
use crate::chainstate::stacks::boot::MINERS_NAME;
use crate::chainstate::stacks::db::StacksChainState;
use crate::net::connection::ConnectionOptions;
use crate::net::neighbors::NeighborComms;
use crate::net::p2p::PeerNetwork;
use crate::net::{
    Error as net_error, NackData, NackErrorCodes, Neighbor, NeighborAddress, NeighborKey, Preamble,
    StackerDBChunkData, StackerDBChunkInvData, StackerDBGetChunkData, StackerDBPushChunkData,
    StacksMessage, StacksMessageType,
};
use crate::util_lib::boot::boot_code_id;
use crate::util_lib::db::{DBConn, DBTx, Error as db_error};

/// maximum chunk inventory size
pub const STACKERDB_INV_MAX: u32 = 4096;
/// maximum length of an inventory page's Clarity list
pub const STACKERDB_PAGE_LIST_MAX: u32 = 4096;
/// maximum number of pages that can be used in a StackerDB contract
pub const STACKERDB_MAX_PAGE_COUNT: u32 = 2;

pub const STACKERDB_SLOTS_FUNCTION: &str = "stackerdb-get-signer-slots";
pub const STACKERDB_CONFIG_FUNCTION: &str = "stackerdb-get-config";
pub const MINER_SLOT_COUNT: u32 = 2;

/// Final result of synchronizing state with a remote set of DB replicas
#[derive(Clone, PartialEq, Debug)]
pub struct StackerDBSyncResult {
    /// which contract this is a replica for
    pub contract_id: QualifiedContractIdentifier,
    /// slot inventory for this replica
    pub chunk_invs: HashMap<NeighborAddress, StackerDBChunkInvData>,
    /// list of data to store
    pub chunks_to_store: Vec<StackerDBChunkData>,
    /// neighbors that have stale views, but are otherwise online
    pub(crate) stale: HashSet<NeighborAddress>,
    /// number of connections made
    pub num_connections: u64,
    /// number of attempted connections
    pub num_attempted_connections: u64,
}

/// Settings for the Stacker DB
#[derive(Clone, Debug, PartialEq)]
pub struct StackerDBConfig {
    /// maximum chunk size
    pub chunk_size: u64,
    /// list of who writes and how many slots they have
    pub signers: Vec<(StacksAddress, u32)>,
    /// minimum wall-clock time between writes to the same slot.
    pub write_freq: u64,
    /// maximum number of times a slot may be written to during a reward cycle.
    pub max_writes: u32,
    /// hint for some initial peers that have replicas of this DB
    pub hint_replicas: Vec<NeighborAddress>,
    /// hint for how many neighbors to connect to
    pub max_neighbors: usize,
}

impl StackerDBConfig {
    /// Config that does nothing
    pub fn noop() -> StackerDBConfig {
        StackerDBConfig {
            chunk_size: u64::MAX,
            write_freq: 0,
            max_writes: u32::MAX,
            hint_replicas: vec![],
            max_neighbors: 8,
            signers: vec![],
        }
    }

    /// How many slots are in this DB total?
    #[cfg_attr(test, mutants::skip)]
    pub fn num_slots(&self) -> u32 {
        self.signers.iter().fold(0, |acc, s| acc + s.1)
    }

    /// What are the slot index ranges for each signer?
    /// Returns the ranges in the same ordering as `self.signers`
    pub fn signer_ranges(&self) -> Vec<Range<u32>> {
        let mut slot_index = 0;
        let mut result = Vec::with_capacity(self.signers.len());
        for (_, slot_count) in self.signers.iter() {
            let end = slot_index + *slot_count;
            result.push(Range {
                start: slot_index,
                end,
            });
            slot_index = end;
        }
        result
    }
}

/// This is the set of replicated chunks in all stacker DBs that this node subscribes to.
///
/// Callers can query chunks from individual stacker DBs by supplying the smart contract address.
pub struct StackerDBs {
    conn: DBConn,
    path: String,
}

impl StackerDBs {
    /// Create a StackerDB.
    /// Fails only if the underlying DB fails
    fn create_stackerdb(
        &mut self,
        stackerdb_contract_id: &QualifiedContractIdentifier,
        new_config: &StackerDBConfig,
    ) -> Result<(), db_error> {
        info!("Creating local replica of StackerDB {stackerdb_contract_id}");
        test_debug!(
            "Creating local replica of StackerDB {stackerdb_contract_id} with config {:?}",
            &new_config
        );
        let tx = self.tx_begin(new_config.clone())?;
        tx.create_stackerdb(stackerdb_contract_id, &new_config.signers)
            .unwrap_or_else(|e| {
                warn!(
                    "Failed to create StackerDB replica {stackerdb_contract_id}: {:?}",
                    &e
                );
            });
        tx.commit()?;
        Ok(())
    }

    /// Reconfigure a StackerDB.
    /// Fails only if the underlying DB fails
    fn reconfigure_stackerdb(
        &mut self,
        stackerdb_contract_id: &QualifiedContractIdentifier,
        new_config: &StackerDBConfig,
    ) -> Result<(), db_error> {
        debug!("Reconfiguring StackerDB {stackerdb_contract_id}...");
        let tx = self.tx_begin(new_config.clone())?;
        tx.reconfigure_stackerdb(stackerdb_contract_id, &new_config.signers)
            .unwrap_or_else(|e| {
                warn!(
                    "Failed to reconfigure StackerDB replica {}: {:?}",
                    stackerdb_contract_id, &e
                );
            });
        tx.commit()?;
        Ok(())
    }

    /// Create or reconfigure the supplied contracts with the appropriate stacker DB config.
    /// Returns a map of the stacker DBs and their loaded configs.
    /// Fails only if the underlying DB fails
    pub fn create_or_reconfigure_stackerdbs(
        &mut self,
        chainstate: &mut StacksChainState,
        sortdb: &SortitionDB,
        stacker_db_configs: HashMap<QualifiedContractIdentifier, StackerDBConfig>,
        connection_opts: &ConnectionOptions,
    ) -> Result<HashMap<QualifiedContractIdentifier, StackerDBConfig>, net_error> {
        let num_neighbors = connection_opts.num_neighbors;
        let existing_contract_ids = self.get_stackerdb_contract_ids()?;
        let mut new_stackerdb_configs = HashMap::new();
        let tip = SortitionDB::get_canonical_burn_chain_tip(sortdb.conn())?;

        for (stackerdb_contract_id, stackerdb_config) in stacker_db_configs.into_iter() {
            // Determine the new config for this StackerDB replica
            let new_config = if stackerdb_contract_id
                == boot_code_id(MINERS_NAME, chainstate.mainnet)
            {
                // .miners contract -- directly generate the config
                NakamotoChainState::make_miners_stackerdb_config(sortdb, &tip)
                    .map(|(config, _)| config)
                    .unwrap_or_else(|e| {
                        warn!(
                            "Failed to generate .miners StackerDB config";
                            "contract" => %stackerdb_contract_id,
                            "err" => ?e,
                        );
                        StackerDBConfig::noop()
                    })
            } else {
                // attempt to load the config from the contract itself
                StackerDBConfig::from_smart_contract(
                    chainstate,
                    sortdb,
                    &stackerdb_contract_id,
                    num_neighbors,
                    connection_opts
                        .stackerdb_hint_replicas
                        .get(&stackerdb_contract_id)
                        .cloned(),
                )
                .unwrap_or_else(|e| {
                    if matches!(e, net_error::NoSuchStackerDB(_)) && stackerdb_contract_id.is_boot()
                    {
                        debug!(
                            "Failed to load StackerDB config";
                            "contract" => %stackerdb_contract_id,
                            "err" => ?e,
                        );
                    } else {
                        warn!(
                            "Failed to load StackerDB config";
                            "contract" => %stackerdb_contract_id,
                            "err" => ?e,
                        );
                    }
                    StackerDBConfig::noop()
                })
            };
            // Create the StackerDB replica if it does not exist already
            if !existing_contract_ids.contains(&stackerdb_contract_id) {
                if let Err(e) = self.create_stackerdb(&stackerdb_contract_id, &new_config) {
                    warn!(
                        "Failed to create or reconfigure StackerDB {stackerdb_contract_id}: DB error {:?}",
                        &e
                    );
                }
            } else if (new_config != stackerdb_config && !new_config.signers.is_empty())
                || (new_config == stackerdb_config
                    && new_config.signers.len()
                        != self.get_slot_versions(&stackerdb_contract_id)?.len())
            {
                // only reconfigure if the config has changed
                // (that second check on the length is needed in case the node is a victim of
                // #5142, which was a bug whereby a stackerdb could never shrink)
                if let Err(e) = self.reconfigure_stackerdb(&stackerdb_contract_id, &new_config) {
                    warn!(
                        "Failed to create or reconfigure StackerDB {stackerdb_contract_id}: DB error {:?}",
                        &e
                    );
                }
            }
            // Even if we failed to create or reconfigure the DB, we still want to keep track of them
            // so that we can attempt to create/reconfigure them again later.
            debug!("Reloaded configuration for {}", &stackerdb_contract_id);
            new_stackerdb_configs.insert(stackerdb_contract_id, new_config);
        }
        Ok(new_stackerdb_configs)
    }
}
/// A transaction against one or more stacker DBs (really, against StackerDBSet)
pub struct StackerDBTx<'a> {
    sql_tx: DBTx<'a>,
    config: StackerDBConfig,
}

/// Possible states a DB sync state-machine can be in
#[derive(Debug)]
pub enum StackerDBSyncState {
    ConnectBegin,
    ConnectFinish,
    GetChunksInvBegin,
    GetChunksInvFinish,
    GetChunks,
    PushChunks,
    Finished,
}

/// Set of peers for a stacker DB
pub struct StackerDBSync<NC: NeighborComms> {
    /// what state are we in?
    state: StackerDBSyncState,
    /// What was the rc consensus hash at the start of sync?
    pub rc_consensus_hash: Option<ConsensusHash>,
    /// which contract this is a replica for
    pub smart_contract_id: QualifiedContractIdentifier,
    /// number of chunks in this DB
    pub num_slots: usize,
    /// how frequently we accept chunk writes, in seconds
    pub write_freq: u64,
    /// What versions of each chunk does each neighbor have?
    pub chunk_invs: HashMap<NeighborAddress, StackerDBChunkInvData>,
    /// What priority should we be fetching chunks in, and from whom?
    pub chunk_fetch_priorities: Vec<(StackerDBGetChunkData, Vec<NeighborAddress>)>,
    /// What priority should we be pushing chunks in, and to whom?
    pub chunk_push_priorities: Vec<(StackerDBPushChunkData, Vec<NeighborAddress>)>,
    /// ID and version of chunk we pushed
    pub(crate) chunk_push_receipts: HashMap<NeighborAddress, (u32, u32)>,
    /// Index into `chunk_fetch_priorities` at which to consider the next download.
    pub next_chunk_fetch_priority: usize,
    /// Index into `chunk_push_priorities` at which to consider the next chunk push.
    pub next_chunk_push_priority: usize,
    /// What is the expected version vector for this DB's chunks?
    pub expected_versions: Vec<u32>,
    /// Downloaded chunks
    pub downloaded_chunks: HashMap<NeighborAddress, Vec<StackerDBChunkData>>,
    /// Replicas to contact
    pub(crate) replicas: HashSet<NeighborAddress>,
    /// Replicas that have connected
    pub(crate) connected_replicas: HashSet<NeighborAddress>,
    /// Comms with neigbors
    pub(crate) comms: NC,
    /// Handle to StackerDBs
    pub(crate) stackerdbs: StackerDBs,
    /// maximum number of inflight requests
    pub(crate) request_capacity: usize,
    /// maximum number of peers
    pub(crate) max_neighbors: usize,
    /// total chunks stored
    pub total_stored: u64,
    /// total chunks pushed
    pub total_pushed: u64,
    /// last time the state-transition function ran to completion
    last_run_ts: u64,
    /// whether or not we should immediately re-fetch chunks because we learned about new chunks
    /// from our peers when they replied to our chunk-pushes with new inventory state
    need_resync: bool,
    /// whether or not the fetched inventory was determined to be stale
    stale_inv: bool,
    /// Track stale neighbors
    pub(crate) stale_neighbors: HashSet<NeighborAddress>,
    /// How many attempted connections have been made in the last pass (gets reset)
    num_attempted_connections: u64,
    /// How many connections have been made in the last pass (gets reset)
    num_connections: u64,
    /// Number of state machine passes
    rounds: u128,
    /// Round when we last pushed
    push_round: u128,
    /// time we last deliberately evicted a peer
    last_eviction_time: u64,
}

impl StackerDBSyncResult {
    /// The receipt of a single StackerDBPushChunk message is equivalent to performing a single
    /// sync
    pub fn from_pushed_chunk(chunk: StackerDBPushChunkData) -> StackerDBSyncResult {
        StackerDBSyncResult {
            contract_id: chunk.contract_id,
            chunk_invs: HashMap::new(),
            chunks_to_store: vec![chunk.chunk_data],
            stale: HashSet::new(),
            num_attempted_connections: 0,
            num_connections: 0,
        }
    }
}

/// Event dispatcher trait for pushing out new chunk arrival info
pub trait StackerDBEventDispatcher {
    /// A set of one or more chunks has been obtained by this replica
    fn new_stackerdb_chunks(
        &self,
        contract_id: QualifiedContractIdentifier,
        chunk_info: Vec<StackerDBChunkData>,
    );
}

impl PeerNetwork {
    /// Run all stacker DB sync state-machines.
    /// Return a list of sync results on success, to be incorporated into the NetworkResult.
    /// Return an error on unrecoverable DB or network error
    pub fn run_stacker_db_sync(&mut self) -> Result<Vec<StackerDBSyncResult>, net_error> {
        let mut results = vec![];
        let mut stacker_db_syncs = self
            .stacker_db_syncs
            .take()
            .expect("FATAL: did not replace stacker dbs");
        let stacker_db_configs = self.stacker_db_configs.clone();

        for (sc, stacker_db_sync) in stacker_db_syncs.iter_mut() {
            if let Some(config) = stacker_db_configs.get(sc) {
                match stacker_db_sync.run(self, config) {
                    Ok(Some(result)) => {
                        results.push(result);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        info!(
                            "Failed to run StackerDB state machine for {}: {:?}",
                            &sc, &e
                        );
                        stacker_db_sync.reset(Some(self), config);
                    }
                }
            } else {
                info!("No stacker DB config for {}", &sc);
            }
        }
        self.stacker_db_syncs = Some(stacker_db_syncs);
        Ok(results)
    }

    /// Create a StackerDBChunksInv, or a Nack if the requested DB isn't replicated here.
    /// Runs in response to a received StackerDBGetChunksInv or a StackerDBPushChunk
    pub fn make_StackerDBChunksInv_or_Nack(
        &self,
        naddr: NeighborAddress,
        chainstate: &mut StacksChainState,
        contract_id: &QualifiedContractIdentifier,
        rc_consensus_hash: &ConsensusHash,
    ) -> StacksMessageType {
        // N.B. check that the DB exists first, since we want to report StaleView only if the DB
        // exists
        let slot_versions = match self.stackerdbs.get_slot_versions(contract_id) {
            Ok(versions) => versions,
            Err(e) => {
                debug!(
                    "{:?}: failed to get chunk versions for {}: {:?}",
                    self.get_local_peer(),
                    contract_id,
                    &e
                );

                // most likely indicates that this DB doesn't exist
                return StacksMessageType::Nack(NackData::new(NackErrorCodes::NoSuchDB));
            }
        };

        // this DB exists, but is the view of this message recent?
        if &self.get_chain_view().rc_consensus_hash != rc_consensus_hash {
            // is there a Stacks block (or tenure) with this consensus hash?
            let tip_block_id = self.stacks_tip.block_id();
            if let Ok(Some(_)) = NakamotoChainState::get_tenure_start_block_header(
                &mut chainstate.index_conn(),
                &tip_block_id,
                rc_consensus_hash,
            ) {
                debug!("{:?}: NACK StackerDBGetChunksInv / StackerDBPushChunk from {} since {} != {} (remote is stale)", self.get_local_peer(), &naddr, &self.get_chain_view().rc_consensus_hash, rc_consensus_hash);
                return StacksMessageType::Nack(NackData::new(NackErrorCodes::StaleView));
            } else {
                debug!("{:?}: NACK StackerDBGetChunksInv / StackerDBPushChunk from {} since {} != {} (local is potentially stale)", self.get_local_peer(), &naddr, &self.get_chain_view().rc_consensus_hash, rc_consensus_hash);
                return StacksMessageType::Nack(NackData::new(NackErrorCodes::FutureView));
            }
        }

        let num_outbound_replicas = self.count_outbound_stackerdb_replicas(contract_id) as u32;

        debug!(
            "{:?}: inventory for {} has {} outbound replicas; versions are {:?}",
            self.get_local_peer(),
            contract_id,
            num_outbound_replicas,
            &slot_versions
        );
        StacksMessageType::StackerDBChunkInv(StackerDBChunkInvData {
            slot_versions,
            num_outbound_replicas,
        })
    }

    /// Validate chunk data -- either pushed to us, or downloaded.
    /// NOTE: does not check write frequency, since the caller has different ways of doing this.
    /// Returns Ok(true) if the chunk is valid
    /// Returns Ok(false) if the chunk is invalid
    /// Returns Err(..) on DB error
    pub fn validate_received_chunk(
        &self,
        smart_contract_id: &QualifiedContractIdentifier,
        config: &StackerDBConfig,
        data: &StackerDBChunkData,
        expected_versions: &[u32],
    ) -> Result<bool, net_error> {
        // validate -- must be a valid chunk
        if data.slot_id >= (expected_versions.len() as u32) {
            info!(
                "Received StackerDBChunk for {} ID {}, which is too big ({})",
                smart_contract_id,
                data.slot_id,
                expected_versions.len()
            );
            return Ok(false);
        }

        // validate -- must be signed by the expected author
        let addr = match self
            .stackerdbs
            .get_slot_signer(smart_contract_id, data.slot_id)?
        {
            Some(addr) => addr,
            None => {
                return Ok(false);
            }
        };

        let slot_metadata = data.get_slot_metadata();
        if !slot_metadata.verify(&addr)? {
            info!(
                "StackerDBChunk for {} ID {} is not signed by {}",
                smart_contract_id, data.slot_id, &addr
            );
            return Ok(false);
        }

        // validate -- must be the current or newer version
        let slot_idx = data.slot_id as usize;
        if data.slot_version < expected_versions[slot_idx] {
            info!(
                "Received StackerDBChunk for {} ID {} version {}, which is stale (expected {})",
                smart_contract_id, data.slot_id, data.slot_version, expected_versions[slot_idx]
            );
            return Ok(false);
        }

        // validate -- must not exceed max writes
        if data.slot_version > config.max_writes {
            info!(
                "Write count exceeded for StackerDBChunk for {} ID {} version {} (max is {})",
                smart_contract_id, data.slot_id, data.slot_version, config.max_writes
            );
            return Ok(false);
        }

        Ok(true)
    }

    /// Handle unsolicited StackerDBPushChunk messages.
    /// Check to see that the message can be stored or buffered.
    ///
    /// Optionally, make a reply handle for a StackerDBChunksInv to be sent to the remote peer, in which
    /// the inventory vector is updated with this chunk's data.  Or, send a NACK if the chunk
    /// cannot be buffered or stored.
    ///
    /// Note that this can happen *during* a StackerDB sync's execution, so be very careful about
    /// modifying a state machine's contents!  The only modification possible here is to wakeup
    /// the state machine in case it's asleep (i.e. blocked on waiting for the next sync round).
    ///
    /// The write frequency is not checked for this chunk. This is because the `ConversationP2P` on
    /// which this chunk arrived will have already bandwidth-throttled the remote peer, and because
    /// messages can be arbitrarily delayed (and bunched up) by the network anyway.
    ///
    /// Returns (true, x) if we should buffer the message and try processing it again later.
    /// Returns (false, x) if we should *not* buffer this message, because it either *won't* be valid
    /// later, or if it can be stored right now.
    ///
    /// Returns (x, true) if we should forward the message to the relayer, so it can be processed.
    /// Returns (x, false) if we should *not* forward the message to the relayer, because it will
    /// *not* be processed.
    pub fn handle_unsolicited_StackerDBPushChunk(
        &mut self,
        chainstate: &mut StacksChainState,
        event_id: usize,
        preamble: &Preamble,
        chunk_data: &StackerDBPushChunkData,
        send_reply: bool,
    ) -> Result<(bool, bool), net_error> {
        let Some(naddr) = self
            .get_p2p_convo(event_id)
            .map(|convo| convo.to_neighbor_address())
        else {
            debug!(
                "Drop unsolicited StackerDBPushChunk: event ID {} is not connected",
                event_id
            );
            return Ok((false, false));
        };

        let mut payload = self.make_StackerDBChunksInv_or_Nack(
            naddr,
            chainstate,
            &chunk_data.contract_id,
            &chunk_data.rc_consensus_hash,
        );
        match payload {
            StacksMessageType::StackerDBChunkInv(ref mut data) => {
                // this message corresponds to an existing DB, and comes from the same view of the
                // stacks chain tip
                let stackerdb_config = if let Some(config) =
                    self.get_stacker_db_configs().get(&chunk_data.contract_id)
                {
                    config
                } else {
                    // not for this DB
                    info!(
                        "StackerDBChunk for {} ID {} is not available locally",
                        &chunk_data.contract_id, chunk_data.chunk_data.slot_id
                    );
                    return Ok((false, false));
                };

                // sanity check
                if !self.validate_received_chunk(
                    &chunk_data.contract_id,
                    stackerdb_config,
                    &chunk_data.chunk_data,
                    &data.slot_versions,
                )? {
                    return Ok((false, false));
                }

                // patch inventory -- we'll accept this chunk
                data.slot_versions[chunk_data.chunk_data.slot_id as usize] =
                    chunk_data.chunk_data.slot_version;

                // wake up the state machine -- force it to begin a new sync if it's asleep
                if let Some(stackerdb_syncs) = self.stacker_db_syncs.as_mut() {
                    if let Some(stackerdb_sync) = stackerdb_syncs.get_mut(&chunk_data.contract_id) {
                        stackerdb_sync.wakeup();
                    }
                }
            }
            StacksMessageType::Nack(ref nack_data) => {
                if nack_data.error_code == NackErrorCodes::FutureView {
                    // chunk corresponds to a known DB but the view of the sender is potentially in
                    // the future.
                    // We should buffer this in case it becomes storable, but don't
                    // store it yet.
                    return Ok((true, false));
                } else {
                    return Ok((false, false));
                }
            }
            _ => {
                // don't recognize the message, so don't buffer
                return Ok((false, false));
            }
        }

        if !send_reply {
            return Ok((false, true));
        }

        // this is a reply to the pushed chunk, and we can store it right now (so don't buffer it)
        let resp = self.sign_for_p2p_reply(event_id, preamble.seq, payload)?;
        let handle = self.send_p2p_message(
            event_id,
            resp,
            self.connection_opts.neighbor_request_timeout,
        )?;
        self.add_relay_handle(event_id, handle);
        Ok((false, true))
    }
}
