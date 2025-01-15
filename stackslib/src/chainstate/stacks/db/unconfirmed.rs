/*
 copyright: (c) 2013-2020 by Blockstack PBC, a public benefit corporation.

 This file is part of Blockstack.

 Blockstack is free software. You may redistribute or modify
 it under the terms of the GNU General Public License as published by
 the Free Software Foundation, either version 3 of the License or
 (at your option) any later version.

 Blockstack is distributed in the hope that it will be useful,
 but WITHOUT ANY WARRANTY, including without the implied warranty of
 MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 GNU General Public License for more details.

 You should have received a copy of the GNU General Public License
 along with Blockstack. If not, see <http://www.gnu.org/licenses/>.
*/

use std::collections::{HashMap, HashSet};
use std::fs;

use clarity::vm::costs::ExecutionCost;
use clarity::vm::database::{BurnStateDB, HeadersDB, NULL_BURN_STATE_DB, NULL_HEADER_DB};
use stacks_common::types::chainstate::{BurnchainHeaderHash, StacksBlockId};

use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::stacks::db::accounts::*;
use crate::chainstate::stacks::db::blocks::*;
use crate::chainstate::stacks::db::*;
use crate::chainstate::stacks::events::*;
use crate::chainstate::stacks::index::marf::MARFOpenOpts;
use crate::chainstate::stacks::{Error, *};
use crate::clarity_vm::clarity::{ClarityInstance, Error as clarity_error};
use crate::clarity_vm::database::marf::MarfedKV;
use crate::core::*;
use crate::net::Error as net_error;
use crate::util_lib::db::Error as db_error;

pub type UnconfirmedTxMap = HashMap<Txid, (StacksTransaction, BlockHeaderHash, u16)>;

pub struct ProcessedUnconfirmedState {
    pub total_burns: u128,
    pub total_fees: u128,
    // each element of this vector is a tuple, where each tuple contains a microblock
    // sequence number, microblock header, and a vector of transaction receipts
    // for that microblock
    pub receipts: Vec<(u16, StacksMicroblockHeader, Vec<StacksTransactionReceipt>)>,
    pub burn_block_hash: BurnchainHeaderHash,
    pub burn_block_height: u32,
    pub burn_block_timestamp: u64,
}

impl Default for ProcessedUnconfirmedState {
    fn default() -> Self {
        ProcessedUnconfirmedState {
            total_burns: 0,
            total_fees: 0,
            receipts: vec![],
            burn_block_hash: BurnchainHeaderHash([0; 32]),
            burn_block_height: 0,
            burn_block_timestamp: 0,
        }
    }
}

pub struct UnconfirmedState {
    pub confirmed_chain_tip: StacksBlockId,
    pub unconfirmed_chain_tip: StacksBlockId,
    pub clarity_inst: ClarityInstance,
    pub mined_txs: UnconfirmedTxMap,
    pub cost_so_far: ExecutionCost,
    pub bytes_so_far: u64,

    pub last_mblock: Option<StacksMicroblockHeader>,
    pub last_mblock_seq: u16,

    readonly: bool,
    dirty: bool,
    num_mblocks_added: u64,
    have_state: bool,

    mainnet: bool,
    chain_id: u32,
    clarity_state_index_root: String,
    marf_opts: Option<MARFOpenOpts>,

    // fault injection for testing
    pub disable_cost_check: bool,
    pub disable_bytes_check: bool,
}

impl UnconfirmedState {
    /// Make a new unconfirmed state, but don't do anything with it yet.  Caller should immediately
    /// call .refresh() to instatiate and store the underlying state trie.
    fn new(chainstate: &StacksChainState, tip: StacksBlockId) -> Result<UnconfirmedState, Error> {
        let marf = MarfedKV::open_unconfirmed(
            &chainstate.clarity_state_index_root,
            None,
            chainstate.marf_opts.clone(),
        )?;

        let clarity_instance = ClarityInstance::new(chainstate.mainnet, chainstate.chain_id, marf);
        let unconfirmed_tip = MARF::make_unconfirmed_chain_tip(&tip);
        let cost_so_far = StacksChainState::get_stacks_block_anchored_cost(chainstate.db(), &tip)?
            .ok_or(Error::NoSuchBlockError)?;

        Ok(UnconfirmedState {
            confirmed_chain_tip: tip,
            unconfirmed_chain_tip: unconfirmed_tip,
            clarity_inst: clarity_instance,
            mined_txs: UnconfirmedTxMap::new(),
            cost_so_far: cost_so_far.clone(),
            bytes_so_far: 0,

            last_mblock: None,
            last_mblock_seq: 0,

            readonly: false,
            dirty: false,
            num_mblocks_added: 0,
            have_state: false,

            mainnet: chainstate.mainnet,
            chain_id: chainstate.chain_id,
            clarity_state_index_root: chainstate.clarity_state_index_root.clone(),
            marf_opts: chainstate.marf_opts.clone(),

            disable_cost_check: check_fault_injection(FAULT_DISABLE_MICROBLOCKS_COST_CHECK),
            disable_bytes_check: check_fault_injection(FAULT_DISABLE_MICROBLOCKS_BYTES_CHECK),
        })
    }

    /// Make a read-only copy of this unconfirmed state.  The resulting unconfiremd state cannot
    /// be refreshed, but it will represent a snapshot of the existing unconfirmed state.
    pub fn make_readonly_owned(&self) -> Result<UnconfirmedState, Error> {
        let marf = MarfedKV::open_unconfirmed(
            &self.clarity_state_index_root,
            None,
            self.marf_opts.clone(),
        )?;

        let clarity_instance = ClarityInstance::new(self.mainnet, self.chain_id, marf);

        Ok(UnconfirmedState {
            confirmed_chain_tip: self.confirmed_chain_tip.clone(),
            unconfirmed_chain_tip: self.unconfirmed_chain_tip.clone(),
            clarity_inst: clarity_instance,
            mined_txs: self.mined_txs.clone(),
            cost_so_far: self.cost_so_far.clone(),
            bytes_so_far: self.bytes_so_far,

            last_mblock: self.last_mblock.clone(),
            last_mblock_seq: self.last_mblock_seq,

            readonly: true,
            dirty: false,
            num_mblocks_added: self.num_mblocks_added,
            have_state: self.have_state,

            mainnet: self.mainnet,
            chain_id: self.chain_id,
            clarity_state_index_root: self.clarity_state_index_root.clone(),
            marf_opts: self.marf_opts.clone(),

            disable_cost_check: self.disable_cost_check,
            disable_bytes_check: self.disable_bytes_check,
        })
    }

    /// Make a new unconfirmed state, but don't do anything with it yet, and deny refreshes.
    fn new_readonly(
        chainstate: &StacksChainState,
        tip: StacksBlockId,
    ) -> Result<UnconfirmedState, Error> {
        let marf = MarfedKV::open_unconfirmed(
            &chainstate.clarity_state_index_root,
            None,
            chainstate.marf_opts.clone(),
        )?;

        let clarity_instance = ClarityInstance::new(chainstate.mainnet, chainstate.chain_id, marf);
        let unconfirmed_tip = MARF::make_unconfirmed_chain_tip(&tip);
        let cost_so_far = StacksChainState::get_stacks_block_anchored_cost(chainstate.db(), &tip)?
            .ok_or(Error::NoSuchBlockError)?;

        Ok(UnconfirmedState {
            confirmed_chain_tip: tip,
            unconfirmed_chain_tip: unconfirmed_tip,
            clarity_inst: clarity_instance,
            mined_txs: UnconfirmedTxMap::new(),
            cost_so_far,
            bytes_so_far: 0,

            last_mblock: None,
            last_mblock_seq: 0,

            readonly: true,
            dirty: false,
            num_mblocks_added: 0,
            have_state: false,

            mainnet: chainstate.mainnet,
            chain_id: chainstate.chain_id,
            clarity_state_index_root: chainstate.clarity_state_index_root.clone(),
            marf_opts: chainstate.marf_opts.clone(),

            disable_cost_check: check_fault_injection(FAULT_DISABLE_MICROBLOCKS_COST_CHECK),
            disable_bytes_check: check_fault_injection(FAULT_DISABLE_MICROBLOCKS_BYTES_CHECK),
        })
    }

    /// Append a sequence of microblocks to this unconfirmed state.
    /// Microblocks with sequence less than the self.last_mblock_seq will be silently ignored.
    /// Produce the total fees, total burns, and total list of transaction receipts.
    /// Updates internal cost_so_far count.
    /// Idempotent.
    fn append_microblocks(
        &mut self,
        chainstate: &StacksChainState,
        burn_dbconn: &dyn BurnStateDB,
        mblocks: Vec<StacksMicroblock>,
    ) -> Result<ProcessedUnconfirmedState, Error> {
        if self.last_mblock_seq == u16::MAX {
            // drop them -- nothing to do
            return Ok(Default::default());
        }

        debug!(
            "Refresh unconfirmed chain state off of {} with {} microblocks",
            &self.confirmed_chain_tip,
            mblocks.len()
        );

        let stacks_dbconn = chainstate.index_conn();
        let headers_db = HeadersDBConn(stacks_dbconn);
        let burn_block_hash = headers_db
            .get_burn_header_hash_for_block(&self.confirmed_chain_tip)
            .expect("BUG: unable to get burn block hash based on chain tip");
        let burn_block_height = headers_db
            .get_burn_block_height_for_block(&self.confirmed_chain_tip)
            .expect("BUG: unable to get burn block height based on chain tip");
        let burn_block_timestamp = headers_db
            .get_burn_block_time_for_block(&self.confirmed_chain_tip, None)
            .expect("BUG: unable to get burn block timestamp based on chain tip");

        let ast_rules = burn_dbconn.get_ast_rules(burn_block_height);

        let mut last_mblock = self.last_mblock.take();
        let mut last_mblock_seq = self.last_mblock_seq;
        let db_config = chainstate.config();

        let mut total_fees = 0;
        let mut total_burns = 0;
        let mut all_receipts = vec![];
        let mut mined_txs = UnconfirmedTxMap::new();
        let mut new_cost = ExecutionCost::ZERO;
        let mut new_bytes = 0;
        let mut num_new_mblocks = 0;
        let mut have_state = self.have_state;

        if !mblocks.is_empty() {
            let cur_cost = self.cost_so_far.clone();

            // NOTE: we *must* commit the clarity_tx now that it's begun.
            // Otherwise, microblock miners can leave the MARF in a partially-initialized state,
            // leading to a node crash.
            let mut clarity_tx = StacksChainState::chainstate_begin_unconfirmed(
                db_config,
                &headers_db,
                &mut self.clarity_inst,
                burn_dbconn,
                &self.confirmed_chain_tip,
            );

            // we must roll this back later
            have_state = true;

            clarity_tx.reset_cost(cur_cost);

            for mblock in mblocks.into_iter() {
                if (last_mblock.is_some() && mblock.header.sequence <= last_mblock_seq)
                    || (last_mblock.is_none() && mblock.header.sequence != 0)
                {
                    debug!(
                        "Skip {} at {} (already represented)",
                        &mblock.block_hash(),
                        mblock.header.sequence
                    );
                    continue;
                }

                let seq = mblock.header.sequence;
                let mblock_hash = mblock.block_hash();
                let mblock_header = mblock.header.clone();

                debug!(
                    "Try to apply microblock {} ({}) to unconfirmed state",
                    &mblock_hash, mblock.header.sequence
                );

                let (stx_fees, stx_burns, receipts) =
                    match StacksChainState::process_microblocks_transactions(
                        &mut clarity_tx,
                        &[mblock.clone()],
                        ast_rules,
                    ) {
                        Ok(x) => x,
                        Err((e, _)) => {
                            // absorb the error
                            warn!("Encountered invalid stacks microblock: {}", &e);
                            break;
                        }
                    };

                total_fees += stx_fees;
                total_burns += stx_burns;
                num_new_mblocks += 1;
                all_receipts.push((seq, mblock.header, receipts));

                last_mblock = Some(mblock_header);
                last_mblock_seq = seq;
                new_bytes += {
                    let mut total = 0;
                    for tx in mblock.txs.iter() {
                        let mut bytes = vec![];
                        tx.consensus_serialize(&mut bytes)
                            .expect("BUG: failed to serialize valid microblock");
                        total += bytes.len();
                    }
                    total as u64
                };

                for tx in &mblock.txs {
                    mined_txs.insert(tx.txid(), (tx.clone(), mblock_hash, seq));
                }
            }

            new_cost = clarity_tx.cost_so_far();
            clarity_tx.commit_unconfirmed();
        };

        self.last_mblock = last_mblock;
        self.last_mblock_seq = last_mblock_seq;
        self.mined_txs.extend(mined_txs);
        self.cost_so_far = new_cost;
        self.bytes_so_far += new_bytes;
        self.num_mblocks_added += num_new_mblocks;
        self.have_state = have_state;

        // apply injected faults
        if self.disable_cost_check {
            warn!("Fault injection: disabling microblock miner's cost tracking");
            self.cost_so_far = ExecutionCost::ZERO;
        }
        if self.disable_bytes_check {
            warn!("Fault injection: disabling microblock miner's size tracking");
            self.bytes_so_far = 0;
        }

        Ok(ProcessedUnconfirmedState {
            total_fees,
            total_burns,
            receipts: all_receipts,
            burn_block_hash,
            burn_block_height,
            burn_block_timestamp,
        })
    }

    /// Load up the Stacks microblock stream to process, composed of only the new microblocks
    fn load_child_microblocks(
        &self,
        chainstate: &StacksChainState,
    ) -> Result<Option<Vec<StacksMicroblock>>, Error> {
        let (consensus_hash, anchored_block_hash) =
            match chainstate.get_block_header_hashes(&self.confirmed_chain_tip)? {
                Some(x) => x,
                None => {
                    return Err(Error::NoSuchBlockError);
                }
            };

        StacksChainState::load_descendant_staging_microblock_stream(
            &chainstate.db(),
            &StacksBlockId::new(&consensus_hash, &anchored_block_hash),
            0,
            u16::MAX,
        )
    }

    /// Update the view of the current confiremd chain tip's unconfirmed microblock state
    /// Returns ProcessedUnconfirmedState for the microblocks newly added to the unconfirmed state
    pub fn refresh(
        &mut self,
        chainstate: &StacksChainState,
        burn_dbconn: &dyn BurnStateDB,
    ) -> Result<ProcessedUnconfirmedState, Error> {
        assert!(
            !self.readonly,
            "BUG: code tried to write unconfirmed state to a read-only instance"
        );

        if self.last_mblock_seq == u16::MAX {
            // no-op
            return Ok(Default::default());
        }

        match self.load_child_microblocks(chainstate)? {
            Some(microblocks) => self.append_microblocks(chainstate, burn_dbconn, microblocks),
            None => Ok(Default::default()),
        }
    }

    /// Is there any state to read?
    pub fn is_readable(&self) -> bool {
        (self.has_data() || self.readonly) && !self.dirty
    }

    /// Can we write to this unconfirmed state?
    pub fn is_writable(&self) -> bool {
        !self.dirty
    }

    /// Mark this unconfirmed state as "dirty", forcing it to be re-instantiated on the next read
    /// or write
    pub fn set_dirty(&mut self, dirty: bool) {
        self.dirty = dirty;
    }

    /// Does the unconfirmed state represent any data?
    fn has_data(&self) -> bool {
        self.last_mblock.is_some()
    }

    /// Does the unconfirmed microblock state represent any transactions?
    pub fn num_mined_txs(&self) -> usize {
        self.mined_txs.len()
    }

    /// Get information about an unconfirmed transaction
    pub fn get_unconfirmed_transaction(
        &self,
        txid: &Txid,
    ) -> Option<(StacksTransaction, BlockHeaderHash, u16)> {
        self.mined_txs.get(txid).cloned()
    }

    pub fn num_microblocks(&self) -> u64 {
        if self.last_mblock.is_some() {
            (self.last_mblock_seq as u64) + 1
        } else {
            0
        }
    }

    /// Try returning the unconfirmed chain tip. Only return the tip if the underlying MARF trie
    /// exists, otherwise return None.
    pub fn get_unconfirmed_state_if_exists(&mut self) -> Result<Option<StacksBlockId>, String> {
        if self.is_readable() {
            let trie_exists = match self
                .clarity_inst
                .trie_exists_for_block(&self.unconfirmed_chain_tip)
            {
                Ok(res) => res,
                Err(e) => {
                    let err_str = format!(
                        "Failed to load Stacks chain tip; error checking underlying trie: {}",
                        e
                    );
                    warn!("{}", err_str);
                    return Err(err_str);
                }
            };

            if trie_exists {
                Ok(Some(self.unconfirmed_chain_tip))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }
}

impl StacksChainState {
    /// Clear the current unconfirmed state
    fn drop_unconfirmed_state(&mut self, mut unconfirmed: UnconfirmedState) {
        if !unconfirmed.have_state {
            debug!(
                "Dropping empty unconfirmed state off of {} ({})",
                &unconfirmed.confirmed_chain_tip, &unconfirmed.unconfirmed_chain_tip
            );
            return;
        }

        // not empty, so do explicit rollback
        debug!(
            "Dropping unconfirmed state off of {} ({})",
            &unconfirmed.confirmed_chain_tip, &unconfirmed.unconfirmed_chain_tip
        );
        unconfirmed
            .clarity_inst
            .drop_unconfirmed_state(&unconfirmed.confirmed_chain_tip)
            .expect("FATAL: failed to drop unconfirmed state");
        debug!(
            "Dropped unconfirmed state off of {} ({})",
            &unconfirmed.confirmed_chain_tip, &unconfirmed.unconfirmed_chain_tip
        );
    }

    /// Instantiate the unconfirmed state of a given chain tip.
    /// Pre-populate it with any microblock state we have.
    fn make_unconfirmed_state(
        &self,
        burn_dbconn: &dyn BurnStateDB,
        anchored_block_id: StacksBlockId,
    ) -> Result<(UnconfirmedState, ProcessedUnconfirmedState), Error> {
        debug!("Make new unconfirmed state off of {}", &anchored_block_id);
        let mut unconfirmed_state = UnconfirmedState::new(self, anchored_block_id)?;
        let processed_unconfirmed_state = unconfirmed_state.refresh(self, burn_dbconn)?;
        debug!(
            "Made new unconfirmed state off of {} (at {})",
            &anchored_block_id, &unconfirmed_state.unconfirmed_chain_tip
        );
        Ok((unconfirmed_state, processed_unconfirmed_state))
    }

    /// Reload the unconfirmed view from a new chain tip.
    /// -- if the canonical chain tip hasn't changed, then just apply any new microblocks that have arrived.
    /// -- if the canonical chain tip has changed, then drop the current view, make a new view, and
    /// process that new view's unconfirmed microblocks.
    /// Call after storing all microblocks from the network.
    pub fn reload_unconfirmed_state(
        &mut self,
        burn_dbconn: &dyn BurnStateDB,
        canonical_tip: StacksBlockId,
    ) -> Result<ProcessedUnconfirmedState, Error> {
        debug!("Reload unconfirmed state off of {}", &canonical_tip);

        let unconfirmed_state = self.unconfirmed_state.take();
        if let Some(mut unconfirmed_state) = unconfirmed_state {
            if unconfirmed_state.is_readable() {
                if canonical_tip == unconfirmed_state.confirmed_chain_tip {
                    // refresh with latest microblocks
                    let res = unconfirmed_state.refresh(self, burn_dbconn);
                    debug!(
                        "Unconfirmed state off of {} ({}) refreshed",
                        canonical_tip, &unconfirmed_state.unconfirmed_chain_tip
                    );

                    self.unconfirmed_state = Some(unconfirmed_state);
                    return res;
                } else {
                    // got a new tip; will imminently drop
                    self.unconfirmed_state = Some(unconfirmed_state);
                }
            } else {
                // will need to drop this anyway -- it's dirty, or not instantiated
                self.drop_unconfirmed_state(unconfirmed_state);
            }
        }

        // tip changed, or we don't have unconfirmed state yet, or we do and it's dirty, or it was
        // never instantiated anyway
        if let Some(unconfirmed_state) = self.unconfirmed_state.take() {
            self.drop_unconfirmed_state(unconfirmed_state);
        }

        let (new_unconfirmed_state, processed_unconfirmed_state) =
            self.make_unconfirmed_state(burn_dbconn, canonical_tip)?;

        debug!(
            "Unconfirmed state off of {} reloaded (new unconfirmed tip is {})",
            canonical_tip, &new_unconfirmed_state.unconfirmed_chain_tip
        );

        self.unconfirmed_state = Some(new_unconfirmed_state);
        Ok(processed_unconfirmed_state)
    }

    /// Refresh the current unconfirmed chain state
    pub fn refresh_unconfirmed_state(
        &mut self,
        burn_dbconn: &dyn BurnStateDB,
    ) -> Result<ProcessedUnconfirmedState, Error> {
        let mut unconfirmed_state = self.unconfirmed_state.take();
        let res = if let Some(ref mut unconfirmed_state) = unconfirmed_state {
            if !unconfirmed_state.is_readable() {
                warn!("Unconfirmed state is not readable; it will soon be refreshed");
                return Ok(Default::default());
            }

            debug!(
                "Refresh unconfirmed state off of {} ({})",
                &unconfirmed_state.confirmed_chain_tip, &unconfirmed_state.unconfirmed_chain_tip
            );
            let res = unconfirmed_state.refresh(self, burn_dbconn);
            if res.is_ok() {
                debug!(
                    "Unconfirmed chain tip is {}",
                    &unconfirmed_state.unconfirmed_chain_tip
                );
            }
            res
        } else {
            warn!("No unconfirmed state instantiated");
            Ok(Default::default())
        };
        self.unconfirmed_state = unconfirmed_state;
        res
    }

    /// Instantiate a read-only view of unconfirmed state.
    /// Use from a dedicated chainstate handle that will only do read-only operations on it (such
    /// as the p2p network thread)
    pub fn refresh_unconfirmed_readonly(
        &mut self,
        canonical_tip: StacksBlockId,
    ) -> Result<(), Error> {
        if let Some(ref unconfirmed) = self.unconfirmed_state {
            assert!(
                unconfirmed.readonly,
                "BUG: tried to replace a read/write unconfirmed state instance"
            );
        }

        let unconfirmed = UnconfirmedState::new_readonly(self, canonical_tip)?;
        self.unconfirmed_state = Some(unconfirmed);
        Ok(())
    }

    pub fn set_unconfirmed_dirty(&mut self, dirty: bool) {
        if let Some(ref mut unconfirmed) = self.unconfirmed_state.as_mut() {
            unconfirmed.dirty = dirty;
        }
    }
}

#[cfg(test)]
mod test {
    use std::fs;

    use clarity::vm::types::StacksAddressExtensions;

    use super::*;
    use crate::burnchains::PublicKey;
    use crate::chainstate::burn::db::sortdb::*;
    use crate::chainstate::burn::db::*;
    use crate::chainstate::stacks::db::test::*;
    use crate::chainstate::stacks::db::*;
    use crate::chainstate::stacks::index::marf::*;
    use crate::chainstate::stacks::index::node::*;
    use crate::chainstate::stacks::index::*;
    use crate::chainstate::stacks::miner::*;
    use crate::chainstate::stacks::tests::make_coinbase;
    use crate::chainstate::stacks::{C32_ADDRESS_VERSION_TESTNET_SINGLESIG, *};
    use crate::core::mempool::*;
    use crate::core::*;
    use crate::net::relay::*;
    use crate::net::test::*;

    #[test]
    fn test_unconfirmed_refresh_one_microblock_stx_transfer() {
        let privk = StacksPrivateKey::new();
        let addr = StacksAddress::from_public_keys(
            C32_ADDRESS_VERSION_TESTNET_SINGLESIG,
            &AddressHashMode::SerializeP2PKH,
            1,
            &vec![StacksPublicKey::from_private(&privk)],
        )
        .unwrap();

        let initial_balance = 1000000000;
        let mut peer_config = TestPeerConfig::new(function_name!(), 7000, 7001);
        peer_config.initial_balances = vec![(addr.to_account_principal(), initial_balance)];
        let burnchain = peer_config.burnchain.clone();

        let mut peer = TestPeer::new(peer_config);

        let chainstate_path = peer.chainstate_path.clone();

        let num_blocks = 10;
        let first_stacks_block_height = {
            let sn =
                SortitionDB::get_canonical_burn_chain_tip(&peer.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            sn.block_height
        };

        let mut last_block: Option<StacksBlock> = None;
        for tenure_id in 0..num_blocks {
            let microblock_privkey = StacksPrivateKey::new();
            let microblock_pubkeyhash =
                Hash160::from_node_public_key(&StacksPublicKey::from_private(&microblock_privkey));

            // send transactions to the mempool
            let tip =
                SortitionDB::get_canonical_burn_chain_tip(&peer.sortdb.as_ref().unwrap().conn())
                    .unwrap();

            assert_eq!(
                tip.block_height,
                first_stacks_block_height + (tenure_id as u64)
            );
            if let Some(block) = last_block {
                assert_eq!(tip.winning_stacks_block_hash, block.block_hash());
            }

            let mut anchor_size = 0;
            let mut anchor_cost = ExecutionCost::ZERO;

            let (burn_ops, stacks_block, _) = peer.make_tenure(
                |ref mut miner,
                 ref mut sortdb,
                 ref mut chainstate,
                 vrf_proof,
                 ref parent_opt,
                 _| {
                    let parent_tip = match parent_opt {
                        None => StacksChainState::get_genesis_header_info(chainstate.db()).unwrap(),
                        Some(block) => {
                            let ic = sortdb.index_conn();
                            let snapshot =
                                SortitionDB::get_block_snapshot_for_winning_stacks_block(
                                    &ic,
                                    &tip.sortition_id,
                                    &block.block_hash(),
                                )
                                .unwrap()
                                .unwrap(); // succeeds because we don't fork
                            StacksChainState::get_anchored_block_header_info(
                                chainstate.db(),
                                &snapshot.consensus_hash,
                                &snapshot.winning_stacks_block_hash,
                            )
                            .unwrap()
                            .unwrap()
                        }
                    };

                    let block_builder = StacksBlockBuilder::make_regtest_block_builder(
                        &burnchain,
                        &parent_tip,
                        vrf_proof,
                        tip.total_burn,
                        microblock_pubkeyhash,
                    )
                    .unwrap();

                    let coinbase_tx = make_coinbase(miner, tenure_id);
                    let (anchored_block, anchored_block_size, anchored_block_cost) =
                        StacksBlockBuilder::make_anchored_block_from_txs(
                            block_builder,
                            chainstate,
                            &sortdb.index_handle_at_tip(),
                            vec![coinbase_tx],
                        )
                        .unwrap();

                    anchor_size = anchored_block_size;
                    anchor_cost = anchored_block_cost;
                    (anchored_block, vec![])
                },
            );

            last_block = Some(stacks_block.clone());
            let (_, _, consensus_hash) = peer.next_burnchain_block(burn_ops.clone());
            peer.process_stacks_epoch_at_tip(&stacks_block, &[]);

            let canonical_tip = StacksBlockId::new(&consensus_hash, &stacks_block.block_hash());

            let recv_addr =
                StacksAddress::from_string("ST1H1B54MY50RMBRRKS7GV2ZWG79RZ1RQ1ETW4E01").unwrap();

            // build 1-block microblock stream
            let microblocks = {
                let sortdb = peer.sortdb.take().unwrap();
                let sort_iconn = sortdb
                    .index_handle_at_block(&peer.chainstate(), &canonical_tip)
                    .unwrap();
                peer.chainstate()
                    .reload_unconfirmed_state(&sort_iconn, canonical_tip.clone())
                    .unwrap();

                let microblock = {
                    let mut microblock_builder = StacksMicroblockBuilder::new(
                        stacks_block.block_hash(),
                        consensus_hash.clone(),
                        peer.chainstate(),
                        &sort_iconn,
                        BlockBuilderSettings::max_value(),
                    )
                    .unwrap();

                    // make a single stx-transfer
                    let auth = TransactionAuth::Standard(
                        TransactionSpendingCondition::new_singlesig_p2pkh(
                            StacksPublicKey::from_private(&privk),
                        )
                        .unwrap(),
                    );
                    let mut tx_stx_transfer = StacksTransaction::new(
                        TransactionVersion::Testnet,
                        auth.clone(),
                        TransactionPayload::TokenTransfer(
                            recv_addr.clone().into(),
                            1,
                            TokenTransferMemo([0u8; 34]),
                        ),
                    );

                    tx_stx_transfer.chain_id = 0x80000000;
                    tx_stx_transfer.post_condition_mode = TransactionPostConditionMode::Allow;
                    tx_stx_transfer.set_tx_fee(0);
                    tx_stx_transfer.set_origin_nonce(tenure_id as u64);

                    let mut signer = StacksTransactionSigner::new(&tx_stx_transfer);
                    signer.sign_origin(&privk).unwrap();

                    let signed_tx = signer.get_tx().unwrap();
                    let signed_tx_len = {
                        let mut bytes = vec![];
                        signed_tx.consensus_serialize(&mut bytes).unwrap();
                        bytes.len() as u64
                    };

                    let microblock = microblock_builder
                        .mine_next_microblock_from_txs(
                            vec![(signed_tx, signed_tx_len)],
                            &microblock_privkey,
                        )
                        .unwrap();
                    microblock
                };

                peer.sortdb = Some(sortdb);
                vec![microblock]
            };

            // store microblock stream
            for mblock in microblocks.into_iter() {
                peer.chainstate()
                    .preprocess_streamed_microblock(
                        &consensus_hash,
                        &stacks_block.block_hash(),
                        &mblock,
                    )
                    .unwrap();
            }

            // process microblock stream to generate unconfirmed state
            let sortdb = peer.sortdb.take().unwrap();
            let iconn = sortdb
                .index_handle_at_block(&peer.chainstate(), &canonical_tip)
                .unwrap();
            peer.chainstate()
                .reload_unconfirmed_state(&iconn, canonical_tip.clone())
                .unwrap();

            let recv_balance = peer
                .chainstate()
                .with_read_only_unconfirmed_clarity_tx(&iconn, |clarity_tx| {
                    clarity_tx.with_clarity_db_readonly(|clarity_db| {
                        clarity_db
                            .get_account_stx_balance(&recv_addr.into())
                            .unwrap()
                    })
                })
                .unwrap()
                .unwrap();
            peer.sortdb = Some(sortdb);

            // move 1 stx per round
            assert_eq!(recv_balance.amount_unlocked(), (tenure_id + 1) as u128);
            let (canonical_burn, canonical_block) =
                SortitionDB::get_canonical_stacks_chain_tip_hash(peer.sortdb().conn()).unwrap();

            let sortdb = peer.sortdb.take().unwrap();
            let iconn = sortdb
                .index_handle_at_block(&peer.chainstate(), &canonical_tip)
                .unwrap();
            let confirmed_recv_balance = peer
                .chainstate()
                .with_read_only_clarity_tx(&iconn, &canonical_tip, |clarity_tx| {
                    clarity_tx.with_clarity_db_readonly(|clarity_db| {
                        clarity_db
                            .get_account_stx_balance(&recv_addr.into())
                            .unwrap()
                    })
                })
                .unwrap();
            peer.sortdb = Some(sortdb);

            assert_eq!(confirmed_recv_balance.amount_unlocked(), tenure_id as u128);
            eprintln!("\nrecv_balance: {}\nconfirmed_recv_balance: {}\nblock header {}: {:?}\ntip: {}/{}\n", recv_balance.amount_unlocked(), confirmed_recv_balance.amount_unlocked(), &stacks_block.block_hash(), &stacks_block.header, &canonical_burn, &canonical_block);
        }
    }

    #[test]
    fn test_unconfirmed_refresh_10_microblocks_10_stx_transfers() {
        let privk = StacksPrivateKey::new();
        let addr = StacksAddress::from_public_keys(
            C32_ADDRESS_VERSION_TESTNET_SINGLESIG,
            &AddressHashMode::SerializeP2PKH,
            1,
            &vec![StacksPublicKey::from_private(&privk)],
        )
        .unwrap();

        let initial_balance = 1000000000;
        let mut peer_config = TestPeerConfig::new(function_name!(), 7002, 7003);
        peer_config.initial_balances = vec![(addr.to_account_principal(), initial_balance)];
        let burnchain = peer_config.burnchain.clone();

        let mut peer = TestPeer::new(peer_config);

        let chainstate_path = peer.chainstate_path.clone();

        let num_blocks = 10;
        let first_stacks_block_height = {
            let tip =
                SortitionDB::get_canonical_burn_chain_tip(&peer.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            tip.block_height
        };

        let mut last_block: Option<StacksBlock> = None;
        for tenure_id in 0..num_blocks {
            let microblock_privkey = StacksPrivateKey::new();
            let microblock_pubkeyhash =
                Hash160::from_node_public_key(&StacksPublicKey::from_private(&microblock_privkey));

            // send transactions to the mempool
            let tip =
                SortitionDB::get_canonical_burn_chain_tip(&peer.sortdb.as_ref().unwrap().conn())
                    .unwrap();

            assert_eq!(
                tip.block_height,
                first_stacks_block_height + (tenure_id as u64)
            );
            if let Some(block) = last_block {
                assert_eq!(tip.winning_stacks_block_hash, block.block_hash());
            }

            let mut anchor_size = 0;
            let mut anchor_cost = ExecutionCost::ZERO;

            let (burn_ops, stacks_block, _) = peer.make_tenure(
                |ref mut miner,
                 ref mut sortdb,
                 ref mut chainstate,
                 vrf_proof,
                 ref parent_opt,
                 _| {
                    let parent_tip = match parent_opt {
                        None => StacksChainState::get_genesis_header_info(chainstate.db()).unwrap(),
                        Some(block) => {
                            let ic = sortdb.index_conn();
                            let snapshot =
                                SortitionDB::get_block_snapshot_for_winning_stacks_block(
                                    &ic,
                                    &tip.sortition_id,
                                    &block.block_hash(),
                                )
                                .unwrap()
                                .unwrap(); // succeeds because we don't fork
                            StacksChainState::get_anchored_block_header_info(
                                chainstate.db(),
                                &snapshot.consensus_hash,
                                &snapshot.winning_stacks_block_hash,
                            )
                            .unwrap()
                            .unwrap()
                        }
                    };

                    let block_builder = StacksBlockBuilder::make_regtest_block_builder(
                        &burnchain,
                        &parent_tip,
                        vrf_proof,
                        tip.total_burn,
                        microblock_pubkeyhash,
                    )
                    .unwrap();

                    let coinbase_tx = make_coinbase(miner, tenure_id);
                    let (anchored_block, anchored_block_size, anchored_block_cost) =
                        StacksBlockBuilder::make_anchored_block_from_txs(
                            block_builder,
                            chainstate,
                            &sortdb.index_handle_at_tip(),
                            vec![coinbase_tx],
                        )
                        .unwrap();

                    anchor_size = anchored_block_size;
                    anchor_cost = anchored_block_cost;
                    (anchored_block, vec![])
                },
            );

            last_block = Some(stacks_block.clone());
            let (_, _, consensus_hash) = peer.next_burnchain_block(burn_ops.clone());
            peer.process_stacks_epoch_at_tip(&stacks_block, &[]);

            let canonical_tip = StacksBlockId::new(&consensus_hash, &stacks_block.block_hash());

            let recv_addr =
                StacksAddress::from_string("ST1H1B54MY50RMBRRKS7GV2ZWG79RZ1RQ1ETW4E01").unwrap();

            // build microblock stream iteratively, and test balances at each additional microblock
            let sortdb = peer.sortdb.take().unwrap();
            let microblocks = {
                let sort_iconn = sortdb
                    .index_handle_at_block(&peer.chainstate(), &canonical_tip)
                    .unwrap();
                peer.chainstate()
                    .reload_unconfirmed_state(&sort_iconn, canonical_tip.clone())
                    .unwrap();

                let mut microblock_builder = StacksMicroblockBuilder::new(
                    stacks_block.block_hash(),
                    consensus_hash.clone(),
                    peer.chainstate(),
                    &sort_iconn,
                    BlockBuilderSettings::max_value(),
                )
                .unwrap();
                let mut microblocks = vec![];
                for i in 0..10 {
                    let mut signed_txs = vec![];
                    for j in 0..10 {
                        // make 10 stx-transfers in 10 microblocks (100 txs total)
                        let auth = TransactionAuth::Standard(
                            TransactionSpendingCondition::new_singlesig_p2pkh(
                                StacksPublicKey::from_private(&privk),
                            )
                            .unwrap(),
                        );
                        let mut tx_stx_transfer = StacksTransaction::new(
                            TransactionVersion::Testnet,
                            auth.clone(),
                            TransactionPayload::TokenTransfer(
                                recv_addr.clone().into(),
                                1,
                                TokenTransferMemo([0u8; 34]),
                            ),
                        );

                        tx_stx_transfer.chain_id = 0x80000000;
                        tx_stx_transfer.post_condition_mode = TransactionPostConditionMode::Allow;
                        tx_stx_transfer.set_tx_fee(0);
                        tx_stx_transfer.set_origin_nonce((100 * tenure_id + 10 * i + j) as u64);

                        let mut signer = StacksTransactionSigner::new(&tx_stx_transfer);
                        signer.sign_origin(&privk).unwrap();

                        let signed_tx = signer.get_tx().unwrap();
                        signed_txs.push(signed_tx);
                    }

                    let signed_mempool_txs = signed_txs
                        .into_iter()
                        .map(|tx| {
                            let bytes = tx.serialize_to_vec();
                            (tx, bytes.len() as u64)
                        })
                        .collect();

                    let microblock = microblock_builder
                        .mine_next_microblock_from_txs(signed_mempool_txs, &microblock_privkey)
                        .unwrap();
                    microblocks.push(microblock);
                }
                microblocks
            };
            peer.sortdb = Some(sortdb);

            // store microblock stream
            for (i, mblock) in microblocks.into_iter().enumerate() {
                peer.chainstate()
                    .preprocess_streamed_microblock(
                        &consensus_hash,
                        &stacks_block.block_hash(),
                        &mblock,
                    )
                    .unwrap();

                // process microblock stream to generate unconfirmed state
                let sortdb = peer.sortdb.take().unwrap();
                peer.chainstate()
                    .reload_unconfirmed_state(&sortdb.index_handle_at_tip(), canonical_tip.clone())
                    .unwrap();

                let recv_balance = peer
                    .chainstate()
                    .with_read_only_unconfirmed_clarity_tx(
                        &sortdb.index_handle_at_tip(),
                        |clarity_tx| {
                            clarity_tx.with_clarity_db_readonly(|clarity_db| {
                                clarity_db
                                    .get_account_stx_balance(&recv_addr.into())
                                    .unwrap()
                            })
                        },
                    )
                    .unwrap()
                    .unwrap();
                peer.sortdb = Some(sortdb);

                // move 100 ustx per round -- 10 per mblock
                assert_eq!(
                    recv_balance.amount_unlocked(),
                    (100 * tenure_id + 10 * (i + 1)) as u128
                );
                let (canonical_burn, canonical_block) =
                    SortitionDB::get_canonical_stacks_chain_tip_hash(peer.sortdb().conn()).unwrap();

                let sortdb = peer.sortdb.take().unwrap();
                let confirmed_recv_balance = peer
                    .chainstate()
                    .with_read_only_clarity_tx(
                        &sortdb.index_handle_at_tip(),
                        &canonical_tip,
                        |clarity_tx| {
                            clarity_tx.with_clarity_db_readonly(|clarity_db| {
                                clarity_db
                                    .get_account_stx_balance(&recv_addr.into())
                                    .unwrap()
                            })
                        },
                    )
                    .unwrap();
                peer.sortdb = Some(sortdb);

                assert_eq!(
                    confirmed_recv_balance.amount_unlocked(),
                    100 * tenure_id as u128
                );
                eprintln!("\nrecv_balance: {}\nconfirmed_recv_balance: {}\nblock header {}: {:?}\ntip: {}/{}\n", recv_balance.amount_unlocked(), confirmed_recv_balance.amount_unlocked(), &stacks_block.block_hash(), &stacks_block.header, &canonical_burn, &canonical_block);
            }
        }
    }

    #[test]
    fn test_unconfirmed_refresh_invalid_microblock() {
        let privk = StacksPrivateKey::new();
        let addr = StacksAddress::from_public_keys(
            C32_ADDRESS_VERSION_TESTNET_SINGLESIG,
            &AddressHashMode::SerializeP2PKH,
            1,
            &vec![StacksPublicKey::from_private(&privk)],
        )
        .unwrap();

        let initial_balance = 1000000000;
        let mut peer_config = TestPeerConfig::new(function_name!(), 7004, 7005);
        peer_config.initial_balances = vec![(addr.to_account_principal(), initial_balance)];
        peer_config.epochs = Some(EpochList::new(&[StacksEpoch {
            epoch_id: StacksEpochId::Epoch20,
            start_height: 0,
            end_height: (i64::MAX) as u64,
            block_limit: BLOCK_LIMIT_MAINNET_20,
            network_epoch: PEER_VERSION_EPOCH_2_0,
        }]));
        let burnchain = peer_config.burnchain.clone();

        let mut peer = TestPeer::new(peer_config);
        let chainstate_path = peer.chainstate_path.clone();

        let num_blocks = 5;
        let num_microblocks = 3;
        let first_stacks_block_height = {
            let tip =
                SortitionDB::get_canonical_burn_chain_tip(&peer.sortdb.as_ref().unwrap().conn())
                    .unwrap();
            tip.block_height
        };

        let mut last_block: Option<StacksBlock> = None;
        let mut next_nonce = 0;
        let recv_addr =
            StacksAddress::from_string("ST1H1B54MY50RMBRRKS7GV2ZWG79RZ1RQ1ETW4E01").unwrap();
        let mut recv_balance = 0;

        for tenure_id in 0..num_blocks {
            let microblock_privkey = StacksPrivateKey::new();
            let microblock_pubkeyhash =
                Hash160::from_node_public_key(&StacksPublicKey::from_private(&microblock_privkey));

            // send transactions to the mempool
            let tip =
                SortitionDB::get_canonical_burn_chain_tip(&peer.sortdb.as_ref().unwrap().conn())
                    .unwrap();

            assert_eq!(
                tip.block_height,
                first_stacks_block_height + (tenure_id as u64)
            );
            if let Some(block) = last_block {
                assert_eq!(tip.winning_stacks_block_hash, block.block_hash());
            }

            let mut anchor_size = 0;
            let mut anchor_cost = ExecutionCost::ZERO;

            let (burn_ops, stacks_block, _) = peer.make_tenure(
                |ref mut miner,
                 ref mut sortdb,
                 ref mut chainstate,
                 vrf_proof,
                 ref parent_opt,
                 _| {
                    let parent_tip = match parent_opt {
                        None => StacksChainState::get_genesis_header_info(chainstate.db()).unwrap(),
                        Some(block) => {
                            let ic = sortdb.index_conn();
                            let snapshot =
                                SortitionDB::get_block_snapshot_for_winning_stacks_block(
                                    &ic,
                                    &tip.sortition_id,
                                    &block.block_hash(),
                                )
                                .unwrap()
                                .unwrap(); // succeeds because we don't fork
                            StacksChainState::get_anchored_block_header_info(
                                chainstate.db(),
                                &snapshot.consensus_hash,
                                &snapshot.winning_stacks_block_hash,
                            )
                            .unwrap()
                            .unwrap()
                        }
                    };

                    let block_builder = StacksBlockBuilder::make_regtest_block_builder(
                        &burnchain,
                        &parent_tip,
                        vrf_proof,
                        tip.total_burn,
                        microblock_pubkeyhash,
                    )
                    .unwrap();

                    let anchored_tx = {
                        let tx = {
                            let auth = TransactionAuth::Standard(
                                TransactionSpendingCondition::new_singlesig_p2pkh(
                                    StacksPublicKey::from_private(&privk),
                                )
                                .unwrap(),
                            );
                            let mut tx_stx_transfer = StacksTransaction::new(
                                TransactionVersion::Testnet,
                                auth.clone(),
                                TransactionPayload::TokenTransfer(
                                    recv_addr.clone().into(),
                                    1,
                                    TokenTransferMemo([0u8; 34]),
                                ),
                            );

                            tx_stx_transfer.chain_id = 0x80000000;
                            tx_stx_transfer.post_condition_mode =
                                TransactionPostConditionMode::Allow;
                            tx_stx_transfer.set_tx_fee(0);
                            tx_stx_transfer.set_origin_nonce(next_nonce);
                            next_nonce += 1;
                            tx_stx_transfer
                        };

                        let mut signer = StacksTransactionSigner::new(&tx);
                        signer.sign_origin(&privk).unwrap();

                        let signed_tx = signer.get_tx().unwrap();
                        signed_tx
                    };
                    // this will be accepted
                    recv_balance += 1;

                    let coinbase_tx = make_coinbase(miner, tenure_id);
                    let (anchored_block, anchored_block_size, anchored_block_cost) =
                        StacksBlockBuilder::make_anchored_block_from_txs(
                            block_builder,
                            chainstate,
                            &sortdb.index_handle_at_tip(),
                            vec![coinbase_tx, anchored_tx],
                        )
                        .unwrap();

                    anchor_size = anchored_block_size;
                    anchor_cost = anchored_block_cost;
                    (anchored_block, vec![])
                },
            );

            last_block = Some(stacks_block.clone());
            let (_, _, consensus_hash) = peer.next_burnchain_block(burn_ops.clone());
            peer.process_stacks_epoch_at_tip(&stacks_block, &[]);

            let canonical_tip = StacksBlockHeader::make_index_block_hash(
                &consensus_hash,
                &stacks_block.block_hash(),
            );

            let mut sortdb = peer.sortdb.take().unwrap();
            let mut inner_node = peer.stacks_node.take().unwrap();

            for i in 0..num_microblocks {
                Relayer::refresh_unconfirmed(&mut inner_node.chainstate, &mut sortdb);

                let microblock = {
                    let sort_iconn = sortdb.index_handle_at_tip();
                    let mut microblock_builder = StacksMicroblockBuilder::resume_unconfirmed(
                        &mut inner_node.chainstate,
                        &sort_iconn,
                        &anchor_cost,
                        BlockBuilderSettings::max_value(),
                    )
                    .unwrap();

                    // make a valid and then an in invalid microblock
                    let mut signed_txs = vec![];
                    let tx = {
                        let auth = TransactionAuth::Standard(
                            TransactionSpendingCondition::new_singlesig_p2pkh(
                                StacksPublicKey::from_private(&privk),
                            )
                            .unwrap(),
                        );
                        let mut tx_stx_transfer = StacksTransaction::new(
                            TransactionVersion::Testnet,
                            auth.clone(),
                            TransactionPayload::TokenTransfer(
                                recv_addr.clone().into(),
                                1,
                                TokenTransferMemo([0u8; 34]),
                            ),
                        );

                        tx_stx_transfer.chain_id = 0x80000000;
                        tx_stx_transfer.post_condition_mode = TransactionPostConditionMode::Allow;
                        tx_stx_transfer.set_tx_fee(0);

                        if tenure_id % 2 == 0 {
                            // stream has an intermittent bad microblock
                            if i > 0 {
                                tx_stx_transfer.set_origin_nonce(next_nonce + i + 1000);
                            // bad nonce
                            } else {
                                tx_stx_transfer.set_origin_nonce(next_nonce);
                                next_nonce += 1;
                                recv_balance += 1;
                            }
                        } else {
                            // stream starts with a bad microblock
                            if i == 0 {
                                tx_stx_transfer.set_origin_nonce(next_nonce + i + 1000);
                            // bad nonce
                            } else {
                                tx_stx_transfer.set_origin_nonce(next_nonce);
                            }
                        }

                        let mut signer = StacksTransactionSigner::new(&tx_stx_transfer);
                        signer.sign_origin(&privk).unwrap();

                        let signed_tx = signer.get_tx().unwrap();
                        signed_tx
                    };

                    signed_txs.push(tx);

                    let microblock = microblock_builder
                        .make_next_microblock(signed_txs, &microblock_privkey, vec![], None)
                        .unwrap();
                    microblock
                };

                inner_node
                    .chainstate
                    .preprocess_streamed_microblock(
                        &consensus_hash,
                        &stacks_block.block_hash(),
                        &microblock,
                    )
                    .unwrap();
            }

            peer.sortdb = Some(sortdb);
            peer.stacks_node = Some(inner_node);
        }

        let (consensus_hash, canonical_block) =
            SortitionDB::get_canonical_stacks_chain_tip_hash(peer.sortdb().conn()).unwrap();
        let canonical_tip =
            StacksBlockHeader::make_index_block_hash(&consensus_hash, &canonical_block);

        // process microblock stream to generate unconfirmed state
        let sortdb = peer.sortdb.take().unwrap();
        let iconn = sortdb
            .index_handle_at_block(&peer.chainstate(), &canonical_tip)
            .unwrap();
        peer.chainstate()
            .reload_unconfirmed_state(&iconn, canonical_tip.clone())
            .unwrap();

        let db_recv_balance = peer
            .chainstate()
            .with_read_only_unconfirmed_clarity_tx(&iconn, |clarity_tx| {
                clarity_tx.with_clarity_db_readonly(|clarity_db| {
                    clarity_db
                        .get_account_stx_balance(&recv_addr.into())
                        .unwrap()
                })
            })
            .unwrap()
            .unwrap();
        peer.sortdb = Some(sortdb);

        // all valid txs were processed
        assert_eq!(db_recv_balance.amount_unlocked(), recv_balance);
    }
}
