// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2021 Stacks Open Internet Foundation
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

/// ## The Problem
///
/// There were two related design flaws in the way the Stacks blockchain deals with PoX anchor blocks:
///
/// * If it is ever the case in which a PoX anchor block is missing, and yet somehow manages to achieve 80% or more
/// confirmations during the prepare phase, then the subsequent arrival of that anchor block will cause a _deep_ chain
/// reorg.  It doesn't matter how many future blocks get mined -- if the anchor block is later revealed, it will
/// invalidate all of the blocks that did not build on it.  While mining and confirming an anchor block is very costly,
/// it's not only possible, but profitable: anyone who manages to do this could hold the blockchain for ransom by
/// threatening to disclose the anchor block and invaldiate all blocks after it unless they were paid not to (i.e. in
/// perpetuity).
///
/// * If it is ever the case that not enough STX get locked for PoX to begin in reward cycle _R_, then a node that
/// processes Stacks blocks first without the anchor block in _R_ and then with the anchor block in _R_ will crash
/// because it will attempt to calculate the same sortition twice.  This is because the same block-commits would be
/// processed in both cases -- they'd both be PoB commits.
///
/// This subsystem fixes both problems by making the _history of anchor blocks itself_ forkable, and by implementing
/// _Nakamoto consensus_ on the anchor block history forks so that there will always be a canonical anchor block
/// history.  In doing so, the Stacks blockchain now has _three_ levels of forks: the Bitcoin chain, the history of PoX
/// anchor blocks, and the history of Stacks blocks.  The canonical Stacks fork is the longest history of Stacks blocks
/// that passes through the canonical history of anchor blocks which resides on the canonical Bitcoin chain.
///
/// ## Background: Sortition Histories
///
/// Recall that each Bitcoin block can contain block-commits that are valid only if certain anchor blocks are known to
/// the node, and invalid if other anchor blocks are known.  Specifically, a block-commit can be a valid PoX
/// block-commit _only if_ the current reward cycle has an anchor block, _and_ that anchor block is known to the node.
/// Otherwise, if the block-commit does not descend from the anchor block, or there is no anchor block for this reward
/// cycle, then the block-commit can only be valid if it's a PoB block-commit.
///
/// What this means is that there is a _set_ of sortition histories on the Bitcoin chainstate that will each yield a
/// unique history of block-commits (which in turn represent a unique set of possible Stacks forks).  This set has
/// _O(2**n)_ members, where _n_ is the number of reward cycles that have anchor blocks.  This is because each time a
/// new reward cycle is processed with an anchor block, there will be a sortition history that descends from it in which
/// the anchor block is known to the node, and a sortition history in which it is _not_ known.
///
/// Which sortition history is the "true" sortition history, and how do we determine this?  This is what this subsystem
/// addresses.
///
/// ## Solution: Weight Sortition Histories by Miner Affirmations
///
/// Can we deduce whether or not an anchor block _should_ exist and be known to the network, using only Bitcoin
/// chainstate?  A likely anchor block's block-commit will have at least 80 confirmations in the prepare phase -- at
/// least F*w (i.e. 80) Bitcoin blocks will contain at least one block-commit that has the likely anchor block-commit as
/// an ancestor.
///
/// Of course, there are competing block-commits in each Bitcoin block; only one will be chosen as the Stacks block.
/// But, recall that in the prepare phase of a reward cycle, all miners must burn BTC.  Because miners are sending BTC
/// to the burn address, you can _compare_ the economic worth of all block-commits within a prepare-phase block.
/// Moreover, you can calculate how much BTC went into confirming a likely anchor block's block-commit.  In doing so, we
/// can introduce an extra criterion for selecting the anchor block in a reward cycle:
///
/// **The PoX anchor block for reward cycle _R_ is a Stacks block that has not yet been chosen to be an anchor block,
/// and is the highest block outside _R_'s prepare phase that has at least F*w confirmations and is confirmed by the
/// most BTC burnt.**
///
/// This is slightly different than the definition in SIP-007.  We're only looking at block-commits now.  If there are
/// two or more reward-phase block-commits that got F*w confirmations, then we select the block-commit that got the most
/// BTC.  If this block-commit doesn't actually correspond to a Stacks block, then there is no anchor block for the
/// reward cycle.  Also, if this block-commit has been an anchor block before in some prior reward cycle, then there is
/// no anchor block for this reward cycle.  If Stacks miners are honest, and no Stacks miner has more than 80% of the
/// mining power, then neither of these two cases arise -- Stacks miners will build Stacks blocks on top of blocks they
/// know about, and their corresponding block-commits in the prepare-phase will confirm the block-commit for an anchor
/// block the miners believe exists.
///
/// The key insight into understanding the solution to #1805 is to see that the act of choosing an anchor block is
/// _also_ the acts of doing the following two things:
///
/// * Picking a likely anchor block-commit is the act of _affirming_ that the anchor block is known to the network.  A
/// bootstrapping node does not know which Stacks blocks actually exist, since it needs to go and actually download
/// them.  But, it can examine only the Bitcoin chainstate and deduce the likely anchor block for each reward cycle.  If
/// a reward cycle has a likely anchor block-commit, then we say that the set of miners who mined that prepare-phase
/// have _affirmed_ to this node and all future bootstrapping nodes that they believed that this anchor block exists.  I
/// say "affirmed" because it's a weaker guarantee than "confirmed" -- the anchor block can still get lost after the
/// miners make their affirmations.
///
/// * Picking a likely anchor block-commit is the act of affirming all of the previous affirmations that this anchor
/// block represents.  An anchor block is a descendant of a history of prior anchor blocks, so miners affirming that it
/// exists by sending block-commits that confirm its block-commit is also the act of miners affirming that all of the
/// ancestor anchor blocks it confirms also exist.  For example, if there are 4 reward cycles, and cycles 1, 2, and 3
/// have anchor blocks, then the act of miners choosing an anchor block in reward cycle 4's prepare phase that descends
/// from the anchor block in reward cycle 3 is _also_ the act of affirming that the anchor block for reward cycle 3
/// exists.  If the anchor block for reward cycle 3 descends from the anchor block of reward cycle 1, but _not_ from the
/// anchor block in reward cycle 2, then the miners have also affirmed that the anchor block for reward cycle 1 exists.
/// Moreover, the anchor block in reward cycle 1 has been affirmed _twice_ -- both by the miners in reward cycle 3's
/// prepare phase, and the miners in reward cycle 4's prepare phase.  The anchor block in reward cycle 2 has _not_ been
/// affirmed.
///
/// The act of building anchor blocks on top of anchor blocks gives us a way to _weight_ the corresponding sortition
/// histories.  An anchor block gets "heavier" as the number of descendant anchor blocks increases, and as the number of
/// reward cycles without anchor blocks increases.  This is because in both cases, miners are _not_ working on an anchor
/// block history that would _invalidate_ this anchor block -- i.e. they are continuously affirming that this anchor
/// block exists.
///
/// We can define the weight of a sortition history as the weight of its heaviest anchor block.  If you want to produce
/// a sortition history that is heavier, but invalidates the last _N_ anchor blocks, you'll have to mine at least _N +
/// 1_ reward cycles.  This gets us a form of Nakamoto consensus for the status of anchor blocks -- the more affirmed an
/// anchor block is, the harder it is to get it unaffirmed.  By doing this, we address the first problem with PoX anchor
/// blocks: in order to hold the chain hostage, you have to _continuously_ mine reward cycles that confirm your missing
/// anchor block.
///
/// ## Implementation: Affirmation Maps
///
/// We track this information through a data structure called an **affirmation map**.  An affirmation map has the
/// following methods:
///
/// * `at(i)`: Determine the network's affirmation status of the anchor block for the _ith_ reward cycle, starting at
/// reward cycle 1 (reward cycle 0 has no anchor block, ever).  The domain of `i` is defined as the set of reward cycles
/// known to the node, excluding 0, and evaluates to one of the following:
///
///    * `p`: There is an anchor block, and it's present
///    * `a`: There is an anchor block, and it's absent
///    * `n`: There is no anchor block
///
/// * `weight()`:  This returns the maximum number of anchor blocks that descend from an anchor block this affirmation
/// map represents
///
/// Each block-commit represents an affirmation by the miner about the state of the anchor blocks that the
/// block-commit's Stacks block confirms.  When processing block-commits, the node will calculate the affirmation map
/// for each block-commit inductively as follows:
///
///    * If the block-commit is in the prepare phase for reward cycle _R_:
///
///         * If there is an anchor block for _R_:
///
///             * If this commit descends from the anchor block, then its affirmation map is the same as the anchor
///               block's, plus having `at(R)` set to `p`
///
///             * Otherwise, its affirmation map the same as the anchor block's, plus having `at(R)`set to `a`
///
///         * Otherwise:
///
///             * If the parent descended from some anchor block at reward cycle _R - k_ then this commit's affirmation
///               map is the same as its parent, plus having `at(R - k)` set to `p`, plus having all `at(R - k < x < R)`
///               set to `n` if reward cycle _x_ doesn't have an anchor block, and `a` if it does.
///
///             * Otherwise, this commit's affirmation map is defined as `at(x)` set to `n` if reward cycle _x_ doesn't
///               have an anchor block, and `a` if it does.
///
///    * Otherwise:
///
///         * If the parent descended from some anchor block in reward cycle _R - k_, then this commit's affirmation map
///           is the same as its parent, plus having `at(R - k < x < R)` set to `n` if reward cycle _x_ doesn't have an
///           anchor block, and `a` if it does.
///
///         * Otherwise, this commit's affirmation map is defined as `at(x)` set to `n` if reward cycle _x_ doesn't have
///           an anchor block, and `a` if it does.
///       
/// Consider the example above, where we have anchor block histories 1,3,4 and 1,2.
///
/// * A block-commit in the prepare-phase for reward cycle 4 that confirms the anchor block for reward cycle 4 would
/// have affirmation map `papp`, because it affirms that the anchor blocks for reward cycles 1, 3, and 4 exist.
///
/// * A block-commit in the prepare-phase for reward cycle 4 that does NOT confirm the anchor block for reward cycle 4, but
/// descends from a block that descends from the anchor block in reward cycle 3, would have the affirmation map `papa`,
/// because it does NOT affirm that the anchor block for reward cycle 4 exists, but it DOES affirm that the anchor block
/// history terminating at the anchor block for reward cycle 3 exists.
///
/// * A block-commit in the prepare-phase for reward cycle 4 that descends from a block that descends from the anchor block
/// for reward cycle 2 would have affirmation map `ppaa`, because it builds on the anchor block for reward cycle 2, but it
/// doesn't build on the anchor blocks for 3 and 4.
///
/// * Suppose reward cycle 5 rolls around, and no anchor block is chosen at all.  Then, a block in the reward
/// phase for reward cycle 5 that builds off the anchor block in reward cycle 4 would have affirmation map `pappn`.
/// Similarly, a block in reward cycle 5's reward phase that builds off of the anchor block in reward cycle 2 would have
/// affirmation map `ppaan`.
///
/// (Here's a small lemma:  if any affirmation map has `at(R) = n` for a given reward cycle `R`, then _all_ affirmation
/// maps will have `at(R) == n`).
///
/// Now that we have a way to measure affirmations on anchor blocks, we can use them to deduce a canonical sortition
/// history as simply the history that represents the affirmation map with the highest `weight()` value.  If there's a
/// tie, then we pick the affirmation map with the highest `i` such that `at(i) = p` (i.e. a later anchor block
/// affirmation is a stronger affirmation than an earlier one).  This is always a tie-breaker, because each
/// prepare-phase either affirms or does not affirm exactly one anchor block.
///
/// ### Using Affirmation Maps
///
/// Each time we finish processing a reward cycle, the burnchain processor identifies the anchor block's commit and
/// updates the affirmation maps for the prepare-phase block-commits in the burnchain DB (now that an anchor block
/// decision has been made).  As the DB receives subsequent reward-phase block-commits, their affirmation maps are
/// calculated using the above definition.
///
/// Each time the chains coordinator processes a burnchain block, it sees if its view of the heaviest affirmation map
/// has changed.  If so, it executes a PoX reorg like before -- it invalidates the sortitions back to the latest
/// sortition that is represented on the now-heaviest affirmation map.  Unlike before, it will _re-validate_ any
/// sortitions that it has processed in the past if a _prefix_ of the now-heaviest affirmation map has been the heaviest
/// affirmation map in the past.  This can arise if there are two competing sets of miners that are fighting over two
/// different sortition histories.  In this case, it also forgets the orphaned statuses of all invalidated and
/// re-validated Stacks blocks, so they can be downloaded and applied again to the Stacks chain state (note that a
/// Stacks block will be applied at most once in any case -- it's just that it can be an orphan on one sortition
/// history, but a valid and accepted block in another).
///
/// Because we take care to re-validate sortitions that have already been processed, we avoid the second design flaw in
/// the PoX anchor block handling -- a sortition will always be processed at most once.  This is further guaranteed by
/// making sure that the consensus hash for each sortition is calculated in part from the PoX bit vector that is
/// _induced_ by the heaviest affirmation map.  That is, the node's PoX ID is no longer calculated from the presence or
/// absence of anchor blocks, but instead calculated from the heaviest affirmation map as follows:
///
/// * If `at(i)` is `p` or `n`, then bit `i` is 1
/// * Otherwise, bit `i` is 0
///
/// In addition, when a late anchor block arrives and is processed by the chains coordinator, the heaviest affirmation
/// map is consulted to determine whether or not it _should_ be processed.  If it's _not_ affirmed, then it is ignored.
///
/// ## Failure Recovery
///
/// In the event that a hidden anchor block arises, this subsystem includes a way to _override_ the heaviest affirmation
/// map for a given reward cycle.  If an anchor block is missing, miners can _declare_ it missing by updating a row in
/// the burnchain DB that marks the anchor block as forever missing.  This prevents a "short" (but still devastating)
/// reorg whereby an anchor block is missing for _almost_ the duration of the reward cycle -- in such a case, the
/// absence of this declaration would cause the reward cycle's blocks to all be invalidated.  Adding this declaration,
/// and then mining an anchor block that does _not_ affirm the missing anchor block would solve this for future
/// bootstrapping nodes.
///
use std::cmp;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::fmt::Write;
use std::sync::mpsc::SyncSender;
use std::time::Duration;

use serde::de::Error as de_Error;
use serde::ser::Error as ser_Error;
use serde::{Deserialize, Serialize};
use stacks_common::types::chainstate::{
    BlockHeaderHash, BurnchainHeaderHash, PoxId, SortitionId, StacksAddress, StacksBlockId,
};

use crate::burnchains::db::{
    BurnchainBlockData, BurnchainDB, BurnchainDBTransaction, BurnchainHeaderReader,
};
use crate::burnchains::{Address, Burnchain, BurnchainBlockHeader, Error, PoxConstants, Txid};
use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::burn::operations::leader_block_commit::{
    RewardSetInfo, BURN_BLOCK_MINED_AT_MODULUS,
};
use crate::chainstate::burn::operations::{BlockstackOperationType, LeaderBlockCommitOp};
use crate::chainstate::burn::{BlockSnapshot, ConsensusHash};
use crate::chainstate::stacks::StacksBlockHeader;
use crate::core::StacksEpochId;
use crate::util_lib::boot::boot_code_id;
use crate::util_lib::db::{DBConn, Error as DBError};

/// Affirmation map entries.  By building on a PoX-mined block,
/// a PoB-mined block (in a PoX reward cycle),
/// or no block in reward cycle _i_, a sortition's miner
/// affirms something about the status of the ancestral anchor blocks.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum AffirmationMapEntry {
    PoxAnchorBlockPresent,
    PoxAnchorBlockAbsent,
    Nothing,
}

impl AffirmationMapEntry {
    pub fn from_chr(c: char) -> Option<AffirmationMapEntry> {
        match c {
            'p' => Some(AffirmationMapEntry::PoxAnchorBlockPresent),
            'a' => Some(AffirmationMapEntry::PoxAnchorBlockAbsent),
            'n' => Some(AffirmationMapEntry::Nothing),
            _ => None,
        }
    }
}

/// An affirmation map is simply a list of affirmation map entries.  This struct merely wraps the
/// list behind accessor and mutator methods.
#[derive(Clone, PartialEq)]
pub struct AffirmationMap {
    pub affirmations: Vec<AffirmationMapEntry>,
}

impl fmt::Display for AffirmationMapEntry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            AffirmationMapEntry::PoxAnchorBlockPresent => write!(f, "p"),
            AffirmationMapEntry::PoxAnchorBlockAbsent => write!(f, "a"),
            AffirmationMapEntry::Nothing => write!(f, "n"),
        }
    }
}

impl fmt::Debug for AffirmationMapEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format!("{}", &self))
    }
}

impl fmt::Display for AffirmationMap {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for entry in self.affirmations.iter() {
            write!(f, "{}", &entry)?;
        }
        Ok(())
    }
}

impl fmt::Debug for AffirmationMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self)
    }
}

impl Serialize for AffirmationMap {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let am_str = self.encode();
        s.serialize_str(am_str.as_str())
    }
}

impl<'de> Deserialize<'de> for AffirmationMap {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<AffirmationMap, D::Error> {
        let am_str = String::deserialize(d)?;
        let am = AffirmationMap::decode(&am_str).ok_or(de_Error::custom(
            "Failed to decode affirmation map".to_string(),
        ))?;
        Ok(am)
    }
}

/// The pointer to the PoX anchor block in the burnchain
pub struct PoxAnchorPtr {
    /// height of the block
    pub block_height: u64,
    /// index in the block
    pub vtxindex: u32,
    /// how any tokens burnt to create it
    pub burnt: u64,
    /// number of confirmations it received
    pub confs: u64,
}

impl AffirmationMap {
    pub fn new(entries: Vec<AffirmationMapEntry>) -> AffirmationMap {
        AffirmationMap {
            affirmations: entries,
        }
    }

    pub fn empty() -> AffirmationMap {
        AffirmationMap {
            affirmations: vec![],
        }
    }

    pub fn at(&self, reward_cycle: u64) -> Option<&AffirmationMapEntry> {
        self.affirmations.get(reward_cycle as usize)
    }

    pub fn push(&mut self, entry: AffirmationMapEntry) {
        self.affirmations.push(entry)
    }

    #[cfg_attr(test, mutants::skip)]
    pub fn pop(&mut self) -> Option<AffirmationMapEntry> {
        self.affirmations.pop()
    }

    pub fn len(&self) -> usize {
        self.affirmations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.affirmations.is_empty()
    }

    pub fn as_slice(&self) -> &[AffirmationMapEntry] {
        &self.affirmations
    }

    // used to store to database
    pub fn encode(&self) -> String {
        let mut ret = String::with_capacity(self.affirmations.len());
        write!(&mut ret, "{}", self).expect("BUG: failed to serialize affirmations -- likely OOM");
        ret
    }

    // used for database from-row
    pub fn decode(s: &str) -> Option<AffirmationMap> {
        if !s.is_ascii() {
            return None;
        }

        let mut affirmations = Vec::with_capacity(s.len());
        for chr in s.chars() {
            if let Some(next) = AffirmationMapEntry::from_chr(chr) {
                affirmations.push(next);
            } else {
                return None;
            }
        }
        Some(AffirmationMap { affirmations })
    }

    /// Has `other` diverged from `self`?
    /// If `other` contains a reward cycle affirmation that is not present in `self`, then yes.
    /// (Note that this means that if `other` is a prefix of `self`, then no divergence).
    /// Return the index into `other` where the affirmation differs from `self`.
    /// Return `None` if no difference exists.
    pub fn find_divergence(&self, other: &AffirmationMap) -> Option<u64> {
        for i in 0..cmp::min(self.len(), other.len()) {
            if self.affirmations[i] != other.affirmations[i] {
                return Some(i as u64);
            }
        }

        if other.len() > self.len() {
            return Some(self.len() as u64);
        }

        None
    }

    /// At what reward cycle should a node start searching for block inventories, given the heaviest
    /// affirmation map?.  This is the lowest reward cycle in which both self and heaviest affirm
    /// "absent" that comes _after_ the highest reward cycle in which both self and heaviest affirm
    /// "present".
    ///
    /// For `paa` and `pap`, it's 1
    /// For `paap` and `paap`, it's 3
    /// For `papa` and `apap`, it's 0
    /// For `paapapap` and `paappapa`, it's 4
    /// For `aaaaa` and `aaaaa`, it's 0.
    /// For `ppppp` and `ppppp`, it's 4.
    pub fn find_inv_search(&self, heaviest: &AffirmationMap) -> u64 {
        let mut highest_p = None;
        for i in 0..cmp::min(self.len(), heaviest.len()) {
            if self.affirmations[i] == heaviest.affirmations[i]
                && self.affirmations[i] == AffirmationMapEntry::PoxAnchorBlockPresent
            {
                highest_p = Some(i);
            }
        }
        if let Some(highest_p) = highest_p {
            for i in highest_p..cmp::min(self.len(), heaviest.len()) {
                if self.affirmations[i] == heaviest.affirmations[i]
                    && self.affirmations[i] == AffirmationMapEntry::PoxAnchorBlockAbsent
                {
                    return i as u64;
                }
                if self.affirmations[i] != heaviest.affirmations[i] {
                    return i as u64;
                }
            }
            return highest_p as u64;
        } else {
            // no agreement on any anchor block
            return 0;
        }
    }

    /// Is `other` a prefix of `self`?
    /// Returns true if so; false if not
    pub fn has_prefix(&self, prefix: &AffirmationMap) -> bool {
        if self.len() < prefix.len() {
            return false;
        }

        for i in 0..prefix.len() {
            if self.affirmations[i] != prefix.affirmations[i] {
                return false;
            }
        }

        true
    }

    /// What is the weight of this affirmation map?
    /// i.e. how many times did the network either affirm an anchor block, or make no election?
    pub fn weight(&self) -> u64 {
        let mut weight = 0;
        for i in 0..self.len() {
            match self.affirmations[i] {
                AffirmationMapEntry::PoxAnchorBlockAbsent => {}
                _ => {
                    weight += 1;
                }
            }
        }
        weight
    }
}

/// Get a parent/child reward cycle.  Only return Some(..) if the reward cycle is known for both --
/// i.e. their block heights are plausible -- they are at or after the first burnchain block
/// height.
pub fn get_parent_child_reward_cycles(
    parent: &LeaderBlockCommitOp,
    block_commit: &LeaderBlockCommitOp,
    burnchain: &Burnchain,
) -> Option<(u64, u64)> {
    let child_reward_cycle = match burnchain.block_height_to_reward_cycle(block_commit.block_height)
    {
        Some(crc) => crc,
        None => return None,
    };

    let parent_reward_cycle = match burnchain.block_height_to_reward_cycle(parent.block_height) {
        Some(prc) => prc,
        None => {
            if parent.block_height == 0 && parent.vtxindex == 0 {
                // this is a first block commit
                0
            } else {
                return None;
            }
        }
    };

    test_debug!(
        "{},{} is rc={},rc={}",
        parent.block_height,
        block_commit.block_height,
        parent_reward_cycle,
        child_reward_cycle
    );
    Some((parent_reward_cycle, child_reward_cycle))
}

/// Read a range of blockstack operations for a prepare phase of a given reward cycle.
/// Only includes block-commits.
/// The returned vec is a vec of vecs of block-commits in block order.  The ith item is a vec of
/// block-commits in block order for the ith prepare-phase block (item 0 is the first prepare-phase
/// block's block-commits).
pub fn read_prepare_phase_commits<B: BurnchainHeaderReader>(
    burnchain_tx: &BurnchainDBTransaction,
    indexer: &B,
    pox_consts: &PoxConstants,
    first_block_height: u64,
    reward_cycle: u64,
) -> Result<Vec<Vec<LeaderBlockCommitOp>>, Error> {
    // start and end heights of the prepare phase for this reward cycle
    let start_height = pox_consts
        .reward_cycle_to_block_height(first_block_height, reward_cycle + 1)
        - (pox_consts.prepare_length as u64);
    let end_height = start_height + (pox_consts.prepare_length as u64);
    let headers = indexer.read_burnchain_headers(start_height, end_height)?;
    let _num_headers = headers.len();

    let mut ret = vec![];
    for header in headers.into_iter() {
        let blk = BurnchainDB::get_burnchain_block(&burnchain_tx.conn(), &header.block_hash)
            .unwrap_or_else(|_| {
                panic!(
                    "BUG: failed to load prepare-phase block {} ({})",
                    &header.block_hash, header.block_height
                )
            });

        let mut block_ops = vec![];
        for op in blk.ops.into_iter() {
            assert!(pox_consts.is_in_prepare_phase(first_block_height, op.block_height()));
            match op {
                BlockstackOperationType::LeaderBlockCommit(opdata) => {
                    // basic validity filtering
                    if opdata.block_height < first_block_height {
                        test_debug!("Skip too-early block commit");
                        continue;
                    }
                    // the block commit's parent must be a burnchain block that is evaluated by the node
                    //  blocks that are <= first_block_height do not meet this requirement.
                    if (opdata.parent_block_ptr as u64) <= first_block_height {
                        test_debug!("Skip orphaned block-commit");
                        continue;
                    }
                    if opdata.block_height <= opdata.parent_block_ptr as u64 {
                        test_debug!("Skip block-commit whose 'parent' comes at or after it");
                        continue;
                    }
                    if opdata.burn_fee == 0 {
                        test_debug!("Skip block-commit without burn");
                        continue;
                    }
                    block_ops.push(opdata);
                }
                _ => {
                    continue;
                }
            }
        }
        block_ops.sort_by(|op1, op2| {
            assert_eq!(
                op1.block_height, op2.block_height,
                "BUG: block loaded ops from a different block height"
            );
            op1.vtxindex.cmp(&op2.vtxindex)
        });
        ret.push(block_ops);
    }

    test_debug!(
        "Read {} headers, {} prepare-phase commits from reward cycle {} ({}-{})",
        _num_headers,
        ret.len(),
        reward_cycle,
        start_height,
        end_height
    );
    Ok(ret)
}

/// Find all referenced parent block-commits already in the burnchain DB, so we can extract their VRF seeds.
/// If this method errors out, it's because it couldn't read the burnchain headers DB (or it's
/// corrupted). Either way, the caller may treat this as a fatal condition.
pub fn read_parent_block_commits<B: BurnchainHeaderReader>(
    burnchain_tx: &BurnchainDBTransaction,
    indexer: &B,
    prepare_phase_ops: &[Vec<LeaderBlockCommitOp>],
) -> Result<Vec<LeaderBlockCommitOp>, Error> {
    let mut parents = HashMap::new();
    for ops in prepare_phase_ops.iter() {
        for opdata in ops.iter() {
            let hdr =
                if let Some(hdr) = indexer.read_burnchain_header(opdata.parent_block_ptr as u64)? {
                    hdr
                } else {
                    // this is pretty bad if this happens
                    error!(
                        "Discontiguous header database: no such block {}, but have block {}",
                        opdata.parent_block_ptr, opdata.block_height
                    );
                    return Err(Error::MissingParentBlock);
                };

            test_debug!("Get header at {}: {:?}", opdata.parent_block_ptr, &hdr);
            assert_eq!(hdr.block_height, opdata.parent_block_ptr as u64);

            let mut found = false;
            let blk = BurnchainDB::get_burnchain_block(burnchain_tx.conn(), &hdr.block_hash)
                .unwrap_or_else(|_| {
                    panic!(
                        "BUG: failed to load existing block {} ({})",
                        &hdr.block_hash, &hdr.block_height
                    )
                });

            for parent_op in blk.ops.into_iter() {
                if let BlockstackOperationType::LeaderBlockCommit(parent_opdata) = parent_op {
                    if parent_opdata.vtxindex == opdata.parent_vtxindex as u32 {
                        test_debug!(
                            "Parent of {},{},{} is {},{},{}",
                            &opdata.txid,
                            opdata.block_height,
                            opdata.vtxindex,
                            &parent_opdata.txid,
                            parent_opdata.block_height,
                            parent_opdata.vtxindex
                        );
                        parents.insert(parent_opdata.txid.clone(), parent_opdata);
                        found = true;
                    }
                }
            }
            if !found {
                test_debug!(
                    "Orphan block commit {},{},{}",
                    &opdata.txid,
                    opdata.block_height,
                    opdata.vtxindex
                );
            }
        }
    }
    let mut parent_list: Vec<_> = parents.into_values().collect();
    parent_list.sort_by(|a, b| {
        if a.block_height != b.block_height {
            a.block_height.cmp(&b.block_height)
        } else {
            a.vtxindex.cmp(&b.vtxindex)
        }
    });

    test_debug!("Read {} parent block-commits", parent_list.len());
    Ok(parent_list)
}

/// Given a list of prepare-phase block-commits, and a list of parent commits, filter out and remove
/// the prepare-phase commits that _don't_ have a parent.
pub fn filter_orphan_block_commits(
    parents: &[LeaderBlockCommitOp],
    prepare_phase_ops: Vec<Vec<LeaderBlockCommitOp>>,
) -> Vec<Vec<LeaderBlockCommitOp>> {
    let mut parent_set = HashSet::new();
    for parent in parents.iter() {
        parent_set.insert((parent.block_height, parent.vtxindex));
    }
    for prepare_phase_block in prepare_phase_ops.iter() {
        for opdata in prepare_phase_block.iter() {
            parent_set.insert((opdata.block_height, opdata.vtxindex));
        }
    }

    prepare_phase_ops
        .into_iter()
        .map(|prepare_phase_block| {
            prepare_phase_block
                .into_iter()
                .filter(|opdata| {
                    if parent_set.contains(&(
                        opdata.parent_block_ptr as u64,
                        opdata.parent_vtxindex as u32,
                    )) {
                        true
                    } else {
                        test_debug!(
                            "Ignore invalid block-commit {},{} ({}): no parent {},{}",
                            opdata.block_height,
                            opdata.vtxindex,
                            &opdata.txid,
                            opdata.parent_block_ptr,
                            opdata.parent_vtxindex
                        );
                        false
                    }
                })
                .collect()
        })
        .collect()
}

/// Given a list of prepare-phase block-commits, filter out the ones that don't have correct burn
/// modulii.  This means that late block-commits don't count as confirmations.
pub fn filter_missed_block_commits(
    prepare_phase_ops: Vec<Vec<LeaderBlockCommitOp>>,
) -> Vec<Vec<LeaderBlockCommitOp>> {
    prepare_phase_ops
        .into_iter()
        .map(|commits| {
            commits
                .into_iter()
                .filter(|cmt| {
                    let intended_modulus =
                        (cmt.burn_block_mined_at() + 1) % BURN_BLOCK_MINED_AT_MODULUS;
                    let actual_modulus = cmt.block_height % BURN_BLOCK_MINED_AT_MODULUS;
                    if actual_modulus == intended_modulus {
                        true
                    } else {
                        test_debug!(
                            "Ignore invalid block-commit {},{} ({}): {} != {}",
                            cmt.block_height,
                            cmt.vtxindex,
                            &cmt.txid,
                            actual_modulus,
                            intended_modulus
                        );
                        false
                    }
                })
                .collect()
        })
        .collect()
}

/// Given a list of block-commits in the prepare-phase, find the block-commit pointer outside the
/// prepare-phase which must be the anchor block, if it exists at all.  This is always
/// the block-commit that has the most cumulative BTC committed behind it (and the highest
/// such in the event of a tie), as well as at least `anchor_threshold` confirmations.
/// Returns the pointer into the burnchain where the anchor block-commit can be found, if it
/// exists at all.
/// Returns None otherwise
fn inner_find_heaviest_block_commit_ptr(
    prepare_phase_ops: &[Vec<LeaderBlockCommitOp>],
    anchor_threshold: u32,
) -> Option<(PoxAnchorPtr, BTreeMap<(u64, u32), (u64, u32)>)> {
    // sanity check -- must be in order by block height and vtxindex
    for prepare_block_ops in prepare_phase_ops.iter() {
        let mut expected_block_height = None;
        let mut last_vtxindex = None;
        for opdata in prepare_block_ops.iter() {
            if let Some(expected_block_height) = expected_block_height.as_ref() {
                assert_eq!(expected_block_height, &opdata.block_height);
            } else {
                expected_block_height = Some(opdata.block_height);
            }

            if let Some(last_vtxindex) = last_vtxindex.as_mut() {
                assert!(
                    *last_vtxindex < opdata.vtxindex,
                    "{} !< {} at block {} (op {:?})",
                    *last_vtxindex,
                    opdata.vtxindex,
                    opdata.block_height,
                    &opdata
                );
                *last_vtxindex = opdata.vtxindex;
            } else {
                last_vtxindex = Some(opdata.vtxindex);
            }
            test_debug!(
                "Prepare-phase block-commit {},{}: {}",
                opdata.block_height,
                opdata.vtxindex,
                &opdata.txid
            );
        }
    }

    // map (block_height, vtxindex) to (parent_block_height, parent_vtxindex)
    let mut parents = BTreeMap::new();

    // map (block_height, vtxindex) to (non-prepare-ancestor-height, non-prepare-ancestor-vtxindex)
    let mut ancestors = BTreeMap::new();

    // map (non-prepare-ancestor-height, non-prepare-ancestor-vtxindex) to (set-of-block-heights, total_burnt)
    // that contain descendants
    let mut ancestor_confirmations: BTreeMap<(u64, u32), (HashSet<u64>, u64)> = BTreeMap::new();

    // calculate each block-commit's parents
    for prepare_block_ops in prepare_phase_ops.iter() {
        for opdata in prepare_block_ops.iter() {
            parents.insert(
                (opdata.block_height, opdata.vtxindex),
                (
                    opdata.parent_block_ptr as u64,
                    opdata.parent_vtxindex as u32,
                ),
            );
        }
    }

    // calculate the ancestor map -- find the highest non-prepare-phase ancestor for each prepare-phase block-commit.
    for prepare_block_ops in prepare_phase_ops.iter().rev() {
        for opdata in prepare_block_ops.iter() {
            let mut cursor = (opdata.block_height, opdata.vtxindex);
            while let Some((parent_block, parent_vtxindex)) = parents.get(&cursor) {
                cursor = (*parent_block, *parent_vtxindex);
                if let Some((block_height, vtxindex)) = ancestors.get(&cursor) {
                    // already processed
                    cursor = (*block_height, *vtxindex);
                    break;
                }
            }
            ancestors.insert((opdata.block_height, opdata.vtxindex), (cursor.0, cursor.1));
        }
    }

    // calculate the ancestor confirmations -- figure out how many distinct blocks contain
    // block-commits that descend from each pre-prepare-phase ancestor
    for prepare_block_ops in prepare_phase_ops.iter() {
        for opdata in prepare_block_ops.iter() {
            if let Some((ancestor_height, ancestor_vtxindex)) =
                ancestors.get(&(opdata.block_height, opdata.vtxindex))
            {
                if let Some((ref mut confirmed_block_set, ref mut ancestor_burnt)) =
                    ancestor_confirmations.get_mut(&(*ancestor_height, *ancestor_vtxindex))
                {
                    confirmed_block_set.insert(opdata.block_height);
                    *ancestor_burnt += opdata.burn_fee;
                } else {
                    let mut block_set = HashSet::new();
                    block_set.insert(opdata.block_height);
                    ancestor_confirmations.insert(
                        (*ancestor_height, *ancestor_vtxindex),
                        (block_set, opdata.burn_fee),
                    );
                }
            }
        }
    }

    test_debug!("parents = {:?}", &parents);
    test_debug!("ancestors = {:?}", &ancestors);
    test_debug!("ancestor_confirmations = {:?}", &ancestor_confirmations);

    if ancestor_confirmations.is_empty() {
        // empty prepare phase
        test_debug!("Prepare-phase has no block-commits");
        return None;
    }

    // find the ancestors with at least $anchor_threshold confirmations, and pick the one that has the
    // most total BTC.  Break ties by ancestor order -- highest ancestor commit wins.
    let mut ancestor_block = 0;
    let mut ancestor_vtxindex = 0;
    let mut most_burnt = 0;
    let mut most_confs = 0;

    // consider ancestor candidates in _highest_-first order
    for ((height, vtxindex), (block_set, burnt)) in ancestor_confirmations.iter().rev() {
        let confs = block_set.len() as u64;
        if confs < anchor_threshold.into() {
            continue;
        }

        // only consider an earlier ancestor if it burned more than the candidate
        if *burnt > most_burnt {
            most_burnt = *burnt;
            most_confs = confs;
            ancestor_block = *height;
            ancestor_vtxindex = *vtxindex;
        }
    }

    if most_burnt == 0 {
        // no anchor block possible -- no block-commit has enough confirmations
        test_debug!("No block-commit has enough support to be an anchor block");
        return None;
    }

    Some((
        PoxAnchorPtr {
            block_height: ancestor_block,
            vtxindex: ancestor_vtxindex,
            burnt: most_burnt,
            confs: most_confs,
        },
        ancestors,
    ))
}

/// Given a list of block-commits in the prepare-phase, find the block-commit outside the
/// prepare-phase which must be the anchor block, if it exists at all.  This is always
/// the block-commit that has the most cumulative BTC committed behind it (and the highest
/// such in the event of a tie), as well as at least `anchor_threshold` confirmations.  If the anchor block
/// commit is found, return the descendancy matrix for it as well.
/// Returns Some(the winning block commit, descendancy matrix, total confirmations, total burnt) if
/// there's an anchor block commit.
/// Returns None otherwise
pub fn find_heaviest_block_commit<B: BurnchainHeaderReader>(
    burnchain_tx: &BurnchainDBTransaction,
    indexer: &B,
    prepare_phase_ops: &[Vec<LeaderBlockCommitOp>],
    anchor_threshold: u32,
) -> Result<Option<(LeaderBlockCommitOp, Vec<Vec<bool>>, u64, u64)>, DBError> {
    let (pox_anchor_ptr, ancestors) =
        match inner_find_heaviest_block_commit_ptr(prepare_phase_ops, anchor_threshold) {
            Some(ptr) => ptr,
            None => {
                return Ok(None);
            }
        };

    let ancestor_block = pox_anchor_ptr.block_height;
    let ancestor_vtxindex = pox_anchor_ptr.vtxindex;
    let most_burnt = pox_anchor_ptr.burnt;
    let most_confs = pox_anchor_ptr.confs;

    // find the ancestor that this tip confirms
    let heaviest_ancestor_header = indexer
        .read_burnchain_headers(ancestor_block, ancestor_block + 1)?
        .first()
        .unwrap_or_else(|| panic!("BUG: no block headers for height {}", ancestor_block))
        .to_owned();

    let heaviest_ancestor_block =
        BurnchainDB::get_burnchain_block(burnchain_tx.conn(), &heaviest_ancestor_header.block_hash)
            .unwrap_or_else(|_| {
                panic!(
                    "BUG: no ancestor block {:?} ({})",
                    &heaviest_ancestor_header.block_hash, heaviest_ancestor_header.block_height
                )
            });

    // find the PoX anchor block-commit, if it exists at all
    // (note that it may not -- a rich attacker can force F*w confirmations with lots of BTC on a
    // commit that was never mined).
    for block_op in heaviest_ancestor_block.ops.into_iter() {
        if let BlockstackOperationType::LeaderBlockCommit(opdata) = block_op {
            if opdata.block_height == ancestor_block && opdata.vtxindex == ancestor_vtxindex {
                // found
                debug!(
                    "PoX anchor block-commit {},{},{} has {} burnt, {} confs",
                    &opdata.txid, opdata.block_height, opdata.vtxindex, most_burnt, most_confs;
                    "stacks_block_hash" => opdata.block_header_hash
                );

                // sanity check -- there should be exactly as many confirmations on the suspected
                // anchor block as there are distinct descendancies.
                let mut conf_count = 0;

                // sanity check -- there should be exactly as many BTC burnt for the suspected
                // anchor block as the most_burnt.
                let mut burn_count = 0;

                let mut descendancy = Vec::with_capacity(prepare_phase_ops.len());
                for prepare_block_ops in prepare_phase_ops.iter() {
                    let mut block_descendancy = Vec::with_capacity(prepare_phase_ops.len());
                    let mut found_conf = false;
                    for opdata in prepare_block_ops.iter() {
                        if let Some((op_ancestor_height, op_ancestor_vtxindex, ..)) =
                            ancestors.get(&(opdata.block_height, opdata.vtxindex))
                        {
                            if *op_ancestor_height == ancestor_block
                                && *op_ancestor_vtxindex == ancestor_vtxindex
                            {
                                debug!("Block-commit {},{} descends from likely PoX anchor block {},{}", opdata.block_height, opdata.vtxindex, op_ancestor_height, op_ancestor_vtxindex;
                                    "stacks_block_hash" => opdata.block_header_hash
                                );
                                block_descendancy.push(true);
                                if !found_conf {
                                    conf_count += 1;
                                    found_conf = true;
                                }
                                burn_count += opdata.burn_fee;
                            } else {
                                debug!("Block-commit {},{} does NOT descend from likely PoX anchor block {},{}", opdata.block_height, opdata.vtxindex, ancestor_block, ancestor_vtxindex;
                                    "stacks_block_hash" => opdata.block_header_hash
                                );
                                block_descendancy.push(false);
                            }
                        } else {
                            debug!("Block-commit {},{} does NOT descend from likely PoX anchor block {},{}", opdata.block_height, opdata.vtxindex, ancestor_block, ancestor_vtxindex;
                                "stacks_block_hash" => opdata.block_header_hash
                            );
                            block_descendancy.push(false);
                        }
                    }
                    descendancy.push(block_descendancy);
                }

                assert_eq!(conf_count, most_confs);
                assert_eq!(burn_count, most_burnt);

                return Ok(Some((opdata, descendancy, most_confs, most_burnt)));
            }
        }
    }

    warn!("Evil miners confirmed a non-existant PoX anchor block!");
    Ok(None)
}

/// Find the valid prepare-phase ops for a given reward cycle
fn inner_find_valid_prepare_phase_commits<B: BurnchainHeaderReader>(
    burnchain_tx: &BurnchainDBTransaction,
    reward_cycle: u64,
    indexer: &B,
    burnchain: &Burnchain,
) -> Result<Vec<Vec<LeaderBlockCommitOp>>, Error> {
    let pox_consts = &burnchain.pox_constants;
    let first_block_height = burnchain.first_block_height;

    let prepare_ops = read_prepare_phase_commits(
        burnchain_tx,
        indexer,
        pox_consts,
        first_block_height,
        reward_cycle,
    )?;
    test_debug!("{} prepare-phase commits", prepare_ops.len());

    let parent_commits = read_parent_block_commits(burnchain_tx, indexer, &prepare_ops)?;
    test_debug!("{} parent block-commits", parent_commits.len());

    let prepare_ops_no_orphans = filter_orphan_block_commits(&parent_commits, prepare_ops);
    test_debug!(
        "{} prepare-phase block-commits that have parents",
        prepare_ops_no_orphans.len()
    );

    let prepare_ops_valid = filter_missed_block_commits(prepare_ops_no_orphans);
    test_debug!(
        "{} prepare-phase block-commits that have parents and are on-time",
        prepare_ops_valid.len()
    );

    Ok(prepare_ops_valid)
}

/// Find the pointer to the PoX anchor block selected in a reward cycle, if it exists.  This is the heaviest F*w-confirmed
/// block-commit before the prepare-phase of this reward cycle, provided that it is not already an
/// anchor block for some other reward cycle.  Note that the anchor block found will be the anchor
/// block for the *next* reward cycle.
/// Returns a pointer to the block-commit transaction in the burnchain, if the prepare phase
/// selected an anchor block.
/// Returns None if not.
pub fn find_pox_anchor_block_ptr<B: BurnchainHeaderReader>(
    burnchain_tx: &BurnchainDBTransaction,
    reward_cycle: u64,
    indexer: &B,
    burnchain: &Burnchain,
) -> Result<Option<PoxAnchorPtr>, Error> {
    let prepare_ops_valid =
        inner_find_valid_prepare_phase_commits(burnchain_tx, reward_cycle, indexer, burnchain)?;
    Ok(inner_find_heaviest_block_commit_ptr(
        &prepare_ops_valid,
        burnchain.pox_constants.anchor_threshold,
    )
    .map(|(ptr, _)| ptr))
}

/// Find the PoX anchor block selected in a reward cycle, if it exists.  This is the heaviest F*w-confirmed
/// block-commit before the prepare-phase of this reward cycle, provided that it is not already an
/// anchor block for some other reward cycle.  Note that the anchor block found will be the anchor
/// block for the *next* reward cycle.
/// Returns:
///     (a) the list of block-commits, grouped by block and ordered by vtxindex, in this prepare phase
///     (b) the PoX anchor block-commit, if it exists, and
///     (c) the descendancy data for the prepare phase.  Descendency[i][j] is true if the jth
///     block-commit in the ith block in the prepare phase descends from the anchor block, or False
///     if not.
/// Returns only database-related errors.
pub fn find_pox_anchor_block<B: BurnchainHeaderReader>(
    burnchain_tx: &BurnchainDBTransaction,
    reward_cycle: u64,
    indexer: &B,
    burnchain: &Burnchain,
) -> Result<
    (
        // (a) prepare-phase block-commits
        Vec<Vec<LeaderBlockCommitOp>>,
        // (b) PoX anchor block commit (if found)
        // (c) descendancy matrix
        Option<(LeaderBlockCommitOp, Vec<Vec<bool>>)>,
    ),
    Error,
> {
    let prepare_ops_valid =
        inner_find_valid_prepare_phase_commits(burnchain_tx, reward_cycle, indexer, burnchain)?;
    let anchor_block_and_descendancy_opt = find_heaviest_block_commit(
        &burnchain_tx,
        indexer,
        &prepare_ops_valid,
        burnchain.pox_constants.anchor_threshold,
    )?;
    if let Some((ref anchor_block_commit, ..)) = anchor_block_and_descendancy_opt.as_ref() {
        // cannot have been an anchor block in some other reward cycle
        let md = BurnchainDB::get_commit_metadata(
            burnchain_tx.conn(),
            &anchor_block_commit.burn_header_hash,
            &anchor_block_commit.txid,
        )?
        .expect("BUG: anchor block commit has no metadata");

        if let Some(rc) = md.anchor_block {
            warn!(
                "Block-commit {} is already an anchor block for reward cycle {}",
                &anchor_block_commit.txid, rc
            );
            return Ok((prepare_ops_valid, None));
        }
    }

    if anchor_block_and_descendancy_opt.is_some() {
        test_debug!(
            "Selected an anchor block in prepare phase of reward cycle {}",
            reward_cycle
        );
    } else {
        test_debug!(
            "Did NOT select an anchor block in prepare phase of reward cycle {}",
            reward_cycle
        );
    }

    Ok((
        prepare_ops_valid,
        anchor_block_and_descendancy_opt
            .map(|(anchor_block_commit, descendancy, ..)| (anchor_block_commit, descendancy)),
    ))
}

/// Update a completed reward cycle's affirmation maps
pub fn update_pox_affirmation_maps<B: BurnchainHeaderReader>(
    burnchain_db: &mut BurnchainDB,
    indexer: &B,
    reward_cycle: u64,
    burnchain: &Burnchain,
) -> Result<(), Error> {
    debug!("Process PoX affirmations for reward cycle {}", reward_cycle);

    let tx = burnchain_db.tx_begin()?;

    let (prepare_ops, pox_anchor_block_info_opt) =
        find_pox_anchor_block(&tx, reward_cycle, indexer, burnchain)?;

    if let Some((anchor_block, descendancy)) = pox_anchor_block_info_opt.clone() {
        debug!(
            "PoX anchor block elected in reward cycle {} for reward cycle {} is {}",
            reward_cycle,
            reward_cycle + 1,
            &anchor_block.block_header_hash
        );

        // anchor block found for this upcoming reward cycle
        tx.set_anchor_block(&anchor_block, reward_cycle + 1)?;
        assert_eq!(descendancy.len(), prepare_ops.len());

        // mark the prepare-phase commits that elected this next reward cycle's anchor block as
        // having descended or not descended from this anchor block.
        for (block_idx, block_ops) in prepare_ops.iter().enumerate() {
            assert_eq!(block_ops.len(), descendancy[block_idx].len());

            for (tx_idx, tx_op) in block_ops.iter().enumerate() {
                test_debug!(
                    "Make affirmation map for block-commit at {},{}",
                    tx_op.block_height,
                    tx_op.vtxindex
                );
                tx.make_prepare_phase_affirmation_map(
                    indexer,
                    burnchain,
                    reward_cycle + 1,
                    tx_op,
                    Some(&anchor_block),
                    descendancy[block_idx][tx_idx],
                )?;
            }
        }
    } else {
        debug!("PoX anchor block selected in reward cycle {} is None. Reward cycle {} has no anchor block", reward_cycle, reward_cycle + 1);

        // anchor block not found for this upcoming reward cycle
        tx.clear_anchor_block(reward_cycle + 1)?;

        // mark all prepare-phase commits as NOT having descended from the next reward cycle's anchor
        // block as NOT having descended from any anchor block (since one was not chosen)
        for block_ops in prepare_ops.iter() {
            for tx_op in block_ops.iter() {
                test_debug!(
                    "Make affirmation map for block-commit at {},{}",
                    tx_op.block_height,
                    tx_op.vtxindex
                );
                tx.make_prepare_phase_affirmation_map(
                    indexer,
                    burnchain,
                    reward_cycle + 1,
                    tx_op,
                    None,
                    false,
                )?;
            }
        }
    }

    tx.commit()?;
    debug!(
        "Processed PoX affirmations for reward cycle {}",
        reward_cycle
    );

    Ok(())
}
