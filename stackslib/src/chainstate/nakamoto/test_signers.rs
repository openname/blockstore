// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
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

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::{fs, io};

use clarity::util::hash::MerkleHashFunc;
use clarity::util::secp256k1::{MessageSignature, Secp256k1PrivateKey, Secp256k1PublicKey};
use clarity::vm::clarity::ClarityConnection;
use clarity::vm::costs::{ExecutionCost, LimitedCostTracker};
use clarity::vm::types::*;
use hashbrown::HashMap;
use rand::distributions::Standard;
use rand::seq::SliceRandom;
use rand::{CryptoRng, Rng, RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use stacks_common::address::*;
use stacks_common::consts::{FIRST_BURNCHAIN_CONSENSUS_HASH, FIRST_STACKS_BLOCK_HASH};
use stacks_common::types::chainstate::{
    BlockHeaderHash, SortitionId, StacksAddress, StacksBlockId, VRFSeed,
};
use stacks_common::util::hash::Hash160;
use stacks_common::util::sleep_ms;
use stacks_common::util::vrf::{VRFProof, VRFPublicKey};

use self::boot::RewardSet;
use crate::burnchains::bitcoin::indexer::BitcoinIndexer;
use crate::burnchains::*;
use crate::chainstate::burn::db::sortdb::*;
use crate::chainstate::burn::operations::{
    BlockstackOperationType, LeaderBlockCommitOp, LeaderKeyRegisterOp,
};
use crate::chainstate::burn::*;
use crate::chainstate::coordinator::{
    ChainsCoordinator, Error as CoordinatorError, OnChainRewardSetProvider,
};
use crate::chainstate::nakamoto::miner::NakamotoBlockBuilder;
use crate::chainstate::nakamoto::{NakamotoBlock, NakamotoBlockHeader, NakamotoChainState};
use crate::chainstate::stacks::address::PoxAddress;
use crate::chainstate::stacks::boot::{NakamotoSignerEntry, PoxStartCycleInfo};
use crate::chainstate::stacks::db::*;
use crate::chainstate::stacks::miner::*;
use crate::chainstate::stacks::{
    Error as ChainstateError, StacksBlock, C32_ADDRESS_VERSION_TESTNET_SINGLESIG, *,
};
use crate::core::{BOOT_BLOCK_HASH, STACKS_EPOCH_3_0_MARKER};
use crate::cost_estimates::metrics::UnitMetric;
use crate::cost_estimates::UnitEstimator;
use crate::net::relay::Relayer;
use crate::util_lib::boot::boot_code_addr;
use crate::util_lib::db::Error as db_error;

#[derive(Debug, Clone, PartialEq)]
pub struct TestSigners {
    /// The number of signatures required to validate a block
    pub threshold: u32,
    /// The signer's private keys
    pub signer_keys: Vec<Secp256k1PrivateKey>,
    /// The aggregate public key
    pub aggregate_public_key: Vec<u8>,
    /// The cycle for which the aggregate public key was generated
    pub cycle: u64,
}

impl Default for TestSigners {
    fn default() -> Self {
        let aggregate_public_key: Vec<u8> =
            rand::thread_rng().sample_iter(Standard).take(33).collect();
        let num_signers = 5;
        let threshold = 5 * 7 / 10;

        let mut signer_keys = Vec::<Secp256k1PrivateKey>::new();
        for _ in 0..num_signers {
            signer_keys.push(Secp256k1PrivateKey::default());
        }
        Self {
            threshold,
            signer_keys,
            aggregate_public_key,
            cycle: 0,
        }
    }
}

impl TestSigners {
    /// Generate TestSigners using a list of signer keys
    pub fn new(signer_keys: Vec<Secp256k1PrivateKey>) -> Self {
        TestSigners::default_with_signers(signer_keys)
    }

    /// Internal function to generate aggregate key information
    fn default_with_signers(signer_keys: Vec<Secp256k1PrivateKey>) -> Self {
        let aggregate_public_key: Vec<u8> =
            rand::thread_rng().sample_iter(Standard).take(33).collect();
        let num_signers = signer_keys.len();
        let threshold = u32::try_from(num_signers * 7 / 10).unwrap();
        Self {
            threshold,
            signer_keys,
            aggregate_public_key,
            cycle: 0,
        }
    }

    /// Sign a Nakamoto block using [`Self::signer_keys`].
    ///
    /// N.B. If any of [`Self::signer_keys`] are not in the reward set, the resulting
    /// signatures will be invalid. Use [`Self::sign_block_with_reward_set()`] to ensure
    /// that any signer keys not in the reward set are not included.
    pub fn sign_nakamoto_block(&mut self, block: &mut NakamotoBlock, cycle: u64) {
        // Update the aggregate public key if the cycle has changed
        if self.cycle != cycle {
            self.generate_aggregate_key(cycle);
        }

        let signer_signature = self.generate_block_signatures(block);

        test_debug!(
            "Signed Nakamoto block {} with {} signatures (rc {})",
            block.block_id(),
            signer_signature.len(),
            cycle
        );
        block.header.signer_signature = signer_signature;
    }

    /// Sign a NakamotoBlock and maintain the order and membership
    /// of the reward set signers in the resulting signatures.
    ///
    /// If any of [`Self::signer_keys`] are not in the reward set, their signatures
    /// will not be included.
    pub fn sign_block_with_reward_set(&self, block: &mut NakamotoBlock, reward_set: &RewardSet) {
        let signatures = self.generate_ordered_signatures(block, reward_set);
        block.header.signer_signature = signatures;
    }

    /// Synthesize a reward set from the signer for the purposes of signing and verifying blocks
    /// later on
    pub fn synthesize_reward_set(&self) -> RewardSet {
        let mut signer_entries = vec![];
        let mut pox_addrs = vec![];
        for key in self.signer_keys.iter() {
            let signing_key_vec = Secp256k1PublicKey::from_private(key).to_bytes_compressed();
            let mut signing_key = [0u8; 33];
            signing_key[0..33].copy_from_slice(&signing_key_vec[0..33]);

            let nakamoto_signer_entry = NakamotoSignerEntry {
                signing_key,
                stacked_amt: 100_000_000_000,
                weight: 1,
            };
            let pox_addr = PoxAddress::Standard(
                StacksAddress {
                    version: AddressHashMode::SerializeP2PKH.to_version_testnet(),
                    bytes: Hash160::from_data(&nakamoto_signer_entry.signing_key),
                },
                Some(AddressHashMode::SerializeP2PKH),
            );
            signer_entries.push(nakamoto_signer_entry);
            pox_addrs.push(pox_addr);
        }

        RewardSet {
            rewarded_addresses: pox_addrs,
            start_cycle_state: PoxStartCycleInfo {
                missed_reward_slots: vec![],
            },
            signers: Some(signer_entries),
            pox_ustx_threshold: Some(100_000_000_000),
        }
    }

    /// Sign a Nakamoto block and generate a vec of signatures. The signatures will
    /// be ordered by the signer's public keys, but will not be checked against the
    /// reward set.
    fn generate_block_signatures(&self, block: &NakamotoBlock) -> Vec<MessageSignature> {
        let msg = block.header.signer_signature_hash().0;
        let mut keys = self.signer_keys.clone();
        keys.sort_by(|a, b| {
            let a = Secp256k1PublicKey::from_private(a).to_bytes_compressed();
            let b = Secp256k1PublicKey::from_private(b).to_bytes_compressed();
            a.cmp(&b)
        });
        keys.iter().map(|key| key.sign(&msg).unwrap()).collect()
    }

    /// Generate an list of signatures for a block. Only
    /// signers in the reward set will be included.
    pub fn generate_ordered_signatures(
        &self,
        block: &NakamotoBlock,
        reward_set: &RewardSet,
    ) -> Vec<MessageSignature> {
        let msg = block.header.signer_signature_hash().0;

        let test_signers_by_pk = self
            .signer_keys
            .iter()
            .cloned()
            .map(|s| {
                let pk = Secp256k1PublicKey::from_private(&s);
                (pk.to_bytes_compressed(), s)
            })
            .collect::<HashMap<_, _>>();

        let reward_set_keys = &reward_set
            .clone()
            .signers
            .unwrap()
            .iter()
            .map(|s| s.signing_key.to_vec())
            .collect::<Vec<_>>();

        info!(
            "TestSigners: Signing Nakamoto block. TestSigners has {} signers. Reward set has {} signers.", 
            test_signers_by_pk.len(),
            reward_set_keys.len(),
        );

        let mut signatures = Vec::with_capacity(reward_set_keys.len());

        let mut missing_keys = 0;

        for key in reward_set_keys {
            if let Some(signer_key) = test_signers_by_pk.get(key) {
                let signature = signer_key.sign(&msg).unwrap();
                signatures.push(signature);
            } else {
                missing_keys += 1;
            }
        }
        if missing_keys > 0 {
            warn!(
                "TestSigners: {} keys are in the reward set but not in signer_keys",
                missing_keys
            );
        }

        signatures
    }

    // Generate and assign a new aggregate public key
    pub fn generate_aggregate_key(&mut self, cycle: u64) -> Vec<u8> {
        // If the key is already generated for this cycle, return it
        if cycle == self.cycle {
            debug!("Returning cached aggregate key for cycle {}", cycle);
            return self.aggregate_public_key.clone();
        }

        let aggregate_public_key: Vec<u8> =
            rand::thread_rng().sample_iter(Standard).take(33).collect();
        self.aggregate_public_key.clone_from(&aggregate_public_key);
        aggregate_public_key
    }
}
