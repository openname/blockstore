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

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::convert::TryFrom;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use rand::seq::SliceRandom;
use rand::{thread_rng, RngCore};
use stacks_common::types::chainstate::{
    BlockHeaderHash, ConsensusHash, PoxId, SortitionId, StacksBlockId,
};
use stacks_common::types::net::{PeerAddress, PeerHost};
use stacks_common::types::StacksEpochId;
use stacks_common::util::hash::to_hex;
use stacks_common::util::secp256k1::{Secp256k1PrivateKey, Secp256k1PublicKey};
use stacks_common::util::{get_epoch_time_ms, get_epoch_time_secs, log};

use crate::burnchains::{Burnchain, BurnchainView, PoxConstants};
use crate::chainstate::burn::db::sortdb::{
    BlockHeaderCache, SortitionDB, SortitionDBConn, SortitionHandleConn,
};
use crate::chainstate::burn::BlockSnapshot;
use crate::chainstate::nakamoto::{
    NakamotoBlock, NakamotoBlockHeader, NakamotoChainState, NakamotoStagingBlocksConnRef,
};
use crate::chainstate::stacks::boot::RewardSet;
use crate::chainstate::stacks::db::StacksChainState;
use crate::chainstate::stacks::{
    Error as chainstate_error, StacksBlockHeader, TenureChangePayload, TransactionPayload,
};
use crate::core::{
    EMPTY_MICROBLOCK_PARENT_HASH, FIRST_BURNCHAIN_CONSENSUS_HASH, FIRST_STACKS_BLOCK_HASH,
};
use crate::net::api::gettenureinfo::RPCGetTenureInfo;
use crate::net::chat::ConversationP2P;
use crate::net::db::{LocalPeer, PeerDB};
use crate::net::http::HttpRequestContents;
use crate::net::httpcore::{StacksHttpRequest, StacksHttpResponse};
use crate::net::inv::epoch2x::InvState;
use crate::net::inv::nakamoto::{NakamotoInvStateMachine, NakamotoTenureInv};
use crate::net::neighbors::rpc::NeighborRPC;
use crate::net::neighbors::NeighborComms;
use crate::net::p2p::{CurrentRewardSet, PeerNetwork};
use crate::net::server::HttpPeer;
use crate::net::{Error as NetError, Neighbor, NeighborAddress, NeighborKey};
use crate::util_lib::db::{DBConn, Error as DBError};

/// Download states for an historic tenure.  This is a tenure for which we know the hashes of the
/// start and end block.  This includes all tenures except for the two most recent ones.
#[derive(Debug, Clone, PartialEq)]
pub enum NakamotoTenureDownloadState {
    /// Getting the tenure-start block (the given StacksBlockId is it's block ID), as well as the
    /// millisecond epoch timestamp at which the request began
    GetTenureStartBlock(StacksBlockId, u128),
    /// Getting the tenure-end block.
    /// The fields here are the block ID of the tenure end block, as well as the millisecond epoch
    /// timestamp at which the request begahn
    GetTenureEndBlock(StacksBlockId, u128),
    /// Receiving tenure blocks.
    /// The fields here are the hash of the _last_ block in the tenure that must be downloaded, as well
    /// as the millisecond epoch timestamp at which the request began.  The first field is needed
    /// because a tenure is fetched in order from highest block to lowest block.
    GetTenureBlocks(StacksBlockId, u128),
    /// We have gotten all the blocks for this tenure
    Done,
}

pub const WAIT_FOR_TENURE_END_BLOCK_TIMEOUT: u64 = 1;

impl fmt::Display for NakamotoTenureDownloadState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Download state machine for an historic tenure -- a tenure for which the start and end block IDs
/// can be inferred from the chainstate and a peer's inventory (this excludes the two most recent
/// tenures).
///
/// This state machine works as follows:
///
/// 1. Fetch the first block in the given tenure
/// 2. Obtain the last block in the given tenure, via one of the following means:
///    a. Another NakamotoTenureDownloader's tenure-start block happens to be the end-block of this
///    machine's tenure, and can be copied into this machine.
///    b. This machine is configured to directly fetch the end-block.  This only happens if this
///    tenure both contains the anchor block for the next reward cycle and happens to be the last
///    tenure in the current reward cycle.
///    c. This machine is given the end-block on instantiation.  This only happens when the machine
///    is configured to fetch the highest complete tenure (i.e. the parent of the ongoing tenure);
///    in this case, the end-block is the start-block of the ongoing tenure.
/// 3. Obtain the blocks that lie between the first and last blocks of the tenure, in reverse
///    order.  As blocks are found, their signer signatures will be validated against the signer
///    public keys for this tenure; their hash-chain continuity will be validated against the start
///    and end block hashes; their quantity will be validated against the tenure-change transaction
///    in the end-block.
///
/// Once the machine has reached the `Done` state, it will have obtained the entire run of Nakamoto
/// blocks for the given tenure (regardless of how many sortitions it straddles, and regardless of
/// whether or not it straddles a reward cycle boundary).
#[derive(Debug, Clone, PartialEq)]
pub struct NakamotoTenureDownloader {
    /// Consensus hash that identifies this tenure
    pub tenure_id_consensus_hash: ConsensusHash,
    /// Consensus hash that identifies the snapshot from whence we obtained tenure_start_block_id
    pub start_block_snapshot_consensus_hash: ConsensusHash,
    /// Stacks block ID of the tenure-start block.  Learned from the inventory state machine and
    /// sortition DB.
    pub tenure_start_block_id: StacksBlockId,
    /// Consensus hash that identifies the snapshot from whence we obtained tenure_end_block_id
    pub end_block_snapshot_consensus_hash: ConsensusHash,
    /// Stacks block ID of the last block in this tenure (this will be the tenure-start block ID
    /// for some other tenure).  Learned from the inventory state machine and sortition DB.
    pub tenure_end_block_id: StacksBlockId,
    /// Address of who we're asking for blocks
    pub naddr: NeighborAddress,
    /// Signer public keys that signed the start-block of this tenure, in reward cycle order
    pub start_signer_keys: RewardSet,
    /// Signer public keys that signed the end-block of this tenure
    pub end_signer_keys: RewardSet,
    /// Whether or not we're idle -- i.e. there are no ongoing network requests associated with
    /// this state machine.
    pub idle: bool,

    /// What state we're in for downloading this tenure
    pub state: NakamotoTenureDownloadState,
    /// Tenure-start block
    pub tenure_start_block: Option<NakamotoBlock>,
    /// Pre-stored tenure end block.
    /// An instance of this state machine will be used to fetch the highest-confirmed tenure, once
    /// the start-block for the current tenure is downloaded.
    pub tenure_end_block: Option<NakamotoBlock>,
    /// Tenure blocks
    pub tenure_blocks: Option<Vec<NakamotoBlock>>,
}

impl NakamotoTenureDownloader {
    pub fn new(
        tenure_id_consensus_hash: ConsensusHash,
        start_block_snapshot_consensus_hash: ConsensusHash,
        tenure_start_block_id: StacksBlockId,
        end_block_snapshot_consensus_hash: ConsensusHash,
        tenure_end_block_id: StacksBlockId,
        naddr: NeighborAddress,
        start_signer_keys: RewardSet,
        end_signer_keys: RewardSet,
    ) -> Self {
        debug!(
            "Instantiate downloader to {}-{} for tenure {}: {}-{}",
            &naddr,
            &tenure_id_consensus_hash,
            &start_block_snapshot_consensus_hash,
            &tenure_start_block_id,
            &tenure_end_block_id,
        );
        Self {
            tenure_id_consensus_hash,
            start_block_snapshot_consensus_hash,
            tenure_start_block_id,
            end_block_snapshot_consensus_hash,
            tenure_end_block_id,
            naddr,
            start_signer_keys,
            end_signer_keys,
            idle: false,
            state: NakamotoTenureDownloadState::GetTenureStartBlock(
                tenure_start_block_id.clone(),
                get_epoch_time_ms(),
            ),
            tenure_start_block: None,
            tenure_end_block: None,
            tenure_blocks: None,
        }
    }

    /// Follow-on constructor used to instantiate a machine for downloading the highest-confirmed
    /// tenure.  This supplies the tenure end-block if known in advance.
    pub fn with_tenure_end_block(mut self, tenure_end_block: NakamotoBlock) -> Self {
        self.tenure_end_block = Some(tenure_end_block);
        self
    }

    /// Validate and accept a given tenure-start block.  If accepted, then advance the state.
    /// Returns Ok(()) if the start-block is valid.
    /// Returns Err(..) if it is not valid.
    pub fn try_accept_tenure_start_block(
        &mut self,
        tenure_start_block: NakamotoBlock,
    ) -> Result<(), NetError> {
        let NakamotoTenureDownloadState::GetTenureStartBlock(..) = &self.state else {
            // not the right state for this
            warn!("Invalid state for this method";
                  "state" => %self.state);
            return Err(NetError::InvalidState);
        };

        if self.tenure_start_block_id != tenure_start_block.header.block_id() {
            // not the block we were expecting
            warn!("Invalid tenure-start block: unexpected"; 
                  "tenure_id" => %self.tenure_id_consensus_hash,
                  "tenure_id_start_block" => %self.tenure_start_block_id,
                  "tenure_start_block ID" => %tenure_start_block.header.block_id(),
                  "state" => %self.state);
            return Err(NetError::InvalidMessage);
        }

        if let Err(e) = tenure_start_block
            .header
            .verify_signer_signatures(&self.start_signer_keys)
        {
            // signature verification failed
            warn!("Invalid tenure-start block: bad signer signature";
                   "tenure_id" => %self.tenure_id_consensus_hash,
                   "block.header.block_id" => %tenure_start_block.header.block_id(),
                   "state" => %self.state,
                   "error" => %e);
            return Err(NetError::InvalidMessage);
        }

        debug!(
            "Accepted tenure-start block for tenure {} block={}",
            &self.tenure_id_consensus_hash,
            &tenure_start_block.block_id()
        );
        self.tenure_start_block = Some(tenure_start_block);

        if let Some(tenure_end_block) = self.tenure_end_block.take() {
            // we already have the tenure-end block, so immediately proceed to accept it.
            debug!(
                "Preemptively process tenure-end block {} for tenure {}",
                tenure_end_block.block_id(),
                &self.tenure_id_consensus_hash
            );
            self.try_accept_tenure_end_block(&tenure_end_block)?;
        } else {
            // need to get tenure_end_block.
            self.state = NakamotoTenureDownloadState::GetTenureEndBlock(
                self.tenure_end_block_id.clone(),
                get_epoch_time_ms(),
            );
        }
        Ok(())
    }

    /// Validate and accept a tenure-end block.  If accepted, then advance the state.
    /// Once accepted, this function extracts the tenure-change transaction and block header from
    /// this block (it does not need the entire block).
    ///
    /// Returns Ok(()) if the block was valid
    /// Returns Err(..) if the block was invalid
    pub fn try_accept_tenure_end_block(
        &mut self,
        tenure_end_block: &NakamotoBlock,
    ) -> Result<(), NetError> {
        if !matches!(
            &self.state,
            NakamotoTenureDownloadState::GetTenureEndBlock(..)
        ) {
            warn!("Invalid state for this method";
                  "state" => %self.state);
            return Err(NetError::InvalidState);
        };
        let Some(tenure_start_block) = self.tenure_start_block.as_ref() else {
            warn!("Invalid state -- tenure_start_block is not set");
            return Err(NetError::InvalidState);
        };

        if self.tenure_end_block_id != tenure_end_block.header.block_id()
            && self.tenure_end_block_id != StacksBlockId([0x00; 32])
        {
            // not the block we asked for
            warn!("Invalid tenure-end block: unexpected";
                  "tenure_id" => %self.tenure_id_consensus_hash,
                  "tenure_id_end_block" => %self.tenure_end_block_id,
                  "block.header.block_id" => %tenure_end_block.header.block_id(),
                  "state" => %self.state);
            return Err(NetError::InvalidMessage);
        }

        if let Err(e) = tenure_end_block
            .header
            .verify_signer_signatures(&self.end_signer_keys)
        {
            // bad signature
            warn!("Invalid tenure-end block: bad signer signature";
                  "tenure_id" => %self.tenure_id_consensus_hash,
                  "block.header.block_id" => %tenure_end_block.header.block_id(),
                  "state" => %self.state,
                  "error" => %e);
            return Err(NetError::InvalidMessage);
        }

        // extract the needful -- need the tenure-change payload (which proves that the tenure-end
        // block is the tenure-start block for the next tenure) and the parent block ID (which is
        // the next block to download).
        let Ok(valid) = tenure_end_block.is_wellformed_tenure_start_block() else {
            warn!("Invalid tenure-end block: failed to validate tenure-start";
                  "block_id" => %tenure_end_block.block_id());
            return Err(NetError::InvalidMessage);
        };

        if !valid {
            warn!("Invalid tenure-end block: not a well-formed tenure-start block";
                  "block_id" => %tenure_end_block.block_id());
            return Err(NetError::InvalidMessage);
        }

        let Some(tc_payload) = tenure_end_block.try_get_tenure_change_payload() else {
            warn!("Invalid tenure-end block: no tenure-change transaction";
                  "block_id" => %tenure_end_block.block_id());
            return Err(NetError::InvalidMessage);
        };

        // tc_payload must point to the tenure-start block's header
        if tc_payload.prev_tenure_consensus_hash != tenure_start_block.header.consensus_hash {
            warn!("Invalid tenure-end block: tenure-change does not point to tenure-start block";
                  "start_block_id" => %tenure_start_block.block_id(),
                  "end_block_id" => %tenure_end_block.block_id(),
                  "tc_payload.prev_tenure_consensus_hash" => %tc_payload.prev_tenure_consensus_hash,
                  "tenure_start.consensus_hash" => %tenure_start_block.header.consensus_hash);
            return Err(NetError::InvalidMessage);
        }

        debug!(
            "Accepted tenure-end block for tenure {} block={}; expect {} blocks",
            &self.tenure_id_consensus_hash,
            &tenure_end_block.block_id(),
            tc_payload.previous_tenure_blocks
        );
        self.tenure_end_block = Some(tenure_end_block.clone());
        self.state = NakamotoTenureDownloadState::GetTenureBlocks(
            tenure_end_block.header.parent_block_id.clone(),
            get_epoch_time_ms(),
        );
        Ok(())
    }

    /// Determine how many blocks must be in this tenure.
    /// Returns None if we don't have the start and end blocks yet.
    pub fn tenure_length(&self) -> Option<u64> {
        self.tenure_end_block.as_ref().and_then(|tenure_end_block| {
            let Some(tc_payload) = tenure_end_block.try_get_tenure_change_payload() else {
                return None;
            };

            Some(u64::from(tc_payload.previous_tenure_blocks))
        })
    }

    /// Add downloaded tenure blocks to this machine.
    /// If we have collected all tenure blocks, then return them and transition to the Done state.
    ///
    /// Returns Ok(Some([blocks])) if we got all the blocks in this tenure. The blocks will be in
    /// ascending order by height, and will include both the tenure-start block and the tenure-end
    /// block.  Including the tenure-end block is necessary because processing it will mark this
    /// tenure as "complete" in the chainstate, which will allow the downloader to deduce when all
    /// confirmed tenures have been completely downloaded.
    ///
    /// Returns Ok(None) if the given blocks were valid, but we still need more.  The pointer to
    /// the next block to fetch (stored in self.state) will be updated.
    /// Returns Err(..) if the blocks were invalid.
    pub fn try_accept_tenure_blocks(
        &mut self,
        mut tenure_blocks: Vec<NakamotoBlock>,
    ) -> Result<Option<Vec<NakamotoBlock>>, NetError> {
        let NakamotoTenureDownloadState::GetTenureBlocks(block_cursor, start_request_time) =
            &self.state
        else {
            warn!("Invalid state for this method";
                  "state" => %self.state);
            return Err(NetError::InvalidState);
        };

        if tenure_blocks.is_empty() {
            // nothing to do
            return Ok(None);
        }

        // blocks must be contiguous and in order from highest to lowest
        let mut expected_block_id = block_cursor;
        let mut count = 0;
        for block in tenure_blocks.iter() {
            // must be from this tenure
            // This may not always be the case, since a remote peer could have processed a
            // different Stacks micro-fork.  The consequence of erroring here (or below) is that we
            // disconnect from the peer that served this to us.
            if block.header.consensus_hash != self.tenure_id_consensus_hash {
                warn!("Unexpected Nakamoto block -- not part of tenure";
                      "block.header.consensus_hash" => %block.header.consensus_hash,
                      "self.tenure_id_consensus_hash" => %self.tenure_id_consensus_hash,
                      "state" => %self.state);
                return Err(NetError::InvalidMessage);
            }

            if &block.header.block_id() != expected_block_id {
                warn!("Unexpected Nakamoto block -- does not match cursor";
                      "expected_block_id" => %expected_block_id,
                      "block_id" => %block.header.block_id(),
                      "state" => %self.state);
                return Err(NetError::InvalidMessage);
            }

            if let Err(e) = block
                .header
                .verify_signer_signatures(&self.start_signer_keys)
            {
                warn!("Invalid block: bad signer signature";
                      "tenure_id" => %self.tenure_id_consensus_hash,
                      "block.header.block_id" => %block.header.block_id(),
                      "state" => %self.state,
                      "error" => %e);
                return Err(NetError::InvalidMessage);
            }

            expected_block_id = &block.header.parent_block_id;
            count += 1;
            if self
                .tenure_blocks
                .as_ref()
                .map(|blocks| blocks.len())
                .unwrap_or(0)
                .saturating_add(count)
                > self.tenure_length().unwrap_or(0).saturating_add(1) as usize
            // + 1 due to the inclusion of the tenure-end block
            {
                // there are more blocks downloaded than indicated by the end-blocks tenure-change
                // transaction.
                warn!("Invalid blocks: exceeded {} tenure blocks", self.tenure_length().unwrap_or(0);
                      "tenure_id" => %self.tenure_id_consensus_hash,
                      "count" => %count,
                      "tenure_length" => self.tenure_length().unwrap_or(0),
                      "num_blocks" => tenure_blocks.len());
                return Err(NetError::InvalidMessage);
            }
        }

        if let Some(blocks) = self.tenure_blocks.as_mut() {
            blocks.append(&mut tenure_blocks);
        } else {
            // include tenure-end block
            if let Some(tenure_end_block) = self.tenure_end_block.as_ref() {
                tenure_blocks.insert(0, tenure_end_block.clone());
            }
            self.tenure_blocks = Some(tenure_blocks);
        }

        // did we reach the tenure start block?
        let Some(blocks) = self.tenure_blocks.as_ref() else {
            // unreachable but be defensive
            warn!("Invalid state: no blocks (infallible -- got None)");
            return Err(NetError::InvalidState);
        };

        let Some(earliest_block) = blocks.last() else {
            // unreachable but be defensive
            warn!("Invalid state: no blocks (infallible -- got empty vec)");
            return Err(NetError::InvalidState);
        };

        let Some(tenure_start_block) = self.tenure_start_block.as_ref() else {
            // unreachable but be defensive
            warn!("Invalid state: no tenure-start block (infallible)");
            return Err(NetError::InvalidState);
        };

        debug!(
            "Accepted tenure blocks for tenure {} cursor={} ({})",
            &self.tenure_id_consensus_hash, &block_cursor, count
        );
        if earliest_block.block_id() != tenure_start_block.block_id() {
            // still have more blocks to download
            let next_block_id = earliest_block.header.parent_block_id.clone();
            debug!(
                "Need more blocks for tenure {} (went from {} to {}, next is {})",
                &self.tenure_id_consensus_hash,
                &block_cursor,
                &earliest_block.block_id(),
                &next_block_id
            );
            self.state =
                NakamotoTenureDownloadState::GetTenureBlocks(next_block_id, *start_request_time);
            return Ok(None);
        }

        // finished!
        self.state = NakamotoTenureDownloadState::Done;
        Ok(self
            .tenure_blocks
            .take()
            .map(|blocks| blocks.into_iter().rev().collect()))
    }

    /// Produce the next HTTP request that, when successfully executed, will fetch the data needed
    /// to advance this state machine.
    /// Not all states require an HTTP request for advanceement.
    ///
    /// Returns Ok(Some(request)) if a request is needed
    /// Returns Ok(None) if a request is not needed (i.e. we're waiting for some other machine's
    /// state)
    /// Returns Err(()) if we're done.
    pub fn make_next_download_request(
        &self,
        peerhost: PeerHost,
    ) -> Result<Option<StacksHttpRequest>, ()> {
        let request = match self.state {
            NakamotoTenureDownloadState::GetTenureStartBlock(
                start_block_id,
                start_request_time,
            ) => {
                debug!(
                    "Request tenure-start block {} at {}",
                    &start_block_id, start_request_time
                );
                StacksHttpRequest::new_get_nakamoto_block(peerhost, start_block_id.clone())
            }
            NakamotoTenureDownloadState::GetTenureEndBlock(end_block_id, start_request_time) => {
                debug!(
                    "Request tenure-end block {} at {}",
                    &end_block_id, start_request_time
                );
                StacksHttpRequest::new_get_nakamoto_block(peerhost, end_block_id.clone())
            }
            NakamotoTenureDownloadState::GetTenureBlocks(end_block_id, start_request_time) => {
                debug!(
                    "Downloading tenure ending at {} at {}",
                    &end_block_id, start_request_time
                );
                StacksHttpRequest::new_get_nakamoto_tenure(peerhost, end_block_id.clone(), None)
            }
            NakamotoTenureDownloadState::Done => {
                // nothing more to do
                return Err(());
            }
        };
        Ok(Some(request))
    }

    /// Advance the state of the downloader from chainstate, if possible.
    /// For example, a tenure-start or tenure-end block may have been pushed to us already (or they
    /// may be shadow blocks)
    pub fn try_advance_from_chainstate(
        &mut self,
        chainstate: &mut StacksChainState,
    ) -> Result<(), NetError> {
        loop {
            match self.state {
                NakamotoTenureDownloadState::GetTenureStartBlock(
                    start_block_id,
                    start_request_time,
                ) => {
                    if chainstate
                        .nakamoto_blocks_db()
                        .is_shadow_tenure(&self.start_block_snapshot_consensus_hash)?
                    {
                        debug!(
                            "Tenure {} start-block confirmed by shadow tenure {}",
                            &self.tenure_id_consensus_hash,
                            &self.start_block_snapshot_consensus_hash
                        );
                        let Some(shadow_block) = chainstate
                            .nakamoto_blocks_db()
                            .get_shadow_tenure_start_block(
                                &self.start_block_snapshot_consensus_hash,
                            )?
                        else {
                            warn!(
                                "No tenure-start block for shadow tenure {}",
                                &self.start_block_snapshot_consensus_hash
                            );
                            break;
                        };

                        // the coinbase of a tenure-start block of a shadow tenure contains the
                        // block-id of the parent tenure's start block (i.e. the information that
                        // would have been gleaned from a block-commit, if there was one).
                        let Some(shadow_coinbase) = shadow_block.get_coinbase_tx() else {
                            warn!("Shadow block {} has no coinbase", &shadow_block.block_id());
                            break;
                        };

                        let TransactionPayload::Coinbase(coinbase_payload, ..) =
                            &shadow_coinbase.payload
                        else {
                            warn!(
                                "Shadow block {} coinbase tx is not a Coinbase",
                                &shadow_block.block_id()
                            );
                            break;
                        };

                        let tenure_start_block_id = StacksBlockId(coinbase_payload.0.clone());

                        info!(
                            "Tenure {} starts at shadow tenure-start {}, not {}",
                            &self.tenure_id_consensus_hash, &tenure_start_block_id, &start_block_id
                        );
                        self.tenure_start_block_id = tenure_start_block_id.clone();
                        self.state = NakamotoTenureDownloadState::GetTenureStartBlock(
                            tenure_start_block_id,
                            start_request_time,
                        );
                        if let Some((tenure_start_block, _sz)) = chainstate
                            .nakamoto_blocks_db()
                            .get_nakamoto_block(&self.tenure_start_block_id)?
                        {
                            // normal block on disk
                            self.try_accept_tenure_start_block(tenure_start_block)?;
                        }
                    } else if let Some((tenure_start_block, _sz)) = chainstate
                        .nakamoto_blocks_db()
                        .get_nakamoto_block(&start_block_id)?
                    {
                        // we have downloaded this block already
                        self.try_accept_tenure_start_block(tenure_start_block)?;
                    } else {
                        break;
                    }
                    if let NakamotoTenureDownloadState::GetTenureStartBlock(..) = &self.state {
                        break;
                    }
                }
                NakamotoTenureDownloadState::GetTenureEndBlock(
                    end_block_id,
                    start_request_time,
                ) => {
                    if chainstate
                        .nakamoto_blocks_db()
                        .is_shadow_tenure(&self.end_block_snapshot_consensus_hash)?
                    {
                        debug!(
                            "Tenure {} end-block confirmed by shadow tenure {}",
                            &self.tenure_id_consensus_hash, &self.end_block_snapshot_consensus_hash
                        );
                        let Some(shadow_block) = chainstate
                            .nakamoto_blocks_db()
                            .get_shadow_tenure_start_block(
                                &self.end_block_snapshot_consensus_hash,
                            )?
                        else {
                            warn!(
                                "No tenure-start block for shadow tenure {}",
                                &self.end_block_snapshot_consensus_hash
                            );
                            break;
                        };

                        // the coinbase of a tenure-start block of a shadow tenure contains the
                        // block-id of the parent tenure's start block (i.e. the information that
                        // would have been gleaned from a block-commit, if there was one).
                        let Some(shadow_coinbase) = shadow_block.get_coinbase_tx() else {
                            warn!("Shadow block {} has no coinbase", &shadow_block.block_id());
                            break;
                        };

                        let TransactionPayload::Coinbase(coinbase_payload, ..) =
                            &shadow_coinbase.payload
                        else {
                            warn!(
                                "Shadow block {} coinbase tx is not a Coinbase",
                                &shadow_block.block_id()
                            );
                            break;
                        };

                        let tenure_end_block_id = StacksBlockId(coinbase_payload.0.clone());

                        info!(
                            "Tenure {} ends at shadow tenure-start {}, not {}",
                            &self.tenure_id_consensus_hash, &tenure_end_block_id, &end_block_id
                        );
                        self.tenure_end_block_id = tenure_end_block_id.clone();
                        self.state = NakamotoTenureDownloadState::GetTenureEndBlock(
                            tenure_end_block_id,
                            start_request_time,
                        );
                        if let Some((tenure_end_block, _sz)) = chainstate
                            .nakamoto_blocks_db()
                            .get_nakamoto_block(&self.tenure_end_block_id)?
                        {
                            // normal block on disk
                            self.try_accept_tenure_end_block(&tenure_end_block)?;
                        }
                    } else if let Some((tenure_end_block, _sz)) = chainstate
                        .nakamoto_blocks_db()
                        .get_nakamoto_block(&end_block_id)?
                    {
                        // normal block on disk
                        self.try_accept_tenure_end_block(&tenure_end_block)?;
                    } else {
                        break;
                    };
                    if let NakamotoTenureDownloadState::GetTenureEndBlock(..) = &self.state {
                        break;
                    }
                }
                NakamotoTenureDownloadState::GetTenureBlocks(..) => {
                    // TODO: look at the chainstate and find out what we don't have to download
                    // TODO: skip shadow tenures
                    break;
                }
                NakamotoTenureDownloadState::Done => {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Begin the next download request for this state machine.  The request will be sent to the
    /// data URL corresponding to self.naddr.
    /// Returns Ok(true) if we sent the request, or there's already an in-flight request.  The
    /// caller should try this again until it gets one of the other possible return values.
    /// Returns Ok(false) if not (e.g. neighbor is known to be dead or broken)
    /// Returns Err(..) if self.naddr is known to be a dead or broken peer, or if we were unable to
    /// resolve its data URL to a socket address.
    pub fn send_next_download_request(
        &mut self,
        network: &mut PeerNetwork,
        neighbor_rpc: &mut NeighborRPC,
    ) -> Result<bool, NetError> {
        if neighbor_rpc.has_inflight(&self.naddr) {
            debug!("Peer {} has an inflight request", &self.naddr);
            return Ok(true);
        }
        if neighbor_rpc.is_dead_or_broken(network, &self.naddr) {
            return Err(NetError::PeerNotConnected);
        }

        let Some(peerhost) = NeighborRPC::get_peer_host(network, &self.naddr) else {
            // no conversation open to this neighbor
            neighbor_rpc.add_dead(network, &self.naddr);
            return Err(NetError::PeerNotConnected);
        };

        let request = match self.make_next_download_request(peerhost) {
            Ok(Some(request)) => request,
            Ok(None) => {
                return Ok(true);
            }
            Err(_) => {
                return Ok(false);
            }
        };

        neighbor_rpc.send_request(network, self.naddr.clone(), request)?;
        self.idle = false;
        Ok(true)
    }

    /// Handle a received StacksHttpResponse and advance the state machine.
    /// If we get the full tenure's blocks, then return them.
    /// Returns Ok(Some([blocks])) if we successfully complete the state machine.
    /// Returns Ok(None) if we accepted the response and did a state-transition, but we're not done
    /// yet.  The caller should now call `send_next_download_request()`
    /// Returns Err(..) on failure to process the response.
    pub fn handle_next_download_response(
        &mut self,
        response: StacksHttpResponse,
    ) -> Result<Option<Vec<NakamotoBlock>>, NetError> {
        let handle_result = match self.state {
            NakamotoTenureDownloadState::GetTenureStartBlock(block_id, start_request_time) => {
                debug!(
                    "Got download response for tenure-start block {} in {}ms",
                    &block_id,
                    get_epoch_time_ms().saturating_sub(start_request_time)
                );
                let block = response.decode_nakamoto_block().map_err(|e| {
                    warn!("Failed to decode response for a Nakamoto block: {:?}", &e);
                    e
                })?;
                self.try_accept_tenure_start_block(block)?;
                Ok(None)
            }
            NakamotoTenureDownloadState::GetTenureEndBlock(block_id, start_request_time) => {
                debug!(
                    "Got download response to tenure-end block {} in {}ms",
                    &block_id,
                    get_epoch_time_ms().saturating_sub(start_request_time)
                );
                let block = response.decode_nakamoto_block().map_err(|e| {
                    warn!("Failed to decode response for a Nakamoto block: {:?}", &e);
                    e
                })?;
                self.try_accept_tenure_end_block(&block)?;
                Ok(None)
            }
            NakamotoTenureDownloadState::GetTenureBlocks(end_block_id, start_request_time) => {
                debug!(
                    "Got download response for tenure blocks ending at {} in {}ms",
                    &end_block_id,
                    get_epoch_time_ms().saturating_sub(start_request_time)
                );
                let blocks = response.decode_nakamoto_tenure().map_err(|e| {
                    warn!("Failed to decode response for a Nakamoto tenure: {:?}", &e);
                    e
                })?;
                let blocks_opt = self.try_accept_tenure_blocks(blocks)?;
                Ok(blocks_opt)
            }
            NakamotoTenureDownloadState::Done => Err(NetError::InvalidState),
        };
        self.idle = true;
        handle_result
    }

    pub fn is_done(&self) -> bool {
        self.state == NakamotoTenureDownloadState::Done
    }
}
