// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020 Stacks Open Internet Foundation
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

use std::collections::BTreeMap;

use rusqlite::Connection;
use stacks_common::consts::{FIRST_BURNCHAIN_CONSENSUS_HASH, FIRST_STACKS_BLOCK_HASH};
use stacks_common::types::chainstate::{
    BlockHeaderHash, PoxId, SortitionId, StacksBlockId, TrieHash, VRFSeed,
};
use stacks_common::util::hash::Hash160;
use stacks_common::util::log;
use stacks_common::util::uint::{BitArray, Uint256, Uint512};

use crate::burnchains::{
    Address, Burnchain, BurnchainBlock, BurnchainBlockHeader, BurnchainSigner,
    BurnchainStateTransition, PublicKey, Txid,
};
use crate::chainstate::burn::atc::{AtcRational, ATC_LOOKUP};
use crate::chainstate::burn::db::sortdb::{SortitionDB, SortitionHandleTx};
use crate::chainstate::burn::distribution::BurnSamplePoint;
use crate::chainstate::burn::operations::{
    BlockstackOperationType, LeaderBlockCommitOp, LeaderKeyRegisterOp,
};
use crate::chainstate::burn::{
    BlockSnapshot, BurnchainHeaderHash, ConsensusHash, ConsensusHashExtensions, OpsHash,
    SortitionHash,
};
use crate::chainstate::stacks::db::StacksChainState;
use crate::chainstate::stacks::index::{ClarityMarfTrieId, MarfTrieId};
use crate::core::*;
use crate::util_lib::db::Error as db_error;

impl BlockSnapshot {
    /// Creates an "empty" (i.e. zeroed out) BlockSnapshot, to make a basis for creating
    /// `BlockSnapshot` with a few key fields filled.
    /// Used for testing
    pub fn empty() -> BlockSnapshot {
        BlockSnapshot {
            block_height: 0,
            burn_header_timestamp: 0,
            burn_header_hash: BurnchainHeaderHash([0; 32]),
            parent_burn_header_hash: BurnchainHeaderHash([0; 32]),
            consensus_hash: ConsensusHash([0; 20]),
            ops_hash: OpsHash([0; 32]),
            total_burn: 0,
            sortition: true,
            sortition_hash: SortitionHash([0; 32]),
            winning_block_txid: Txid([0; 32]),
            winning_stacks_block_hash: BlockHeaderHash([0; 32]),
            index_root: TrieHash([0; 32]),
            num_sortitions: 0,
            stacks_block_accepted: true,
            stacks_block_height: 0,
            arrival_index: 0,
            canonical_stacks_tip_height: 0,
            canonical_stacks_tip_hash: BlockHeaderHash([0; 32]),
            canonical_stacks_tip_consensus_hash: ConsensusHash([0; 20]),
            sortition_id: SortitionId([0; 32]),
            parent_sortition_id: SortitionId([0; 32]),
            pox_valid: true,
            accumulated_coinbase_ustx: 0,
            miner_pk_hash: None,
        }
    }

    /// Create the sentinel block snapshot -- the first one
    pub fn initial(
        first_block_height: u64,
        first_burn_header_hash: &BurnchainHeaderHash,
        first_burn_header_timestamp: u64,
    ) -> BlockSnapshot {
        BlockSnapshot {
            block_height: first_block_height,
            burn_header_hash: first_burn_header_hash.clone(),
            burn_header_timestamp: first_burn_header_timestamp,
            parent_burn_header_hash: BurnchainHeaderHash::sentinel(),
            consensus_hash: ConsensusHash([0u8; 20]),
            ops_hash: OpsHash([0u8; 32]),
            total_burn: 0,
            sortition: true,
            sortition_hash: SortitionHash::initial(),
            winning_block_txid: Txid([0u8; 32]),
            winning_stacks_block_hash: FIRST_STACKS_BLOCK_HASH.clone(),
            index_root: TrieHash::from_empty_data(),
            num_sortitions: 0,
            stacks_block_accepted: false,
            stacks_block_height: 0,
            arrival_index: 0,
            canonical_stacks_tip_height: 0,
            canonical_stacks_tip_hash: FIRST_STACKS_BLOCK_HASH.clone(),
            canonical_stacks_tip_consensus_hash: FIRST_BURNCHAIN_CONSENSUS_HASH.clone(),
            // Initial snapshot sets sortition_id = burn_header_hash,
            //  we shouldn't need to update this to use PoxId::initial(),
            //  but if we do, we need to update a lot of test cases.
            sortition_id: SortitionId::stubbed(first_burn_header_hash),
            parent_sortition_id: SortitionId::stubbed(first_burn_header_hash),
            pox_valid: true,
            accumulated_coinbase_ustx: 0,
            miner_pk_hash: None,
        }
    }

    pub fn is_initial(&self) -> bool {
        self.sortition_hash == SortitionHash::initial()
    }

    pub fn get_canonical_stacks_block_id(&self) -> StacksBlockId {
        StacksBlockId::new(
            &self.canonical_stacks_tip_consensus_hash,
            &self.canonical_stacks_tip_hash,
        )
    }

    /// Given the weighted burns, VRF seed of the last winner, and sortition hash, pick the next
    /// winner.  Return the index into the distribution *if there is a sample to take*.
    fn sample_burn_distribution(
        dist: &[BurnSamplePoint],
        VRF_seed: &VRFSeed,
        sortition_hash: &SortitionHash,
    ) -> Option<usize> {
        if dist.is_empty() {
            // no winners
            return None;
        }
        if dist.len() == 1 {
            // only one winner
            return Some(0);
        }

        let index = sortition_hash.mix_VRF_seed(VRF_seed).to_uint256();
        for i in 0..dist.len() {
            if (dist[i].range_start <= index) && (index < dist[i].range_end) {
                debug!(
                    "Sampled {}: i = {}, sortition index = {}",
                    dist[i].candidate.block_header_hash, i, &index
                );
                return Some(i);
            }
        }

        // should never happen
        panic!("FATAL ERROR: unable to map {} to a range", index);
    }

    /// Get the last winning miner's VRF seed in this block's fork.
    /// Returns Ok(VRF seed) on success
    /// Returns Err(..) on DB error
    /// An initial VRF seed value will be returned if there are no prior commits.
    fn get_last_vrf_seed(
        sort_tx: &mut SortitionHandleTx,
        block_header: &BurnchainBlockHeader,
    ) -> Result<VRFSeed, db_error> {
        let burn_block_height = block_header.block_height;

        // get the last winner's VRF seed in this block's fork
        let last_sortition_snapshot =
            sort_tx.get_last_snapshot_with_sortition(burn_block_height - 1)?;

        let vrf_seed = if last_sortition_snapshot.is_initial() {
            // this is the sentinal "first-sortition" block
            VRFSeed::initial()
        } else {
            // there may have been a prior winning block commit.  Use its VRF seed if possible
            sort_tx
                .get_block_commit(
                    &last_sortition_snapshot.winning_block_txid,
                    &last_sortition_snapshot.sortition_id,
                )?
                .expect("FATAL ERROR: no winning block commits in database (indicates corruption)")
                .new_seed
        };
        Ok(vrf_seed)
    }

    /// Select the next Stacks block header hash using cryptographic sortition.
    /// Go through all block commits at this height, find out how many burn tokens
    /// were spent for them, and select one at random using the relative burn amounts
    /// to weight the sample.  Use HASH(sortition_hash ++ last_VRF_seed) to pick the
    /// winning block commit, and by extension, the next VRF seed.
    ///
    /// If there are no block commits outstanding, then no winner is picked.
    ///
    /// Note that the VRF seed is not guaranteed to be the hash of a valid VRF
    /// proof.  Miners would only build off of leader block commits for which they
    /// (1) have the associated block data and (2) the proof in that block is valid.
    fn select_winning_block(
        sort_tx: &mut SortitionHandleTx,
        block_header: &BurnchainBlockHeader,
        sortition_hash: &SortitionHash,
        burn_dist: &[BurnSamplePoint],
    ) -> Result<Option<(LeaderBlockCommitOp, usize)>, db_error> {
        let vrf_seed = Self::get_last_vrf_seed(sort_tx, block_header)?;

        // pick the next winner
        let win_idx_opt =
            BlockSnapshot::sample_burn_distribution(burn_dist, &vrf_seed, sortition_hash);
        match win_idx_opt {
            None => {
                // no winner
                Ok(None)
            }
            Some(win_idx) => {
                // winner!
                Ok(Some((burn_dist[win_idx].candidate.clone(), win_idx)))
            }
        }
    }

    /// Make the snapshot struct for the case where _no sortition_ takes place
    fn make_snapshot_no_sortition(
        sort_tx: &mut SortitionHandleTx,
        sortition_id: &SortitionId,
        pox_id: &PoxId,
        parent_snapshot: &BlockSnapshot,
        block_header: &BurnchainBlockHeader,
        first_block_height: u64,
        burn_total: u64,
        sortition_hash: &SortitionHash,
        txids: &[Txid],
        accumulated_coinbase_ustx: u128,
    ) -> Result<BlockSnapshot, db_error> {
        let block_height = block_header.block_height;
        let block_hash = block_header.block_hash.clone();
        let parent_block_hash = block_header.parent_block_hash.clone();

        let non_winning_block_txid = Txid::from_bytes(&[0u8; 32]).unwrap();
        let non_winning_block_hash = BlockHeaderHash::from_bytes(&[0u8; 32]).unwrap();

        let ops_hash = OpsHash::from_txids(txids);
        let ch = ConsensusHash::from_parent_block_data(
            sort_tx,
            &ops_hash,
            block_height - 1,
            first_block_height,
            &block_hash,
            burn_total,
            pox_id,
        )?;

        debug!("SORTITION({}): NO BLOCK CHOSEN", block_height);

        Ok(BlockSnapshot {
            block_height,
            burn_header_hash: block_hash,
            burn_header_timestamp: block_header.timestamp,
            parent_burn_header_hash: parent_block_hash,
            consensus_hash: ch,
            ops_hash,
            total_burn: burn_total,
            sortition: false,
            sortition_hash: sortition_hash.clone(),
            winning_block_txid: non_winning_block_txid,
            winning_stacks_block_hash: non_winning_block_hash,
            index_root: TrieHash::from_empty_data(), // will be overwritten
            num_sortitions: parent_snapshot.num_sortitions,
            stacks_block_accepted: false,
            stacks_block_height: 0,
            arrival_index: 0,
            canonical_stacks_tip_height: parent_snapshot.canonical_stacks_tip_height,
            canonical_stacks_tip_hash: parent_snapshot.canonical_stacks_tip_hash.clone(),
            canonical_stacks_tip_consensus_hash: parent_snapshot
                .canonical_stacks_tip_consensus_hash
                .clone(),
            sortition_id: sortition_id.clone(),
            parent_sortition_id: parent_snapshot.sortition_id.clone(),
            pox_valid: true,
            accumulated_coinbase_ustx,
            miner_pk_hash: None,
        })
    }

    /// Determine if we need to reject a block-commit due to miner inactivity.
    /// Return true if the miner is sufficiently active.
    /// Return false if not.
    fn check_miner_is_active(
        epoch_id: StacksEpochId,
        sampled_window_len: usize,
        winning_block_sender: &BurnchainSigner,
        miner_frequency: u8,
    ) -> bool {
        // miner frequency only applies if the window is at least as long as the commit window
        // sampled from the chain state (e.g. because this window can be 1 during the prepare
        // phase)
        let epoch_frequency_usize =
            usize::try_from(epoch_id.mining_commitment_frequency()).expect("Infallible");
        if usize::from(miner_frequency) < epoch_frequency_usize.min(sampled_window_len) {
            // this miner didn't mine often enough to win anyway
            info!("Miner did not mine often enough to win";
                   "miner_sender" => %winning_block_sender,
                   "miner_frequency" => miner_frequency,
                   "minimum_frequency" => epoch_id.mining_commitment_frequency(),
                   "window_length" => sampled_window_len);

            return false;
        }

        true
    }

    /// Determine the miner's assumed total commit carryover.
    ///
    ///                              total-block-spend
    /// This is ATC = min(1, ----------------------------------- )
    ///                       median-windowed-total-block-spend
    ///
    /// Now, this value is 1.0 in the "happy path" case where miners commit the same BTC in this
    /// block as they had done so over the majority of the windowed burnchain blocks.
    ///
    /// It's also 1.0 if miners spend _more_ than this median.
    ///
    /// It's between 0.0 and 1.0 only if miners spend _less_ than this median.  At this point, it's
    /// possible that the "null miner" can win sortition, and the probability of that null miner
    /// winning is a function of (1.0 - ATC).
    ///
    /// Returns the ATC value, and whether or not it decreased.  If the ATC decreased, then we must
    /// invoke the null miner.
    fn get_miner_commit_carryover(
        total_burns: Option<u64>,
        windowed_median_burns: Option<u64>,
    ) -> (AtcRational, bool) {
        let Some(block_burn_total) = total_burns else {
            // overflow
            return (AtcRational::zero(), false);
        };

        let Some(windowed_median_burns) = windowed_median_burns else {
            // overflow
            return (AtcRational::zero(), false);
        };

        if windowed_median_burns == 0 {
            // no carried commit, so null miner wins by default.
            return (AtcRational::zero(), true);
        }

        if block_burn_total >= windowed_median_burns {
            // clamp to 1.0, and ATC increased
            return (AtcRational::one(), false);
        }

        (
            AtcRational::frac(block_burn_total, windowed_median_burns),
            true,
        )
    }

    /// Evaluate the advantage logistic function on the given ATC value.
    /// The ATC value will be used to index a lookup table of AtcRationals.
    pub(crate) fn null_miner_logistic(atc: AtcRational) -> AtcRational {
        let atc_clamp = atc.min(&AtcRational::one());
        let index_max =
            u64::try_from(ATC_LOOKUP.len() - 1).expect("infallible -- u64 can't hold 1023usize");
        let index_u64 = if let Some(index_rational) = atc_clamp.mul(&AtcRational::frac(1024, 1)) {
            // extract integer part
            index_rational.ipart().min(index_max)
        } else {
            index_max
        };
        let index = usize::try_from(index_u64)
            .expect("infallible -- usize can't hold u64 integers in [0, 1024)");
        ATC_LOOKUP
            .get(index)
            .cloned()
            .unwrap_or_else(|| ATC_LOOKUP.last().cloned().expect("infallible"))
    }

    /// Determine the probability that the null miner will win, given the atc shortage.
    ///
    /// This is NullP(atc) = (1 - atc) + atc * adv(atc).
    ///
    /// Where adv(x) is an "advantage function", such that the null miner is more heavily favored
    /// to win based on how comparatively little commit carryover there is.  Here, adv(x) is a
    /// logistic function.
    ///
    /// In a linear setting -- i.e. the probability of the null miner winning being proportional to
    /// the missing carryover -- the probability would simply be (1 - atc).  If miners spent only
    /// X% of the assumed total commit, then the null miner ought to win with probability (1 - X)%.
    /// However, the null miner is advantaged more if the missing carryover is smaller.  This is
    /// captured with the extra `atc * adv(atc)` term.
    pub(crate) fn null_miner_probability(atc: AtcRational) -> AtcRational {
        // compute min(1.0, (1.0 - atc) + (atc * adv))
        let adv = Self::null_miner_logistic(atc);
        let Some(one_minus_atc) = AtcRational::one().sub(&atc) else {
            // somehow, ATC > 1.0, then miners spent more than they did in the last sortition.
            // So, the null miner loses.
            warn!("ATC > 1.0 ({})", &atc.to_hex());
            return AtcRational::zero();
        };

        let Some(atc_prod_adv) = atc.mul(&adv) else {
            // if this is somehow too big (impossible), it would otherwise imply that the null
            // miner advantage is overwhelming
            warn!("ATC * ADV == INF ({} * {})", &atc.to_hex(), &adv.to_hex());
            return AtcRational::one();
        };

        let Some(sum) = one_minus_atc.add(&atc_prod_adv) else {
            // if this is somehow too big (impossible), it would otherwise imply that the null
            // miner advantage is overwhelming
            warn!(
                "(1.0 - ATC) + (ATC * ADV) == INF ({} * {})",
                &one_minus_atc.to_hex(),
                &atc_prod_adv.to_hex()
            );
            return AtcRational::one();
        };
        sum.min(&AtcRational::one())
    }

    /// Determine whether or not the null miner has won sortition.
    /// This works by creating a second burn distribution: one with the winning block-commit, and
    /// one with the null miner.  The null miner's mining power will be computed as a function of
    /// their ATC advantage.
    fn null_miner_wins(
        sort_tx: &mut SortitionHandleTx,
        block_header: &BurnchainBlockHeader,
        sortition_hash: &SortitionHash,
        commit_winner: &LeaderBlockCommitOp,
        atc: AtcRational,
    ) -> Result<bool, db_error> {
        let vrf_seed = Self::get_last_vrf_seed(sort_tx, block_header)?;

        let mut null_winner = commit_winner.clone();
        null_winner.block_header_hash = {
            // make the block header hash different, to render it different from the winner.
            // Just flip the block header bits.
            let mut bhh_bytes = null_winner.block_header_hash.0.clone();
            for byte in bhh_bytes.iter_mut() {
                *byte = !*byte;
            }
            BlockHeaderHash(bhh_bytes)
        };

        let mut null_sample_winner = BurnSamplePoint::zero(null_winner.clone());
        let mut burn_sample_winner = BurnSamplePoint::zero(commit_winner.clone());

        let null_prob = Self::null_miner_probability(atc);
        let null_prob_u256 = null_prob.into_sortition_probability();

        test_debug!(
            "atc = {}, null_prob = {}, null_prob_u256 = {}, sortition_hash: {}",
            atc.to_hex(),
            null_prob.to_hex(),
            null_prob_u256.to_hex_be(),
            sortition_hash
        );
        null_sample_winner.range_start = Uint256::zero();
        null_sample_winner.range_end = null_prob_u256;

        burn_sample_winner.range_start = null_prob_u256;
        burn_sample_winner.range_end = Uint256::max();

        let burn_dist = [
            // the only fields that matter here are:
            // * range_start
            // * range_end
            // * candidate
            null_sample_winner,
            burn_sample_winner,
        ];

        // pick the next winner
        let Some(win_idx) =
            BlockSnapshot::sample_burn_distribution(&burn_dist, &vrf_seed, sortition_hash)
        else {
            // miner wins by default if there's no winner index
            return Ok(false);
        };

        test_debug!("win_idx = {}", win_idx);

        // null miner is index 0
        Ok(win_idx == 0)
    }

    /// Make a block snapshot from is block's data and the previous block.
    /// This process will:
    /// * calculate the new consensus hash
    /// * calculate the total burn so far
    /// * determine whether or not we can do a sortition, and if so,
    /// * carry out the sortition to select the next candidate block.
    ///
    /// All of this is rolled into the BlockSnapshot struct.
    ///
    /// Call this *after* you store all of the block's transactions to the burn db.
    pub fn make_snapshot(
        mainnet: bool,
        sort_tx: &mut SortitionHandleTx,
        burnchain: &Burnchain,
        my_sortition_id: &SortitionId,
        my_pox_id: &PoxId,
        parent_snapshot: &BlockSnapshot,
        block_header: &BurnchainBlockHeader,
        state_transition: &BurnchainStateTransition,
        initial_mining_bonus_ustx: u128,
    ) -> Result<BlockSnapshot, db_error> {
        // what epoch will this snapshot be in?
        let epoch_id = SortitionDB::get_stacks_epoch(sort_tx, parent_snapshot.block_height + 1)?
            .unwrap_or_else(|| {
                panic!(
                    "FATAL: no epoch defined at burn height {}",
                    parent_snapshot.block_height + 1
                )
            })
            .epoch_id;

        Self::make_snapshot_in_epoch(
            mainnet,
            sort_tx,
            burnchain,
            my_sortition_id,
            my_pox_id,
            parent_snapshot,
            block_header,
            state_transition,
            initial_mining_bonus_ustx,
            epoch_id,
        )
    }

    pub fn make_snapshot_in_epoch(
        mainnet: bool,
        sort_tx: &mut SortitionHandleTx,
        burnchain: &Burnchain,
        my_sortition_id: &SortitionId,
        my_pox_id: &PoxId,
        parent_snapshot: &BlockSnapshot,
        block_header: &BurnchainBlockHeader,
        state_transition: &BurnchainStateTransition,
        initial_mining_bonus_ustx: u128,
        epoch_id: StacksEpochId,
    ) -> Result<BlockSnapshot, db_error> {
        assert_eq!(
            parent_snapshot.burn_header_hash,
            block_header.parent_block_hash
        );
        assert_eq!(parent_snapshot.block_height + 1, block_header.block_height);

        let block_height = block_header.block_height;
        let block_hash = block_header.block_hash.clone();
        let parent_block_hash = block_header.parent_block_hash.clone();
        let first_block_height = burnchain.first_block_height;

        let last_sortition_hash = parent_snapshot.sortition_hash.clone();
        let last_burn_total = parent_snapshot.total_burn;

        let accumulated_coinbase_ustx = if parent_snapshot.total_burn == 0 {
            0
        } else if parent_snapshot.sortition {
            initial_mining_bonus_ustx
        } else {
            let missed_coinbase = StacksChainState::get_coinbase_reward(
                epoch_id,
                mainnet,
                parent_snapshot.block_height,
                first_block_height,
            );
            parent_snapshot
                .accumulated_coinbase_ustx
                .saturating_add(missed_coinbase)
                .saturating_add(initial_mining_bonus_ustx)
        };

        // next sortition hash
        let next_sortition_hash = last_sortition_hash.mix_burn_header(&block_hash);
        let mut make_snapshot_no_sortition = || {
            BlockSnapshot::make_snapshot_no_sortition(
                sort_tx,
                my_sortition_id,
                my_pox_id,
                parent_snapshot,
                block_header,
                first_block_height,
                last_burn_total,
                &next_sortition_hash,
                &state_transition.txids(),
                accumulated_coinbase_ustx,
            )
        };

        if state_transition.burn_dist.is_empty() {
            // no burns happened
            debug!(
                "No burns happened in block";
                "burn_block_height" => %block_height.to_string(),
                "burn_block_hash" => %block_hash.to_string(),
            );

            return make_snapshot_no_sortition();
        }

        // NOTE: this only counts burns from leader block commits and user burns that match them.
        // It ignores user burns that don't match any block.
        let block_burn_total = match state_transition.total_burns() {
            Some(total) => {
                if total == 0 {
                    // no one burned, so no sortition
                    debug!(
                        "No transactions submitted burns in block";
                        "burn_block_height" => %block_height.to_string(),
                        "burn_block_hash" => %block_hash.to_string(),
                    );
                    return make_snapshot_no_sortition();
                } else {
                    total
                }
            }
            None => {
                // overflow -- treat as 0 (no sortition)
                warn!("Burn count exceeds maximum threshold");
                return make_snapshot_no_sortition();
            }
        };

        // total burn.  If this ever overflows, then just stall the chain and deny all future
        // sortitions (at least the chain will remain available to serve queries, but it won't be
        // able to make progress).
        let next_burn_total = match last_burn_total.checked_add(block_burn_total) {
            Some(new_total) => new_total,
            None => {
                // overflow.  Deny future sortitions
                warn!("Cumulative sortition burn has overflown.  Subsequent sortitions will be denied.");
                return make_snapshot_no_sortition();
            }
        };

        // Try to pick a next block.
        let (winning_block, winning_block_burn_dist_index) = BlockSnapshot::select_winning_block(
            sort_tx,
            block_header,
            &next_sortition_hash,
            &state_transition.burn_dist,
        )?
        .expect("FATAL: there must be a winner if the burn distribution has 1 or more points");

        // in epoch 3.x and later (Nakamoto and later), there's two additional changes:
        // * if the winning miner didn't mine in more than k of n blocks of the window, then their chances of
        // winning are 0.
        // * There exists a "null miner" that can win sortition, in which case there is no
        // sortition.  This happens if the assumed total commit with carry-over is sufficiently low.
        let mut reject_winner_reason = None;
        if epoch_id >= StacksEpochId::Epoch30 {
            if !Self::check_miner_is_active(
                epoch_id,
                state_transition.windowed_block_commits.len(),
                &winning_block.apparent_sender,
                state_transition.burn_dist[winning_block_burn_dist_index].frequency,
            ) {
                reject_winner_reason = Some("Miner did not mine often enough to win".to_string());
            }
            let (atc, null_active) = Self::get_miner_commit_carryover(
                state_transition.total_burns(),
                state_transition.windowed_median_burns(),
            );
            if null_active && reject_winner_reason.is_none() {
                // there's a chance the null miner can win
                if Self::null_miner_wins(
                    sort_tx,
                    block_header,
                    &next_sortition_hash,
                    &winning_block,
                    atc,
                )? {
                    // null wins
                    reject_winner_reason = Some(
                        "Null miner defeats block winner due to insufficient commit carryover"
                            .to_string(),
                    );
                }
            }
        }

        if let Some(reject_winner_reason) = reject_winner_reason {
            info!("SORTITION({block_height}): WINNER REJECTED: {reject_winner_reason:?}";
                  "txid" => %winning_block.txid,
                  "stacks_block_hash" => %winning_block.block_header_hash,
                  "burn_block_hash" => %winning_block.burn_header_hash);

            // N.B. can't use `make_snapshot_no_sortition()` helper here because then `sort_tx`
            // would be mutably borrowed twice.
            return BlockSnapshot::make_snapshot_no_sortition(
                sort_tx,
                my_sortition_id,
                my_pox_id,
                parent_snapshot,
                block_header,
                first_block_height,
                last_burn_total,
                &next_sortition_hash,
                &state_transition.txids(),
                accumulated_coinbase_ustx,
            );
        }

        // mix in the winning block's VRF seed to the sortition hash.  The next block commits must
        // prove on this final sortition hash.
        let final_sortition_hash = next_sortition_hash.mix_VRF_seed(&winning_block.new_seed);
        let next_ops_hash = OpsHash::from_txids(&state_transition.txids());
        let next_ch = ConsensusHash::from_parent_block_data(
            sort_tx,
            &next_ops_hash,
            block_height - 1,
            first_block_height,
            &block_hash,
            next_burn_total,
            my_pox_id,
        )?;

        info!("SORTITION({block_height}): WINNER SELECTED";
            "txid" => %winning_block.txid,
            "stacks_block_hash" => %winning_block.block_header_hash,
            "burn_block_hash" => %winning_block.burn_header_hash);

        let miner_pk_hash = sort_tx
            .get_leader_key_at(
                winning_block.key_block_ptr.into(),
                winning_block.key_vtxindex.into(),
                &parent_snapshot.sortition_id,
            )?
            .and_then(|key_op| key_op.interpret_nakamoto_signing_key());

        Ok(BlockSnapshot {
            block_height,
            burn_header_hash: block_hash,
            burn_header_timestamp: block_header.timestamp,
            parent_burn_header_hash: parent_block_hash,
            consensus_hash: next_ch,
            ops_hash: next_ops_hash,
            total_burn: next_burn_total,
            sortition: true,
            sortition_hash: final_sortition_hash,
            winning_block_txid: winning_block.txid,
            winning_stacks_block_hash: winning_block.block_header_hash,
            index_root: TrieHash::from_empty_data(), // will be overwritten,
            num_sortitions: parent_snapshot.num_sortitions + 1,
            stacks_block_accepted: false,
            stacks_block_height: 0,
            arrival_index: 0,
            canonical_stacks_tip_height: parent_snapshot.canonical_stacks_tip_height,
            canonical_stacks_tip_hash: parent_snapshot.canonical_stacks_tip_hash.clone(),
            canonical_stacks_tip_consensus_hash: parent_snapshot
                .canonical_stacks_tip_consensus_hash
                .clone(),
            sortition_id: my_sortition_id.clone(),
            parent_sortition_id: parent_snapshot.sortition_id.clone(),
            pox_valid: true,
            accumulated_coinbase_ustx,
            miner_pk_hash,
        })
    }
}

#[cfg(test)]
mod test {
    use stacks_common::address::*;
    use stacks_common::types::chainstate::{BlockHeaderHash, BurnchainHeaderHash, VRFSeed};
    use stacks_common::util::get_epoch_time_secs;
    use stacks_common::util::hash::hex_bytes;
    use stacks_common::util::vrf::{VRFPrivateKey, VRFPublicKey};

    use super::*;
    use crate::burnchains::tests::*;
    use crate::burnchains::{BurnchainSigner, *};
    use crate::chainstate::burn::atc::AtcRational;
    use crate::chainstate::burn::db::sortdb::tests::test_append_snapshot_with_winner;
    use crate::chainstate::burn::db::sortdb::*;
    use crate::chainstate::burn::operations::leader_block_commit::BURN_BLOCK_MINED_AT_MODULUS;
    use crate::chainstate::burn::operations::*;
    use crate::chainstate::stacks::*;

    fn test_make_snapshot(
        sort_tx: &mut SortitionHandleTx,
        burnchain: &Burnchain,
        my_sortition_id: &SortitionId,
        my_pox_id: &PoxId,
        parent_snapshot: &BlockSnapshot,
        block_header: &BurnchainBlockHeader,
        burnchain_state_transition: &BurnchainStateTransition,
    ) -> Result<BlockSnapshot, db_error> {
        BlockSnapshot::make_snapshot(
            false,
            sort_tx,
            burnchain,
            my_sortition_id,
            my_pox_id,
            parent_snapshot,
            block_header,
            burnchain_state_transition,
            0,
        )
    }

    #[test]
    fn make_snapshot_no_sortition() {
        let first_burn_hash = BurnchainHeaderHash::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000123",
        )
        .unwrap();
        let first_block_height = 120;

        let burnchain = Burnchain {
            pox_constants: PoxConstants::test_default(),
            peer_version: 0x012345678,
            network_id: 0x9abcdef0,
            chain_name: "bitcoin".to_string(),
            network_name: "testnet".to_string(),
            working_dir: "/nope".to_string(),
            consensus_hash_lifetime: 24,
            stable_confirmations: 7,
            first_block_timestamp: 0,
            first_block_height,
            initial_reward_start_block: first_block_height,
            first_block_hash: first_burn_hash.clone(),
        };

        let mut db = SortitionDB::connect_test(first_block_height, &first_burn_hash).unwrap();

        let empty_block_header = BurnchainBlockHeader {
            block_height: first_block_height + 1,
            block_hash: BurnchainHeaderHash([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0x01, 0x24,
            ]),
            parent_block_hash: first_burn_hash.clone(),
            num_txs: 0,
            timestamp: get_epoch_time_secs(),
        };

        let initial_snapshot = SortitionDB::get_first_block_snapshot(db.conn()).unwrap();

        let snapshot_no_transactions = {
            let pox_id = PoxId::stubbed();
            let sort_id = SortitionId::stubbed(&empty_block_header.block_hash);
            let mut ic = SortitionHandleTx::begin(&mut db, &sort_id).unwrap();
            let sn = test_make_snapshot(
                &mut ic,
                &burnchain,
                &sort_id,
                &pox_id,
                &initial_snapshot,
                &empty_block_header,
                &BurnchainStateTransition::noop(),
            )
            .unwrap();
            sn
        };

        assert!(!snapshot_no_transactions.sortition);
        assert_eq!(snapshot_no_transactions.total_burn, 0);

        let key = LeaderKeyRegisterOp::new_from_secrets(
            1,
            &AddressHashMode::SerializeP2PKH,
            &VRFPrivateKey::new(),
        )
        .unwrap();

        let empty_burn_point = BurnSamplePoint {
            burns: 0,
            median_burn: 0,
            range_start: Uint256::from_u64(0),
            range_end: Uint256([
                0xFFFFFFFFFFFFFFFF,
                0xFFFFFFFFFFFFFFFF,
                0xFFFFFFFFFFFFFFFF,
                0xFFFFFFFFFFFFFFFF,
            ]),
            frequency: 10,
            candidate: LeaderBlockCommitOp::initial(
                &BlockHeaderHash([1u8; 32]),
                first_block_height + 1,
                &VRFSeed::initial(),
                &key,
                0,
                &(Txid([0; 32]), 0),
                &BurnchainSigner::new_p2pkh(
                    &StacksPublicKey::from_hex(
                        "03ef2340518b5867b23598a9cf74611f8b98064f7d55cdb8c107c67b5efcbc5c77",
                    )
                    .unwrap(),
                ),
            ),
        };

        let snapshot_no_burns = {
            let sort_id = SortitionId::stubbed(&empty_block_header.block_hash);
            let pox_id = PoxId::stubbed();
            let mut ic = SortitionHandleTx::begin(&mut db, &sort_id).unwrap();
            let sn = test_make_snapshot(
                &mut ic,
                &burnchain,
                &sort_id,
                &pox_id,
                &initial_snapshot,
                &empty_block_header,
                &BurnchainStateTransition {
                    burn_dist: vec![empty_burn_point.clone()],
                    accepted_ops: vec![BlockstackOperationType::LeaderKeyRegister(key.clone())],
                    ..BurnchainStateTransition::noop()
                },
            )
            .unwrap();
            sn
        };

        assert!(!snapshot_no_burns.sortition);
        assert_eq!(snapshot_no_transactions.total_burn, 0);
    }

    #[test]
    fn test_check_is_miner_active() {
        assert_eq!(StacksEpochId::Epoch30.mining_commitment_frequency(), 3);
        assert_eq!(StacksEpochId::Epoch25.mining_commitment_frequency(), 0);

        // reward phase
        assert!(BlockSnapshot::check_miner_is_active(
            StacksEpochId::Epoch30,
            6,
            &BurnchainSigner("".to_string()),
            6
        ));
        assert!(BlockSnapshot::check_miner_is_active(
            StacksEpochId::Epoch30,
            6,
            &BurnchainSigner("".to_string()),
            5
        ));
        assert!(BlockSnapshot::check_miner_is_active(
            StacksEpochId::Epoch30,
            6,
            &BurnchainSigner("".to_string()),
            4
        ));
        assert!(BlockSnapshot::check_miner_is_active(
            StacksEpochId::Epoch30,
            6,
            &BurnchainSigner("".to_string()),
            3
        ));
        assert!(!BlockSnapshot::check_miner_is_active(
            StacksEpochId::Epoch30,
            6,
            &BurnchainSigner("".to_string()),
            2
        ));

        // prepare phase
        assert!(BlockSnapshot::check_miner_is_active(
            StacksEpochId::Epoch30,
            1,
            &BurnchainSigner("".to_string()),
            5
        ));
        assert!(BlockSnapshot::check_miner_is_active(
            StacksEpochId::Epoch30,
            1,
            &BurnchainSigner("".to_string()),
            4
        ));
        assert!(BlockSnapshot::check_miner_is_active(
            StacksEpochId::Epoch30,
            1,
            &BurnchainSigner("".to_string()),
            3
        ));
        assert!(BlockSnapshot::check_miner_is_active(
            StacksEpochId::Epoch30,
            1,
            &BurnchainSigner("".to_string()),
            2
        ));
        assert!(BlockSnapshot::check_miner_is_active(
            StacksEpochId::Epoch30,
            1,
            &BurnchainSigner("".to_string()),
            1
        ));
        assert!(!BlockSnapshot::check_miner_is_active(
            StacksEpochId::Epoch30,
            1,
            &BurnchainSigner("".to_string()),
            0
        ));
    }

    #[test]
    fn test_get_miner_commit_carryover() {
        assert_eq!(
            BlockSnapshot::get_miner_commit_carryover(None, None),
            (AtcRational::zero(), false)
        );
        assert_eq!(
            BlockSnapshot::get_miner_commit_carryover(None, Some(1)),
            (AtcRational::zero(), false)
        );
        assert_eq!(
            BlockSnapshot::get_miner_commit_carryover(Some(1), None),
            (AtcRational::zero(), false)
        );

        // ATC increased
        assert_eq!(
            BlockSnapshot::get_miner_commit_carryover(Some(1), Some(1)),
            (AtcRational::one(), false)
        );
        assert_eq!(
            BlockSnapshot::get_miner_commit_carryover(Some(2), Some(1)),
            (AtcRational::one(), false)
        );

        // no carried commit
        assert_eq!(
            BlockSnapshot::get_miner_commit_carryover(Some(2), Some(0)),
            (AtcRational::zero(), true)
        );

        // assumed carryover
        assert_eq!(
            BlockSnapshot::get_miner_commit_carryover(Some(2), Some(4)),
            (AtcRational::frac(2, 4), true)
        );
    }

    #[test]
    fn test_null_miner_logistic() {
        for i in 0..1024 {
            let atc_u256 = ATC_LOOKUP[i];
            let null_miner_lgst =
                BlockSnapshot::null_miner_logistic(AtcRational::frac(i as u64, 1024));
            assert_eq!(null_miner_lgst, atc_u256);
        }
        assert_eq!(
            BlockSnapshot::null_miner_logistic(AtcRational::zero()),
            ATC_LOOKUP[0]
        );
        assert_eq!(
            BlockSnapshot::null_miner_logistic(AtcRational::one()),
            *ATC_LOOKUP.last().as_ref().cloned().unwrap()
        );
        assert_eq!(
            BlockSnapshot::null_miner_logistic(AtcRational::frac(100, 1)),
            *ATC_LOOKUP.last().as_ref().cloned().unwrap()
        );
    }

    /// This test runs 100 sortitions, and in each sortition, it verifies that the null miner will
    /// win for the range of ATC-C values which put the sortition index into the null miner's
    /// BurnSamplePoint range.  The ATC-C values directly influence the null miner's
    /// BurnSamplePoint range, so given a fixed sortition index, we can verify that the
    /// `null_miner_wins()` function returns `true` exactly when the sortition index falls into the
    /// null miner's range.  The ATC-C values are sampled through linear interpolation between 0.0
    /// and 1.0 in steps of 0.01.
    #[test]
    fn test_null_miner_wins() {
        let first_burn_hash = BurnchainHeaderHash([0xfe; 32]);
        let parent_first_burn_hash = BurnchainHeaderHash([0xff; 32]);
        let first_block_height = 120;

        let mut prev_block_header = BurnchainBlockHeader {
            block_height: first_block_height,
            block_hash: first_burn_hash.clone(),
            parent_block_hash: parent_first_burn_hash.clone(),
            num_txs: 0,
            timestamp: 12345,
        };

        let burnchain = Burnchain {
            pox_constants: PoxConstants::test_default(),
            peer_version: 0x012345678,
            network_id: 0x9abcdef0,
            chain_name: "bitcoin".to_string(),
            network_name: "testnet".to_string(),
            working_dir: "/nope".to_string(),
            consensus_hash_lifetime: 24,
            stable_confirmations: 7,
            first_block_timestamp: 0,
            first_block_height,
            initial_reward_start_block: first_block_height,
            first_block_hash: first_burn_hash.clone(),
        };

        let mut db = SortitionDB::connect_test(first_block_height, &first_burn_hash).unwrap();

        for i in 0..100 {
            let header = BurnchainBlockHeader {
                block_height: prev_block_header.block_height + 1,
                block_hash: BurnchainHeaderHash([i; 32]),
                parent_block_hash: prev_block_header.block_hash.clone(),
                num_txs: 0,
                timestamp: prev_block_header.timestamp + u64::from(i) + 1,
            };

            let sortition_hash = SortitionHash([i; 32]);

            let commit_winner = LeaderBlockCommitOp {
                sunset_burn: 0,
                block_header_hash: BlockHeaderHash([i; 32]),
                new_seed: VRFSeed([i; 32]),
                parent_block_ptr: 0,
                parent_vtxindex: 0,
                key_block_ptr: 0,
                key_vtxindex: 0,
                memo: vec![0x80],
                commit_outs: vec![],

                burn_fee: 100,
                input: (Txid([0; 32]), 0),
                apparent_sender: BurnchainSigner(format!("signer {i}")),
                txid: Txid([i; 32]),
                vtxindex: 0,
                block_height: header.block_height,
                burn_parent_modulus: i % BURN_BLOCK_MINED_AT_MODULUS as u8,
                burn_header_hash: header.block_hash.clone(),
                treatment: vec![],
            };

            let tip = SortitionDB::get_canonical_burn_chain_tip(db.conn()).unwrap();
            test_append_snapshot_with_winner(
                &mut db,
                header.block_hash.clone(),
                &[BlockstackOperationType::LeaderBlockCommit(
                    commit_winner.clone(),
                )],
                Some(tip),
                Some(commit_winner.clone()),
            );

            let mut sort_tx = db.tx_begin_at_tip();

            for j in 0..100 {
                let atc = AtcRational::from_f64_unit((j as f64) / 100.0);
                let null_prob = BlockSnapshot::null_miner_probability(atc);

                // NOTE: this tests .into_sortition_probability()
                let null_prob_u256 = if null_prob.inner() >= AtcRational::one().inner() {
                    // prevent left-shift overflow
                    AtcRational::one_sup().into_inner() << 192
                } else {
                    null_prob.into_inner() << 192
                };

                let null_wins = BlockSnapshot::null_miner_wins(
                    &mut sort_tx,
                    &header,
                    &sortition_hash,
                    &commit_winner,
                    atc,
                )
                .unwrap();
                debug!("null_wins: {},{}: {}", i, j, null_wins);

                let vrf_seed = BlockSnapshot::get_last_vrf_seed(&mut sort_tx, &header).unwrap();
                let index = sortition_hash.mix_VRF_seed(&vrf_seed).to_uint256();

                if index < null_prob_u256 {
                    assert!(null_wins);
                } else {
                    assert!(!null_wins);
                }
            }

            prev_block_header = header.clone();
        }
    }
}
