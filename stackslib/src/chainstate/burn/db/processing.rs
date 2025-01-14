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

use stacks_common::address::AddressHashMode;
use stacks_common::types::chainstate::{BurnchainHeaderHash, PoxId, SortitionId, TrieHash};

use crate::burnchains::{
    Burnchain, BurnchainBlockHeader, BurnchainStateTransition, Error as BurnchainError,
};
use crate::chainstate::burn::db::sortdb::{InitialMiningBonus, SortitionDB, SortitionHandleTx};
use crate::chainstate::burn::operations::leader_block_commit::{MissedBlockCommit, RewardSetInfo};
use crate::chainstate::burn::operations::{BlockstackOperationType, Error as OpError};
use crate::chainstate::burn::BlockSnapshot;
use crate::chainstate::coordinator::RewardCycleInfo;
use crate::chainstate::stacks::db::StacksChainState;
use crate::chainstate::stacks::index::marf::MARF;
use crate::chainstate::stacks::index::storage::TrieFileStorage;
use crate::chainstate::stacks::index::{Error as MARFError, MARFValue, MarfTrieId};
use crate::core::INITIAL_MINING_BONUS_WINDOW;
use crate::util_lib::db::Error as DBError;

impl SortitionHandleTx<'_> {
    /// Run a blockstack operation's "check()" method and return the result.
    fn check_transaction(
        &mut self,
        burnchain: &Burnchain,
        blockstack_op: &mut BlockstackOperationType,
        reward_info: Option<&RewardSetInfo>,
    ) -> Result<(), BurnchainError> {
        match blockstack_op {
            BlockstackOperationType::LeaderKeyRegister(ref op) => {
                op.check(burnchain, self).map_err(|e| {
                    warn!(
                        "REJECTED({}) leader key register {} at {},{}: {:?}",
                        op.block_height, &op.txid, op.block_height, op.vtxindex, &e;
                        "consensus_hash" => %op.consensus_hash
                    );
                    BurnchainError::OpError(e)
                })
            }
            BlockstackOperationType::LeaderBlockCommit(ref mut op) => {
                op.check(burnchain, self, reward_info).map_err(|e| {
                    warn!(
                        "REJECTED({}) leader block commit {} at {},{} (parent {},{}): {:?}",
                        op.block_height,
                        &op.txid,
                        op.block_height,
                        op.vtxindex,
                        op.parent_block_ptr,
                        op.parent_vtxindex,
                        &e;
                        "stacks_block_hash" => %op.block_header_hash
                    );
                    BurnchainError::OpError(e)
                })
            }
            BlockstackOperationType::StackStx(ref op) => op.check().map_err(|e| {
                warn!(
                    "REJECTED({}) stack stx op {} at {},{}: {:?}",
                    op.block_height, &op.txid, op.block_height, op.vtxindex, &e
                );
                BurnchainError::OpError(e)
            }),
            BlockstackOperationType::TransferStx(ref op) => op.check().map_err(|e| {
                warn!(
                    "REJECTED({}) transfer stx op {} at {},{}: {:?}",
                    op.block_height, &op.txid, op.block_height, op.vtxindex, &e
                );
                BurnchainError::OpError(e)
            }),
            BlockstackOperationType::PreStx(_) => {
                // no check() required for PreStx
                Ok(())
            }
            BlockstackOperationType::DelegateStx(ref op) => op.check().map_err(|e| {
                warn!(
                    "REJECTED({}) delegate stx op {} at {},{}: {:?}",
                    op.block_height, &op.txid, op.block_height, op.vtxindex, &e
                );
                BurnchainError::OpError(e)
            }),
            BlockstackOperationType::VoteForAggregateKey(ref op) => op.check().map_err(|e| {
                warn!(
                    "REJECTED({}) vote for aggregate key op {} at {},{}: {:?}",
                    op.block_height, &op.txid, op.block_height, op.vtxindex, &e
                );
                BurnchainError::OpError(e)
            }),
        }
    }

    /// Process all block's checked transactions
    /// * make the burn distribution
    /// * insert the ones that went into the burn distribution
    /// * snapshot the block and run the sortition
    /// * return the snapshot (and sortition results)
    fn process_checked_block_ops(
        &mut self,
        mainnet: bool,
        burnchain: &Burnchain,
        parent_snapshot: &BlockSnapshot,
        block_header: &BurnchainBlockHeader,
        this_block_ops: &[BlockstackOperationType],
        missed_commits: &[MissedBlockCommit],
        next_pox_info: Option<RewardCycleInfo>,
        parent_pox: PoxId,
        reward_info: Option<&RewardSetInfo>,
        initial_mining_bonus_ustx: u128,
    ) -> Result<(BlockSnapshot, BurnchainStateTransition), BurnchainError> {
        let this_block_height = block_header.block_height;
        let this_block_hash = block_header.block_hash.clone();

        // make the burn distribution, and in doing so, identify the user burns that we'll keep
        let state_transition = BurnchainStateTransition::from_block_ops(self, burnchain, parent_snapshot, this_block_ops, missed_commits)
            .map_err(|e| {
                error!("TRANSACTION ABORTED when converting {} blockstack operations in block {} ({}) to a burn distribution: {:?}", this_block_ops.len(), this_block_height, &this_block_hash, e);
                e
            })?;

        let next_pox = SortitionDB::make_next_pox_id(parent_pox.clone(), next_pox_info.as_ref());
        let next_sortition_id = SortitionDB::make_next_sortition_id(
            parent_pox.clone(),
            &this_block_hash,
            next_pox_info.as_ref(),
        );

        // do the cryptographic sortition and pick the next winning block.
        let mut snapshot = BlockSnapshot::make_snapshot(
            mainnet,
            self,
            burnchain,
            &next_sortition_id,
            &next_pox,
            parent_snapshot,
            block_header,
            &state_transition,
            initial_mining_bonus_ustx,
        )
        .map_err(|e| {
            error!(
                "TRANSACTION ABORTED when taking snapshot at block {} ({}): {:?}",
                this_block_height, &this_block_hash, e
            );
            BurnchainError::DBError(e)
        })?;

        let snapshot_epoch = SortitionDB::get_stacks_epoch(self, snapshot.block_height)?
            .unwrap_or_else(|| {
                panic!("FATAL: no epoch defined for snapshot");
            });

        // was this snapshot the first with mining?
        //  compute the initial block rewards.
        let initialize_bonus = if snapshot.sortition && parent_snapshot.total_burn == 0 {
            let blocks_without_winners =
                snapshot.block_height - burnchain.initial_reward_start_block;
            let mut total_reward = 0;
            for burn_block_height in burnchain.initial_reward_start_block..snapshot.block_height {
                total_reward += StacksChainState::get_coinbase_reward(
                    snapshot_epoch.epoch_id,
                    mainnet,
                    burn_block_height,
                    self.context.first_block_height,
                );
            }
            let per_block = total_reward / INITIAL_MINING_BONUS_WINDOW as u128;

            info!("First sortition winner chosen";
                  "blocks_without_winners" => blocks_without_winners,
                  "initial_mining_per_block_reward" => per_block,
                  "initial_mining_bonus_block_window" => INITIAL_MINING_BONUS_WINDOW);

            assert_eq!(snapshot.accumulated_coinbase_ustx, 0,
                       "First block should not have receive additional coinbase before initial reward calculation");
            snapshot.accumulated_coinbase_ustx = per_block;

            Some(InitialMiningBonus {
                total_reward,
                per_block,
            })
        } else {
            None
        };

        // store the snapshot
        let index_root = self.append_chain_tip_snapshot(
            parent_snapshot,
            &snapshot,
            &state_transition.accepted_ops,
            missed_commits,
            next_pox_info,
            reward_info,
            initialize_bonus,
        )?;

        snapshot.index_root = index_root;

        debug!("OPS-HASH({}): {}", this_block_height, &snapshot.ops_hash);
        debug!(
            "INDEX-ROOT({}): {}",
            this_block_height, &snapshot.index_root
        );
        debug!(
            "SORTITION-HASH({}): {}",
            this_block_height, &snapshot.sortition_hash
        );
        info!(
            "CONSENSUS({}): {}",
            this_block_height, &snapshot.consensus_hash
        );
        Ok((snapshot, state_transition))
    }

    /// Check and then commit all blockstack operations to our chainstate.
    /// * pull out all the transactions that are blockstack ops
    /// * select the ones that are _valid_
    /// * do a cryptographic sortition to select the next Stacks block
    /// * commit all valid transactions
    /// * commit the results of the sortition
    /// Returns the BlockSnapshot created from this block.
    pub fn process_block_ops(
        &mut self,
        mainnet: bool,
        burnchain: &Burnchain,
        parent_snapshot: &BlockSnapshot,
        block_header: &BurnchainBlockHeader,
        mut blockstack_txs: Vec<BlockstackOperationType>,
        next_pox_info: Option<RewardCycleInfo>,
        parent_pox: PoxId,
        reward_set_info: Option<&RewardSetInfo>,
        initial_mining_bonus_ustx: u128,
    ) -> Result<(BlockSnapshot, BurnchainStateTransition), BurnchainError> {
        debug!(
            "BEGIN({}) block ({},{}) with sortition_id: {}",
            block_header.block_height,
            block_header.block_hash,
            block_header.parent_block_hash,
            &self.context.chain_tip
        );
        debug!(
            "Append {} operation(s) from block {} {}",
            blockstack_txs.len(),
            block_header.block_height,
            &block_header.block_hash
        );

        blockstack_txs.sort_by(|ref a, ref b| a.vtxindex().partial_cmp(&b.vtxindex()).unwrap());

        // check each transaction, and filter out only the ones that are valid
        debug!(
            "Check Blockstack transactions from sortition_id: {}",
            &self.context.chain_tip
        );

        let mut missed_block_commits = vec![];

        // classify and check each transaction
        blockstack_txs.retain_mut(|blockstack_op| {
            match self.check_transaction(burnchain, blockstack_op, reward_set_info) {
                Ok(_) => true,
                Err(BurnchainError::OpError(OpError::MissedBlockCommit(missed_op))) => {
                    missed_block_commits.push(missed_op);
                    false
                }
                Err(_) => false,
            }
        });

        // block-wide check: no duplicate keys registered
        let block_ops = Burnchain::filter_block_VRF_dups(blockstack_txs);
        assert!(Burnchain::ops_are_sorted(&block_ops));

        // process them
        let res = self
            .process_checked_block_ops(
                mainnet,
                burnchain,
                parent_snapshot,
                block_header,
                &block_ops,
                &missed_block_commits,
                next_pox_info,
                parent_pox,
                reward_set_info,
                initial_mining_bonus_ustx,
            )
            .map_err(|e| {
                error!(
                    "TRANSACTION ABORTED when snapshotting block {} ({}): {:?}",
                    block_header.block_height, &block_header.block_hash, e
                );
                e
            })?;

        Ok(res)
    }

    /// Given the extracted txs, and a block header, go process them into the next
    /// snapshot.  Unlike process_block_ops, this method applies safety checks against the given
    /// list of blockstack transactions.
    pub fn process_block_txs(
        &mut self,
        mainnet: bool,
        parent_snapshot: &BlockSnapshot,
        this_block_header: &BurnchainBlockHeader,
        burnchain: &Burnchain,
        blockstack_txs: Vec<BlockstackOperationType>,
        next_pox_info: Option<RewardCycleInfo>,
        parent_pox: PoxId,
        reward_set_info: Option<&RewardSetInfo>,
        initial_mining_bonus_ustx: u128,
    ) -> Result<(BlockSnapshot, BurnchainStateTransition), BurnchainError> {
        assert_eq!(
            parent_snapshot.block_height + 1,
            this_block_header.block_height
        );
        assert_eq!(
            parent_snapshot.burn_header_hash,
            this_block_header.parent_block_hash
        );

        let new_snapshot = self.process_block_ops(
            mainnet,
            burnchain,
            &parent_snapshot,
            &this_block_header,
            blockstack_txs,
            next_pox_info,
            parent_pox,
            reward_set_info,
            initial_mining_bonus_ustx,
        )?;
        Ok(new_snapshot)
    }
}

#[cfg(test)]
mod tests {
    use stacks_common::types::chainstate::{BlockHeaderHash, StacksAddress, VRFSeed};
    use stacks_common::util::hash::hex_bytes;
    use stacks_common::util::vrf::VRFPublicKey;

    use super::*;
    use crate::burnchains::bitcoin::address::BitcoinAddress;
    use crate::burnchains::bitcoin::BitcoinNetworkType;
    use crate::burnchains::*;
    use crate::chainstate::burn::db::sortdb::tests::test_append_snapshot;
    use crate::chainstate::burn::db::sortdb::SortitionDB;
    use crate::chainstate::burn::operations::leader_block_commit::BURN_BLOCK_MINED_AT_MODULUS;
    use crate::chainstate::burn::operations::{LeaderBlockCommitOp, LeaderKeyRegisterOp};
    use crate::chainstate::burn::*;
    use crate::chainstate::stacks::address::StacksAddressExtensions;
    use crate::chainstate::stacks::StacksPublicKey;
    use crate::core::MICROSTACKS_PER_STACKS;

    #[test]
    fn test_initial_block_reward() {
        let first_burn_hash = BurnchainHeaderHash([0; 32]);

        let leader_key = LeaderKeyRegisterOp {
            consensus_hash: ConsensusHash([0x22; 20]),
            public_key: VRFPublicKey::from_hex(
                "a366b51292bef4edd64063d9145c617fec373bceb0758e98cd72becd84d54c7a",
            )
            .unwrap(),
            memo: vec![1, 2, 3, 4, 5],

            txid: Txid::from_bytes_be(
                &hex_bytes("1bfa831b5fc56c858198acb8e77e5863c1e9d8ac26d49ddb914e24d8d4083562")
                    .unwrap(),
            )
            .unwrap(),
            vtxindex: 400,
            block_height: 101,
            burn_header_hash: BurnchainHeaderHash([0x01; 32]),
        };

        let block_commit = LeaderBlockCommitOp {
            sunset_burn: 0,
            block_header_hash: BlockHeaderHash([0x22; 32]),
            new_seed: VRFSeed::from_hex(
                "3333333333333333333333333333333333333333333333333333333333333333",
            )
            .unwrap(),
            parent_block_ptr: 0,
            parent_vtxindex: 0,
            key_block_ptr: 101,
            key_vtxindex: 400,
            memo: vec![0x80],
            apparent_sender: BurnchainSigner("hello-world".to_string()),

            commit_outs: vec![],
            burn_fee: 12345,
            input: (Txid([0; 32]), 0),

            txid: Txid::from_bytes_be(
                &hex_bytes("3c07a0a93360bc85047bbaadd49e30c8af770f73a37e10fec400174d2e5f27cf")
                    .unwrap(),
            )
            .unwrap(),
            vtxindex: 400,
            block_height: 102,
            burn_parent_modulus: (101 % BURN_BLOCK_MINED_AT_MODULUS) as u8,
            burn_header_hash: BurnchainHeaderHash([0x03; 32]),
            treatment: vec![],
        };

        let mut burnchain = Burnchain::default_unittest(100, &first_burn_hash);
        burnchain.initial_reward_start_block = 90;
        let mut db = SortitionDB::connect_test(100, &first_burn_hash).unwrap();

        let snapshot = test_append_snapshot(
            &mut db,
            BurnchainHeaderHash([0x01; 32]),
            &[BlockstackOperationType::LeaderKeyRegister(leader_key)],
        );

        let next_block_header = BurnchainBlockHeader {
            block_height: 102,
            block_hash: BurnchainHeaderHash([0x03; 32]),
            parent_block_hash: BurnchainHeaderHash([0x01; 32]),
            num_txs: 1,
            timestamp: 10,
        };

        {
            let mut ic = SortitionHandleTx::begin(&mut db, &snapshot.sortition_id).unwrap();

            let processed = ic
                .process_block_ops(
                    false,
                    &burnchain,
                    &snapshot,
                    &next_block_header,
                    vec![BlockstackOperationType::LeaderBlockCommit(block_commit)],
                    None,
                    PoxId::initial(),
                    None,
                    0,
                )
                .unwrap();

            let reward_per_block = ic
                .get_initial_mining_bonus_per_block(&processed.0.sortition_id)
                .unwrap()
                .unwrap();
            let remaining = ic
                .get_initial_mining_bonus_remaining(&processed.0.sortition_id)
                .unwrap();
            assert_eq!(
                reward_per_block,
                1000 * (MICROSTACKS_PER_STACKS as u128) * (102 - 90)
                    / (INITIAL_MINING_BONUS_WINDOW as u128)
            );
            assert_eq!(
                remaining,
                reward_per_block * (INITIAL_MINING_BONUS_WINDOW as u128 - 1)
            );
        }
    }
}
