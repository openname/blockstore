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

use std::io::prelude::*;
use std::io::{Read, Write};
use std::ops::{Deref, DerefMut};
use std::{error, fmt, io};

use clarity::vm::contexts::GlobalContext;
use clarity::vm::costs::{CostErrors, ExecutionCost};
use clarity::vm::errors::Error as clarity_interpreter_error;
use clarity::vm::representations::{ClarityName, ContractName};
use clarity::vm::types::{
    PrincipalData, QualifiedContractIdentifier, StandardPrincipalData, Value,
};
use clarity::vm::ClarityVersion;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::Error as RusqliteError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha512_256};
use stacks_common::address::AddressHashMode;
use stacks_common::codec::{
    read_next, write_next, Error as codec_error, StacksMessageCodec, MAX_MESSAGE_LEN,
};
use stacks_common::deps_common::bitcoin::util::hash::Sha256dHash;
use stacks_common::types::chainstate::{
    BlockHeaderHash, BurnchainHeaderHash, StacksAddress, StacksBlockId, StacksWorkScore, TrieHash,
    TRIEHASH_ENCODED_SIZE,
};
use stacks_common::util::hash::{
    hex_bytes, to_hex, Hash160, Sha512Trunc256Sum, HASH160_ENCODED_SIZE,
};
use stacks_common::util::secp256k1;
use stacks_common::util::secp256k1::MessageSignature;
use stacks_common::util::vrf::VRFProof;

use crate::burnchains::Txid;
use crate::chainstate::burn::operations::LeaderBlockCommitOp;
use crate::chainstate::burn::ConsensusHash;
use crate::chainstate::stacks::db::accounts::MinerReward;
use crate::chainstate::stacks::db::blocks::MemPoolRejection;
use crate::chainstate::stacks::db::{MinerRewardInfo, StacksHeaderInfo};
use crate::chainstate::stacks::index::Error as marf_error;
use crate::clarity_vm::clarity::Error as clarity_error;
use crate::net::Error as net_error;
use crate::util_lib::db::{DBConn, Error as db_error};
use crate::util_lib::strings::StacksString;

pub mod address;
pub mod auth;
pub mod block;
pub mod boot;
pub mod db;
pub mod events;
pub mod index;
pub mod miner;
pub mod transaction;

#[cfg(test)]
pub mod tests;

pub use stacks_common::address::{
    C32_ADDRESS_VERSION_MAINNET_MULTISIG, C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
    C32_ADDRESS_VERSION_TESTNET_MULTISIG, C32_ADDRESS_VERSION_TESTNET_SINGLESIG,
};
pub use stacks_common::types::chainstate::{StacksPrivateKey, StacksPublicKey};

pub const STACKS_BLOCK_VERSION: u8 = 7;
pub const STACKS_BLOCK_VERSION_AST_PRECHECK_SIZE: u8 = 1;

pub const MAX_BLOCK_LEN: u32 = 2 * 1024 * 1024;
pub const MAX_TRANSACTION_LEN: u32 = MAX_BLOCK_LEN;

#[derive(Debug)]
pub enum Error {
    InvalidFee,
    InvalidStacksBlock(String),
    ExpectedTenureChange,
    InvalidStacksMicroblock(String, BlockHeaderHash),
    // The bool is true if the invalid transaction was quietly ignored.
    InvalidStacksTransaction(String, bool),
    /// This error indicates that the considered transaction was skipped
    /// because of the current state of the block assembly algorithm,
    /// but the transaction otherwise may be valid (e.g., block assembly is
    /// only considering STX transfers and this tx isn't a transfer).
    StacksTransactionSkipped(String),
    PostConditionFailed(String),
    NoSuchBlockError,
    /// The supplied Sortition IDs, consensus hashes, or stacks blocks are not in the same fork.
    NotInSameFork,
    InvalidChainstateDB,
    BlockTooBigError,
    TransactionTooBigError(Option<ExecutionCost>),
    BlockCostExceeded,
    NoTransactionsToMine,
    MicroblockStreamTooLongError,
    IncompatibleSpendingConditionError,
    CostOverflowError(ExecutionCost, ExecutionCost, ExecutionCost),
    ClarityError(clarity_error),
    DBError(db_error),
    NetError(net_error),
    CodecError(codec_error),
    MARFError(marf_error),
    ReadError(io::Error),
    WriteError(io::Error),
    MemPoolError(String),
    PoxAlreadyLocked,
    PoxInsufficientBalance,
    PoxNoRewardCycle,
    PoxExtendNotLocked,
    PoxIncreaseOnV1,
    PoxInvalidIncrease,
    DefunctPoxContract,
    ProblematicTransaction(Txid),
    MinerAborted,
    ChannelClosed(String),
    /// This error indicates a Epoch2 block attempted to build off of a Nakamoto block.
    InvalidChildOfNakomotoBlock,
    NoRegisteredSigners(u64),
}

impl From<marf_error> for Error {
    fn from(e: marf_error) -> Error {
        Error::MARFError(e)
    }
}

impl From<clarity_error> for Error {
    fn from(e: clarity_error) -> Error {
        Error::ClarityError(e)
    }
}

impl From<net_error> for Error {
    fn from(e: net_error) -> Error {
        Error::NetError(e)
    }
}

impl From<codec_error> for Error {
    fn from(e: codec_error) -> Error {
        Error::CodecError(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::InvalidFee => write!(f, "Invalid fee"),
            Error::InvalidStacksBlock(ref s) => fmt::Display::fmt(s, f),
            Error::InvalidStacksMicroblock(ref s, _) => fmt::Display::fmt(s, f),
            Error::InvalidStacksTransaction(ref s, _) => fmt::Display::fmt(s, f),
            Error::PostConditionFailed(ref s) => fmt::Display::fmt(s, f),
            Error::NoSuchBlockError => write!(f, "No such Stacks block"),
            Error::InvalidChainstateDB => write!(f, "Invalid chainstate database"),
            Error::BlockTooBigError => write!(f, "Too much data in block"),
            Error::TransactionTooBigError(ref c) => {
                write!(f, "Too much data in transaction: measured_cost={c:?}")
            }
            Error::BlockCostExceeded => write!(f, "Block execution budget exceeded"),
            Error::MicroblockStreamTooLongError => write!(f, "Too many microblocks in stream"),
            Error::IncompatibleSpendingConditionError => {
                write!(f, "Spending condition is incompatible with this operation")
            }
            Error::CostOverflowError(ref c1, ref c2, ref c3) => write!(
                f,
                "{}",
                &format!(
                    "Cost overflow: before={:?}, after={:?}, budget={:?}",
                    c1, c2, c3
                )
            ),
            Error::ClarityError(ref e) => fmt::Display::fmt(e, f),
            Error::DBError(ref e) => fmt::Display::fmt(e, f),
            Error::NetError(ref e) => fmt::Display::fmt(e, f),
            Error::CodecError(ref e) => fmt::Display::fmt(e, f),
            Error::MARFError(ref e) => fmt::Display::fmt(e, f),
            Error::ReadError(ref e) => fmt::Display::fmt(e, f),
            Error::WriteError(ref e) => fmt::Display::fmt(e, f),
            Error::MemPoolError(ref s) => fmt::Display::fmt(s, f),
            Error::NoTransactionsToMine => write!(f, "No transactions to mine"),
            Error::PoxAlreadyLocked => write!(f, "Account has already locked STX for PoX"),
            Error::PoxExtendNotLocked => {
                write!(f, "Account has not already locked STX for PoX extend")
            }
            Error::PoxInsufficientBalance => write!(f, "Not enough STX to lock"),
            Error::PoxNoRewardCycle => write!(f, "No such reward cycle"),
            Error::StacksTransactionSkipped(ref r) => {
                write!(
                    f,
                    "Stacks transaction skipped during assembly due to: {}",
                    r
                )
            }
            Error::DefunctPoxContract => {
                write!(f, "A defunct PoX contract was called after transition")
            }
            Error::ProblematicTransaction(ref txid) => write!(
                f,
                "Transaction {} is problematic and will not be mined again",
                txid
            ),
            Error::PoxIncreaseOnV1 => write!(f, "PoX increase only allowed for pox-2 locks"),
            Error::PoxInvalidIncrease => write!(f, "PoX increase was invalid"),
            Error::MinerAborted => write!(f, "Mining attempt aborted by signal"),
            Error::ChannelClosed(ref s) => write!(f, "Channel '{}' closed", s),
            Error::InvalidChildOfNakomotoBlock => write!(
                f,
                "Stacks Epoch 2-style block building off of Nakamoto block"
            ),
            Error::ExpectedTenureChange => write!(
                f,
                "Block has a different tenure than parent, but no tenure change transaction"
            ),
            Error::NoRegisteredSigners(reward_cycle) => {
                write!(f, "No registered signers for reward cycle {reward_cycle}")
            }
            Error::NotInSameFork => {
                write!(f, "The supplied block identifiers are not in the same fork")
            }
        }
    }
}

impl error::Error for Error {
    #[cfg_attr(test, mutants::skip)]
    fn cause(&self) -> Option<&dyn error::Error> {
        match *self {
            Error::InvalidFee => None,
            Error::InvalidStacksBlock(ref _s) => None,
            Error::InvalidStacksMicroblock(ref _s, ref _h) => None,
            Error::InvalidStacksTransaction(ref _s, _q) => None,
            Error::PostConditionFailed(ref _s) => None,
            Error::NoSuchBlockError => None,
            Error::InvalidChainstateDB => None,
            Error::BlockTooBigError => None,
            Error::TransactionTooBigError(..) => None,
            Error::BlockCostExceeded => None,
            Error::MicroblockStreamTooLongError => None,
            Error::IncompatibleSpendingConditionError => None,
            Error::CostOverflowError(..) => None,
            Error::ClarityError(ref e) => Some(e),
            Error::DBError(ref e) => Some(e),
            Error::NetError(ref e) => Some(e),
            Error::CodecError(ref e) => Some(e),
            Error::MARFError(ref e) => Some(e),
            Error::ReadError(ref e) => Some(e),
            Error::WriteError(ref e) => Some(e),
            Error::MemPoolError(ref _s) => None,
            Error::NoTransactionsToMine => None,
            Error::PoxAlreadyLocked => None,
            Error::PoxInsufficientBalance => None,
            Error::PoxNoRewardCycle => None,
            Error::PoxExtendNotLocked => None,
            Error::DefunctPoxContract => None,
            Error::StacksTransactionSkipped(ref _r) => None,
            Error::ProblematicTransaction(ref _txid) => None,
            Error::PoxIncreaseOnV1 => None,
            Error::PoxInvalidIncrease => None,
            Error::MinerAborted => None,
            Error::ChannelClosed(ref _s) => None,
            Error::InvalidChildOfNakomotoBlock => None,
            Error::ExpectedTenureChange => None,
            Error::NoRegisteredSigners(_) => None,
            Error::NotInSameFork => None,
        }
    }
}

impl Error {
    #[cfg_attr(test, mutants::skip)]
    fn name(&self) -> &'static str {
        match self {
            Error::InvalidFee => "InvalidFee",
            Error::InvalidStacksBlock(ref _s) => "InvalidStacksBlock",
            Error::InvalidStacksMicroblock(ref _s, ref _h) => "InvalidStacksMicroblock",
            Error::InvalidStacksTransaction(ref _s, _q) => "InvalidStacksTransaction",
            Error::PostConditionFailed(ref _s) => "PostConditionFailed",
            Error::NoSuchBlockError => "NoSuchBlockError",
            Error::InvalidChainstateDB => "InvalidChainstateDB",
            Error::BlockTooBigError => "BlockTooBigError",
            Error::TransactionTooBigError(..) => "TransactionTooBigError",
            Error::BlockCostExceeded => "BlockCostExceeded",
            Error::MicroblockStreamTooLongError => "MicroblockStreamTooLongError",
            Error::IncompatibleSpendingConditionError => "IncompatibleSpendingConditionError",
            Error::CostOverflowError(..) => "CostOverflowError",
            Error::ClarityError(ref _e) => "ClarityError",
            Error::DBError(ref _e) => "DBError",
            Error::NetError(ref _e) => "NetError",
            Error::CodecError(ref _e) => "CodecError",
            Error::MARFError(ref _e) => "MARFError",
            Error::ReadError(ref _e) => "ReadError",
            Error::WriteError(ref _e) => "WriteError",
            Error::MemPoolError(ref _s) => "MemPoolError",
            Error::NoTransactionsToMine => "NoTransactionsToMine",
            Error::PoxAlreadyLocked => "PoxAlreadyLocked",
            Error::PoxInsufficientBalance => "PoxInsufficientBalance",
            Error::PoxNoRewardCycle => "PoxNoRewardCycle",
            Error::PoxExtendNotLocked => "PoxExtendNotLocked",
            Error::DefunctPoxContract => "DefunctPoxContract",
            Error::StacksTransactionSkipped(ref _r) => "StacksTransactionSkipped",
            Error::ProblematicTransaction(ref _txid) => "ProblematicTransaction",
            Error::PoxIncreaseOnV1 => "PoxIncreaseOnV1",
            Error::PoxInvalidIncrease => "PoxInvalidIncrease",
            Error::MinerAborted => "MinerAborted",
            Error::ChannelClosed(ref _s) => "ChannelClosed",
            Error::InvalidChildOfNakomotoBlock => "InvalidChildOfNakomotoBlock",
            Error::ExpectedTenureChange => "ExpectedTenureChange",
            Error::NoRegisteredSigners(_) => "NoRegisteredSigners",
            Error::NotInSameFork => "NotInSameFork",
        }
    }

    #[cfg_attr(test, mutants::skip)]
    pub fn into_json(&self) -> serde_json::Value {
        let reason_code = self.name();
        let reason_data = format!("{:?}", &self);
        let result = json!({
            "error": "chainstate error",
            "reason": reason_code,
            "reason_data": reason_data
        });
        result
    }
}

impl From<RusqliteError> for Error {
    fn from(e: RusqliteError) -> Error {
        Error::DBError(db_error::SqliteError(e))
    }
}

impl From<db_error> for Error {
    fn from(e: db_error) -> Error {
        Error::DBError(e)
    }
}

impl From<clarity_interpreter_error> for Error {
    fn from(e: clarity_interpreter_error) -> Error {
        Error::ClarityError(clarity_error::Interpreter(e))
    }
}

impl Error {
    pub fn from_cost_error(
        err: CostErrors,
        cost_before: ExecutionCost,
        context: &GlobalContext,
    ) -> Error {
        match err {
            CostErrors::CostBalanceExceeded(used, budget) => {
                Error::CostOverflowError(cost_before, used, budget)
            }
            _ => {
                let cur_cost = context.cost_track.get_total();
                let budget = context.cost_track.get_limit();
                Error::CostOverflowError(cost_before, cur_cost, budget)
            }
        }
    }
}

impl Txid {
    /// A Stacks transaction ID is a sha512/256 hash (not a double-sha256 hash)
    pub fn from_stacks_tx(txdata: &[u8]) -> Txid {
        let h = Sha512Trunc256Sum::from_data(txdata);
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(h.as_bytes());
        Txid(bytes)
    }

    /// A sighash is calculated the same way as a txid
    pub fn from_sighash_bytes(txdata: &[u8]) -> Txid {
        Txid::from_stacks_tx(txdata)
    }

    /// Create a Txid from the tx hash bytes used in bitcoin.
    /// This just reverses the inner bytes of the input.
    pub fn from_bitcoin_tx_hash(tx_hash: &Sha256dHash) -> Txid {
        let mut txid_bytes = tx_hash.0.clone();
        txid_bytes.reverse();
        Self(txid_bytes)
    }
}

/// How a transaction may be appended to the Stacks blockchain
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Copy, Serialize, Deserialize)]
pub enum TransactionAnchorMode {
    OnChainOnly = 1,  // must be included in a StacksBlock
    OffChainOnly = 2, // must be included in a StacksMicroBlock
    Any = 3,          // either
}

#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Copy, Serialize, Deserialize)]
pub enum TransactionAuthFlags {
    // types of auth
    AuthStandard = 0x04,
    AuthSponsored = 0x05,
}

/// Transaction signatures are validated by calculating the public key from the signature, and
/// verifying that all public keys hash to the signing account's hash.  To do so, we must preserve
/// enough information in the auth structure to recover each public key's bytes.
///
/// An auth field can be a public key or a signature.  In both cases, the public key (either given
/// in-the-raw or embedded in a signature) may be encoded as compressed or uncompressed.
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Copy, Serialize, Deserialize)]
pub enum TransactionAuthFieldID {
    // types of auth fields
    PublicKeyCompressed = 0x00,
    PublicKeyUncompressed = 0x01,
    SignatureCompressed = 0x02,
    SignatureUncompressed = 0x03,
}

#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Copy, Serialize, Deserialize)]
pub enum TransactionPublicKeyEncoding {
    // ways we can encode a public key
    Compressed = 0x00,
    Uncompressed = 0x01,
}

impl TransactionPublicKeyEncoding {
    pub fn from_u8(n: u8) -> Option<TransactionPublicKeyEncoding> {
        match n {
            x if x == TransactionPublicKeyEncoding::Compressed as u8 => {
                Some(TransactionPublicKeyEncoding::Compressed)
            }
            x if x == TransactionPublicKeyEncoding::Uncompressed as u8 => {
                Some(TransactionPublicKeyEncoding::Uncompressed)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransactionAuthField {
    PublicKey(StacksPublicKey),
    Signature(TransactionPublicKeyEncoding, MessageSignature),
}

impl TransactionAuthField {
    pub fn is_public_key(&self) -> bool {
        match *self {
            TransactionAuthField::PublicKey(_) => true,
            _ => false,
        }
    }

    pub fn is_signature(&self) -> bool {
        match *self {
            TransactionAuthField::Signature(_, _) => true,
            _ => false,
        }
    }

    pub fn as_public_key(&self) -> Option<StacksPublicKey> {
        match *self {
            TransactionAuthField::PublicKey(ref pubk) => Some(pubk.clone()),
            _ => None,
        }
    }

    pub fn as_signature(&self) -> Option<(TransactionPublicKeyEncoding, MessageSignature)> {
        match *self {
            TransactionAuthField::Signature(ref key_fmt, ref sig) => {
                Some((key_fmt.clone(), sig.clone()))
            }
            _ => None,
        }
    }

    // TODO: enforce u8; 32
    pub fn get_public_key(&self, sighash_bytes: &[u8]) -> Result<StacksPublicKey, net_error> {
        match *self {
            TransactionAuthField::PublicKey(ref pubk) => Ok(pubk.clone()),
            TransactionAuthField::Signature(ref key_fmt, ref sig) => {
                let mut pubk = StacksPublicKey::recover_to_pubkey(sighash_bytes, sig)
                    .map_err(|e| net_error::VerifyingError(e.to_string()))?;
                pubk.set_compressed(if *key_fmt == TransactionPublicKeyEncoding::Compressed {
                    true
                } else {
                    false
                });
                Ok(pubk)
            }
        }
    }
}

// tag address hash modes as "singlesig" or "multisig" so we can't accidentally construct an
// invalid spending condition
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SinglesigHashMode {
    P2PKH = 0x00,
    P2WPKH = 0x02,
}

#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MultisigHashMode {
    P2SH = 0x01,
    P2WSH = 0x03,
}

#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OrderIndependentMultisigHashMode {
    P2SH = 0x05,
    P2WSH = 0x07,
}

impl SinglesigHashMode {
    pub fn to_address_hash_mode(&self) -> AddressHashMode {
        match *self {
            SinglesigHashMode::P2PKH => AddressHashMode::SerializeP2PKH,
            SinglesigHashMode::P2WPKH => AddressHashMode::SerializeP2WPKH,
        }
    }

    pub fn from_address_hash_mode(hm: AddressHashMode) -> Option<SinglesigHashMode> {
        match hm {
            AddressHashMode::SerializeP2PKH => Some(SinglesigHashMode::P2PKH),
            AddressHashMode::SerializeP2WPKH => Some(SinglesigHashMode::P2WPKH),
            _ => None,
        }
    }

    pub fn from_u8(n: u8) -> Option<SinglesigHashMode> {
        match n {
            x if x == SinglesigHashMode::P2PKH as u8 => Some(SinglesigHashMode::P2PKH),
            x if x == SinglesigHashMode::P2WPKH as u8 => Some(SinglesigHashMode::P2WPKH),
            _ => None,
        }
    }
}

impl MultisigHashMode {
    pub fn to_address_hash_mode(&self) -> AddressHashMode {
        match *self {
            MultisigHashMode::P2SH => AddressHashMode::SerializeP2SH,
            MultisigHashMode::P2WSH => AddressHashMode::SerializeP2WSH,
        }
    }

    pub fn from_address_hash_mode(hm: AddressHashMode) -> Option<MultisigHashMode> {
        match hm {
            AddressHashMode::SerializeP2SH => Some(MultisigHashMode::P2SH),
            AddressHashMode::SerializeP2WSH => Some(MultisigHashMode::P2WSH),
            _ => None,
        }
    }

    pub fn from_u8(n: u8) -> Option<MultisigHashMode> {
        match n {
            x if x == MultisigHashMode::P2SH as u8 => Some(MultisigHashMode::P2SH),
            x if x == MultisigHashMode::P2WSH as u8 => Some(MultisigHashMode::P2WSH),
            _ => None,
        }
    }
}

impl OrderIndependentMultisigHashMode {
    pub fn to_address_hash_mode(&self) -> AddressHashMode {
        match *self {
            OrderIndependentMultisigHashMode::P2SH => AddressHashMode::SerializeP2SH,
            OrderIndependentMultisigHashMode::P2WSH => AddressHashMode::SerializeP2WSH,
        }
    }

    pub fn from_address_hash_mode(hm: AddressHashMode) -> Option<OrderIndependentMultisigHashMode> {
        match hm {
            AddressHashMode::SerializeP2SH => Some(OrderIndependentMultisigHashMode::P2SH),
            AddressHashMode::SerializeP2WSH => Some(OrderIndependentMultisigHashMode::P2WSH),
            _ => None,
        }
    }

    pub fn from_u8(n: u8) -> Option<OrderIndependentMultisigHashMode> {
        match n {
            x if x == OrderIndependentMultisigHashMode::P2SH as u8 => {
                Some(OrderIndependentMultisigHashMode::P2SH)
            }
            x if x == OrderIndependentMultisigHashMode::P2WSH as u8 => {
                Some(OrderIndependentMultisigHashMode::P2WSH)
            }
            _ => None,
        }
    }
}

/// A structure that encodes enough state to authenticate
/// a transaction's execution against a Stacks address.
/// public_keys + signatures_required determines the Principal.
/// nonce is the "check number" for the Principal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultisigSpendingCondition {
    pub hash_mode: MultisigHashMode,
    pub signer: Hash160,
    pub nonce: u64,  // nth authorization from this account
    pub tx_fee: u64, // microSTX/compute rate offered by this account
    pub fields: Vec<TransactionAuthField>,
    pub signatures_required: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SinglesigSpendingCondition {
    pub hash_mode: SinglesigHashMode,
    pub signer: Hash160,
    pub nonce: u64,  // nth authorization from this account
    pub tx_fee: u64, // microSTX/compute rate offerred by this account
    pub key_encoding: TransactionPublicKeyEncoding,
    pub signature: MessageSignature,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderIndependentMultisigSpendingCondition {
    pub hash_mode: OrderIndependentMultisigHashMode,
    pub signer: Hash160,
    pub nonce: u64,  // nth authorization from this account
    pub tx_fee: u64, // microSTX/compute rate offered by this account
    pub fields: Vec<TransactionAuthField>,
    pub signatures_required: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransactionSpendingCondition {
    Singlesig(SinglesigSpendingCondition),
    Multisig(MultisigSpendingCondition),
    OrderIndependentMultisig(OrderIndependentMultisigSpendingCondition),
}

/// Types of transaction authorizations
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransactionAuth {
    Standard(TransactionSpendingCondition),
    Sponsored(TransactionSpendingCondition, TransactionSpendingCondition), // the second account pays on behalf of the first account
}

/// A transaction that calls into a smart contract
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransactionContractCall {
    pub address: StacksAddress,
    pub contract_name: ContractName,
    pub function_name: ClarityName,
    pub function_args: Vec<Value>,
}

impl TransactionContractCall {
    pub fn contract_identifier(&self) -> QualifiedContractIdentifier {
        let standard_principal =
            StandardPrincipalData(self.address.version, self.address.bytes.0.clone());
        QualifiedContractIdentifier::new(standard_principal, self.contract_name.clone())
    }
}

/// A transaction that instantiates a smart contract
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransactionSmartContract {
    pub name: ContractName,
    pub code_body: StacksString,
}

/// A coinbase commits to 32 bytes of control-plane information
pub struct CoinbasePayload(pub [u8; 32]);
impl_byte_array_message_codec!(CoinbasePayload, 32);
impl_array_newtype!(CoinbasePayload, u8, 32);
impl_array_hexstring_fmt!(CoinbasePayload);
impl_byte_array_newtype!(CoinbasePayload, u8, 32);
impl_byte_array_serde!(CoinbasePayload);

pub struct TokenTransferMemo(pub [u8; 34]); // same length as it is in stacks v1
impl_byte_array_message_codec!(TokenTransferMemo, 34);
impl_array_newtype!(TokenTransferMemo, u8, 34);
impl_array_hexstring_fmt!(TokenTransferMemo);
impl_byte_array_newtype!(TokenTransferMemo, u8, 34);
impl_byte_array_serde!(TokenTransferMemo);

/// Cause of change in mining tenure
/// Depending on cause, tenure can be ended or extended
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TenureChangeCause {
    /// A valid winning block-commit
    BlockFound = 0,
    /// The next burnchain block is taking too long, so extend the runtime budget
    Extended = 1,
}

impl TryFrom<u8> for TenureChangeCause {
    type Error = ();

    fn try_from(num: u8) -> Result<Self, Self::Error> {
        match num {
            0 => Ok(Self::BlockFound),
            1 => Ok(Self::Extended),
            _ => Err(()),
        }
    }
}

impl TenureChangeCause {
    /// Does this tenure change cause require a sortition to be valid?
    pub fn expects_sortition(&self) -> bool {
        match self {
            Self::BlockFound => true,
            Self::Extended => false,
        }
    }

    /// Convert to u8 representation
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Reasons why a `TenureChange` transaction can be bad
pub enum TenureChangeError {
    /// Not signed by required threshold (>70%)
    SignatureInvalid,
    /// `previous_tenure_end` does not match parent block
    PreviousTenureInvalid,
    /// Block is not a Nakamoto block
    NotNakamoto,
}

/// A transaction from Stackers to signal new mining tenure
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TenureChangePayload {
    /// Consensus hash of this tenure.  Corresponds to the sortition in which the miner of this
    /// block was chosen.  It may be the case that this miner's tenure gets _extended_ across
    /// subsequent sortitions; if this happens, then this `consensus_hash` value _remains the same_
    /// as the sortition in which the winning block-commit was mined.
    pub tenure_consensus_hash: ConsensusHash,
    /// Consensus hash of the previous tenure.  Corresponds to the sortition of the previous
    /// winning block-commit.
    pub prev_tenure_consensus_hash: ConsensusHash,
    /// Current consensus hash on the underlying burnchain.  Corresponds to the last-seen
    /// sortition.
    pub burn_view_consensus_hash: ConsensusHash,
    /// The StacksBlockId of the last block from the previous tenure
    pub previous_tenure_end: StacksBlockId,
    /// The number of blocks produced since the last sortition-linked tenure
    pub previous_tenure_blocks: u32,
    /// A flag to indicate the cause of this tenure change
    pub cause: TenureChangeCause,
    /// The ECDSA public key hash of the current tenure
    pub pubkey_hash: Hash160,
}

impl TenureChangePayload {
    pub fn extend(
        &self,
        burn_view_consensus_hash: ConsensusHash,
        last_tenure_block_id: StacksBlockId,
        num_blocks_so_far: u32,
    ) -> Self {
        TenureChangePayload {
            tenure_consensus_hash: self.tenure_consensus_hash.clone(),
            prev_tenure_consensus_hash: self.tenure_consensus_hash.clone(),
            burn_view_consensus_hash,
            previous_tenure_end: last_tenure_block_id,
            previous_tenure_blocks: num_blocks_so_far,
            cause: TenureChangeCause::Extended,
            pubkey_hash: self.pubkey_hash.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransactionPayload {
    TokenTransfer(PrincipalData, u64, TokenTransferMemo),
    ContractCall(TransactionContractCall),
    SmartContract(TransactionSmartContract, Option<ClarityVersion>),
    // the previous epoch leader sent two microblocks with the same sequence, and this is proof
    PoisonMicroblock(StacksMicroblockHeader, StacksMicroblockHeader),
    Coinbase(CoinbasePayload, Option<PrincipalData>, Option<VRFProof>),
    TenureChange(TenureChangePayload),
}

impl TransactionPayload {
    pub fn name(&self) -> &'static str {
        match self {
            TransactionPayload::TokenTransfer(..) => "TokenTransfer",
            TransactionPayload::ContractCall(..) => "ContractCall",
            TransactionPayload::SmartContract(_, version_opt) => {
                if version_opt.is_some() {
                    "SmartContract(Versioned)"
                } else {
                    "SmartContract"
                }
            }
            TransactionPayload::PoisonMicroblock(..) => "PoisonMicroblock",
            TransactionPayload::Coinbase(_, _, vrf_opt) => {
                if vrf_opt.is_some() {
                    "Coinbase(Nakamoto)"
                } else {
                    "Coinbase"
                }
            }
            TransactionPayload::TenureChange(payload) => match payload.cause {
                TenureChangeCause::BlockFound => "TenureChange(BlockFound)",
                TenureChangeCause::Extended => "TenureChange(Extension)",
            },
        }
    }
}

define_u8_enum!(TransactionPayloadID {
    TokenTransfer = 0,
    SmartContract = 1,
    ContractCall = 2,
    PoisonMicroblock = 3,
    Coinbase = 4,
    // has an alt principal, but no VRF proof
    CoinbaseToAltRecipient = 5,
    VersionedSmartContract = 6,
    TenureChange = 7,
    // has a VRF proof, and may have an alt principal
    NakamotoCoinbase = 8
});

/// Encoding of an asset type identifier
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetInfo {
    pub contract_address: StacksAddress,
    pub contract_name: ContractName,
    pub asset_name: ClarityName,
}

/// numeric wire-format ID of an asset info type variant
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Copy, Serialize, Deserialize)]
pub enum AssetInfoID {
    STX = 0,
    FungibleAsset = 1,
    NonfungibleAsset = 2,
}

impl AssetInfoID {
    pub fn from_u8(b: u8) -> Option<AssetInfoID> {
        match b {
            0 => Some(AssetInfoID::STX),
            1 => Some(AssetInfoID::FungibleAsset),
            2 => Some(AssetInfoID::NonfungibleAsset),
            _ => None,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Copy, Serialize, Deserialize)]
pub enum FungibleConditionCode {
    SentEq = 0x01,
    SentGt = 0x02,
    SentGe = 0x03,
    SentLt = 0x04,
    SentLe = 0x05,
}

impl FungibleConditionCode {
    pub fn from_u8(b: u8) -> Option<FungibleConditionCode> {
        match b {
            0x01 => Some(FungibleConditionCode::SentEq),
            0x02 => Some(FungibleConditionCode::SentGt),
            0x03 => Some(FungibleConditionCode::SentGe),
            0x04 => Some(FungibleConditionCode::SentLt),
            0x05 => Some(FungibleConditionCode::SentLe),
            _ => None,
        }
    }

    pub fn check(&self, amount_sent_condition: u128, amount_sent: u128) -> bool {
        match *self {
            FungibleConditionCode::SentEq => amount_sent == amount_sent_condition,
            FungibleConditionCode::SentGt => amount_sent > amount_sent_condition,
            FungibleConditionCode::SentGe => amount_sent >= amount_sent_condition,
            FungibleConditionCode::SentLt => amount_sent < amount_sent_condition,
            FungibleConditionCode::SentLe => amount_sent <= amount_sent_condition,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Copy, Serialize, Deserialize)]
pub enum NonfungibleConditionCode {
    Sent = 0x10,
    NotSent = 0x11,
}

impl NonfungibleConditionCode {
    pub fn from_u8(b: u8) -> Option<NonfungibleConditionCode> {
        match b {
            0x10 => Some(NonfungibleConditionCode::Sent),
            0x11 => Some(NonfungibleConditionCode::NotSent),
            _ => None,
        }
    }

    pub fn was_sent(nft_sent_condition: &Value, nfts_sent: &[Value]) -> bool {
        for asset_sent in nfts_sent.iter() {
            if *asset_sent == *nft_sent_condition {
                // asset was sent, and is no longer owned by this principal
                return true;
            }
        }
        return false;
    }

    pub fn check(&self, nft_sent_condition: &Value, nfts_sent: &[Value]) -> bool {
        match *self {
            NonfungibleConditionCode::Sent => {
                NonfungibleConditionCode::was_sent(nft_sent_condition, nfts_sent)
            }
            NonfungibleConditionCode::NotSent => {
                !NonfungibleConditionCode::was_sent(nft_sent_condition, nfts_sent)
            }
        }
    }
}

/// Post-condition principal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PostConditionPrincipal {
    Origin,
    Standard(StacksAddress),
    Contract(StacksAddress, ContractName),
}

impl PostConditionPrincipal {
    pub fn to_principal_data(&self, origin_principal: &PrincipalData) -> PrincipalData {
        match *self {
            PostConditionPrincipal::Origin => origin_principal.clone(),
            PostConditionPrincipal::Standard(ref addr) => {
                PrincipalData::Standard(StandardPrincipalData::from(addr.clone()))
            }
            PostConditionPrincipal::Contract(ref addr, ref contract_name) => {
                PrincipalData::Contract(QualifiedContractIdentifier::new(
                    StandardPrincipalData::from(addr.clone()),
                    contract_name.clone(),
                ))
            }
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Copy, Serialize, Deserialize)]
pub enum PostConditionPrincipalID {
    Origin = 0x01,
    Standard = 0x02,
    Contract = 0x03,
}

/// Post-condition on a transaction
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransactionPostCondition {
    STX(PostConditionPrincipal, FungibleConditionCode, u64),
    Fungible(
        PostConditionPrincipal,
        AssetInfo,
        FungibleConditionCode,
        u64,
    ),
    Nonfungible(
        PostConditionPrincipal,
        AssetInfo,
        Value,
        NonfungibleConditionCode,
    ),
}

/// Post-condition modes for unspecified assets
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Copy, Serialize, Deserialize)]
pub enum TransactionPostConditionMode {
    Allow = 0x01, // allow any other changes not specified
    Deny = 0x02,  // deny any other changes not specified
}

/// Stacks transaction versions
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Copy, Serialize, Deserialize)]
pub enum TransactionVersion {
    Mainnet = 0x00,
    Testnet = 0x80,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StacksTransaction {
    pub version: TransactionVersion,
    pub chain_id: u32,
    pub auth: TransactionAuth,
    pub anchor_mode: TransactionAnchorMode,
    pub post_condition_mode: TransactionPostConditionMode,
    pub post_conditions: Vec<TransactionPostCondition>,
    pub payload: TransactionPayload,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StacksTransactionSigner {
    pub tx: StacksTransaction,
    pub sighash: Txid,
    origin_done: bool,
    check_oversign: bool,
    check_overlap: bool,
}

/// A block that contains blockchain-anchored data
/// (corresponding to a LeaderBlockCommitOp)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StacksBlock {
    pub header: StacksBlockHeader,
    pub txs: Vec<StacksTransaction>,
}

/// A microblock that contains non-blockchain-anchored data,
/// but is tied to an on-chain block
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StacksMicroblock {
    pub header: StacksMicroblockHeader,
    pub txs: Vec<StacksTransaction>,
}

/// The header for an on-chain-anchored Stacks block
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StacksBlockHeader {
    pub version: u8,
    pub total_work: StacksWorkScore, // NOTE: this is the work done on the chain tip this block builds on (i.e. take this from the parent)
    pub proof: VRFProof,
    pub parent_block: BlockHeaderHash, // NOTE: even though this is also present in the burn chain, we need this here for super-light clients that don't even have burn chain headers
    pub parent_microblock: BlockHeaderHash,
    pub parent_microblock_sequence: u16,
    pub tx_merkle_root: Sha512Trunc256Sum,
    pub state_index_root: TrieHash,
    pub microblock_pubkey_hash: Hash160, // we'll get the public key back from the first signature (note that this is the Hash160 of the _compressed_ public key)
}

/// Header structure for a microblock
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StacksMicroblockHeader {
    pub version: u8,
    pub sequence: u16,
    pub prev_block: BlockHeaderHash,
    pub tx_merkle_root: Sha512Trunc256Sum,
    pub signature: MessageSignature,
}

// values a miner uses to produce the next block
pub const MINER_BLOCK_CONSENSUS_HASH: ConsensusHash = ConsensusHash([1u8; 20]);
pub const MINER_BLOCK_HEADER_HASH: BlockHeaderHash = BlockHeaderHash([1u8; 32]);

/// A structure for incrementially building up a block
#[derive(Clone)]
pub struct StacksBlockBuilder {
    pub chain_tip: StacksHeaderInfo,
    pub header: StacksBlockHeader,
    pub txs: Vec<StacksTransaction>,
    pub micro_txs: Vec<StacksTransaction>,
    pub total_anchored_fees: u64,
    pub total_confirmed_streamed_fees: u64,
    pub total_streamed_fees: u64,
    anchored_done: bool,
    bytes_so_far: u64,
    prev_microblock_header: StacksMicroblockHeader,
    miner_privkey: StacksPrivateKey,
    miner_payouts: Option<(MinerReward, Vec<MinerReward>, MinerReward, MinerRewardInfo)>,
    parent_consensus_hash: ConsensusHash,
    parent_header_hash: BlockHeaderHash,
    parent_microblock_hash: Option<BlockHeaderHash>,
    miner_id: usize,
}

// maximum amount of data a leader can send during its epoch (2MB)
pub const MAX_EPOCH_SIZE: u32 = 2 * 1024 * 1024;

// maximum microblock size is 64KB, but note that the current leader has a space budget of
// $MAX_EPOCH_SIZE bytes (so the average microblock size needs to be 4kb if there are 256 of them)
pub const MAX_MICROBLOCK_SIZE: u32 = 65536;

#[cfg(test)]
pub mod test {
    use clarity::vm::representations::{ClarityName, ContractName};
    use clarity::vm::ClarityVersion;
    use stacks_common::bitvec::BitVec;
    use stacks_common::util::hash::*;
    use stacks_common::util::secp256k1::Secp256k1PrivateKey;
    use stacks_common::util::{get_epoch_time_secs, log};

    use super::*;
    use crate::chainstate::burn::BlockSnapshot;
    use crate::chainstate::nakamoto::miner::NakamotoBlockBuilder;
    use crate::chainstate::nakamoto::{NakamotoBlock, NakamotoBlockHeader};
    use crate::chainstate::stacks::{StacksPublicKey as PubKey, *};
    use crate::core::*;
    use crate::net::codec::test::check_codec_and_corruption;
    use crate::net::codec::*;
    use crate::net::*;

    /// Make a representative of each kind of transaction we support
    pub fn codec_all_transactions(
        version: &TransactionVersion,
        chain_id: u32,
        anchor_mode: &TransactionAnchorMode,
        post_condition_mode: &TransactionPostConditionMode,
        epoch_id: StacksEpochId,
    ) -> Vec<StacksTransaction> {
        let addr = StacksAddress {
            version: 1,
            bytes: Hash160([0xff; 20]),
        };
        let asset_name = ClarityName::try_from("hello-asset").unwrap();
        let asset_value = Value::buff_from(vec![0, 1, 2, 3]).unwrap();
        let contract_name = ContractName::try_from("hello-world").unwrap();
        let hello_contract_call = "hello contract call";
        let hello_contract_name = "hello-contract-name";
        let hello_contract_body = "hello contract code body";
        let asset_info = AssetInfo {
            contract_address: addr.clone(),
            contract_name: contract_name.clone(),
            asset_name: asset_name.clone(),
        };

        let mblock_header_1 = StacksMicroblockHeader {
            version: 0x12,
            sequence: 0x34,
            prev_block: EMPTY_MICROBLOCK_PARENT_HASH.clone(),
            tx_merkle_root: Sha512Trunc256Sum([1u8; 32]),
            signature: MessageSignature([2u8; 65]),
        };

        let mblock_header_2 = StacksMicroblockHeader {
            version: 0x12,
            sequence: 0x34,
            prev_block: EMPTY_MICROBLOCK_PARENT_HASH.clone(),
            tx_merkle_root: Sha512Trunc256Sum([2u8; 32]),
            signature: MessageSignature([3u8; 65]),
        };

        let mut spending_conditions = vec![
            TransactionSpendingCondition::Singlesig(SinglesigSpendingCondition {
                signer: Hash160([0x11; 20]),
                hash_mode: SinglesigHashMode::P2PKH,
                key_encoding: TransactionPublicKeyEncoding::Uncompressed,
                nonce: 123,
                tx_fee: 456,
                signature: MessageSignature::from_raw(&vec![0xff; 65])
            }),
            TransactionSpendingCondition::Singlesig(SinglesigSpendingCondition {
                signer: Hash160([0x11; 20]),
                hash_mode: SinglesigHashMode::P2PKH,
                key_encoding: TransactionPublicKeyEncoding::Compressed,
                nonce: 234,
                tx_fee: 567,
                signature: MessageSignature::from_raw(&vec![0xff; 65])
            }),
            TransactionSpendingCondition::Multisig(MultisigSpendingCondition {
                signer: Hash160([0x11; 20]),
                hash_mode: MultisigHashMode::P2SH,
                nonce: 345,
                tx_fee: 678,
                fields: vec![
                    TransactionAuthField::Signature(TransactionPublicKeyEncoding::Uncompressed, MessageSignature::from_raw(&vec![0xff; 65])),
                    TransactionAuthField::Signature(TransactionPublicKeyEncoding::Uncompressed, MessageSignature::from_raw(&vec![0xfe; 65])),
                    TransactionAuthField::PublicKey(PubKey::from_hex("04ef2340518b5867b23598a9cf74611f8b98064f7d55cdb8c107c67b5efcbc5c771f112f919b00a6c6c5f51f7c63e1762fe9fac9b66ec75a053db7f51f4a52712b").unwrap()),
                ],
                signatures_required: 2
            }),
            TransactionSpendingCondition::Multisig(MultisigSpendingCondition {
                signer: Hash160([0x11; 20]),
                hash_mode: MultisigHashMode::P2SH,
                nonce: 456,
                tx_fee: 789,
                fields: vec![
                    TransactionAuthField::Signature(TransactionPublicKeyEncoding::Compressed, MessageSignature::from_raw(&vec![0xff; 65])),
                    TransactionAuthField::Signature(TransactionPublicKeyEncoding::Compressed, MessageSignature::from_raw(&vec![0xfe; 65])),
                    TransactionAuthField::PublicKey(PubKey::from_hex("03ef2340518b5867b23598a9cf74611f8b98064f7d55cdb8c107c67b5efcbc5c77").unwrap())
                ],
                signatures_required: 2
            }),
            TransactionSpendingCondition::Singlesig(SinglesigSpendingCondition {
                signer: Hash160([0x11; 20]),
                hash_mode: SinglesigHashMode::P2WPKH,
                key_encoding: TransactionPublicKeyEncoding::Compressed,
                nonce: 567,
                tx_fee: 890,
                signature: MessageSignature::from_raw(&vec![0xfe; 65]),
            }),
            TransactionSpendingCondition::Multisig(MultisigSpendingCondition {
                signer: Hash160([0x11; 20]),
                hash_mode: MultisigHashMode::P2WSH,
                nonce: 678,
                tx_fee: 901,
                fields: vec![
                    TransactionAuthField::Signature(TransactionPublicKeyEncoding::Compressed, MessageSignature::from_raw(&vec![0xff; 65])),
                    TransactionAuthField::Signature(TransactionPublicKeyEncoding::Compressed, MessageSignature::from_raw(&vec![0xfe; 65])),
                    TransactionAuthField::PublicKey(PubKey::from_hex("03ef2340518b5867b23598a9cf74611f8b98064f7d55cdb8c107c67b5efcbc5c77").unwrap())
                ],
                signatures_required: 2
            }),
        ];

        if epoch_id >= StacksEpochId::Epoch30 {
            spending_conditions.append(&mut vec![
                TransactionSpendingCondition::OrderIndependentMultisig(OrderIndependentMultisigSpendingCondition {
                    signer: Hash160([0x11; 20]),
                    hash_mode: OrderIndependentMultisigHashMode::P2WSH,
                    nonce: 678,
                    tx_fee: 901,
                    fields: vec![
                        TransactionAuthField::Signature(TransactionPublicKeyEncoding::Compressed, MessageSignature::from_raw(&vec![0xff; 65])),
                        TransactionAuthField::Signature(TransactionPublicKeyEncoding::Compressed, MessageSignature::from_raw(&vec![0xfe; 65])),
                        TransactionAuthField::PublicKey(PubKey::from_hex("03ef2340518b5867b23598a9cf74611f8b98064f7d55cdb8c107c67b5efcbc5c77").unwrap())
                    ],
                    signatures_required: 2
                }),
                TransactionSpendingCondition::OrderIndependentMultisig(OrderIndependentMultisigSpendingCondition {
                    signer: Hash160([0x11; 20]),
                    hash_mode: OrderIndependentMultisigHashMode::P2SH,
                    nonce: 345,
                    tx_fee: 678,
                    fields: vec![
                        TransactionAuthField::Signature(TransactionPublicKeyEncoding::Uncompressed, MessageSignature::from_raw(&vec![0xff; 65])),
                        TransactionAuthField::Signature(TransactionPublicKeyEncoding::Uncompressed, MessageSignature::from_raw(&vec![0xfe; 65])),
                        TransactionAuthField::PublicKey(PubKey::from_hex("04ef2340518b5867b23598a9cf74611f8b98064f7d55cdb8c107c67b5efcbc5c771f112f919b00a6c6c5f51f7c63e1762fe9fac9b66ec75a053db7f51f4a52712b").unwrap()),
                    ],
                    signatures_required: 2
                }),
                TransactionSpendingCondition::OrderIndependentMultisig(OrderIndependentMultisigSpendingCondition {
                    signer: Hash160([0x11; 20]),
                    hash_mode: OrderIndependentMultisigHashMode::P2SH,
                    nonce: 456,
                    tx_fee: 789,
                    fields: vec![
                        TransactionAuthField::Signature(TransactionPublicKeyEncoding::Compressed, MessageSignature::from_raw(&vec![0xff; 65])),
                        TransactionAuthField::Signature(TransactionPublicKeyEncoding::Compressed, MessageSignature::from_raw(&vec![0xfe; 65])),
                        TransactionAuthField::PublicKey(PubKey::from_hex("03ef2340518b5867b23598a9cf74611f8b98064f7d55cdb8c107c67b5efcbc5c77").unwrap())
                    ],
                    signatures_required: 2
                }),
            ])
        }

        let mut tx_auths = vec![];
        for i in 0..spending_conditions.len() {
            let spending_condition = &spending_conditions[i];
            let next_spending_condition = &spending_conditions[(i + 1) % spending_conditions.len()];

            tx_auths.push(TransactionAuth::Standard(spending_condition.clone()));
            tx_auths.push(TransactionAuth::Sponsored(
                spending_condition.clone(),
                next_spending_condition.clone(),
            ));
        }

        let tx_post_condition_principals = vec![
            PostConditionPrincipal::Origin,
            PostConditionPrincipal::Standard(StacksAddress {
                version: 1,
                bytes: Hash160([1u8; 20]),
            }),
            PostConditionPrincipal::Contract(
                StacksAddress {
                    version: 2,
                    bytes: Hash160([2u8; 20]),
                },
                ContractName::try_from("hello-world").unwrap(),
            ),
        ];

        let mut tx_post_conditions = vec![];
        for tx_pcp in tx_post_condition_principals {
            tx_post_conditions.append(&mut vec![
                vec![TransactionPostCondition::STX(
                    tx_pcp.clone(),
                    FungibleConditionCode::SentLt,
                    12345,
                )],
                vec![TransactionPostCondition::Fungible(
                    tx_pcp.clone(),
                    AssetInfo {
                        contract_address: addr.clone(),
                        contract_name: contract_name.clone(),
                        asset_name: asset_name.clone(),
                    },
                    FungibleConditionCode::SentGt,
                    23456,
                )],
                vec![TransactionPostCondition::Nonfungible(
                    tx_pcp.clone(),
                    AssetInfo {
                        contract_address: addr.clone(),
                        contract_name: contract_name.clone(),
                        asset_name: asset_name.clone(),
                    },
                    asset_value.clone(),
                    NonfungibleConditionCode::NotSent,
                )],
                vec![
                    TransactionPostCondition::STX(
                        tx_pcp.clone(),
                        FungibleConditionCode::SentLt,
                        12345,
                    ),
                    TransactionPostCondition::Fungible(
                        tx_pcp.clone(),
                        AssetInfo {
                            contract_address: addr.clone(),
                            contract_name: contract_name.clone(),
                            asset_name: asset_name.clone(),
                        },
                        FungibleConditionCode::SentGt,
                        23456,
                    ),
                ],
                vec![
                    TransactionPostCondition::STX(
                        tx_pcp.clone(),
                        FungibleConditionCode::SentLt,
                        12345,
                    ),
                    TransactionPostCondition::Nonfungible(
                        tx_pcp.clone(),
                        AssetInfo {
                            contract_address: addr.clone(),
                            contract_name: contract_name.clone(),
                            asset_name: asset_name.clone(),
                        },
                        asset_value.clone(),
                        NonfungibleConditionCode::NotSent,
                    ),
                ],
                vec![
                    TransactionPostCondition::Fungible(
                        tx_pcp.clone(),
                        AssetInfo {
                            contract_address: addr.clone(),
                            contract_name: contract_name.clone(),
                            asset_name: asset_name.clone(),
                        },
                        FungibleConditionCode::SentGt,
                        23456,
                    ),
                    TransactionPostCondition::Nonfungible(
                        tx_pcp.clone(),
                        AssetInfo {
                            contract_address: addr.clone(),
                            contract_name: contract_name.clone(),
                            asset_name: asset_name.clone(),
                        },
                        asset_value.clone(),
                        NonfungibleConditionCode::NotSent,
                    ),
                ],
                vec![
                    TransactionPostCondition::STX(
                        tx_pcp.clone(),
                        FungibleConditionCode::SentLt,
                        12345,
                    ),
                    TransactionPostCondition::Nonfungible(
                        tx_pcp.clone(),
                        AssetInfo {
                            contract_address: addr.clone(),
                            contract_name: contract_name.clone(),
                            asset_name: asset_name.clone(),
                        },
                        asset_value.clone(),
                        NonfungibleConditionCode::NotSent,
                    ),
                    TransactionPostCondition::Fungible(
                        tx_pcp.clone(),
                        AssetInfo {
                            contract_address: addr.clone(),
                            contract_name: contract_name.clone(),
                            asset_name: asset_name.clone(),
                        },
                        FungibleConditionCode::SentGt,
                        23456,
                    ),
                ],
            ]);
        }

        let stx_address = StacksAddress {
            version: 1,
            bytes: Hash160([0xff; 20]),
        };
        let proof_bytes = hex_bytes("9275df67a68c8745c0ff97b48201ee6db447f7c93b23ae24cdc2400f52fdb08a1a6ac7ec71bf9c9c76e96ee4675ebff60625af28718501047bfd87b810c2d2139b73c23bd69de66360953a642c2a330a").unwrap();
        let proof = VRFProof::from_bytes(&proof_bytes[..].to_vec()).unwrap();
        let mut tx_payloads = vec![
            TransactionPayload::TokenTransfer(
                stx_address.into(),
                123,
                TokenTransferMemo([0u8; 34]),
            ),
            TransactionPayload::TokenTransfer(
                PrincipalData::from(QualifiedContractIdentifier {
                    issuer: stx_address.into(),
                    name: "hello-contract-name".into(),
                }),
                123,
                TokenTransferMemo([0u8; 34]),
            ),
            TransactionPayload::ContractCall(TransactionContractCall {
                address: StacksAddress {
                    version: 4,
                    bytes: Hash160([0xfc; 20]),
                },
                contract_name: ContractName::try_from("hello-contract-name").unwrap(),
                function_name: ClarityName::try_from("hello-contract-call").unwrap(),
                function_args: vec![Value::Int(0)],
            }),
            TransactionPayload::SmartContract(
                TransactionSmartContract {
                    name: ContractName::try_from(hello_contract_name).unwrap(),
                    code_body: StacksString::from_str(hello_contract_body).unwrap(),
                },
                None,
            ),
            TransactionPayload::SmartContract(
                TransactionSmartContract {
                    name: ContractName::try_from(hello_contract_name).unwrap(),
                    code_body: StacksString::from_str(hello_contract_body).unwrap(),
                },
                Some(ClarityVersion::Clarity1),
            ),
            TransactionPayload::SmartContract(
                TransactionSmartContract {
                    name: ContractName::try_from(hello_contract_name).unwrap(),
                    code_body: StacksString::from_str(hello_contract_body).unwrap(),
                },
                Some(ClarityVersion::Clarity2),
            ),
            TransactionPayload::PoisonMicroblock(mblock_header_1, mblock_header_2),
        ];

        if epoch_id >= StacksEpochId::Epoch30 {
            tx_payloads.append(&mut vec![
                TransactionPayload::TenureChange(TenureChangePayload {
                    tenure_consensus_hash: ConsensusHash([0x01; 20]),
                    prev_tenure_consensus_hash: ConsensusHash([0x02; 20]),
                    burn_view_consensus_hash: ConsensusHash([0x03; 20]),
                    previous_tenure_end: StacksBlockId([0x00; 32]),
                    previous_tenure_blocks: 0,
                    cause: TenureChangeCause::BlockFound,
                    pubkey_hash: Hash160([0x00; 20]),
                }),
                TransactionPayload::Coinbase(
                    CoinbasePayload([0x12; 32]),
                    None,
                    Some(proof.clone()),
                ),
                TransactionPayload::Coinbase(
                    CoinbasePayload([0x12; 32]),
                    Some(PrincipalData::Contract(
                        QualifiedContractIdentifier::transient(),
                    )),
                    Some(proof.clone()),
                ),
                TransactionPayload::Coinbase(
                    CoinbasePayload([0x12; 32]),
                    Some(PrincipalData::Standard(StandardPrincipalData(
                        0x01, [0x02; 20],
                    ))),
                    Some(proof.clone()),
                ),
            ])
        } else {
            tx_payloads.append(&mut vec![
                TransactionPayload::Coinbase(CoinbasePayload([0x12; 32]), None, None),
                TransactionPayload::Coinbase(
                    CoinbasePayload([0x12; 32]),
                    Some(PrincipalData::Contract(
                        QualifiedContractIdentifier::transient(),
                    )),
                    None,
                ),
                TransactionPayload::Coinbase(
                    CoinbasePayload([0x12; 32]),
                    Some(PrincipalData::Standard(StandardPrincipalData(
                        0x01, [0x02; 20],
                    ))),
                    None,
                ),
            ])
        }

        // create all kinds of transactions
        let mut all_txs = vec![];
        for tx_auth in tx_auths.iter() {
            for tx_post_condition in tx_post_conditions.iter() {
                for tx_payload in tx_payloads.iter() {
                    match tx_payload {
                        // poison microblock and coinbase must be on-chain
                        TransactionPayload::Coinbase(..) => {
                            if *anchor_mode != TransactionAnchorMode::OnChainOnly {
                                continue;
                            }
                        }
                        TransactionPayload::PoisonMicroblock(_, _) => {
                            if *anchor_mode != TransactionAnchorMode::OnChainOnly {
                                continue;
                            }
                        }
                        _ => {}
                    }

                    let auth = tx_auth.clone();

                    let tx = StacksTransaction {
                        version: (*version).clone(),
                        chain_id,
                        auth,
                        anchor_mode: (*anchor_mode).clone(),
                        post_condition_mode: (*post_condition_mode).clone(),
                        post_conditions: tx_post_condition.clone(),
                        payload: tx_payload.clone(),
                    };
                    all_txs.push(tx);
                }
            }
        }
        all_txs
    }

    pub fn make_codec_test_block(num_txs: usize, epoch_id: StacksEpochId) -> StacksBlock {
        let proof_bytes = hex_bytes("9275df67a68c8745c0ff97b48201ee6db447f7c93b23ae24cdc2400f52fdb08a1a6ac7ec71bf9c9c76e96ee4675ebff60625af28718501047bfd87b810c2d2139b73c23bd69de66360953a642c2a330a").unwrap();
        let proof = VRFProof::from_bytes(&proof_bytes[..].to_vec()).unwrap();

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
        let mut tx_coinbase = StacksTransaction::new(
            TransactionVersion::Mainnet,
            origin_auth.clone(),
            TransactionPayload::Coinbase(CoinbasePayload([0u8; 32]), None, None),
        );
        let tx_coinbase_proof = StacksTransaction::new(
            TransactionVersion::Mainnet,
            origin_auth.clone(),
            TransactionPayload::Coinbase(CoinbasePayload([0u8; 32]), None, Some(proof.clone())),
        );

        tx_coinbase.anchor_mode = TransactionAnchorMode::OnChainOnly;

        let all_txs = codec_all_transactions(
            &TransactionVersion::Testnet,
            0x80000000,
            &TransactionAnchorMode::OnChainOnly,
            &TransactionPostConditionMode::Allow,
            epoch_id,
        );

        // remove all coinbases, except for an initial coinbase
        let mut txs_anchored = vec![];

        if epoch_id >= StacksEpochId::Epoch30 {
            txs_anchored.push(tx_coinbase_proof);
        } else {
            txs_anchored.push(tx_coinbase);
        }

        for tx in all_txs.into_iter() {
            match tx.payload {
                TransactionPayload::Coinbase(..) => {
                    continue;
                }
                _ => {}
            }
            txs_anchored.push(tx);
            if txs_anchored.len() >= num_txs {
                break;
            }
        }

        let txid_vecs: Vec<_> = txs_anchored
            .iter()
            .map(|tx| tx.txid().as_bytes().to_vec())
            .collect();

        let merkle_tree = MerkleTree::<Sha512Trunc256Sum>::new(&txid_vecs);
        let tx_merkle_root = merkle_tree.root();
        let tr = tx_merkle_root.as_bytes().to_vec();

        let work_score = StacksWorkScore {
            burn: 123,
            work: 456,
        };

        let header = StacksBlockHeader {
            version: 0x01,
            total_work: StacksWorkScore {
                burn: 234,
                work: 567,
            },
            proof: proof.clone(),
            parent_block: BlockHeaderHash([5u8; 32]),
            parent_microblock: BlockHeaderHash([6u8; 32]),
            parent_microblock_sequence: 4,
            tx_merkle_root,
            state_index_root: TrieHash([8u8; 32]),
            microblock_pubkey_hash: Hash160([9u8; 20]),
        };

        StacksBlock {
            header,
            txs: txs_anchored,
        }
    }

    pub fn make_codec_test_nakamoto_block(
        epoch_id: StacksEpochId,
        miner_privk: &StacksPrivateKey,
    ) -> NakamotoBlock {
        let proof_bytes = hex_bytes("9275df67a68c8745c0ff97b48201ee6db447f7c93b23ae24cdc2400f52fdb08a1a6ac7ec71bf9c9c76e96ee4675ebff60625af28718501047bfd87b810c2d2139b73c23bd69de66360953a642c2a330a").unwrap();
        let proof = VRFProof::from_bytes(&proof_bytes[..].to_vec()).unwrap();

        let privk = StacksPrivateKey::from_hex(
            "6d430bb91222408e7706c9001cfaeb91b08c2be6d5ac95779ab52c6b431950e001",
        )
        .unwrap();

        let stx_address = StacksAddress {
            version: 1,
            bytes: Hash160([0xff; 20]),
        };
        let payload = TransactionPayload::TokenTransfer(
            stx_address.into(),
            123,
            TokenTransferMemo([0u8; 34]),
        );

        let auth = TransactionAuth::from_p2pkh(miner_privk).unwrap();
        let addr = auth.origin().address_testnet();
        let mut tx = StacksTransaction::new(TransactionVersion::Testnet, auth, payload);
        tx.chain_id = 0x80000000;
        tx.auth.set_origin_nonce(34);
        tx.set_post_condition_mode(TransactionPostConditionMode::Allow);
        tx.set_tx_fee(300);
        let mut tx_signer = StacksTransactionSigner::new(&tx);
        tx_signer.sign_origin(miner_privk).unwrap();
        let tx = tx_signer.get_tx().unwrap();

        let txid_vecs = vec![tx.txid().as_bytes().to_vec()];
        let txs_anchored = vec![tx];
        let merkle_tree = MerkleTree::<Sha512Trunc256Sum>::new(&txid_vecs);
        let tx_merkle_root = merkle_tree.root();

        let header = NakamotoBlockHeader {
            version: 0x00,
            chain_length: 107,
            burn_spent: 25000,
            consensus_hash: MINER_BLOCK_CONSENSUS_HASH.clone(),
            parent_block_id: StacksBlockId::from_bytes(&[0x11; 32]).unwrap(),
            tx_merkle_root,
            state_index_root: TrieHash::from_hex(
                "fb419c3d8f40ae154018f2abf3935e2275a14c091e071bacaf6cbf5579743a0f",
            )
            .unwrap(),
            timestamp: get_epoch_time_secs(),
            miner_signature: MessageSignature::empty(),
            signer_signature: Vec::new(),
            pox_treatment: BitVec::ones(8).unwrap(),
        };

        NakamotoBlock {
            header,
            txs: txs_anchored,
        }
    }

    pub fn make_codec_test_microblock(num_txs: usize) -> StacksMicroblock {
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
        let all_txs = codec_all_transactions(
            &TransactionVersion::Testnet,
            0x80000000,
            &TransactionAnchorMode::OffChainOnly,
            &TransactionPostConditionMode::Allow,
            StacksEpochId::latest(),
        );

        let txs_mblock: Vec<_> = all_txs.into_iter().take(num_txs).collect();
        let txid_vecs: Vec<_> = txs_mblock
            .iter()
            .map(|tx| tx.txid().as_bytes().to_vec())
            .collect();

        let merkle_tree = MerkleTree::<Sha512Trunc256Sum>::new(&txid_vecs);
        let tx_merkle_root = merkle_tree.root();

        let mut header = StacksMicroblockHeader {
            version: 6,
            sequence: 1,
            prev_block: BlockHeaderHash([0x11; 32]),
            tx_merkle_root,
            signature: MessageSignature::empty(),
        };

        header.sign(&privk).unwrap();
        StacksMicroblock {
            header,
            txs: txs_mblock,
        }
    }
}
