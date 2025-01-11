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

use std::collections::HashMap;
use std::process::{Command, Stdio};

use stacks_common::types::chainstate::{BlockHeaderHash, BurnchainHeaderHash, VRFSeed};
use stacks_common::util::hash::hex_bytes;

use crate::burnchains::bitcoin::address::BitcoinAddress;
use crate::burnchains::bitcoin::{BitcoinNetworkType, BitcoinTxOutput};
use crate::burnchains::{Burnchain, BurnchainSigner, Error as BurnchainError, Txid};
use crate::chainstate::burn::db::sortdb::{SortitionDB, SortitionHandle};
use crate::chainstate::burn::distribution::BurnSamplePoint;
use crate::chainstate::burn::operations::leader_block_commit::{
    MissedBlockCommit, BURN_BLOCK_MINED_AT_MODULUS,
};
use crate::chainstate::burn::operations::LeaderBlockCommitOp;
use crate::chainstate::stacks::address::PoxAddress;
use crate::core::MINING_COMMITMENT_WINDOW;
use crate::util_lib::db::Error as DBError;

pub struct MinerStats {
    pub unconfirmed_commits_helper: String,
}

/// Unconfirmed block-commit transaction as emitted by our helper
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct UnconfirmedBlockCommit {
    /// burnchain signer
    address: String,
    /// PoX payouts
    pox_addrs: Vec<String>,
    /// UTXO spent to create this block-commit
    input_index: u32,
    input_txid: String,
    /// transaction ID
    txid: String,
    /// amount spent
    burn: u64,
}

const DEADBEEF: [u8; 32] = [
    0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef,
    0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef,
];

impl MinerStats {
    /// Find the burn distribution for a single sortition's block-commits and missed-commits
    fn get_burn_distribution<SH: SortitionHandle>(
        sort_handle: &mut SH,
        burnchain: &Burnchain,
        burn_block_height: u64,
        block_commits: Vec<LeaderBlockCommitOp>,
        missed_commits: Vec<MissedBlockCommit>,
    ) -> Result<Vec<BurnSamplePoint>, BurnchainError> {
        // assemble the commit windows
        let mut windowed_block_commits = vec![block_commits];
        let mut windowed_missed_commits = vec![];

        if !burnchain.is_in_prepare_phase(burn_block_height) {
            // PoX reward-phase is active!
            // build a map of intended sortition -> missed commit for the missed commits
            //   discovered in this block.
            let mut missed_commits_map: HashMap<_, Vec<_>> = HashMap::new();
            for missed in missed_commits.iter() {
                if let Some(commits_at_sortition) =
                    missed_commits_map.get_mut(&missed.intended_sortition)
                {
                    commits_at_sortition.push(missed);
                } else {
                    missed_commits_map.insert(missed.intended_sortition, vec![missed]);
                }
            }

            for blocks_back in 0..(MINING_COMMITMENT_WINDOW - 1) {
                if burn_block_height.saturating_sub(1) < (blocks_back as u64) {
                    debug!("Mining commitment window shortened because block height is less than window size";
                           "block_height" => %burn_block_height.saturating_sub(1),
                           "window_size" => %MINING_COMMITMENT_WINDOW);
                    break;
                }
                let block_height = (burn_block_height.saturating_sub(1)) - (blocks_back as u64);
                let sortition_id = match sort_handle.get_block_snapshot_by_height(block_height)? {
                    Some(sn) => sn.sortition_id,
                    None => break,
                };
                windowed_block_commits.push(SortitionDB::get_block_commits_by_block(
                    sort_handle.sqlite(),
                    &sortition_id,
                )?);
                let mut missed_commits_at_height = SortitionDB::get_missed_commits_by_intended(
                    sort_handle.sqlite(),
                    &sortition_id,
                )?;
                if let Some(missed_commit_in_block) = missed_commits_map.remove(&sortition_id) {
                    missed_commits_at_height.extend(missed_commit_in_block.into_iter().cloned());
                }

                windowed_missed_commits.push(missed_commits_at_height);
            }
        } else {
            // PoX reward-phase is not active
            debug!(
                "Block {burn_block_height} is in a prepare phase or post-PoX sunset, so no windowing will take place"
            );

            assert_eq!(windowed_block_commits.len(), 1);
            assert_eq!(windowed_missed_commits.len(), 0);
        }

        // reverse vecs so that windows are in ascending block height order
        windowed_block_commits.reverse();
        windowed_missed_commits.reverse();

        // figure out if the PoX sunset finished during the window,
        // and/or which sortitions must be PoB due to them falling in a prepare phase.
        let window_end_height = burn_block_height;
        let window_start_height = window_end_height + 1 - (windowed_block_commits.len() as u64);
        let mut burn_blocks = vec![false; windowed_block_commits.len()];

        // set burn_blocks flags to accomodate prepare phases and PoX sunset
        for (i, b) in burn_blocks.iter_mut().enumerate() {
            if burnchain.is_in_prepare_phase(window_start_height + (i as u64)) {
                // must burn
                *b = true;
            } else {
                // must not burn
                *b = false;
            }
        }

        // not all commits in windowed_block_commits have been confirmed, so make sure that they
        // are in the right order
        let mut block_height_at_index = None;
        for (index, commits) in windowed_block_commits.iter_mut().enumerate() {
            let index = index as u64;
            for commit in commits.iter_mut() {
                if let Some((first_block_height, first_index)) = block_height_at_index {
                    if commit.block_height != first_block_height + (index - first_index) {
                        commit.block_height = first_block_height + (index - first_index);
                    }
                } else {
                    block_height_at_index = Some((commit.block_height, index));
                }
            }
        }

        // calculate the burn distribution from these operations.
        // The resulting distribution will contain the user burns that match block commits
        let burn_dist = BurnSamplePoint::make_min_median_distribution(
            if burnchain.is_in_prepare_phase(burn_block_height) {
                1
            } else {
                MINING_COMMITMENT_WINDOW
            },
            windowed_block_commits,
            windowed_missed_commits,
            burn_blocks,
        );

        Ok(burn_dist)
    }

    fn fmt_bin_args(bin: &str, args: &[&str]) -> String {
        let mut all = Vec::with_capacity(1 + args.len());
        all.push(bin);
        for arg in args {
            all.push(arg);
        }
        all.join(" ")
    }

    /// Returns (exit code, stdout, stderr)
    fn run_subprocess(
        bin_fullpath: &str,
        args: &[&str],
    ) -> Result<(i32, Vec<u8>, Vec<u8>), String> {
        let full_args = Self::fmt_bin_args(bin_fullpath, args);
        let mut cmd = Command::new(bin_fullpath);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .args(args);

        debug!("Run: `{cmd:?}`");

        let output = cmd
            .spawn()
            .map_err(|e| format!("Failed to run `{full_args}`: {e:?}"))?
            .wait_with_output()
            .map_err(|ioe| format!("Failed to run `{full_args}`: {ioe:?}"))?;

        let exit_code = match output.status.code() {
            Some(code) => code,
            None => {
                // failed due to signal
                return Err(format!("Failed to run `{full_args}`: killed by signal"));
            }
        };

        Ok((exit_code, output.stdout, output.stderr))
    }

    /// Get the list of all unconfirmed block-commits.
    pub fn get_unconfirmed_commits(
        &self,
        next_block_height: u64,
        all_miners: &[&str],
    ) -> Result<Vec<LeaderBlockCommitOp>, String> {
        let (exit_code, stdout, _stderr) =
            Self::run_subprocess(&self.unconfirmed_commits_helper, all_miners)?;
        if exit_code != 0 {
            return Err(format!(
                "Failed to run `{}`: exit code {exit_code}",
                &self.unconfirmed_commits_helper
            ));
        }

        // decode stdout to JSON
        let unconfirmed_commits: Vec<UnconfirmedBlockCommit> = serde_json::from_slice(&stdout)
            .map_err(|e| {
                format!(
                    "Failed to decode output from `{}`: {e:?}. Output was `{}`",
                    &self.unconfirmed_commits_helper,
                    String::from_utf8_lossy(&stdout)
                )
            })?;

        let mut unconfirmed_spends = vec![];
        for unconfirmed_commit in unconfirmed_commits.into_iter() {
            let Ok(txid) = Txid::from_hex(&unconfirmed_commit.txid) else {
                return Err(format!("Not a valid txid: `{}`", &unconfirmed_commit.txid));
            };
            let Ok(input_txid) = Txid::from_hex(&unconfirmed_commit.input_txid) else {
                return Err(format!(
                    "Not a valid txid: `{}`",
                    &unconfirmed_commit.input_txid
                ));
            };
            let mut decoded_pox_addrs = vec![];
            for pox_addr_hex in unconfirmed_commit.pox_addrs.iter() {
                let Ok(pox_addr_bytes) = hex_bytes(pox_addr_hex) else {
                    return Err(format!("Not a hex string: `{pox_addr_hex}`"));
                };
                let Some(bitcoin_addr) =
                    BitcoinAddress::from_scriptpubkey(BitcoinNetworkType::Mainnet, &pox_addr_bytes)
                else {
                    return Err(format!(
                        "Not a recognized Bitcoin scriptpubkey: {pox_addr_hex}"
                    ));
                };
                let Some(pox_addr) = PoxAddress::try_from_bitcoin_output(&BitcoinTxOutput {
                    address: bitcoin_addr.clone(),
                    units: 1,
                }) else {
                    return Err(format!("Not a recognized PoX address: {bitcoin_addr}"));
                };
                decoded_pox_addrs.push(pox_addr);
            }

            // mocked commit
            let mocked_commit = LeaderBlockCommitOp {
                treatment: vec![],
                sunset_burn: 0,
                block_header_hash: BlockHeaderHash(DEADBEEF),
                new_seed: VRFSeed(DEADBEEF),
                parent_block_ptr: 1,
                parent_vtxindex: 1,
                key_block_ptr: 1,
                key_vtxindex: 1,
                memo: vec![],
                commit_outs: decoded_pox_addrs,
                burn_fee: unconfirmed_commit.burn,
                input: (input_txid, unconfirmed_commit.input_index),
                apparent_sender: BurnchainSigner(unconfirmed_commit.address),
                txid,
                vtxindex: 1,
                block_height: next_block_height,
                burn_parent_modulus: ((next_block_height.saturating_sub(1))
                    % BURN_BLOCK_MINED_AT_MODULUS) as u8,
                burn_header_hash: BurnchainHeaderHash(DEADBEEF),
            };

            unconfirmed_spends.push(mocked_commit);
        }
        Ok(unconfirmed_spends)
    }

    /// Convert a list of burn sample points into a probability distribution by candidate's
    /// apparent sender (e.g. miner address).
    pub fn burn_dist_to_prob_dist(burn_dist: &[BurnSamplePoint]) -> HashMap<String, f64> {
        if burn_dist.is_empty() {
            return HashMap::new();
        }
        if burn_dist.len() == 1 {
            let mut ret = HashMap::new();
            ret.insert(burn_dist[0].candidate.apparent_sender.to_string(), 1.0);
            return ret;
        }

        let mut ret = HashMap::new();
        for pt in burn_dist.iter() {
            // take the upper 32 bits
            let range_lower_64 = (pt.range_end - pt.range_start) >> 192;
            let int_prob = (range_lower_64.low_u64() >> 32) as u32;

            ret.insert(
                pt.candidate.apparent_sender.to_string(),
                (int_prob as f64) / (u32::MAX as f64),
            );
        }

        ret
    }

    /// Get the spend distribution and total spend.
    /// If the miner has both a confirmed and unconfirmed spend, then take the latter.
    pub fn get_spend_distribution(
        active_miners_and_commits: &[(String, LeaderBlockCommitOp)],
        unconfirmed_block_commits: &[LeaderBlockCommitOp],
        expected_pox_addrs: &[PoxAddress],
    ) -> (HashMap<String, u64>, u64) {
        let unconfirmed_block_commits: Vec<_> = unconfirmed_block_commits
            .iter()
            .filter(|commit| {
                if commit.commit_outs.len() != expected_pox_addrs.len() {
                    return false;
                }
                for (i, commit_out) in commit.commit_outs.iter().enumerate() {
                    if commit_out.to_burnchain_repr() != expected_pox_addrs[i].to_burnchain_repr() {
                        info!(
                            "Skipping invalid unconfirmed block-commit: {:?} != {:?}",
                            &commit_out.to_burnchain_repr(),
                            expected_pox_addrs[i].to_burnchain_repr()
                        );
                        return false;
                    }
                }
                true
            })
            .collect();

        let mut total_spend = 0;
        let mut dist = HashMap::new();
        for commit in unconfirmed_block_commits {
            let addr = commit.apparent_sender.to_string();
            dist.insert(addr, commit.burn_fee);
        }

        for (_, commit) in active_miners_and_commits.iter() {
            let addr = commit.apparent_sender.to_string();
            if dist.contains_key(&addr) {
                continue;
            }
            dist.insert(addr, commit.burn_fee);
        }

        for (_, spend) in dist.iter() {
            total_spend += *spend;
        }

        (dist, total_spend)
    }

    /// Get the probability distribution for the Bitcoin block 6+ blocks in the future, assuming
    /// all block-commit spends remain the same.
    pub fn get_future_win_distribution(
        active_miners_and_commits: &[(String, LeaderBlockCommitOp)],
        unconfirmed_block_commits: &[LeaderBlockCommitOp],
        expected_pox_addrs: &[PoxAddress],
    ) -> HashMap<String, f64> {
        let (dist, total_spend) = Self::get_spend_distribution(
            active_miners_and_commits,
            unconfirmed_block_commits,
            expected_pox_addrs,
        );

        let mut probs = HashMap::new();
        for (addr, spend) in dist.into_iter() {
            if total_spend == 0 {
                probs.insert(addr, 0.0);
            } else {
                probs.insert(addr, (spend as f64) / (total_spend as f64));
            }
        }
        probs
    }

    /// Get the burn distribution for the _next_ Bitcoin block, assuming that the given list of
    /// block-commit data will get mined.  For miners that are known to the system but who do not
    /// have unconfirmed block-commits, infer that they'll just mine the same block-commit value
    /// again.
    pub fn get_unconfirmed_burn_distribution(
        &self,
        burnchain: &Burnchain,
        sortdb: &SortitionDB,
        active_miners_and_commits: &[(String, LeaderBlockCommitOp)],
        unconfirmed_block_commits: Vec<LeaderBlockCommitOp>,
        expected_pox_addrs: &[PoxAddress],
        at_block: Option<u64>,
    ) -> Result<Vec<BurnSamplePoint>, BurnchainError> {
        let mut commit_table = HashMap::new();
        for commit in unconfirmed_block_commits.iter() {
            commit_table.insert(commit.apparent_sender.to_string(), commit.clone());
        }

        let tip = if let Some(at_block) = at_block {
            let tip = SortitionDB::get_canonical_burn_chain_tip(sortdb.conn())?;
            let ih = sortdb.index_handle(&tip.sortition_id);
            ih.get_block_snapshot_by_height(at_block)?
                .ok_or(BurnchainError::MissingParentBlock)?
        } else {
            SortitionDB::get_canonical_burn_chain_tip(sortdb.conn())?
        };

        let next_block_height = tip.block_height + 1;
        let expected_input_index = if burnchain.is_in_prepare_phase(tip.block_height) {
            LeaderBlockCommitOp::expected_chained_utxo(true)
        } else {
            LeaderBlockCommitOp::expected_chained_utxo(false)
        };

        for (miner, last_commit) in active_miners_and_commits.iter() {
            if !commit_table.contains_key(miner) {
                let mocked_commit = LeaderBlockCommitOp {
                    treatment: vec![],
                    sunset_burn: 0,
                    block_header_hash: BlockHeaderHash(DEADBEEF),
                    new_seed: VRFSeed(DEADBEEF),
                    parent_block_ptr: 2,
                    parent_vtxindex: 2,
                    key_block_ptr: 2,
                    key_vtxindex: 2,
                    memo: vec![],
                    commit_outs: expected_pox_addrs.to_vec(),
                    burn_fee: last_commit.burn_fee,
                    input: (last_commit.txid, expected_input_index),
                    apparent_sender: last_commit.apparent_sender.clone(),
                    txid: Txid(DEADBEEF),
                    vtxindex: 1,
                    block_height: next_block_height,
                    burn_parent_modulus: ((next_block_height.saturating_sub(1))
                        % BURN_BLOCK_MINED_AT_MODULUS)
                        as u8,
                    burn_header_hash: BurnchainHeaderHash(DEADBEEF),
                };
                commit_table.insert(miner.to_string(), mocked_commit);
            }
        }

        let unconfirmed_block_commits: Vec<_> = commit_table
            .into_values()
            .filter(|commit| {
                if commit.commit_outs.len() != expected_pox_addrs.len() {
                    return false;
                }
                for (i, commit_out) in commit.commit_outs.iter().enumerate() {
                    if commit_out.to_burnchain_repr() != expected_pox_addrs[i].to_burnchain_repr() {
                        info!(
                            "Skipping invalid unconfirmed block-commit: {:?} != {:?}",
                            &commit_out.to_burnchain_repr(),
                            expected_pox_addrs[i].to_burnchain_repr()
                        );
                        return false;
                    }
                }
                true
            })
            .collect();

        let mut handle = sortdb.index_handle(&tip.sortition_id);
        Self::get_burn_distribution(
            &mut handle,
            burnchain,
            tip.block_height + 1,
            unconfirmed_block_commits,
            vec![],
        )
    }

    /// Given the sortition DB, get the list of all miners in the past MINING_COMMITMENT_WINDOW
    /// blocks, as well as their last block-commits
    pub fn get_active_miners(
        sortdb: &SortitionDB,
        at_burn_block: Option<u64>,
    ) -> Result<Vec<(String, LeaderBlockCommitOp)>, DBError> {
        let mut tip = if let Some(at_burn_block) = at_burn_block {
            let tip = SortitionDB::get_canonical_burn_chain_tip(sortdb.conn())?;
            let ih = sortdb.index_handle(&tip.sortition_id);
            ih.get_block_snapshot_by_height(at_burn_block)?
                .ok_or(DBError::NotFoundError)?
        } else {
            SortitionDB::get_canonical_burn_chain_tip(sortdb.conn())?
        };

        let mut miners = HashMap::new();
        for _i in 0..MINING_COMMITMENT_WINDOW {
            let commits =
                SortitionDB::get_block_commits_by_block(sortdb.conn(), &tip.sortition_id)?;
            for commit in commits.into_iter() {
                let miner = commit.apparent_sender.to_string();
                miners.entry(miner).or_insert(commit);
            }
            tip = SortitionDB::get_block_snapshot(sortdb.conn(), &tip.parent_sortition_id)?
                .ok_or(DBError::NotFoundError)?;
        }
        Ok(miners.into_iter().collect())
    }
}

#[cfg(test)]
pub mod tests {
    use std::fs;
    use std::io::Write;

    use stacks_common::types::chainstate::{
        BlockHeaderHash, BurnchainHeaderHash, StacksAddress, StacksPublicKey, VRFSeed,
    };
    use stacks_common::util::hash::{hex_bytes, Hash160};
    use stacks_common::util::uint::{BitArray, Uint256};

    use super::MinerStats;
    use crate::burnchains::{BurnchainSigner, Txid};
    use crate::chainstate::burn::distribution::BurnSamplePoint;
    use crate::chainstate::burn::operations::leader_block_commit::BURN_BLOCK_MINED_AT_MODULUS;
    use crate::chainstate::burn::operations::LeaderBlockCommitOp;
    use crate::chainstate::stacks::address::{PoxAddress, PoxAddressType20};

    #[test]
    fn test_burn_dist_to_prob_dist() {
        let block_commit_1 = LeaderBlockCommitOp {
            treatment: vec![],
            sunset_burn: 0,
            block_header_hash: BlockHeaderHash([0x22; 32]),
            new_seed: VRFSeed([0x33; 32]),
            parent_block_ptr: 111,
            parent_vtxindex: 456,
            key_block_ptr: 123,
            key_vtxindex: 456,
            memo: vec![0x80],

            burn_fee: 12345,
            input: (Txid([0; 32]), 0),
            apparent_sender: BurnchainSigner::new_p2pkh(
                &StacksPublicKey::from_hex(
                    "02d8015134d9db8178ac93acbc43170a2f20febba5087a5b0437058765ad5133d0",
                )
                .unwrap(),
            ),

            commit_outs: vec![],

            txid: Txid::from_bytes_be(
                &hex_bytes("3c07a0a93360bc85047bbaadd49e30c8af770f73a37e10fec400174d2e5f27cf")
                    .unwrap(),
            )
            .unwrap(),
            vtxindex: 443,
            block_height: 124,
            burn_parent_modulus: (123 % BURN_BLOCK_MINED_AT_MODULUS) as u8,
            burn_header_hash: BurnchainHeaderHash([0x00; 32]),
        };

        let block_commit_2 = LeaderBlockCommitOp {
            treatment: vec![],
            sunset_burn: 0,
            block_header_hash: BlockHeaderHash([0x22; 32]),
            new_seed: VRFSeed([0x33; 32]),
            parent_block_ptr: 112,
            parent_vtxindex: 111,
            key_block_ptr: 122,
            key_vtxindex: 457,
            memo: vec![0x80],

            burn_fee: 12345,
            input: (Txid([0; 32]), 0),
            apparent_sender: BurnchainSigner::new_p2pkh(
                &StacksPublicKey::from_hex(
                    "023616a344700c9455bf0b55cc65e404c7b8f82e815da885398a44f6dc70e64045",
                )
                .unwrap(),
            ),

            commit_outs: vec![],

            txid: Txid::from_bytes_be(
                &hex_bytes("3c07a0a93360bc85047bbaadd49e30c8af770f73a37e10fec400174d2e5f27d0")
                    .unwrap(),
            )
            .unwrap(),
            vtxindex: 444,
            block_height: 124,
            burn_parent_modulus: (123 % BURN_BLOCK_MINED_AT_MODULUS) as u8,
            burn_header_hash: BurnchainHeaderHash::from_hex(
                "0000000000000000000000000000000000000000000000000000000000000004",
            )
            .unwrap(),
        };

        let block_commit_3 = LeaderBlockCommitOp {
            treatment: vec![],
            sunset_burn: 0,
            block_header_hash: BlockHeaderHash([0x22; 32]),
            new_seed: VRFSeed([0x33; 32]),
            parent_block_ptr: 113,
            parent_vtxindex: 111,
            key_block_ptr: 121,
            key_vtxindex: 10,
            memo: vec![0x80],

            burn_fee: 23456,
            input: (Txid([0; 32]), 0),
            apparent_sender: BurnchainSigner::new_p2pkh(
                &StacksPublicKey::from_hex(
                    "020a9b0a938a2226694fe4f867193cf0b78cd6264e4277fd686468a00a9afdc36d",
                )
                .unwrap(),
            ),

            commit_outs: vec![],

            txid: Txid::from_bytes_be(
                &hex_bytes("301dc687a9f06a1ae87a013f27133e9cec0843c2983567be73e185827c7c13de")
                    .unwrap(),
            )
            .unwrap(),
            vtxindex: 445,
            block_height: 124,
            burn_parent_modulus: (123 % BURN_BLOCK_MINED_AT_MODULUS) as u8,
            burn_header_hash: BurnchainHeaderHash::from_hex(
                "0000000000000000000000000000000000000000000000000000000000000004",
            )
            .unwrap(),
        };
        let burn_dist = vec![
            BurnSamplePoint {
                frequency: 10,
                burns: block_commit_1.burn_fee.into(),
                median_burn: block_commit_2.burn_fee.into(),
                range_start: Uint256::zero(),
                range_end: Uint256([
                    0x3ed94d3cb0a84709,
                    0x0963dded799a7c1a,
                    0x70989faf596c8b65,
                    0x41a3ed94d3cb0a84,
                ]),
                candidate: block_commit_1.clone(),
            },
            BurnSamplePoint {
                frequency: 10,
                burns: block_commit_2.burn_fee.into(),
                median_burn: block_commit_2.burn_fee.into(),
                range_start: Uint256([
                    0x3ed94d3cb0a84709,
                    0x0963dded799a7c1a,
                    0x70989faf596c8b65,
                    0x41a3ed94d3cb0a84,
                ]),
                range_end: Uint256([
                    0x7db29a7961508e12,
                    0x12c7bbdaf334f834,
                    0xe1313f5eb2d916ca,
                    0x8347db29a7961508,
                ]),
                candidate: block_commit_2.clone(),
            },
            BurnSamplePoint {
                frequency: 10,
                burns: (block_commit_3.burn_fee).into(),
                median_burn: block_commit_3.burn_fee.into(),
                range_start: Uint256([
                    0x7db29a7961508e12,
                    0x12c7bbdaf334f834,
                    0xe1313f5eb2d916ca,
                    0x8347db29a7961508,
                ]),
                range_end: Uint256::max(),
                candidate: block_commit_3.clone(),
            },
        ];

        let prob_dist = MinerStats::burn_dist_to_prob_dist(&burn_dist);
        assert_eq!(prob_dist.len(), 3);
        assert!(
            (prob_dist
                .get(&format!("{}", &block_commit_1.apparent_sender))
                .unwrap()
                - 0.25641)
                .abs()
                < 0.001
        );
        assert!(
            (prob_dist
                .get(&format!("{}", &block_commit_2.apparent_sender))
                .unwrap()
                - 0.25641)
                .abs()
                < 0.001
        );
        assert!(
            (prob_dist
                .get(&format!("{}", &block_commit_3.apparent_sender))
                .unwrap()
                - 0.48718)
                .abs()
                < 0.001
        );
    }

    #[test]
    fn test_get_unconfirmed_commits() {
        use std::os::unix::fs::PermissionsExt;
        let shell_code = r#"#!/bin/bash
echo <<EOF '[
 {
  "txid": "73c318be8cd272a73200b9630089d77a44342d84b2c0d81c937da714152cf402",
  "burn": 555000,
  "address": "1FCcoFSKWvNyhjazNvVdLLw8mGkGdcRMux",
  "input_txid": "ef0dbf0fc4755de5e94843a4da7c1d943571299afb15f32b76bac5d18d8668ce",
  "input_index": 3,
  "pox_addrs": [
      "0014db14133a9dbb1d0e16b60513453e48b6ff2847a9",
      "a91418c42080a1e87fd02dd3fca94c4513f9ecfe741487"
  ]
 }
]'
EOF
"#;
        let path = "/tmp/test-get-unconfirmed-commits.sh";
        if fs::metadata(path).is_ok() {
            fs::remove_file(path).unwrap();
        }
        {
            let mut f = fs::File::create(path).unwrap();
            f.write_all(shell_code.as_bytes()).unwrap();

            let md = f.metadata().unwrap();
            let mut permissions = md.permissions();
            permissions.set_mode(0o744);

            fs::set_permissions(path, permissions).unwrap();
            f.sync_all().unwrap();
        }

        let ms = MinerStats {
            unconfirmed_commits_helper: path.to_string(),
        };

        let mut commits = ms.get_unconfirmed_commits(123, &[]).unwrap();
        assert_eq!(commits.len(), 1);
        let commit = commits.pop().unwrap();

        assert_eq!(
            commit.txid,
            Txid::from_hex("73c318be8cd272a73200b9630089d77a44342d84b2c0d81c937da714152cf402")
                .unwrap()
        );
        assert_eq!(commit.burn_fee, 555000);
        assert_eq!(
            commit.apparent_sender.0,
            "1FCcoFSKWvNyhjazNvVdLLw8mGkGdcRMux".to_string()
        );
        assert_eq!(
            commit.input.0,
            Txid::from_hex("ef0dbf0fc4755de5e94843a4da7c1d943571299afb15f32b76bac5d18d8668ce")
                .unwrap()
        );
        assert_eq!(commit.input.1, 3);
        assert_eq!(commit.block_height, 123);

        assert_eq!(
            commit.commit_outs,
            vec![
                PoxAddress::Addr20(
                    true,
                    PoxAddressType20::P2WPKH,
                    [
                        219, 20, 19, 58, 157, 187, 29, 14, 22, 182, 5, 19, 69, 62, 72, 182, 255,
                        40, 71, 169
                    ]
                ),
                PoxAddress::Standard(
                    StacksAddress {
                        version: 20,
                        bytes: Hash160([
                            0x18, 0xc4, 0x20, 0x80, 0xa1, 0xe8, 0x7f, 0xd0, 0x2d, 0xd3, 0xfc, 0xa9,
                            0x4c, 0x45, 0x13, 0xf9, 0xec, 0xfe, 0x74, 0x14
                        ])
                    },
                    None
                )
            ]
        );
    }

    #[test]
    fn test_get_spend_and_win_distribution() {
        let active_miners_and_commits = vec![
            (
                "miner-1".to_string(),
                LeaderBlockCommitOp {
                    treatment: vec![],
                    sunset_burn: 0,
                    block_header_hash: BlockHeaderHash([0x22; 32]),
                    new_seed: VRFSeed([0x33; 32]),
                    parent_block_ptr: 111,
                    parent_vtxindex: 456,
                    key_block_ptr: 123,
                    key_vtxindex: 456,
                    memo: vec![0x80],

                    burn_fee: 2,
                    input: (Txid([0; 32]), 0),
                    apparent_sender: BurnchainSigner("miner-1".into()),

                    commit_outs: vec![],

                    txid: Txid::from_bytes_be(
                        &hex_bytes(
                            "3c07a0a93360bc85047bbaadd49e30c8af770f73a37e10fec400174d2e5f27cf",
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                    vtxindex: 443,
                    block_height: 124,
                    burn_parent_modulus: (123 % BURN_BLOCK_MINED_AT_MODULUS) as u8,
                    burn_header_hash: BurnchainHeaderHash([0x00; 32]),
                },
            ),
            (
                "miner-2".to_string(),
                LeaderBlockCommitOp {
                    treatment: vec![],
                    sunset_burn: 0,
                    block_header_hash: BlockHeaderHash([0x22; 32]),
                    new_seed: VRFSeed([0x33; 32]),
                    parent_block_ptr: 112,
                    parent_vtxindex: 111,
                    key_block_ptr: 122,
                    key_vtxindex: 457,
                    memo: vec![0x80],

                    burn_fee: 3,
                    input: (Txid([0; 32]), 0),
                    apparent_sender: BurnchainSigner("miner-2".into()),

                    commit_outs: vec![],

                    txid: Txid::from_bytes_be(
                        &hex_bytes(
                            "3c07a0a93360bc85047bbaadd49e30c8af770f73a37e10fec400174d2e5f27d0",
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                    vtxindex: 444,
                    block_height: 124,
                    burn_parent_modulus: (123 % BURN_BLOCK_MINED_AT_MODULUS) as u8,
                    burn_header_hash: BurnchainHeaderHash::from_hex(
                        "0000000000000000000000000000000000000000000000000000000000000004",
                    )
                    .unwrap(),
                },
            ),
            (
                "miner-3".to_string(),
                LeaderBlockCommitOp {
                    treatment: vec![],
                    sunset_burn: 0,
                    block_header_hash: BlockHeaderHash([0x22; 32]),
                    new_seed: VRFSeed([0x33; 32]),
                    parent_block_ptr: 113,
                    parent_vtxindex: 111,
                    key_block_ptr: 121,
                    key_vtxindex: 10,
                    memo: vec![0x80],

                    burn_fee: 5,
                    input: (Txid([0; 32]), 0),
                    apparent_sender: BurnchainSigner("miner-3".into()),
                    commit_outs: vec![],

                    txid: Txid::from_bytes_be(
                        &hex_bytes(
                            "301dc687a9f06a1ae87a013f27133e9cec0843c2983567be73e185827c7c13de",
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                    vtxindex: 445,
                    block_height: 124,
                    burn_parent_modulus: (123 % BURN_BLOCK_MINED_AT_MODULUS) as u8,
                    burn_header_hash: BurnchainHeaderHash::from_hex(
                        "0000000000000000000000000000000000000000000000000000000000000004",
                    )
                    .unwrap(),
                },
            ),
        ];

        let unconfirmed_block_commits = vec![
            LeaderBlockCommitOp {
                treatment: vec![],
                sunset_burn: 0,
                block_header_hash: BlockHeaderHash([0x22; 32]),
                new_seed: VRFSeed([0x33; 32]),
                parent_block_ptr: 124,
                parent_vtxindex: 456,
                key_block_ptr: 123,
                key_vtxindex: 456,
                memo: vec![0x80],

                burn_fee: 2,
                input: (Txid([0; 32]), 0),
                apparent_sender: BurnchainSigner("miner-1".into()),

                commit_outs: vec![],

                txid: Txid::from_bytes_be(
                    &hex_bytes("3c07a0a93360bc85047bbaadd49e30c8af770f73a37e10fec400174d2e5f27cf")
                        .unwrap(),
                )
                .unwrap(),
                vtxindex: 443,
                block_height: 125,
                burn_parent_modulus: (124 % BURN_BLOCK_MINED_AT_MODULUS) as u8,
                burn_header_hash: BurnchainHeaderHash([0x01; 32]),
            },
            LeaderBlockCommitOp {
                treatment: vec![],
                sunset_burn: 0,
                block_header_hash: BlockHeaderHash([0x22; 32]),
                new_seed: VRFSeed([0x33; 32]),
                parent_block_ptr: 124,
                parent_vtxindex: 444,
                key_block_ptr: 123,
                key_vtxindex: 456,
                memo: vec![0x80],

                burn_fee: 3,
                input: (Txid([0; 32]), 0),
                apparent_sender: BurnchainSigner("miner-2".into()),

                commit_outs: vec![],

                txid: Txid::from_bytes_be(
                    &hex_bytes("3c07a0a93360bc85047bbaadd49e30c8af770f73a37e10fec400174d2e5f27cf")
                        .unwrap(),
                )
                .unwrap(),
                vtxindex: 444,
                block_height: 125,
                burn_parent_modulus: (124 % BURN_BLOCK_MINED_AT_MODULUS) as u8,
                burn_header_hash: BurnchainHeaderHash([0x01; 32]),
            },
            LeaderBlockCommitOp {
                treatment: vec![],
                sunset_burn: 0,
                block_header_hash: BlockHeaderHash([0x22; 32]),
                new_seed: VRFSeed([0x33; 32]),
                parent_block_ptr: 124,
                parent_vtxindex: 445,
                key_block_ptr: 123,
                key_vtxindex: 456,
                memo: vec![0x80],

                burn_fee: 10,
                input: (Txid([0; 32]), 0),
                apparent_sender: BurnchainSigner("miner-3".into()),

                commit_outs: vec![],

                txid: Txid::from_bytes_be(
                    &hex_bytes("3c07a0a93360bc85047bbaadd49e30c8af770f73a37e10fec400174d2e5f27cf")
                        .unwrap(),
                )
                .unwrap(),
                vtxindex: 445,
                block_height: 125,
                burn_parent_modulus: (124 % BURN_BLOCK_MINED_AT_MODULUS) as u8,
                burn_header_hash: BurnchainHeaderHash([0x01; 32]),
            },
            LeaderBlockCommitOp {
                treatment: vec![],
                sunset_burn: 0,
                block_header_hash: BlockHeaderHash([0x22; 32]),
                new_seed: VRFSeed([0x33; 32]),
                parent_block_ptr: 124,
                parent_vtxindex: 445,
                key_block_ptr: 123,
                key_vtxindex: 456,
                memo: vec![0x80],

                burn_fee: 10,
                input: (Txid([0; 32]), 0),
                apparent_sender: BurnchainSigner("miner-4".into()),

                commit_outs: vec![],

                txid: Txid::from_bytes_be(
                    &hex_bytes("3c07a0a93360bc85047bbaadd49e30c8af770f73a37e10fec400174d2e5f27cf")
                        .unwrap(),
                )
                .unwrap(),
                vtxindex: 446,
                block_height: 125,
                burn_parent_modulus: (124 % BURN_BLOCK_MINED_AT_MODULUS) as u8,
                burn_header_hash: BurnchainHeaderHash([0x01; 32]),
            },
        ];

        let (spend_dist, total_spend) = MinerStats::get_spend_distribution(
            &active_miners_and_commits,
            &unconfirmed_block_commits,
            &[],
        );
        assert_eq!(total_spend, 2 + 3 + 10 + 10);

        assert_eq!(spend_dist.len(), 4);
        for miner in &[
            "miner-1".to_string(),
            "miner-2".to_string(),
            "miner-3".to_string(),
            "miner-4".to_string(),
        ] {
            let spend = *spend_dist
                .get(miner)
                .unwrap_or_else(|| panic!("no spend for {miner}"));
            match miner.as_str() {
                "miner-1" => {
                    assert_eq!(spend, 2);
                }
                "miner-2" => {
                    assert_eq!(spend, 3);
                }
                "miner-3" => {
                    assert_eq!(spend, 10);
                }
                "miner-4" => {
                    assert_eq!(spend, 10);
                }
                _ => {
                    panic!("unknown miner {miner}");
                }
            }
        }

        let win_probs = MinerStats::get_future_win_distribution(
            &active_miners_and_commits,
            &unconfirmed_block_commits,
            &[],
        );
        for miner in &[
            "miner-1".to_string(),
            "miner-2".to_string(),
            "miner-3".to_string(),
            "miner-4".to_string(),
        ] {
            let prob = *win_probs
                .get(miner)
                .unwrap_or_else(|| panic!("no probability for {miner}"));
            match miner.as_str() {
                "miner-1" => {
                    assert!((prob - (2.0 / 25.0)).abs() < 0.00001);
                }
                "miner-2" => {
                    assert!((prob - (3.0 / 25.0)).abs() < 0.00001);
                }
                "miner-3" => {
                    assert!((prob - (10.0 / 25.0)).abs() < 0.00001);
                }
                "miner-4" => {
                    assert!((prob - (10.0 / 25.0)).abs() < 0.00001);
                }
                _ => {
                    panic!("unknown miner {miner}");
                }
            }
        }
    }
}
