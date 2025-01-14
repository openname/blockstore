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

use std::{error, fmt, fs, io};

use clarity::vm::types::PrincipalData;
use serde::Deserialize;
use serde_json::json;
use stacks_common::types::chainstate::{
    BlockHeaderHash, BurnchainHeaderHash, StacksAddress, StacksBlockId, TrieHash, VRFSeed,
};
use stacks_common::types::StacksPublicKeyBuffer;
use stacks_common::util::hash::{hex_bytes, to_hex, Hash160, Sha512Trunc256Sum};
use stacks_common::util::secp256k1::MessageSignature;
use stacks_common::util::vrf::VRFPublicKey;

use self::leader_block_commit::Treatment;
use crate::burnchains::{
    Address, Burnchain, BurnchainBlockHeader, BurnchainRecipient, BurnchainSigner,
    BurnchainTransaction, Error as BurnchainError, PublicKey, Txid,
};
use crate::chainstate::burn::db::sortdb::SortitionHandleTx;
use crate::chainstate::burn::operations::leader_block_commit::{
    MissedBlockCommit, BURN_BLOCK_MINED_AT_MODULUS,
};
use crate::chainstate::burn::{ConsensusHash, Opcodes};
use crate::chainstate::stacks::address::PoxAddress;
use crate::util_lib::db::{DBConn, DBTx, Error as db_error};

pub mod delegate_stx;
pub mod leader_block_commit;
pub mod leader_key_register;
pub mod stack_stx;
pub mod transfer_stx;
pub mod vote_for_aggregate_key;

#[cfg(test)]
mod test;

/// This module contains all burn-chain operations

#[derive(Debug)]
pub enum Error {
    /// Failed to parse the operation from the burnchain transaction
    ParseError,
    /// Invalid input data
    InvalidInput,
    /// Database error
    DBError(db_error),

    // block commits related errors
    BlockCommitPredatesGenesis,
    BlockCommitAlreadyExists,
    BlockCommitNoLeaderKey,
    BlockCommitNoParent,
    BlockCommitBadInput,
    BlockCommitBadOutputs,
    BlockCommitAnchorCheck,
    BlockCommitBadModulus,
    BlockCommitBadEpoch,
    BlockCommitMissDistanceTooBig,
    MissedBlockCommit(MissedBlockCommit),

    // leader key register related errors
    LeaderKeyAlreadyRegistered,

    // transfer stx related errors
    TransferStxMustBePositive,
    TransferStxSelfSend,

    // stack stx related errors
    StackStxMustBePositive,
    StackStxInvalidCycles,
    StackStxInvalidKey,

    // errors associated with delegate stx
    DelegateStxMustBePositive,

    // sBTC errors
    AmountMustBePositive,

    // vote-for-aggregate-public-key errors
    VoteForAggregateKeyInvalidKey,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::ParseError => write!(f, "Failed to parse transaction into Blockstack operation"),
            Error::InvalidInput => write!(f, "Invalid input"),
            Error::DBError(ref e) => fmt::Display::fmt(e, f),

            Error::BlockCommitPredatesGenesis => write!(f, "Block commit predates genesis block"),
            Error::BlockCommitAlreadyExists => {
                write!(f, "Block commit commits to an already-seen block")
            }
            Error::BlockCommitNoLeaderKey => write!(f, "Block commit has no matching register key"),
            Error::BlockCommitNoParent => write!(f, "Block commit parent does not exist"),
            Error::BlockCommitBadInput => write!(
                f,
                "Block commit tx input does not match register key tx output"
            ),
            Error::BlockCommitAnchorCheck => {
                write!(f, "Failure checking PoX anchor block for commit")
            }
            Error::BlockCommitBadOutputs => {
                write!(f, "Block commit included a bad commitment output")
            }
            Error::BlockCommitBadModulus => {
                write!(f, "Block commit included a bad burn block height modulus")
            }
            Error::BlockCommitBadEpoch => {
                write!(f, "Block commit has an invalid epoch")
            }
            Error::BlockCommitMissDistanceTooBig => {
                write!(
                    f,
                    "Block commit missed its target sortition height by too much"
                )
            }
            Error::MissedBlockCommit(_) => write!(
                f,
                "Block commit included in a burn block that was not intended"
            ),
            Error::LeaderKeyAlreadyRegistered => {
                write!(f, "Leader key has already been registered")
            }
            Error::TransferStxMustBePositive => write!(f, "Transfer STX must be positive amount"),
            Error::TransferStxSelfSend => write!(f, "Transfer STX must not send to self"),
            Error::StackStxMustBePositive => write!(f, "Stack STX must be positive amount"),
            Error::StackStxInvalidCycles => write!(
                f,
                "Stack STX must set num cycles between 1 and max num cycles"
            ),
            Error::StackStxInvalidKey => write!(f, "Signer key is invalid"),
            Error::DelegateStxMustBePositive => write!(f, "Delegate STX must be positive amount"),
            Error::VoteForAggregateKeyInvalidKey => {
                write!(f, "Aggregate key is invalid")
            }
            Self::AmountMustBePositive => write!(f, "Peg in amount must be positive"),
        }
    }
}

impl error::Error for Error {
    fn cause(&self) -> Option<&dyn error::Error> {
        match *self {
            Error::DBError(ref e) => Some(e),
            _ => None,
        }
    }
}

impl From<db_error> for Error {
    fn from(e: db_error) -> Error {
        Error::DBError(e)
    }
}

#[derive(Debug, PartialEq, Clone, Eq, Serialize, Deserialize)]
pub struct TransferStxOp {
    pub sender: StacksAddress,
    pub recipient: StacksAddress,
    pub transfered_ustx: u128,
    pub memo: Vec<u8>,

    // common to all transactions
    pub txid: Txid,                            // transaction ID
    pub vtxindex: u32,                         // index in the block where this tx occurs
    pub block_height: u64,                     // block height at which this tx occurs
    pub burn_header_hash: BurnchainHeaderHash, // hash of the burn chain block header
}

#[derive(Debug, PartialEq, Clone, Eq, Serialize, Deserialize)]
pub struct StackStxOp {
    pub sender: StacksAddress,
    /// the PoX reward address.
    /// NOTE: the address in .pox will be tagged as either p2pkh or p2sh; it's impossible to tell
    /// if it's a segwit-p2sh since that looks identical to a p2sh address.
    pub reward_addr: PoxAddress,
    /// how many ustx this transaction locks
    pub stacked_ustx: u128,
    pub num_cycles: u8,
    pub signer_key: Option<StacksPublicKeyBuffer>,
    pub max_amount: Option<u128>,
    pub auth_id: Option<u32>,

    // common to all transactions
    pub txid: Txid,                            // transaction ID
    pub vtxindex: u32,                         // index in the block where this tx occurs
    pub block_height: u64,                     // block height at which this tx occurs
    pub burn_header_hash: BurnchainHeaderHash, // hash of the burn chain block header
}

#[derive(Debug, PartialEq, Clone, Eq, Serialize, Deserialize)]
pub struct PreStxOp {
    /// the output address
    /// (must be a legacy Bitcoin address)
    pub output: StacksAddress,

    // common to all transactions
    pub txid: Txid,                            // transaction ID
    pub vtxindex: u32,                         // index in the block where this tx occurs
    pub block_height: u64,                     // block height at which this tx occurs
    pub burn_header_hash: BurnchainHeaderHash, // hash of the burn chain block header
}

#[derive(Debug, PartialEq, Clone, Eq, Serialize, Deserialize)]
pub struct LeaderBlockCommitOp {
    pub block_header_hash: BlockHeaderHash, // hash of Stacks block header (sha512/256)

    pub new_seed: VRFSeed,     // new seed for this block
    pub parent_block_ptr: u32, // block height of the block that contains the parent block hash
    pub parent_vtxindex: u16, // offset in the parent block where the parent block hash can be found
    pub key_block_ptr: u32,   // pointer to the block that contains the leader key registration
    pub key_vtxindex: u16,    // offset in the block where the leader key can be found
    pub memo: Vec<u8>,        // extra unused byte

    /// how many burn tokens (e.g. satoshis) were committed to produce this block
    pub burn_fee: u64,
    /// the input transaction, used in mining commitment smoothing
    pub input: (Txid, u32),

    pub burn_parent_modulus: u8,

    /// the apparent sender of the transaction. note: this
    ///  is *not* authenticated, and should be used only
    ///  for informational purposes (e.g., log messages)
    pub apparent_sender: BurnchainSigner,

    /// PoX/Burn outputs
    pub commit_outs: Vec<PoxAddress>,

    /// If the active epoch supports PoX reward/punishment
    /// via burns, this vector will contain the treatment (rewarded or punished)
    /// of the PoX addresses active during the block commit.
    ///
    /// This value is set by the check() call, not during parsing.
    #[serde(default = "default_treatment")]
    pub treatment: Vec<Treatment>,

    // PoX sunset burn
    pub sunset_burn: u64,

    // common to all transactions
    pub txid: Txid,                            // transaction ID
    pub vtxindex: u32,                         // index in the block where this tx occurs
    pub block_height: u64,                     // block height at which this tx occurs
    pub burn_header_hash: BurnchainHeaderHash, // hash of the burn chain block header
}

fn default_treatment() -> Vec<Treatment> {
    Vec::new()
}

#[derive(Debug, PartialEq, Clone, Eq, Serialize, Deserialize)]
pub struct LeaderKeyRegisterOp {
    pub consensus_hash: ConsensusHash, // consensus hash at time of issuance
    pub public_key: VRFPublicKey,      // EdDSA public key
    pub memo: Vec<u8>,                 // extra bytes in the op-return

    // common to all transactions
    pub txid: Txid,                            // transaction ID
    pub vtxindex: u32,                         // index in the block where this tx occurs
    pub block_height: u64,                     // block height at which this tx occurs
    pub burn_header_hash: BurnchainHeaderHash, // hash of burn chain block
}

#[derive(Debug, PartialEq, Clone, Eq, Serialize, Deserialize)]
pub struct DelegateStxOp {
    pub sender: StacksAddress,
    pub delegate_to: StacksAddress,
    /// a tuple representing the output index of the reward address in the BTC transaction,
    ///  and the actual  PoX reward address.
    /// NOTE: the address in .pox-2 will be tagged as either p2pkh or p2sh; it's impossible to tell
    /// if it's a segwit-p2sh since that looks identical to a p2sh address.
    pub reward_addr: Option<(u32, PoxAddress)>,
    pub delegated_ustx: u128,
    pub until_burn_height: Option<u64>,

    // common to all transactions
    pub txid: Txid,                            // transaction ID
    pub vtxindex: u32,                         // index in the block where this tx occurs
    pub block_height: u64,                     // block height at which this tx occurs
    pub burn_header_hash: BurnchainHeaderHash, // hash of the burn chain block header
}

#[derive(Debug, PartialEq, Clone, Eq, Serialize, Deserialize)]
pub struct VoteForAggregateKeyOp {
    pub sender: StacksAddress,
    pub aggregate_key: StacksPublicKeyBuffer,
    pub round: u32,
    pub reward_cycle: u64,
    pub signer_index: u16,
    pub signer_key: StacksPublicKeyBuffer,

    // common to all transactions
    pub txid: Txid,                            // transaction ID
    pub vtxindex: u32,                         // index in the block where this tx occurs
    pub block_height: u64,                     // block height at which this tx occurs
    pub burn_header_hash: BurnchainHeaderHash, // hash of the burn chain block header
}

fn hex_ser_memo<S: serde::Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
    let inst = to_hex(bytes);
    s.serialize_str(inst.as_str())
}

fn hex_deser_memo<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    let inst_str = String::deserialize(d)?;
    hex_bytes(&inst_str).map_err(serde::de::Error::custom)
}

fn hex_serialize<S: serde::Serializer>(bhh: &BurnchainHeaderHash, s: S) -> Result<S::Ok, S::Error> {
    let inst = bhh.to_hex();
    s.serialize_str(inst.as_str())
}

fn hex_deserialize<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BurnchainHeaderHash, D::Error> {
    let inst_str = String::deserialize(d)?;
    BurnchainHeaderHash::from_hex(&inst_str).map_err(serde::de::Error::custom)
}

fn principal_serialize<S: serde::Serializer>(pd: &PrincipalData, s: S) -> Result<S::Ok, S::Error> {
    let inst = pd.to_string();
    s.serialize_str(inst.as_str())
}

fn principal_deserialize<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<PrincipalData, D::Error> {
    let inst_str = String::deserialize(d)?;
    PrincipalData::parse(&inst_str).map_err(serde::de::Error::custom)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockstackOperationType {
    LeaderKeyRegister(LeaderKeyRegisterOp),
    LeaderBlockCommit(LeaderBlockCommitOp),
    PreStx(PreStxOp),
    StackStx(StackStxOp),
    TransferStx(TransferStxOp),
    DelegateStx(DelegateStxOp),
    VoteForAggregateKey(VoteForAggregateKeyOp),
}

// serialization helpers for blockstack_op_to_json function
pub fn memo_serialize(memo: &[u8]) -> String {
    let hex_inst = to_hex(memo);
    format!("0x{}", hex_inst)
}

pub fn stacks_addr_serialize(addr: &StacksAddress) -> serde_json::Value {
    let addr_str = addr.to_string();
    json!({
        "address": addr_str,
        "address_hash_bytes": format!("0x{}", addr.bytes),
        "address_version": addr.version
    })
}

impl BlockstackOperationType {
    pub fn opcode(&self) -> Opcodes {
        match *self {
            BlockstackOperationType::LeaderKeyRegister(_) => Opcodes::LeaderKeyRegister,
            BlockstackOperationType::LeaderBlockCommit(_) => Opcodes::LeaderBlockCommit,
            BlockstackOperationType::StackStx(_) => Opcodes::StackStx,
            BlockstackOperationType::PreStx(_) => Opcodes::PreStx,
            BlockstackOperationType::TransferStx(_) => Opcodes::TransferStx,
            BlockstackOperationType::DelegateStx(_) => Opcodes::DelegateStx,
            BlockstackOperationType::VoteForAggregateKey(_) => Opcodes::VoteForAggregateKey,
        }
    }

    pub fn txid(&self) -> Txid {
        self.txid_ref().clone()
    }

    pub fn txid_ref(&self) -> &Txid {
        match *self {
            BlockstackOperationType::LeaderKeyRegister(ref data) => &data.txid,
            BlockstackOperationType::LeaderBlockCommit(ref data) => &data.txid,
            BlockstackOperationType::StackStx(ref data) => &data.txid,
            BlockstackOperationType::PreStx(ref data) => &data.txid,
            BlockstackOperationType::TransferStx(ref data) => &data.txid,
            BlockstackOperationType::DelegateStx(ref data) => &data.txid,
            BlockstackOperationType::VoteForAggregateKey(ref data) => &data.txid,
        }
    }

    pub fn vtxindex(&self) -> u32 {
        match *self {
            BlockstackOperationType::LeaderKeyRegister(ref data) => data.vtxindex,
            BlockstackOperationType::LeaderBlockCommit(ref data) => data.vtxindex,
            BlockstackOperationType::StackStx(ref data) => data.vtxindex,
            BlockstackOperationType::PreStx(ref data) => data.vtxindex,
            BlockstackOperationType::TransferStx(ref data) => data.vtxindex,
            BlockstackOperationType::DelegateStx(ref data) => data.vtxindex,
            BlockstackOperationType::VoteForAggregateKey(ref data) => data.vtxindex,
        }
    }

    pub fn block_height(&self) -> u64 {
        match *self {
            BlockstackOperationType::LeaderKeyRegister(ref data) => data.block_height,
            BlockstackOperationType::LeaderBlockCommit(ref data) => data.block_height,
            BlockstackOperationType::StackStx(ref data) => data.block_height,
            BlockstackOperationType::PreStx(ref data) => data.block_height,
            BlockstackOperationType::TransferStx(ref data) => data.block_height,
            BlockstackOperationType::DelegateStx(ref data) => data.block_height,
            BlockstackOperationType::VoteForAggregateKey(ref data) => data.block_height,
        }
    }

    pub fn burn_header_hash(&self) -> BurnchainHeaderHash {
        match *self {
            BlockstackOperationType::LeaderKeyRegister(ref data) => data.burn_header_hash.clone(),
            BlockstackOperationType::LeaderBlockCommit(ref data) => data.burn_header_hash.clone(),
            BlockstackOperationType::StackStx(ref data) => data.burn_header_hash.clone(),
            BlockstackOperationType::PreStx(ref data) => data.burn_header_hash.clone(),
            BlockstackOperationType::TransferStx(ref data) => data.burn_header_hash.clone(),
            BlockstackOperationType::DelegateStx(ref data) => data.burn_header_hash.clone(),
            BlockstackOperationType::VoteForAggregateKey(ref data) => data.burn_header_hash.clone(),
        }
    }

    #[cfg(test)]
    pub fn set_block_height(&mut self, height: u64) {
        match self {
            BlockstackOperationType::LeaderKeyRegister(ref mut data) => data.block_height = height,
            BlockstackOperationType::LeaderBlockCommit(ref mut data) => {
                data.set_burn_height(height)
            }
            BlockstackOperationType::StackStx(ref mut data) => data.block_height = height,
            BlockstackOperationType::PreStx(ref mut data) => data.block_height = height,
            BlockstackOperationType::TransferStx(ref mut data) => data.block_height = height,
            BlockstackOperationType::DelegateStx(ref mut data) => data.block_height = height,
            BlockstackOperationType::VoteForAggregateKey(ref mut data) => {
                data.block_height = height
            }
        };
    }

    #[cfg(test)]
    pub fn set_burn_header_hash(&mut self, hash: BurnchainHeaderHash) {
        match self {
            BlockstackOperationType::LeaderKeyRegister(ref mut data) => {
                data.burn_header_hash = hash
            }
            BlockstackOperationType::LeaderBlockCommit(ref mut data) => {
                data.burn_header_hash = hash
            }
            BlockstackOperationType::StackStx(ref mut data) => data.burn_header_hash = hash,
            BlockstackOperationType::PreStx(ref mut data) => data.burn_header_hash = hash,
            BlockstackOperationType::TransferStx(ref mut data) => data.burn_header_hash = hash,
            BlockstackOperationType::DelegateStx(ref mut data) => data.burn_header_hash = hash,
            BlockstackOperationType::VoteForAggregateKey(ref mut data) => {
                data.burn_header_hash = hash
            }
        };
    }

    pub fn pre_stx_to_json(op: &PreStxOp) -> serde_json::Value {
        json!({
            "pre_stx": {
                "burn_block_height": op.block_height,
                "burn_header_hash": &op.burn_header_hash.to_hex(),
                "output": stacks_addr_serialize(&op.output),
                "burn_txid": op.txid,
                "vtxindex": op.vtxindex,
            }
        })
    }

    pub fn stack_stx_to_json(op: &StackStxOp) -> serde_json::Value {
        json!({
            "stack_stx": {
                "burn_block_height": op.block_height,
                "burn_header_hash": &op.burn_header_hash.to_hex(),
                "num_cycles": op.num_cycles,
                "reward_addr": op.reward_addr.clone().to_b58(),
                "sender": stacks_addr_serialize(&op.sender),
                "stacked_ustx": op.stacked_ustx,
                "burn_txid": op.txid,
                "vtxindex": op.vtxindex,
                "signer_key": op.signer_key.as_ref().map(|k| serde_json::Value::String(k.to_hex())).unwrap_or(serde_json::Value::Null),
                "max_amount": op.max_amount.map_or(serde_json::Value::Null, |amount| serde_json::Value::Number(serde_json::Number::from(amount))),
                "auth_id": op.auth_id.map_or(serde_json::Value::Null, |id| serde_json::Value::Number(serde_json::Number::from(id))),
            }
        })
    }

    pub fn transfer_stx_to_json(op: &TransferStxOp) -> serde_json::Value {
        json!({
            "transfer_stx": {
                "burn_block_height": op.block_height,
                "burn_header_hash": &op.burn_header_hash.to_hex(),
                "memo": memo_serialize(&op.memo),
                "recipient": stacks_addr_serialize(&op.recipient),
                "sender": stacks_addr_serialize(&op.sender),
                "transfered_ustx": op.transfered_ustx,
                "burn_txid": op.txid,
                "vtxindex": op.vtxindex,
            }
        })
    }

    pub fn delegate_stx_to_json(op: &DelegateStxOp) -> serde_json::Value {
        json!({
            "delegate_stx": {
                "burn_block_height": op.block_height,
                "burn_header_hash": &op.burn_header_hash.to_hex(),
                "delegate_to": stacks_addr_serialize(&op.delegate_to),
                "delegated_ustx": op.delegated_ustx,
                "sender": stacks_addr_serialize(&op.sender),
                "reward_addr": &op.reward_addr.as_ref().map(|(index, addr)| (index, addr.clone().to_b58())),
                "burn_txid": op.txid,
                "until_burn_height": op.until_burn_height,
                "vtxindex": op.vtxindex,
            }

        })
    }

    pub fn vote_for_aggregate_key_to_json(op: &VoteForAggregateKeyOp) -> serde_json::Value {
        json!({
            "vote_for_aggregate_key": {
                "burn_block_height": op.block_height,
                "burn_header_hash": &op.burn_header_hash.to_hex(),
                "aggregate_key": op.aggregate_key.to_hex(),
                "reward_cycle": op.reward_cycle,
                "round": op.round,
                "sender": stacks_addr_serialize(&op.sender),
                "signer_index": op.signer_index,
                "signer_key": op.signer_key.to_hex(),
                "burn_txid": op.txid,
                "vtxindex": op.vtxindex,
            }
        })
    }

    // An explicit JSON serialization function is used (instead of using the default serialization
    // function) for the Blockstack ops. This is because (a) we wanted the serialization to be
    // more readable, and (b) the serialization used to display PoxAddress as a string is lossy,
    // so we wouldn't want to use this serialization by default (because there will be issues with
    // deserialization).
    pub fn blockstack_op_to_json(&self) -> serde_json::Value {
        match self {
            BlockstackOperationType::PreStx(op) => Self::pre_stx_to_json(op),
            BlockstackOperationType::StackStx(op) => Self::stack_stx_to_json(op),
            BlockstackOperationType::TransferStx(op) => Self::transfer_stx_to_json(op),
            BlockstackOperationType::DelegateStx(op) => Self::delegate_stx_to_json(op),
            BlockstackOperationType::VoteForAggregateKey(op) => {
                Self::vote_for_aggregate_key_to_json(op)
            }
            // json serialization for the remaining op types is not implemented for now. This function
            // is currently only used to json-ify burnchain ops executed as Stacks transactions (so,
            // stack_stx, transfer_stx, delegate_stx, and vote_for_aggregate_key).
            _ => json!(null),
        }
    }
}

impl fmt::Display for BlockstackOperationType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            BlockstackOperationType::LeaderKeyRegister(ref op) => write!(f, "{:?}", op),
            BlockstackOperationType::PreStx(ref op) => write!(f, "{:?}", op),
            BlockstackOperationType::StackStx(ref op) => write!(f, "{:?}", op),
            BlockstackOperationType::LeaderBlockCommit(ref op) => write!(f, "{:?}", op),
            BlockstackOperationType::TransferStx(ref op) => write!(f, "{:?}", op),
            BlockstackOperationType::DelegateStx(ref op) => write!(f, "{:?}", op),
            BlockstackOperationType::VoteForAggregateKey(ref op) => write!(f, "{:?}", op),
        }
    }
}

// parser helpers
pub fn parse_u128_from_be(bytes: &[u8]) -> Option<u128> {
    bytes.try_into().ok().map(u128::from_be_bytes)
}

pub fn parse_u64_from_be(bytes: &[u8]) -> Option<u64> {
    bytes.try_into().ok().map(u64::from_be_bytes)
}

pub fn parse_u32_from_be(bytes: &[u8]) -> Option<u32> {
    bytes.try_into().ok().map(u32::from_be_bytes)
}

pub fn parse_u16_from_be(bytes: &[u8]) -> Option<u16> {
    bytes.try_into().ok().map(u16::from_be_bytes)
}
