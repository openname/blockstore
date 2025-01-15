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

use std::collections::{HashMap, HashSet};
use std::io;
use std::io::prelude::*;
use std::io::{Read, Write};

use sha2::{Digest, Sha512_256};
use stacks_common::codec::{
    read_next, read_next_at_most, write_next, Error as codec_error, StacksMessageCodec,
    MAX_MESSAGE_LEN,
};
use stacks_common::types::chainstate::{
    BlockHeaderHash, BurnchainHeaderHash, StacksBlockId, StacksWorkScore, TrieHash, VRFSeed,
};
use stacks_common::types::StacksPublicKeyBuffer;
use stacks_common::util::hash::{MerkleTree, Sha512Trunc256Sum};
use stacks_common::util::retry::BoundReader;
use stacks_common::util::secp256k1::MessageSignature;
use stacks_common::util::vrf::*;

use crate::burnchains::{PrivateKey, PublicKey};
use crate::chainstate::burn::operations::*;
use crate::chainstate::burn::{ConsensusHash, *};
use crate::chainstate::stacks::db::StacksBlockHeaderTypes;
use crate::chainstate::stacks::{Error, StacksBlockHeader, StacksMicroblockHeader, *};
use crate::core::*;
use crate::net::Error as net_error;

impl StacksMessageCodec for StacksBlockHeader {
    fn consensus_serialize<W: Write>(&self, fd: &mut W) -> Result<(), codec_error> {
        write_next(fd, &self.version)?;
        write_next(fd, &self.total_work)?;
        write_next(fd, &self.proof)?;
        write_next(fd, &self.parent_block)?;
        write_next(fd, &self.parent_microblock)?;
        write_next(fd, &self.parent_microblock_sequence)?;
        write_next(fd, &self.tx_merkle_root)?;
        write_next(fd, &self.state_index_root)?;
        write_next(fd, &self.microblock_pubkey_hash)?;
        Ok(())
    }

    fn consensus_deserialize<R: Read>(fd: &mut R) -> Result<StacksBlockHeader, codec_error> {
        let version: u8 = read_next(fd)?;
        let total_work: StacksWorkScore = read_next(fd)?;
        let proof: VRFProof = read_next(fd)?;
        let parent_block: BlockHeaderHash = read_next(fd)?;
        let parent_microblock: BlockHeaderHash = read_next(fd)?;
        let parent_microblock_sequence: u16 = read_next(fd)?;
        let tx_merkle_root: Sha512Trunc256Sum = read_next(fd)?;
        let state_index_root: TrieHash = read_next(fd)?;
        let pubkey_hash_buf: Hash160 = read_next(fd)?;

        Ok(StacksBlockHeader {
            version,
            total_work,
            proof,
            parent_block,
            parent_microblock,
            parent_microblock_sequence,
            tx_merkle_root,
            state_index_root,
            microblock_pubkey_hash: pubkey_hash_buf,
        })
    }
}

impl StacksBlockHeader {
    pub fn pubkey_hash(pubk: &StacksPublicKey) -> Hash160 {
        Hash160::from_node_public_key(pubk)
    }

    pub fn genesis_block_header() -> StacksBlockHeader {
        StacksBlockHeader {
            version: 0,
            total_work: StacksWorkScore::genesis(),
            proof: VRFProof::empty(),
            parent_block: BOOT_BLOCK_HASH.clone(),
            parent_microblock: EMPTY_MICROBLOCK_PARENT_HASH.clone(),
            parent_microblock_sequence: 0,
            tx_merkle_root: Sha512Trunc256Sum([0u8; 32]),
            state_index_root: TrieHash([0u8; 32]),
            microblock_pubkey_hash: Hash160([0u8; 20]),
        }
    }

    /// Is `to_check` equal to the `FIRST_STACKS_BLOCK_HASH`?
    pub fn is_first_block_hash(to_check: &BlockHeaderHash) -> bool {
        *to_check == FIRST_STACKS_BLOCK_HASH
    }

    /// Is this a first-mined block header?  i.e. builds off of the boot code?
    pub fn is_first_mined(&self) -> bool {
        Self::is_first_block_hash(&self.parent_block)
    }

    pub fn block_hash(&self) -> BlockHeaderHash {
        if self.total_work.work == 0 {
            // this is the boot block
            return FIRST_STACKS_BLOCK_HASH.clone();
        }
        let mut buf = vec![];
        self.consensus_serialize(&mut buf)
            .expect("BUG: failed to serialize to a vec");
        BlockHeaderHash::from_serialized_header(&buf[..])
    }

    /// This is the "block hash" used for extending the state index root.
    /// This method is necessary because the index root must be globally unique (but, the same stacks
    /// block header can show up multiple times on different burn chain forks and different PoX
    /// forks).  Thus, we mix it with a burnchain block's ConsensusHash, which identifies both the
    /// burnchain block and the PoX fork.
    pub fn make_index_block_hash(
        consensus_hash: &ConsensusHash,
        block_hash: &BlockHeaderHash,
    ) -> StacksBlockId {
        StacksBlockId::new(consensus_hash, block_hash)
    }

    pub fn index_block_hash(&self, consensus_hash: &ConsensusHash) -> StacksBlockId {
        let block_hash = self.block_hash();
        StacksBlockHeader::make_index_block_hash(consensus_hash, &block_hash)
    }

    pub fn from_parent(
        parent_header_hash: BlockHeaderHash,
        parent_microblock_header: Option<&StacksMicroblockHeader>,
        total_work: &StacksWorkScore,
        proof: &VRFProof,
        tx_merkle_root: &Sha512Trunc256Sum,
        state_index_root: &TrieHash,
        microblock_pubkey_hash: &Hash160,
    ) -> StacksBlockHeader {
        let (parent_microblock, parent_microblock_sequence) = match parent_microblock_header {
            Some(header) => (header.block_hash(), header.sequence),
            None => (EMPTY_MICROBLOCK_PARENT_HASH.clone(), 0),
        };

        StacksBlockHeader {
            version: STACKS_BLOCK_VERSION,
            total_work: total_work.clone(),
            proof: proof.clone(),
            parent_block: parent_header_hash,
            parent_microblock,
            parent_microblock_sequence,
            tx_merkle_root: tx_merkle_root.clone(),
            state_index_root: state_index_root.clone(),
            microblock_pubkey_hash: microblock_pubkey_hash.clone(),
        }
    }

    pub fn from_parent_empty(
        parent_header: &StacksBlockHeaderTypes,
        parent_microblock_header: Option<&StacksMicroblockHeader>,
        work_delta: &StacksWorkScore,
        proof: &VRFProof,
        microblock_pubkey_hash: &Hash160,
    ) -> StacksBlockHeader {
        StacksBlockHeader::from_parent(
            parent_header.block_hash(),
            parent_microblock_header,
            work_delta,
            proof,
            &Sha512Trunc256Sum([0u8; 32]),
            &TrieHash([0u8; 32]),
            microblock_pubkey_hash,
        )
    }

    /// Validate this block header against the burnchain.
    /// Used to determine whether or not we'll keep a block around (even if we don't yet have its parent).
    /// * burn_chain_tip is the BlockSnapshot encoding the sortition that selected this block for
    /// inclusion in the Stacks blockchain chain state.
    /// * parent_stacks_chain_tip is the BlockSnapshot for the parent Stacks block this header builds on
    /// (i.e. this is the BlockSnapshot that corresponds to the parent of the given block_commit).
    pub fn validate_burnchain(
        &self,
        burn_chain_tip: &BlockSnapshot,
        sortition_chain_tip: &BlockSnapshot,
        leader_key: &LeaderKeyRegisterOp,
        block_commit: &LeaderBlockCommitOp,
        parent_stacks_chain_tip: &BlockSnapshot,
    ) -> Result<(), Error> {
        // the burn chain tip's sortition must have chosen given block commit
        assert_eq!(
            burn_chain_tip.winning_stacks_block_hash,
            block_commit.block_header_hash
        );
        assert_eq!(burn_chain_tip.winning_block_txid, block_commit.txid);

        // this header must match the header that won sortition on the burn chain
        if self.block_hash() != burn_chain_tip.winning_stacks_block_hash {
            let msg = format!(
                "Invalid Stacks block header {}: invalid commit: {} != {}",
                self.block_hash(),
                self.block_hash(),
                burn_chain_tip.winning_stacks_block_hash
            );
            debug!("{}", msg);
            return Err(Error::InvalidStacksBlock(msg));
        }

        // this header must match the parent header as recorded on the burn chain
        if self.parent_block != parent_stacks_chain_tip.winning_stacks_block_hash {
            let msg = format!(
                "Invalid Stacks block header {}: invalid parent hash: {} != {}",
                self.block_hash(),
                self.parent_block,
                parent_stacks_chain_tip.winning_stacks_block_hash
            );
            debug!("{}", msg);
            return Err(Error::InvalidStacksBlock(msg));
        }

        // this header's proof must hash to the burn chain tip's VRF seed
        if !block_commit.new_seed.is_from_proof(&self.proof) {
            let msg = format!(
                "Invalid Stacks block header {}: invalid VRF proof: hash({}) != {} (but {})",
                self.block_hash(),
                self.proof.to_hex(),
                block_commit.new_seed,
                VRFSeed::from_proof(&self.proof)
            );
            debug!("{}", msg);
            return Err(Error::InvalidStacksBlock(msg));
        }

        // this header must commit to all of the work seen so far in this stacks blockchain fork.
        if self.total_work.burn != parent_stacks_chain_tip.total_burn {
            let msg = format!(
                "Invalid Stacks block header {}: invalid total burns: {} != {}",
                self.block_hash(),
                self.total_work.burn,
                parent_stacks_chain_tip.total_burn
            );
            debug!("{}", msg);
            return Err(Error::InvalidStacksBlock(msg));
        }

        // this header's VRF proof must have been generated from the last sortition's sortition
        // hash (which includes the last commit's VRF seed)
        let valid = match VRF::verify(
            &leader_key.public_key,
            &self.proof,
            sortition_chain_tip.sortition_hash.as_bytes().as_ref(),
        ) {
            Ok(v) => {
                if !v {
                    warn!("Failed to verify proof '{}'", &self.proof.to_hex());
                }
                v
            }
            Err(e) => {
                warn!(
                    "Invalid Stacks block header {}: failed to verify VRF proof: {}",
                    self.block_hash(),
                    e
                );
                false
            }
        };

        if !valid {
            let msg = format!("Invalid Stacks block header {}: leader VRF key {} did not produce a valid proof over {}", self.block_hash(), leader_key.public_key.to_hex(), burn_chain_tip.sortition_hash);
            warn!("{}", msg);
            return Err(Error::InvalidStacksBlock(msg));
        }

        // not verified by this method:
        // * parent_microblock and parent_microblock_sequence (checked in process_block())
        // * total_work.work (need the parent block header for that)
        // * tx_merkle_root     (already verified; validated on deserialization)
        // * state_index_root   (validated on process_block())
        Ok(())
    }

    /// Does this header have a microblock parent?
    pub fn has_microblock_parent(&self) -> bool {
        self.parent_microblock != EMPTY_MICROBLOCK_PARENT_HASH
            || self.parent_microblock_sequence != 0
    }
}

impl StacksMessageCodec for StacksBlock {
    fn consensus_serialize<W: Write>(&self, fd: &mut W) -> Result<(), codec_error> {
        write_next(fd, &self.header)?;
        write_next(fd, &self.txs)?;
        Ok(())
    }

    fn consensus_deserialize<R: Read>(fd: &mut R) -> Result<StacksBlock, codec_error> {
        // NOTE: don't worry about size clamps here; do that when receiving the data from the peer
        // network.  This code assumes that the block will be small enough.
        let header: StacksBlockHeader = read_next(fd)?;
        let txs: Vec<StacksTransaction> = {
            let mut bound_read = BoundReader::from_reader(fd, MAX_MESSAGE_LEN as u64);
            read_next_at_most(&mut bound_read, u32::MAX)
        }?;

        // there must be at least one transaction (the coinbase)
        if txs.is_empty() {
            warn!("Invalid block: Zero-transaction block");
            return Err(codec_error::DeserializeError(
                "Invalid block: zero transactions".to_string(),
            ));
        }

        // all transactions must have anchor mode either OnChainOnly or Any
        // (no OffChainOnly allowed)
        if !StacksBlock::validate_anchor_mode(&txs, true) {
            warn!("Invalid block: Found offchain-only transaction");
            return Err(codec_error::DeserializeError(
                "Invalid block: Found offchain-only transaction".to_string(),
            ));
        }

        // all transactions are unique
        if !StacksBlock::validate_transactions_unique(&txs) {
            warn!("Invalid block: Found duplicate transaction");
            return Err(codec_error::DeserializeError(
                "Invalid block: found duplicate transaction".to_string(),
            ));
        }

        // header and transactions must be consistent
        let txid_vecs: Vec<_> = txs.iter().map(|tx| tx.txid().as_bytes().to_vec()).collect();

        let merkle_tree = MerkleTree::<Sha512Trunc256Sum>::new(&txid_vecs);
        let tx_merkle_root = merkle_tree.root();

        if tx_merkle_root != header.tx_merkle_root {
            warn!("Invalid block: Tx Merkle root mismatch");
            return Err(codec_error::DeserializeError(
                "Invalid block: tx Merkle root mismatch".to_string(),
            ));
        }

        // must be only one coinbase
        let mut coinbase_count = 0;
        for tx in txs.iter() {
            match tx.payload {
                TransactionPayload::Coinbase(..) => {
                    coinbase_count += 1;
                    if coinbase_count > 1 {
                        return Err(codec_error::DeserializeError(
                            "Invalid block: multiple coinbases found".to_string(),
                        ));
                    }
                }
                _ => {}
            }
        }

        // coinbase is present at the right place
        if !StacksBlock::validate_coinbase(&txs, true) {
            warn!("Invalid block: no coinbase found at first transaction slot");
            return Err(codec_error::DeserializeError(
                "Invalid block: no coinbase found at first transaction slot".to_string(),
            ));
        }

        Ok(StacksBlock { header, txs })
    }
}

impl StacksBlock {
    pub fn from_parent(
        parent_header: &StacksBlockHeader,
        parent_microblock_header: &StacksMicroblockHeader,
        txs: Vec<StacksTransaction>,
        work_delta: &StacksWorkScore,
        proof: &VRFProof,
        state_index_root: &TrieHash,
        microblock_pubkey_hash: &Hash160,
    ) -> StacksBlock {
        let txids: Vec<_> = txs
            .iter()
            .map(|ref tx| tx.txid().as_bytes().to_vec())
            .collect();
        let merkle_tree = MerkleTree::<Sha512Trunc256Sum>::new(&txids);
        let tx_merkle_root = merkle_tree.root();
        let header = StacksBlockHeader::from_parent(
            parent_header.block_hash(),
            Some(parent_microblock_header),
            work_delta,
            proof,
            &tx_merkle_root,
            state_index_root,
            microblock_pubkey_hash,
        );
        StacksBlock { header, txs }
    }

    pub fn genesis_block() -> StacksBlock {
        StacksBlock {
            header: StacksBlockHeader::genesis_block_header(),
            txs: vec![],
        }
    }

    /// Is this a first-mined block?  i.e. builds off of the boot code?
    pub fn is_first_mined(&self) -> bool {
        self.header.is_first_mined()
    }

    pub fn block_hash(&self) -> BlockHeaderHash {
        self.header.block_hash()
    }

    pub fn index_block_hash(&self, consensus_hash: &ConsensusHash) -> StacksBlockId {
        self.header.index_block_hash(consensus_hash)
    }

    /// Find and return the coinbase transaction.  It's always the first transaction.
    /// If there are 0 coinbase txs, or more than 1, then return None
    pub fn get_coinbase_tx(&self) -> Option<StacksTransaction> {
        if self.txs.is_empty() {
            return None;
        }
        match self.txs[0].payload {
            TransactionPayload::Coinbase(..) => Some(self.txs[0].clone()),
            _ => None,
        }
    }

    /// verify no duplicate txids
    pub fn validate_transactions_unique(txs: &[StacksTransaction]) -> bool {
        // no duplicates
        let mut txids = HashMap::new();
        for (i, tx) in txs.iter().enumerate() {
            let txid = tx.txid();
            if txids.contains_key(&txid) {
                warn!(
                    "Duplicate tx {}: at index {} and {}",
                    txid,
                    txids.get(&txid).unwrap(),
                    i
                );
                test_debug!("{tx:?}");
                return false;
            }
            txids.insert(txid, i);
        }
        return true;
    }

    /// verify all txs are same mainnet/testnet
    pub fn validate_transactions_network(txs: &[StacksTransaction], mainnet: bool) -> bool {
        for tx in txs {
            if mainnet && !tx.is_mainnet() {
                warn!("Tx {} is not mainnet", tx.txid());
                return false;
            } else if !mainnet && tx.is_mainnet() {
                warn!("Tx {} is not testnet", tx.txid());
                return false;
            }
        }
        return true;
    }

    /// verify all txs are same chain ID
    pub fn validate_transactions_chain_id(txs: &[StacksTransaction], chain_id: u32) -> bool {
        for tx in txs {
            if tx.chain_id != chain_id {
                warn!(
                    "Tx {} has chain ID {:08x}; expected {:08x}",
                    tx.txid(),
                    tx.chain_id,
                    chain_id
                );
                return false;
            }
        }
        return true;
    }

    /// verify anchor modes
    pub fn validate_anchor_mode(txs: &[StacksTransaction], anchored: bool) -> bool {
        for tx in txs {
            match (anchored, tx.anchor_mode) {
                (true, TransactionAnchorMode::OffChainOnly) => {
                    warn!(
                        "Tx {} is off-chain-only; expected on-chain-only or any",
                        tx.txid()
                    );
                    return false;
                }
                (false, TransactionAnchorMode::OnChainOnly) => {
                    warn!(
                        "Tx {} is on-chain-only; expected off-chain-only or any",
                        tx.txid()
                    );
                    return false;
                }
                (_, _) => {}
            }
        }
        return true;
    }

    /// verify that a coinbase is present and is on-chain only, or is absent
    pub fn validate_coinbase(txs: &[StacksTransaction], check_present: bool) -> bool {
        let mut found_coinbase = false;
        let mut coinbase_index = 0;
        for (i, tx) in txs.iter().enumerate() {
            match tx.payload {
                TransactionPayload::Coinbase(..) => {
                    if !check_present {
                        warn!("Found unexpected coinbase tx {}", tx.txid());
                        return false;
                    }

                    if found_coinbase {
                        warn!("Found duplicate coinbase tx {}", tx.txid());
                        return false;
                    }

                    if tx.anchor_mode != TransactionAnchorMode::OnChainOnly {
                        warn!("Invalid coinbase tx {}: not on-chain only", tx.txid());
                        return false;
                    }
                    found_coinbase = true;
                    coinbase_index = i;
                }
                _ => {}
            }
        }

        if coinbase_index != 0 {
            warn!("Found coinbase at index {} (expected 0)", coinbase_index);
            return false;
        }

        match (check_present, found_coinbase) {
            (true, true) => {
                return true;
            }
            (false, false) => {
                return true;
            }
            (true, false) => {
                error!("Expected coinbase, but not found");
                return false;
            }
            (false, true) => {
                error!("Found coinbase, but it was unexpected");
                return false;
            }
        }
    }

    /// Verify that all transactions are supported in the given epoch, as indicated by `epoch_id`
    pub fn validate_transactions_static_epoch(
        txs: &[StacksTransaction],
        epoch_id: StacksEpochId,
    ) -> bool {
        for tx in txs.iter() {
            if !StacksBlock::validate_transaction_static_epoch(tx, epoch_id) {
                return false;
            }
        }
        return true;
    }

    /// Verify that one transaction is supported in the given epoch, as indicated by `epoch_id`
    pub fn validate_transaction_static_epoch(
        tx: &StacksTransaction,
        epoch_id: StacksEpochId,
    ) -> bool {
        if let TransactionPayload::Coinbase(_, ref recipient_opt, ref proof_opt) = &tx.payload {
            if proof_opt.is_some() && epoch_id < StacksEpochId::Epoch30 {
                // not supported
                error!("Coinbase with VRF proof not supported before Stacks 3.0"; "txid" => %tx.txid());
                return false;
            }
            if proof_opt.is_none() && epoch_id >= StacksEpochId::Epoch30 {
                // not supported
                error!("Coinbase with VRF proof is required in Stacks 3.0 and later"; "txid" => %tx.txid());
                return false;
            }
            if recipient_opt.is_some() && epoch_id < StacksEpochId::Epoch21 {
                // not supported
                error!("Coinbase pay-to-alt-recipient not supported before Stacks 2.1"; "txid" => %tx.txid());
                return false;
            }
        }
        if let TransactionPayload::SmartContract(_, ref version_opt) = &tx.payload {
            if version_opt.is_some() && epoch_id < StacksEpochId::Epoch21 {
                // not supported
                error!("Versioned smart contracts not supported before Stacks 2.1");
                return false;
            }
        }
        if let TransactionPayload::TenureChange(..) = &tx.payload {
            if epoch_id < StacksEpochId::Epoch30 {
                error!("TenureChange transaction not supported before Stacks 3.0"; "txid" => %tx.txid());
                return false;
            }
        }
        if !tx.auth.is_supported_in_epoch(epoch_id) {
            error!("Authentication mode not supported in Epoch {epoch_id}");
            return false;
        }
        return true;
    }

    /// static sanity checks on transactions.
    pub fn validate_transactions_static(
        &self,
        mainnet: bool,
        chain_id: u32,
        epoch_id: StacksEpochId,
    ) -> bool {
        if !StacksBlock::validate_transactions_unique(&self.txs) {
            return false;
        }
        if !StacksBlock::validate_transactions_network(&self.txs, mainnet) {
            return false;
        }
        if !StacksBlock::validate_transactions_chain_id(&self.txs, chain_id) {
            return false;
        }
        if !StacksBlock::validate_anchor_mode(&self.txs, true) {
            return false;
        }
        if !StacksBlock::validate_coinbase(&self.txs, true) {
            return false;
        }
        if !StacksBlock::validate_transactions_static_epoch(&self.txs, epoch_id) {
            return false;
        }
        return true;
    }

    /// Does this block have a microblock parent?
    pub fn has_microblock_parent(&self) -> bool {
        self.header.has_microblock_parent()
    }

    /// Returns size in bytes of `StacksMessageCodec` representation
    /// Note that this will serialize the block, so don't call if there is a better way to get block size
    pub fn block_size(&self) -> Result<usize, codec_error> {
        let mut buf = vec![];
        self.consensus_serialize(&mut buf)?;
        Ok(buf.len())
    }
}

impl StacksMessageCodec for StacksMicroblockHeader {
    fn consensus_serialize<W: Write>(&self, fd: &mut W) -> Result<(), codec_error> {
        self.serialize(fd, false)
    }

    fn consensus_deserialize<R: Read>(fd: &mut R) -> Result<StacksMicroblockHeader, codec_error> {
        let version: u8 = read_next(fd)?;
        let sequence: u16 = read_next(fd)?;
        let prev_block: BlockHeaderHash = read_next(fd)?;
        let tx_merkle_root: Sha512Trunc256Sum = read_next(fd)?;
        let signature: MessageSignature = read_next(fd)?;

        // signature must be well-formed
        let _ = signature
            .to_secp256k1_recoverable()
            .ok_or(codec_error::DeserializeError(
                "Failed to parse signature".to_string(),
            ))?;

        Ok(StacksMicroblockHeader {
            version,
            sequence,
            prev_block,
            tx_merkle_root,
            signature,
        })
    }
}

impl StacksMicroblockHeader {
    pub fn sign(&mut self, privk: &StacksPrivateKey) -> Result<(), net_error> {
        self.signature = MessageSignature::empty();
        let mut bytes = vec![];
        self.consensus_serialize(&mut bytes)
            .expect("BUG: failed to serialize to a vec");

        let mut digest_bits = [0u8; 32];
        let mut sha2 = Sha512_256::new();

        sha2.update(&bytes[..]);
        digest_bits.copy_from_slice(sha2.finalize().as_slice());

        let sig = privk
            .sign(&digest_bits)
            .map_err(|se| net_error::SigningError(se.to_string()))?;

        self.signature = sig;
        Ok(())
    }

    fn serialize<W: Write>(&self, fd: &mut W, empty_sig: bool) -> Result<(), codec_error> {
        write_next(fd, &self.version)?;
        write_next(fd, &self.sequence)?;
        write_next(fd, &self.prev_block)?;
        write_next(fd, &self.tx_merkle_root)?;
        if empty_sig {
            write_next(fd, &MessageSignature::empty())?;
        } else {
            write_next(fd, &self.signature)?;
        }
        Ok(())
    }

    pub fn check_recover_pubkey(&self) -> Result<Hash160, net_error> {
        let mut digest_bits = [0u8; 32];
        let mut sha2 = Sha512_256::new();

        self.serialize(&mut sha2, true)
            .expect("BUG: failed to serialize to a vec");
        digest_bits.copy_from_slice(sha2.finalize().as_slice());

        let mut pubk =
            StacksPublicKey::recover_to_pubkey(&digest_bits, &self.signature).map_err(|_ve| {
                test_debug!(
                    "Failed to verify signature: failed to recover public key from {:?}: {:?}",
                    &self.signature,
                    &_ve
                );
                net_error::VerifyingError(
                    "Failed to verify signature: failed to recover public key".to_string(),
                )
            })?;

        pubk.set_compressed(true);
        Ok(StacksBlockHeader::pubkey_hash(&pubk))
    }

    pub fn verify(&self, pubk_hash: &Hash160) -> Result<(), net_error> {
        let pubkh = self.check_recover_pubkey()?;

        if pubkh != *pubk_hash {
            test_debug!(
                "Failed to verify signature: public key did not recover to hash {}",
                &pubkh.to_hex()
            );
            return Err(net_error::VerifyingError(format!(
                "Failed to verify signature: public key did not recover to expected hash {}",
                pubkh.to_hex()
            )));
        }

        Ok(())
    }

    pub fn block_hash(&self) -> BlockHeaderHash {
        let mut bytes = vec![];
        self.consensus_serialize(&mut bytes)
            .expect("BUG: failed to serialize to a vec");
        BlockHeaderHash::from_serialized_header(&bytes[..])
    }

    /// Create the first microblock header in a microblock stream.
    /// The header will not be signed
    pub fn first_unsigned(
        parent_block_hash: &BlockHeaderHash,
        tx_merkle_root: &Sha512Trunc256Sum,
    ) -> StacksMicroblockHeader {
        StacksMicroblockHeader {
            version: 0,
            sequence: 0,
            prev_block: parent_block_hash.clone(),
            tx_merkle_root: tx_merkle_root.clone(),
            signature: MessageSignature::empty(),
        }
    }

    /// Create the first microblock header in a microblock stream for an empty microblock stream.
    /// The header will not be signed
    pub fn first_empty_unsigned(parent_block_hash: &BlockHeaderHash) -> StacksMicroblockHeader {
        StacksMicroblockHeader::first_unsigned(parent_block_hash, &Sha512Trunc256Sum([0u8; 32]))
    }

    /// Create an unsigned microblock header from its parent
    /// Return an error on overflow
    pub fn from_parent_unsigned(
        parent_header: &StacksMicroblockHeader,
        tx_merkle_root: &Sha512Trunc256Sum,
    ) -> Option<StacksMicroblockHeader> {
        let next_sequence = match parent_header.sequence.checked_add(1) {
            Some(next) => next,
            None => {
                return None;
            }
        };

        Some(StacksMicroblockHeader {
            version: 0,
            sequence: next_sequence,
            prev_block: parent_header.block_hash(),
            tx_merkle_root: tx_merkle_root.clone(),
            signature: MessageSignature::empty(),
        })
    }
}

impl StacksMessageCodec for StacksMicroblock {
    fn consensus_serialize<W: Write>(&self, fd: &mut W) -> Result<(), codec_error> {
        write_next(fd, &self.header)?;
        write_next(fd, &self.txs)?;
        Ok(())
    }

    fn consensus_deserialize<R: Read>(fd: &mut R) -> Result<StacksMicroblock, codec_error> {
        // NOTE: maximum size must be checked elsewhere!
        let header: StacksMicroblockHeader = read_next(fd)?;
        let txs: Vec<StacksTransaction> = {
            let mut bound_read = BoundReader::from_reader(fd, MAX_MESSAGE_LEN as u64);
            read_next(&mut bound_read)
        }?;

        if txs.is_empty() {
            warn!("Invalid microblock: zero transactions");
            return Err(codec_error::DeserializeError(
                "Invalid microblock: zero transactions".to_string(),
            ));
        }

        if !StacksBlock::validate_transactions_unique(&txs) {
            warn!("Invalid microblock: duplicate transaction");
            return Err(codec_error::DeserializeError(
                "Invalid microblock: duplicate transaction".to_string(),
            ));
        }

        if !StacksBlock::validate_anchor_mode(&txs, false) {
            warn!("Invalid microblock: found on-chain-only transaction");
            return Err(codec_error::DeserializeError(
                "Invalid microblock: found on-chain-only transaction".to_string(),
            ));
        }

        // header and transactions must be consistent
        let txid_vecs: Vec<_> = txs.iter().map(|tx| tx.txid().as_bytes().to_vec()).collect();

        let merkle_tree = MerkleTree::<Sha512Trunc256Sum>::new(&txid_vecs);
        let tx_merkle_root = merkle_tree.root();

        if tx_merkle_root != header.tx_merkle_root {
            return Err(codec_error::DeserializeError(
                "Invalid microblock: tx Merkle root mismatch".to_string(),
            ));
        }

        if !StacksBlock::validate_coinbase(&txs, false) {
            warn!("Invalid microblock: found coinbase transaction");
            return Err(codec_error::DeserializeError(
                "Invalid microblock: found coinbase transaction".to_string(),
            ));
        }

        Ok(StacksMicroblock { header, txs })
    }
}

impl StacksMicroblock {
    pub fn first_unsigned(
        parent_block_hash: &BlockHeaderHash,
        txs: Vec<StacksTransaction>,
    ) -> StacksMicroblock {
        let txids: Vec<_> = txs
            .iter()
            .map(|ref tx| tx.txid().as_bytes().to_vec())
            .collect();
        let merkle_tree = MerkleTree::<Sha512Trunc256Sum>::new(&txids);
        let tx_merkle_root = merkle_tree.root();
        let header = StacksMicroblockHeader::first_unsigned(parent_block_hash, &tx_merkle_root);
        StacksMicroblock { header, txs }
    }

    pub fn from_parent_unsigned(
        parent_header: &StacksMicroblockHeader,
        txs: Vec<StacksTransaction>,
    ) -> Option<StacksMicroblock> {
        let txids: Vec<_> = txs
            .iter()
            .map(|ref tx| tx.txid().as_bytes().to_vec())
            .collect();
        let merkle_tree = MerkleTree::<Sha512Trunc256Sum>::new(&txids);
        let tx_merkle_root = merkle_tree.root();
        let header =
            match StacksMicroblockHeader::from_parent_unsigned(parent_header, &tx_merkle_root) {
                Some(h) => h,
                None => {
                    return None;
                }
            };

        Some(StacksMicroblock { header, txs })
    }

    pub fn sign(&mut self, privk: &StacksPrivateKey) -> Result<(), net_error> {
        self.header.sign(privk)
    }

    pub fn verify(&mut self, pubk_hash: &Hash160) -> Result<(), net_error> {
        self.header.verify(pubk_hash)
    }

    pub fn block_hash(&self) -> BlockHeaderHash {
        self.header.block_hash()
    }

    /// static sanity checks on transactions.
    pub fn validate_transactions_static(&self, mainnet: bool, chain_id: u32) -> bool {
        if !StacksBlock::validate_transactions_unique(&self.txs) {
            return false;
        }
        if !StacksBlock::validate_transactions_network(&self.txs, mainnet) {
            return false;
        }
        if !StacksBlock::validate_transactions_chain_id(&self.txs, chain_id) {
            return false;
        }
        if !StacksBlock::validate_anchor_mode(&self.txs, false) {
            return false;
        }
        if !StacksBlock::validate_coinbase(&self.txs, false) {
            return false;
        }
        return true;
    }
}

#[cfg(test)]
mod test {
    use std::error::Error;

    use stacks_common::address::*;
    use stacks_common::types::chainstate::StacksAddress;
    use stacks_common::util::hash::*;

    use super::*;
    use crate::burnchains::bitcoin::address::BitcoinAddress;
    use crate::burnchains::bitcoin::blocks::BitcoinBlockParser;
    use crate::burnchains::bitcoin::keys::BitcoinPublicKey;
    use crate::burnchains::bitcoin::BitcoinNetworkType;
    use crate::burnchains::{BurnchainBlockHeader, BurnchainSigner, Txid};
    use crate::chainstate::burn::operations::leader_block_commit::BURN_BLOCK_MINED_AT_MODULUS;
    use crate::chainstate::stacks::address::StacksAddressExtensions;
    use crate::chainstate::stacks::test::{make_codec_test_block, *};
    use crate::chainstate::stacks::*;
    use crate::net::codec::test::*;
    use crate::net::codec::*;
    use crate::net::*;

    #[test]
    fn codec_stacks_block_ecvrf_proof() {
        let proof_bytes = hex_bytes("9275df67a68c8745c0ff97b48201ee6db447f7c93b23ae24cdc2400f52fdb08a1a6ac7ec71bf9c9c76e96ee4675ebff60625af28718501047bfd87b810c2d2139b73c23bd69de66360953a642c2a330a").unwrap();
        let proof = VRFProof::from_bytes(&proof_bytes[..].to_vec()).unwrap();

        check_codec_and_corruption::<VRFProof>(&proof, &proof_bytes);
    }

    #[test]
    fn codec_stacks_block_work_score() {
        let work_score = StacksWorkScore {
            burn: 123,
            work: 456,
        };
        let work_score_bytes = vec![
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 123, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x01, 200,
        ];

        check_codec_and_corruption::<StacksWorkScore>(&work_score, &work_score_bytes);
    }

    #[test]
    fn codec_stacks_block_header() {
        let proof_bytes = hex_bytes("9275df67a68c8745c0ff97b48201ee6db447f7c93b23ae24cdc2400f52fdb08a1a6ac7ec71bf9c9c76e96ee4675ebff60625af28718501047bfd87b810c2d2139b73c23bd69de66360953a642c2a330a").unwrap();
        let proof = VRFProof::from_bytes(&proof_bytes[..].to_vec()).unwrap();

        let header = StacksBlockHeader {
            version: 0x12,
            total_work: StacksWorkScore {
                burn: 123,
                work: 456,
            },
            proof,
            parent_block: FIRST_STACKS_BLOCK_HASH.clone(),
            parent_microblock: BlockHeaderHash([1u8; 32]),
            parent_microblock_sequence: 3,
            tx_merkle_root: Sha512Trunc256Sum([2u8; 32]),
            state_index_root: TrieHash([3u8; 32]),
            microblock_pubkey_hash: Hash160([4u8; 20]),
        };

        let header_bytes = vec![
            // version
            0x12, // work score
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 123, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x01, 200, // proof
            0x92, 0x75, 0xdf, 0x67, 0xa6, 0x8c, 0x87, 0x45, 0xc0, 0xff, 0x97, 0xb4, 0x82, 0x01,
            0xee, 0x6d, 0xb4, 0x47, 0xf7, 0xc9, 0x3b, 0x23, 0xae, 0x24, 0xcd, 0xc2, 0x40, 0x0f,
            0x52, 0xfd, 0xb0, 0x8a, 0x1a, 0x6a, 0xc7, 0xec, 0x71, 0xbf, 0x9c, 0x9c, 0x76, 0xe9,
            0x6e, 0xe4, 0x67, 0x5e, 0xbf, 0xf6, 0x06, 0x25, 0xaf, 0x28, 0x71, 0x85, 0x01, 0x04,
            0x7b, 0xfd, 0x87, 0xb8, 0x10, 0xc2, 0xd2, 0x13, 0x9b, 0x73, 0xc2, 0x3b, 0xd6, 0x9d,
            0xe6, 0x63, 0x60, 0x95, 0x3a, 0x64, 0x2c, 0x2a, 0x33, 0x0a, // parent block
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, // parent microblock
            0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
            0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
            0x01, 0x01, 0x01, 0x01, // parent microblock sequence
            0x00, 0x03, // tx merkle root
            0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02,
            0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02,
            0x02, 0x02, 0x02, 0x02, // state index root
            0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03,
            0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03,
            0x03, 0x03, 0x03, 0x03, // public key hash buf
            0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04,
            0x04, 0x04, 0x04, 0x04, 0x04, 0x04,
        ];

        check_codec_and_corruption::<StacksBlockHeader>(&header, &header_bytes);
    }

    #[test]
    fn codec_stacks_microblock_header() {
        let header = StacksMicroblockHeader {
            version: 0x12,
            sequence: 0x34,
            prev_block: EMPTY_MICROBLOCK_PARENT_HASH.clone(),
            tx_merkle_root: Sha512Trunc256Sum([1u8; 32]),
            signature: MessageSignature([2u8; 65]),
        };

        let header_bytes = vec![
            // version
            0x12, // sequence
            0x00, 0x34, // prev block
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, // tx merkle root
            0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
            0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
            0x01, 0x01, 0x01, 0x01, // signature
            0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02,
            0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02,
            0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02,
            0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02,
            0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02,
        ];

        check_codec_and_corruption::<StacksMicroblockHeader>(&header, &header_bytes);
    }

    #[test]
    fn codec_stacks_block() {
        let parent_microblock_header = StacksMicroblockHeader {
            version: 0x12,
            sequence: 0x34,
            prev_block: BlockHeaderHash([0x0au8; 32]),
            tx_merkle_root: Sha512Trunc256Sum([0x0bu8; 32]),
            signature: MessageSignature([0x0cu8; 65]),
        };

        let mut block = make_codec_test_block(100000000, StacksEpochId::latest());
        block.header.version = 0x24;

        let ph = block.header.parent_block.as_bytes().to_vec();
        let mh = block.header.parent_microblock.as_bytes().to_vec();
        let tr = block.header.tx_merkle_root.as_bytes().to_vec();
        let sr = block.header.state_index_root.as_bytes().to_vec();
        let pk = block.header.microblock_pubkey_hash.as_bytes().to_vec();

        let mut block_bytes = vec![
            // header
            // version
            0x24, // work score (parent work score + current work score)
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 234, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x02, 55, // proof
            0x92, 0x75, 0xdf, 0x67, 0xa6, 0x8c, 0x87, 0x45, 0xc0, 0xff, 0x97, 0xb4, 0x82, 0x01,
            0xee, 0x6d, 0xb4, 0x47, 0xf7, 0xc9, 0x3b, 0x23, 0xae, 0x24, 0xcd, 0xc2, 0x40, 0x0f,
            0x52, 0xfd, 0xb0, 0x8a, 0x1a, 0x6a, 0xc7, 0xec, 0x71, 0xbf, 0x9c, 0x9c, 0x76, 0xe9,
            0x6e, 0xe4, 0x67, 0x5e, 0xbf, 0xf6, 0x06, 0x25, 0xaf, 0x28, 0x71, 0x85, 0x01, 0x04,
            0x7b, 0xfd, 0x87, 0xb8, 0x10, 0xc2, 0xd2, 0x13, 0x9b, 0x73, 0xc2, 0x3b, 0xd6, 0x9d,
            0xe6, 0x63, 0x60, 0x95, 0x3a, 0x64, 0x2c, 0x2a, 0x33, 0x0a, // parent block
            ph[0], ph[1], ph[2], ph[3], ph[4], ph[5], ph[6], ph[7], ph[8], ph[9], ph[10], ph[11],
            ph[12], ph[13], ph[14], ph[15], ph[16], ph[17], ph[18], ph[19], ph[20], ph[21], ph[22],
            ph[23], ph[24], ph[25], ph[26], ph[27], ph[28], ph[29], ph[30], ph[31],
            // parent microblock
            mh[0], mh[1], mh[2], mh[3], mh[4], mh[5], mh[6], mh[7], mh[8], mh[9], mh[10], mh[11],
            mh[12], mh[13], mh[14], mh[15], mh[16], mh[17], mh[18], mh[19], mh[20], mh[21], mh[22],
            mh[23], mh[24], mh[25], mh[26], mh[27], mh[28], mh[29], mh[30], mh[31],
            // parent microblock sequence
            0x00, 0x04, // tx merkle root
            tr[0], tr[1], tr[2], tr[3], tr[4], tr[5], tr[6], tr[7], tr[8], tr[9], tr[10], tr[11],
            tr[12], tr[13], tr[14], tr[15], tr[16], tr[17], tr[18], tr[19], tr[20], tr[21], tr[22],
            tr[23], tr[24], tr[25], tr[26], tr[27], tr[28], tr[29], tr[30], tr[31],
            // state index root
            sr[0], sr[1], sr[2], sr[3], sr[4], sr[5], sr[6], sr[7], sr[8], sr[9], sr[10], sr[11],
            sr[12], sr[13], sr[14], sr[15], sr[16], sr[17], sr[18], sr[19], sr[20], sr[21], sr[22],
            sr[23], sr[24], sr[25], sr[26], sr[27], sr[28], sr[29], sr[30], sr[31],
            // public key hash buf
            pk[0], pk[1], pk[2], pk[3], pk[4], pk[5], pk[6], pk[7], pk[8], pk[9], pk[10], pk[11],
            pk[12], pk[13], pk[14], pk[15], pk[16], pk[17], pk[18], pk[19],
        ];

        check_codec_and_corruption::<StacksBlockHeader>(&block.header, &block_bytes);

        let mut tx_bytes: Vec<u8> = vec![];
        block.txs.consensus_serialize(&mut tx_bytes).unwrap();
        block_bytes.append(&mut tx_bytes);

        eprintln!(
            "block is {} bytes with {} txs",
            block_bytes.len(),
            block.txs.len()
        );
        check_codec_and_corruption::<StacksBlock>(&block, &block_bytes);
    }

    #[test]
    fn codec_stacks_microblock() {
        // make a block with each and every kind of transaction
        let all_txs = codec_all_transactions(
            &TransactionVersion::Testnet,
            0x80000000,
            &TransactionAnchorMode::OffChainOnly,
            &TransactionPostConditionMode::Allow,
            StacksEpochId::latest(),
        );

        // make microblocks with 3 transactions each (or fewer)
        for i in 0..(all_txs.len() / 3) {
            let txs = vec![
                all_txs[3 * i].clone(),
                all_txs[3 * i + 1].clone(),
                all_txs[3 * i + 2].clone(),
            ];

            let txid_vecs: Vec<_> = txs.iter().map(|tx| tx.txid().as_bytes().to_vec()).collect();

            let merkle_tree = MerkleTree::<Sha512Trunc256Sum>::new(&txid_vecs);
            let tx_merkle_root = merkle_tree.root();
            let tr = tx_merkle_root.as_bytes().to_vec();

            let header = StacksMicroblockHeader {
                version: 0x12,
                sequence: 0x34,
                prev_block: EMPTY_MICROBLOCK_PARENT_HASH.clone(),
                tx_merkle_root,
                signature: MessageSignature([
                    0x00, 0x35, 0x44, 0x45, 0xa1, 0xdc, 0x98, 0xa1, 0xbd, 0x27, 0x98, 0x4d, 0xbe,
                    0x69, 0x97, 0x9a, 0x5c, 0xd7, 0x78, 0x86, 0xb4, 0xd9, 0x13, 0x4a, 0xf5, 0xc4,
                    0x0e, 0x63, 0x4d, 0x96, 0xe1, 0xcb, 0x44, 0x5b, 0x97, 0xde, 0x5b, 0x63, 0x25,
                    0x82, 0xd3, 0x17, 0x04, 0xf8, 0x67, 0x06, 0xa7, 0x80, 0x88, 0x6e, 0x6e, 0x38,
                    0x1b, 0xfe, 0xd6, 0x52, 0x28, 0x26, 0x73, 0x58, 0x26, 0x2d, 0x20, 0x3f, 0xe6,
                ]),
            };

            let mut block_bytes = vec![
                // header
                // version
                0x12, // sequence
                0x00, 0x34, // prev block
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, // tx merkle root
                tr[0], tr[1], tr[2], tr[3], tr[4], tr[5], tr[6], tr[7], tr[8], tr[9], tr[10],
                tr[11], tr[12], tr[13], tr[14], tr[15], tr[16], tr[17], tr[18], tr[19], tr[20],
                tr[21], tr[22], tr[23], tr[24], tr[25], tr[26], tr[27], tr[28], tr[29], tr[30],
                tr[31], // signature
                0x00, 0x35, 0x44, 0x45, 0xa1, 0xdc, 0x98, 0xa1, 0xbd, 0x27, 0x98, 0x4d, 0xbe, 0x69,
                0x97, 0x9a, 0x5c, 0xd7, 0x78, 0x86, 0xb4, 0xd9, 0x13, 0x4a, 0xf5, 0xc4, 0x0e, 0x63,
                0x4d, 0x96, 0xe1, 0xcb, 0x44, 0x5b, 0x97, 0xde, 0x5b, 0x63, 0x25, 0x82, 0xd3, 0x17,
                0x04, 0xf8, 0x67, 0x06, 0xa7, 0x80, 0x88, 0x6e, 0x6e, 0x38, 0x1b, 0xfe, 0xd6, 0x52,
                0x28, 0x26, 0x73, 0x58, 0x26, 0x2d, 0x20, 0x3f, 0xe6,
            ];

            let mut tx_bytes: Vec<u8> = vec![];
            txs.consensus_serialize(&mut tx_bytes).unwrap();
            block_bytes.append(&mut tx_bytes);

            let mblock = StacksMicroblock { header, txs };

            check_codec_and_corruption::<StacksMicroblock>(&mblock, &block_bytes);
        }
    }

    #[test]
    fn stacks_microblock_sign_verify() {
        let privk = StacksPrivateKey::from_hex(
            "6d430bb91222408e7706c9001cfaeb91b08c2be6d5ac95779ab52c6b431950e001",
        )
        .unwrap();
        let mut mblock_header = StacksMicroblockHeader {
            version: 0x12,
            sequence: 0x34,
            prev_block: EMPTY_MICROBLOCK_PARENT_HASH.clone(),
            tx_merkle_root: Sha512Trunc256Sum([0u8; 32]),
            signature: MessageSignature::empty(),
        };

        let pubk = StacksPublicKey::from_private(&privk);
        let pubkh = Hash160::from_node_public_key(&pubk);

        mblock_header.sign(&privk).unwrap();
        mblock_header.verify(&pubkh).unwrap();
    }

    #[test]
    fn stacks_microblock_sign_verify_uncompressed() {
        let privk = StacksPrivateKey::from_hex(
            "6d430bb91222408e7706c9001cfaeb91b08c2be6d5ac95779ab52c6b431950e0",
        )
        .unwrap();
        let mut mblock_header = StacksMicroblockHeader {
            version: 0x12,
            sequence: 0x34,
            prev_block: EMPTY_MICROBLOCK_PARENT_HASH.clone(),
            tx_merkle_root: Sha512Trunc256Sum([0u8; 32]),
            signature: MessageSignature::empty(),
        };

        let mut pubk = StacksPublicKey::from_private(&privk);
        pubk.set_compressed(false);

        let mut pubk_compressed = StacksPublicKey::from_private(&privk);
        pubk_compressed.set_compressed(true);

        let pubkh = Hash160::from_data(&pubk.to_bytes());
        let pubkh_compressed = Hash160::from_data(&pubk_compressed.to_bytes());

        mblock_header.sign(&privk).unwrap();
        mblock_header.verify(&pubkh).unwrap_err();
        mblock_header.verify(&pubkh_compressed).unwrap();
    }

    #[test]
    fn stacks_header_validate_burnchain() {
        let mut header = StacksBlockHeader {
            version: 0x01,
            total_work: StacksWorkScore {
                burn: 234,
                work: 567,
            },
            proof: VRFProof::empty(),
            parent_block: BlockHeaderHash([5u8; 32]),
            parent_microblock: BlockHeaderHash([6u8; 32]),
            parent_microblock_sequence: 4,
            tx_merkle_root: Sha512Trunc256Sum([7u8; 32]),
            state_index_root: TrieHash([8u8; 32]),
            microblock_pubkey_hash: Hash160([9u8; 20]),
        };

        let mut burn_chain_tip = BlockSnapshot::initial(122, &BurnchainHeaderHash([3u8; 32]), 0);
        let mut stacks_chain_tip = BlockSnapshot::initial(122, &BurnchainHeaderHash([3u8; 32]), 1);
        let sortition_chain_tip = BlockSnapshot::initial(122, &BurnchainHeaderHash([3u8; 32]), 2);

        let leader_key = LeaderKeyRegisterOp {
            consensus_hash: ConsensusHash::from_bytes(
                &hex_bytes("0000000000000000000000000000000000000000").unwrap(),
            )
            .unwrap(),
            public_key: VRFPublicKey::from_bytes(
                &hex_bytes("a366b51292bef4edd64063d9145c617fec373bceb0758e98cd72becd84d54c7a")
                    .unwrap(),
            )
            .unwrap(),
            memo: vec![1, 2, 3, 4, 5],

            txid: Txid::from_bytes_be(
                &hex_bytes("1bfa831b5fc56c858198acb8e77e5863c1e9d8ac26d49ddb914e24d8d4083562")
                    .unwrap(),
            )
            .unwrap(),
            vtxindex: 455,
            block_height: 123,
            burn_header_hash: BurnchainHeaderHash([0xfe; 32]),
        };

        let mut block_commit = LeaderBlockCommitOp {
            sunset_burn: 0,
            treatment: vec![],
            block_header_hash: header.block_hash(),
            new_seed: VRFSeed::from_proof(&header.proof),
            parent_block_ptr: 0,
            parent_vtxindex: 0,
            key_block_ptr: leader_key.block_height as u32,
            key_vtxindex: leader_key.vtxindex as u16,
            memo: vec![0x80],
            commit_outs: vec![],

            burn_fee: 12345,
            input: (Txid([0; 32]), 0),
            apparent_sender: BurnchainSigner::mock_parts(
                AddressHashMode::SerializeP2PKH,
                1,
                vec![StacksPublicKey::from_hex(
                    "02d8015134d9db8178ac93acbc43170a2f20febba5087a5b0437058765ad5133d0",
                )
                .unwrap()],
            ),

            txid: Txid::from_bytes_be(
                &hex_bytes("3c07a0a93360bc85047bbaadd49e30c8af770f73a37e10fec400174d2e5f27cf")
                    .unwrap(),
            )
            .unwrap(),
            vtxindex: 444,
            block_height: 125,
            burn_parent_modulus: (124 % BURN_BLOCK_MINED_AT_MODULUS) as u8,

            burn_header_hash: BurnchainHeaderHash([0xff; 32]),
        };

        burn_chain_tip.winning_stacks_block_hash = header.block_hash();
        burn_chain_tip.winning_block_txid = block_commit.txid.clone();

        stacks_chain_tip.winning_stacks_block_hash = header.parent_block.clone();
        stacks_chain_tip.total_burn = header.total_work.burn;

        // should fail due to invalid proof
        assert!(header
            .validate_burnchain(
                &burn_chain_tip,
                &sortition_chain_tip,
                &leader_key,
                &block_commit,
                &stacks_chain_tip
            )
            .unwrap_err()
            .to_string()
            .find("did not produce a valid proof")
            .is_some());

        // should fail due to invalid burns
        stacks_chain_tip.total_burn += 1;
        assert!(header
            .validate_burnchain(
                &burn_chain_tip,
                &sortition_chain_tip,
                &leader_key,
                &block_commit,
                &stacks_chain_tip
            )
            .unwrap_err()
            .to_string()
            .find("invalid total burns")
            .is_some());

        // should fail due to invalid VRF seed
        block_commit.new_seed = VRFSeed::initial();
        assert!(header
            .validate_burnchain(
                &burn_chain_tip,
                &sortition_chain_tip,
                &leader_key,
                &block_commit,
                &stacks_chain_tip
            )
            .unwrap_err()
            .to_string()
            .find("invalid VRF proof")
            .is_some());

        // should fail due to invalid parent hash
        stacks_chain_tip.winning_stacks_block_hash = BlockHeaderHash([0u8; 32]);
        assert!(header
            .validate_burnchain(
                &burn_chain_tip,
                &sortition_chain_tip,
                &leader_key,
                &block_commit,
                &stacks_chain_tip
            )
            .unwrap_err()
            .to_string()
            .find("invalid parent hash")
            .is_some());

        // should fail due to bad commit
        header.version += 1;
        assert!(header
            .validate_burnchain(
                &burn_chain_tip,
                &sortition_chain_tip,
                &leader_key,
                &block_commit,
                &stacks_chain_tip
            )
            .unwrap_err()
            .to_string()
            .find("invalid commit")
            .is_some());
    }

    #[test]
    fn stacks_block_invalid() {
        let header = StacksBlockHeader {
            version: 0x01,
            total_work: StacksWorkScore {
                burn: 234,
                work: 567,
            },
            proof: VRFProof::empty(),
            parent_block: BlockHeaderHash([5u8; 32]),
            parent_microblock: BlockHeaderHash([6u8; 32]),
            parent_microblock_sequence: 4,
            tx_merkle_root: Sha512Trunc256Sum([7u8; 32]),
            state_index_root: TrieHash([8u8; 32]),
            microblock_pubkey_hash: Hash160([9u8; 20]),
        };

        let privk = StacksPrivateKey::from_hex(
            "6d430bb91222408e7706c9001cfaeb91b08c2be6d5ac95779ab52c6b431950e001",
        )
        .unwrap();
        let origin_auth = TransactionAuth::Standard(
            TransactionSpendingCondition::new_singlesig_p2pkh(StacksPublicKey::from_private(
                &privk,
            ))
            .unwrap(),
        );
        let tx_coinbase = StacksTransaction::new(
            TransactionVersion::Testnet,
            origin_auth.clone(),
            TransactionPayload::Coinbase(CoinbasePayload([0u8; 32]), None, None),
        );

        let tx_coinbase_2 = StacksTransaction::new(
            TransactionVersion::Testnet,
            origin_auth.clone(),
            TransactionPayload::Coinbase(CoinbasePayload([1u8; 32]), None, None),
        );

        let mut tx_invalid_coinbase = tx_coinbase.clone();
        tx_invalid_coinbase.anchor_mode = TransactionAnchorMode::OffChainOnly;

        let stx_address = StacksAddress {
            version: 0,
            bytes: Hash160([0u8; 20]),
        };
        let mut tx_invalid_anchor = StacksTransaction::new(
            TransactionVersion::Testnet,
            origin_auth.clone(),
            TransactionPayload::TokenTransfer(
                stx_address.into(),
                123,
                TokenTransferMemo([1u8; 34]),
            ),
        );

        tx_invalid_anchor.anchor_mode = TransactionAnchorMode::OffChainOnly;

        let mut tx_dup = tx_invalid_anchor.clone();
        tx_dup.anchor_mode = TransactionAnchorMode::OnChainOnly;

        let txs_bad_coinbase = vec![tx_invalid_coinbase.clone()];
        let txs_no_coinbase = vec![tx_dup.clone()];
        let txs_multiple_coinbases = vec![tx_coinbase.clone(), tx_coinbase_2.clone()];
        let txs_bad_anchor = vec![tx_coinbase.clone(), tx_invalid_anchor.clone()];
        let txs_dup = vec![tx_coinbase.clone(), tx_dup.clone(), tx_dup.clone()];

        let get_tx_root = |txs: &[StacksTransaction]| {
            let txid_vecs: Vec<_> = txs.iter().map(|tx| tx.txid().as_bytes().to_vec()).collect();

            let merkle_tree = MerkleTree::<Sha512Trunc256Sum>::new(&txid_vecs);
            let tx_merkle_root = merkle_tree.root();
            tx_merkle_root
        };

        let mut block_header_no_coinbase = header.clone();
        block_header_no_coinbase.tx_merkle_root = get_tx_root(&txs_no_coinbase);

        let mut block_header_multiple_coinbase = header.clone();
        block_header_multiple_coinbase.tx_merkle_root = get_tx_root(&txs_multiple_coinbases);

        let mut block_header_invalid_coinbase = header.clone();
        block_header_invalid_coinbase.tx_merkle_root = get_tx_root(&txs_bad_coinbase);

        let mut block_header_invalid_anchor = header.clone();
        block_header_invalid_anchor.tx_merkle_root = get_tx_root(&txs_bad_anchor);

        let mut block_header_dup_tx = header.clone();
        block_header_dup_tx.tx_merkle_root = get_tx_root(&txs_dup);

        let mut block_header_empty = header.clone();
        block_header_empty.tx_merkle_root = get_tx_root(&[]);

        let invalid_blocks = vec![
            (
                StacksBlock {
                    header: block_header_no_coinbase,
                    txs: txs_no_coinbase,
                },
                "no coinbase found",
            ),
            (
                StacksBlock {
                    header: block_header_multiple_coinbase,
                    txs: txs_multiple_coinbases,
                },
                "multiple coinbases found",
            ),
            (
                StacksBlock {
                    header: block_header_invalid_anchor,
                    txs: txs_bad_anchor,
                },
                "Found offchain-only transaction",
            ),
            (
                StacksBlock {
                    header: block_header_dup_tx,
                    txs: txs_dup,
                },
                "found duplicate transaction",
            ),
            (
                StacksBlock {
                    header: block_header_empty,
                    txs: vec![],
                },
                "zero transactions",
            ),
        ];
        for (ref block, ref msg) in invalid_blocks.iter() {
            let mut bytes: Vec<u8> = vec![];
            block.consensus_serialize(&mut bytes).unwrap();
            assert!(StacksBlock::consensus_deserialize(&mut &bytes[..])
                .unwrap_err()
                .to_string()
                .find(msg)
                .is_some());
        }
    }

    #[test]
    fn stacks_microblock_invalid() {
        let header = StacksMicroblockHeader {
            version: 0x12,
            sequence: 0x34,
            prev_block: EMPTY_MICROBLOCK_PARENT_HASH.clone(),
            tx_merkle_root: Sha512Trunc256Sum([0u8; 32]),
            signature: MessageSignature::empty(),
        };

        let privk = StacksPrivateKey::from_hex(
            "6d430bb91222408e7706c9001cfaeb91b08c2be6d5ac95779ab52c6b431950e001",
        )
        .unwrap();
        let origin_auth = TransactionAuth::Standard(
            TransactionSpendingCondition::new_singlesig_p2pkh(StacksPublicKey::from_private(
                &privk,
            ))
            .unwrap(),
        );
        let tx_coinbase = StacksTransaction::new(
            TransactionVersion::Testnet,
            origin_auth.clone(),
            TransactionPayload::Coinbase(CoinbasePayload([0u8; 32]), None, None),
        );

        let mut tx_coinbase_offchain = tx_coinbase.clone();
        tx_coinbase_offchain.anchor_mode = TransactionAnchorMode::OffChainOnly;

        let stx_address = StacksAddress {
            version: 0,
            bytes: Hash160([0u8; 20]),
        };
        let mut tx_invalid_anchor = StacksTransaction::new(
            TransactionVersion::Testnet,
            origin_auth.clone(),
            TransactionPayload::TokenTransfer(
                stx_address.into(),
                123,
                TokenTransferMemo([1u8; 34]),
            ),
        );

        tx_invalid_anchor.anchor_mode = TransactionAnchorMode::OnChainOnly;

        let mut tx_dup = tx_invalid_anchor.clone();
        tx_dup.anchor_mode = TransactionAnchorMode::OffChainOnly;

        let txs_coinbase = vec![tx_coinbase.clone()];
        let txs_offchain_coinbase = vec![tx_coinbase_offchain.clone()];
        let txs_bad_anchor = vec![tx_invalid_anchor.clone()];
        let txs_dup = vec![tx_dup.clone(), tx_dup.clone()];

        let get_tx_root = |txs: &[StacksTransaction]| {
            let txid_vecs: Vec<_> = txs.iter().map(|tx| tx.txid().as_bytes().to_vec()).collect();

            let merkle_tree = MerkleTree::<Sha512Trunc256Sum>::new(&txid_vecs);
            let tx_merkle_root = merkle_tree.root();
            tx_merkle_root
        };

        let mut block_header_coinbase = header.clone();
        block_header_coinbase.tx_merkle_root = get_tx_root(&txs_coinbase);

        let mut block_header_offchain_coinbase = header.clone();
        block_header_offchain_coinbase.tx_merkle_root = get_tx_root(&txs_coinbase);

        let mut block_header_invalid_anchor = header.clone();
        block_header_invalid_anchor.tx_merkle_root = get_tx_root(&txs_bad_anchor);

        let mut block_header_dup_tx = header.clone();
        block_header_dup_tx.tx_merkle_root = get_tx_root(&txs_dup);

        let mut block_header_empty = header.clone();
        block_header_empty.tx_merkle_root = get_tx_root(&[]);

        let invalid_blocks = vec![
            (
                StacksMicroblock {
                    header: block_header_offchain_coinbase,
                    txs: txs_offchain_coinbase,
                },
                "invalid anchor mode for Coinbase",
            ),
            (
                StacksMicroblock {
                    header: block_header_coinbase,
                    txs: txs_coinbase,
                },
                "found on-chain-only transaction",
            ),
            (
                StacksMicroblock {
                    header: block_header_invalid_anchor,
                    txs: txs_bad_anchor,
                },
                "found on-chain-only transaction",
            ),
            (
                StacksMicroblock {
                    header: block_header_dup_tx,
                    txs: txs_dup,
                },
                "duplicate transaction",
            ),
            (
                StacksMicroblock {
                    header: block_header_empty,
                    txs: vec![],
                },
                "zero transactions",
            ),
        ];
        for (ref block, ref msg) in invalid_blocks.iter() {
            let mut bytes: Vec<u8> = vec![];
            block.consensus_serialize(&mut bytes).unwrap();
            assert!(StacksMicroblock::consensus_deserialize(&mut &bytes[..])
                .unwrap_err()
                .to_string()
                .find(msg)
                .is_some());
        }
    }

    fn verify_block_epoch_validation(
        txs: &[StacksTransaction],
        tx_coinbase_old: Option<StacksTransaction>,
        tx_coinbase_nakamoto: Option<StacksTransaction>,
        activation_epoch_id: StacksEpochId,
        header: StacksBlockHeader,
        deactivation_epoch_id: Option<StacksEpochId>,
    ) {
        let epoch_list = [
            StacksEpochId::Epoch10,
            StacksEpochId::Epoch20,
            StacksEpochId::Epoch2_05,
            StacksEpochId::Epoch21,
            StacksEpochId::Epoch22,
            StacksEpochId::Epoch23,
            StacksEpochId::Epoch24,
            StacksEpochId::Epoch25,
            StacksEpochId::Epoch30,
        ];
        let get_tx_root = |txs: &[StacksTransaction]| {
            let txid_vecs: Vec<_> = txs.iter().map(|tx| tx.txid().as_bytes().to_vec()).collect();

            let merkle_tree = MerkleTree::<Sha512Trunc256Sum>::new(&txid_vecs);
            let tx_merkle_root = merkle_tree.root();
            tx_merkle_root
        };
        let mut block_header_dup_tx = header.clone();
        block_header_dup_tx.tx_merkle_root = get_tx_root(&txs.to_vec());

        let block = StacksBlock {
            header: block_header_dup_tx.clone(),
            txs: txs.to_vec(),
        };

        let block_with_coinbase_tx = tx_coinbase_old.map(|coinbase| {
            let mut txs_with_coinbase = txs.to_vec();
            txs_with_coinbase.insert(0, coinbase);

            let mut block_header_dup_tx_with_coinbase = header.clone();
            block_header_dup_tx_with_coinbase.tx_merkle_root =
                get_tx_root(&txs_with_coinbase.to_vec());

            StacksBlock {
                header: block_header_dup_tx_with_coinbase.clone(),
                txs: txs_with_coinbase,
            }
        });

        let block_with_coinbase_tx_nakamoto = tx_coinbase_nakamoto.map(|coinbase| {
            let mut txs_with_coinbase_nakamoto = txs.to_vec();
            txs_with_coinbase_nakamoto.insert(0, coinbase);

            let mut block_header_dup_tx_with_coinbase_nakamoto = header.clone();
            block_header_dup_tx_with_coinbase_nakamoto.tx_merkle_root =
                get_tx_root(&txs_with_coinbase_nakamoto.to_vec());

            StacksBlock {
                header: block_header_dup_tx_with_coinbase_nakamoto.clone(),
                txs: txs_with_coinbase_nakamoto,
            }
        });

        for epoch_id in epoch_list.iter() {
            let block_to_check = if *epoch_id >= StacksEpochId::Epoch30
                && block_with_coinbase_tx_nakamoto.is_some()
            {
                block_with_coinbase_tx_nakamoto.clone().unwrap()
            } else if *epoch_id >= StacksEpochId::Epoch21
                && *epoch_id < StacksEpochId::Epoch30
                && block_with_coinbase_tx.is_some()
            {
                block_with_coinbase_tx.clone().unwrap()
            } else {
                block.clone()
            };

            let mut bytes: Vec<u8> = vec![];
            block_to_check.consensus_serialize(&mut bytes).unwrap();

            if *epoch_id < activation_epoch_id {
                assert!(!StacksBlock::validate_transactions_static_epoch(
                    &txs,
                    epoch_id.clone(),
                ));
            } else if deactivation_epoch_id.is_none() || deactivation_epoch_id.unwrap() > *epoch_id
            {
                assert!(StacksBlock::validate_transactions_static_epoch(
                    &txs, *epoch_id,
                ));
            } else {
                assert!(!StacksBlock::validate_transactions_static_epoch(
                    &txs, *epoch_id,
                ));
            }
        }
    }

    #[test]
    fn test_block_validate_transactions_static() {
        let header = StacksBlockHeader {
            version: 0x01,
            total_work: StacksWorkScore {
                burn: 234,
                work: 567,
            },
            proof: VRFProof::empty(),
            parent_block: BlockHeaderHash([5u8; 32]),
            parent_microblock: BlockHeaderHash([6u8; 32]),
            parent_microblock_sequence: 4,
            tx_merkle_root: Sha512Trunc256Sum([7u8; 32]),
            state_index_root: TrieHash([8u8; 32]),
            microblock_pubkey_hash: Hash160([9u8; 20]),
        };

        let stx_address = StacksAddress {
            version: 0,
            bytes: Hash160([0u8; 20]),
        };

        let privk = StacksPrivateKey::from_hex(
            "6d430bb91222408e7706c9001cfaeb91b08c2be6d5ac95779ab52c6b431950e001",
        )
        .unwrap();
        let origin_auth = TransactionAuth::Standard(
            TransactionSpendingCondition::new_singlesig_p2pkh(StacksPublicKey::from_private(
                &privk,
            ))
            .unwrap(),
        );

        let privk_1 = StacksPrivateKey::from_hex(
            "6d430bb91222408e7706c9001cfaeb91b08c2be6d5ac95779ab52c6b431950e001",
        )
        .unwrap();
        let privk_2 = StacksPrivateKey::from_hex(
            "2a584d899fed1d24e26b524f202763c8ab30260167429f157f1c119f550fa6af01",
        )
        .unwrap();
        let privk_3 = StacksPrivateKey::from_hex(
            "d5200dee706ee53ae98a03fba6cf4fdcc5084c30cfa9e1b3462dcdeaa3e0f1d201",
        )
        .unwrap();

        let pubk_1 = StacksPublicKey::from_private(&privk_1);
        let pubk_2 = StacksPublicKey::from_private(&privk_2);
        let pubk_3 = StacksPublicKey::from_private(&privk_3);

        let order_independent_multisig_condition_p2wsh =
            TransactionSpendingCondition::new_multisig_order_independent_p2wsh(
                2,
                vec![pubk_1.clone(), pubk_2.clone(), pubk_3.clone()],
            )
            .unwrap();

        let order_independent_multisig_condition_p2sh =
            TransactionSpendingCondition::new_multisig_order_independent_p2sh(
                2,
                vec![pubk_1.clone(), pubk_2.clone(), pubk_3.clone()],
            )
            .unwrap();

        let order_independent_sponsored_auth_p2sh = TransactionAuth::Sponsored(
            TransactionSpendingCondition::new_singlesig_p2pkh(StacksPublicKey::from_private(
                &privk,
            ))
            .unwrap(),
            order_independent_multisig_condition_p2sh.clone(),
        );

        let order_independent_sponsored_auth_p2wsh = TransactionAuth::Sponsored(
            TransactionSpendingCondition::new_singlesig_p2pkh(StacksPublicKey::from_private(
                &privk,
            ))
            .unwrap(),
            order_independent_multisig_condition_p2wsh.clone(),
        );
        let order_independent_origin_auth_p2sh =
            TransactionAuth::Standard(order_independent_multisig_condition_p2sh.clone());

        let order_independent_origin_auth_p2wsh =
            TransactionAuth::Standard(order_independent_multisig_condition_p2wsh.clone());

        let order_independent_multisig_tx_transfer_mainnet_p2sh = StacksTransaction::new(
            TransactionVersion::Mainnet,
            order_independent_origin_auth_p2sh.clone(),
            TransactionPayload::TokenTransfer(
                stx_address.into(),
                123,
                TokenTransferMemo([1u8; 34]),
            ),
        );

        let order_independent_multisig_tx_transfer_mainnet_p2wsh = StacksTransaction::new(
            TransactionVersion::Mainnet,
            order_independent_origin_auth_p2wsh.clone(),
            TransactionPayload::TokenTransfer(
                stx_address.into(),
                123,
                TokenTransferMemo([1u8; 34]),
            ),
        );

        let order_independent_sponsored_multisig_tx_transfer_mainnet_p2sh = StacksTransaction::new(
            TransactionVersion::Mainnet,
            order_independent_sponsored_auth_p2sh.clone(),
            TransactionPayload::TokenTransfer(
                stx_address.into(),
                123,
                TokenTransferMemo([1u8; 34]),
            ),
        );

        let order_independent_sponsored_multisig_tx_transfer_mainnet_p2wsh = StacksTransaction::new(
            TransactionVersion::Mainnet,
            order_independent_sponsored_auth_p2wsh.clone(),
            TransactionPayload::TokenTransfer(
                stx_address.into(),
                123,
                TokenTransferMemo([1u8; 34]),
            ),
        );

        let mut tx_signer =
            StacksTransactionSigner::new(&order_independent_multisig_tx_transfer_mainnet_p2sh);
        tx_signer.sign_origin(&privk_1).unwrap();
        tx_signer.sign_origin(&privk_2).unwrap();
        tx_signer.append_origin(&pubk_3).unwrap();
        let order_independent_multisig_tx_transfer_mainnet_p2sh_signed =
            tx_signer.get_tx().unwrap();

        let mut tx_signer =
            StacksTransactionSigner::new(&order_independent_multisig_tx_transfer_mainnet_p2wsh);
        tx_signer.sign_origin(&privk_1).unwrap();
        tx_signer.sign_origin(&privk_2).unwrap();
        tx_signer.append_origin(&pubk_3).unwrap();
        let order_independent_multisig_tx_transfer_mainnet_p2wsh_signed =
            tx_signer.get_tx().unwrap();

        let mut tx_signer = StacksTransactionSigner::new(
            &order_independent_sponsored_multisig_tx_transfer_mainnet_p2sh,
        );
        tx_signer.sign_origin(&privk).unwrap();
        tx_signer.sign_sponsor(&privk_1).unwrap();
        tx_signer.sign_sponsor(&privk_2).unwrap();
        tx_signer.append_sponsor(&pubk_3).unwrap();
        let order_independent_sponsored_multisig_tx_transfer_mainnet_p2sh_signed =
            tx_signer.get_tx().unwrap();

        let mut tx_signer = StacksTransactionSigner::new(
            &order_independent_sponsored_multisig_tx_transfer_mainnet_p2wsh,
        );
        tx_signer.sign_origin(&privk).unwrap();
        tx_signer.sign_sponsor(&privk_1).unwrap();
        tx_signer.sign_sponsor(&privk_2).unwrap();
        tx_signer.append_sponsor(&pubk_3).unwrap();
        let order_independent_sponsored_multisig_tx_transfer_mainnet_p2wsh_signed =
            tx_signer.get_tx().unwrap();

        let tx_coinbase = StacksTransaction::new(
            TransactionVersion::Testnet,
            origin_auth.clone(),
            TransactionPayload::Coinbase(CoinbasePayload([0u8; 32]), None, None),
        );

        let tx_coinbase_contract = StacksTransaction::new(
            TransactionVersion::Testnet,
            origin_auth.clone(),
            TransactionPayload::Coinbase(
                CoinbasePayload([1u8; 32]),
                Some(PrincipalData::Contract(
                    QualifiedContractIdentifier::transient(),
                )),
                None,
            ),
        );

        let proof_bytes = hex_bytes("9275df67a68c8745c0ff97b48201ee6db447f7c93b23ae24cdc2400f52fdb08a1a6ac7ec71bf9c9c76e96ee4675ebff60625af28718501047bfd87b810c2d2139b73c23bd69de66360953a642c2a330a").unwrap();
        let proof = VRFProof::from_bytes(&proof_bytes[..].to_vec()).unwrap();
        let tx_coinbase_proof = StacksTransaction::new(
            TransactionVersion::Testnet,
            origin_auth.clone(),
            TransactionPayload::Coinbase(CoinbasePayload([0u8; 32]), None, Some(proof)),
        );

        let stx_address = StacksAddress {
            version: 0,
            bytes: Hash160([0u8; 20]),
        };
        let tx_transfer = StacksTransaction::new(
            TransactionVersion::Testnet,
            origin_auth.clone(),
            TransactionPayload::TokenTransfer(
                stx_address.into(),
                123,
                TokenTransferMemo([1u8; 34]),
            ),
        );
        let tx_transfer_mainnet = StacksTransaction::new(
            TransactionVersion::Mainnet,
            origin_auth.clone(),
            TransactionPayload::TokenTransfer(
                stx_address.into(),
                123,
                TokenTransferMemo([1u8; 34]),
            ),
        );
        let mut tx_transfer_alt_chain = StacksTransaction::new(
            TransactionVersion::Testnet,
            origin_auth.clone(),
            TransactionPayload::TokenTransfer(
                stx_address.into(),
                123,
                TokenTransferMemo([1u8; 34]),
            ),
        );
        tx_transfer_alt_chain.chain_id += 1;

        let mut tx_transfer_bad_anchor = StacksTransaction::new(
            TransactionVersion::Testnet,
            origin_auth.clone(),
            TransactionPayload::TokenTransfer(
                stx_address.into(),
                123,
                TokenTransferMemo([1u8; 34]),
            ),
        );
        tx_transfer_bad_anchor.anchor_mode = TransactionAnchorMode::OffChainOnly;

        let tx_versioned_smart_contract = StacksTransaction::new(
            TransactionVersion::Testnet,
            origin_auth.clone(),
            TransactionPayload::SmartContract(
                TransactionSmartContract {
                    name: ContractName::try_from("hello-world").unwrap(),
                    code_body: StacksString::from_str("(print \"hello world\")").unwrap(),
                },
                Some(ClarityVersion::Clarity1),
            ),
        );

        let tenure_change_payload = TenureChangePayload {
            tenure_consensus_hash: ConsensusHash([0x01; 20]),
            prev_tenure_consensus_hash: ConsensusHash([0x02; 20]),
            burn_view_consensus_hash: ConsensusHash([0x03; 20]),
            previous_tenure_end: StacksBlockId([0x00; 32]),
            previous_tenure_blocks: 0,
            cause: TenureChangeCause::BlockFound,
            pubkey_hash: Hash160([0x00; 20]),
        };
        let tx_tenure_change = StacksTransaction::new(
            TransactionVersion::Testnet,
            origin_auth.clone(),
            TransactionPayload::TenureChange(tenure_change_payload),
        );

        let dup_txs = vec![
            tx_coinbase.clone(),
            tx_transfer.clone(),
            tx_transfer.clone(),
        ];
        let mainnet_txs = vec![tx_coinbase.clone(), tx_transfer_mainnet.clone()];
        let alt_chain_id_txs = vec![tx_coinbase.clone(), tx_transfer_alt_chain.clone()];
        let offchain_txs = vec![tx_coinbase.clone(), tx_transfer_bad_anchor.clone()];
        let no_coinbase = vec![tx_transfer.clone()];
        let coinbase_contract = vec![tx_coinbase_contract.clone()];
        let versioned_contract = vec![tx_versioned_smart_contract.clone()];
        let nakamoto_coinbase = vec![tx_coinbase_proof.clone()];
        let tenure_change_tx = vec![tx_tenure_change.clone()];
        let nakamoto_txs = vec![tx_coinbase_proof.clone(), tx_tenure_change.clone()];
        let order_independent_multisig_txs = vec![
            order_independent_multisig_tx_transfer_mainnet_p2sh_signed.clone(),
            order_independent_sponsored_multisig_tx_transfer_mainnet_p2sh_signed.clone(),
            order_independent_multisig_tx_transfer_mainnet_p2wsh_signed.clone(),
            order_independent_sponsored_multisig_tx_transfer_mainnet_p2wsh_signed.clone(),
        ];

        assert!(!StacksBlock::validate_transactions_unique(&dup_txs));
        assert!(!StacksBlock::validate_transactions_network(
            &mainnet_txs,
            false
        ));
        assert!(!StacksBlock::validate_transactions_chain_id(
            &alt_chain_id_txs,
            0x80000000
        ));
        assert!(!StacksBlock::validate_anchor_mode(&offchain_txs, true));
        assert!(!StacksBlock::validate_coinbase(&no_coinbase, true));

        verify_block_epoch_validation(
            &versioned_contract,
            Some(tx_coinbase.clone()),
            Some(tx_coinbase_proof.clone()),
            StacksEpochId::Epoch21,
            header.clone(),
            None,
        );
        verify_block_epoch_validation(
            &coinbase_contract,
            None,
            None,
            StacksEpochId::Epoch21,
            header.clone(),
            Some(StacksEpochId::Epoch30),
        );
        verify_block_epoch_validation(
            &order_independent_multisig_txs,
            Some(tx_coinbase.clone()),
            Some(tx_coinbase_proof.clone()),
            StacksEpochId::Epoch30,
            header.clone(),
            None,
        );
        verify_block_epoch_validation(
            &nakamoto_txs,
            Some(tx_coinbase.clone()),
            None,
            StacksEpochId::Epoch30,
            header.clone(),
            None,
        );
        verify_block_epoch_validation(
            &nakamoto_coinbase,
            Some(tx_coinbase.clone()),
            None,
            StacksEpochId::Epoch30,
            header.clone(),
            None,
        );
        verify_block_epoch_validation(
            &tenure_change_tx,
            Some(tx_coinbase.clone()),
            Some(tx_coinbase_proof.clone()),
            StacksEpochId::Epoch30,
            header.clone(),
            None,
        );
    }

    // TODO:
    // * size limits
}
