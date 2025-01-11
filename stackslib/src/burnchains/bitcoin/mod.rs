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

// This module is concerned with the implementation of the BitcoinIndexer
// structure and its methods and traits.

use std::sync::Arc;
use std::{error, fmt, io};

use stacks_common::deps_common::bitcoin::network::serialize::Error as btc_serialize_error;
use stacks_common::types::chainstate::BurnchainHeaderHash;
use stacks_common::util::HexError as btc_hex_error;

use crate::burnchains::bitcoin::address::BitcoinAddress;
use crate::burnchains::bitcoin::keys::BitcoinPublicKey;
use crate::burnchains::Txid;
use crate::chainstate::burn::operations::BlockstackOperationType;
use crate::deps;
use crate::util_lib::db::Error as db_error;

pub mod address;
pub mod bits;
pub mod blocks;
pub mod indexer;
pub mod keys;
pub mod messages;
pub mod network;
pub mod spv;

pub type PeerMessage = stacks_common::deps_common::bitcoin::network::message::NetworkMessage;

// Borrowed from Andrew Poelstra's rust-bitcoin

/// Network error
#[derive(Debug)]
pub enum Error {
    /// I/O error
    Io(io::Error),
    /// Not connected to peer
    SocketNotConnectedToPeer,
    /// Serialization error
    SerializationError(btc_serialize_error),
    /// Invalid Message to peer
    InvalidMessage(PeerMessage),
    /// Invalid Reply from peer
    InvalidReply,
    /// Invalid magic
    InvalidMagic,
    /// Unhandled message
    UnhandledMessage(PeerMessage),
    /// Connection is broken and ought to be re-established
    ConnectionBroken,
    /// Connection could not be (re-)established
    ConnectionError,
    /// general filesystem error
    FilesystemError(io::Error),
    /// Database error
    DBError(db_error),
    /// Hashing error
    HashError(btc_hex_error),
    /// Non-contiguous header
    NoncontiguousHeader,
    /// Missing header
    MissingHeader,
    /// Invalid header proof-of-work (i.e. due to a bad timestamp or a bad `bits` field)
    InvalidPoW,
    /// Chainwork would decrease by including a given header
    InvalidChainWork,
    /// Wrong number of bytes for constructing an address
    InvalidByteSequence,
    /// Configuration error
    ConfigError(String),
    /// Tried to synchronize to a point above the chain tip
    BlockchainHeight,
    /// Request timed out
    TimedOut,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::Io(ref e) => fmt::Display::fmt(e, f),
            Error::SocketNotConnectedToPeer => write!(f, "not connected to peer"),
            Error::SerializationError(ref e) => fmt::Display::fmt(e, f),
            Error::InvalidMessage(ref _msg) => write!(f, "Invalid message to send"),
            Error::InvalidReply => write!(f, "invalid reply for given message"),
            Error::InvalidMagic => write!(f, "invalid network magic"),
            Error::UnhandledMessage(ref _msg) => write!(f, "Unhandled message"),
            Error::ConnectionBroken => write!(f, "connection to peer node is broken"),
            Error::ConnectionError => write!(f, "connection to peer could not be (re-)established"),
            Error::FilesystemError(ref e) => fmt::Display::fmt(e, f),
            Error::DBError(ref e) => fmt::Display::fmt(e, f),
            Error::HashError(ref e) => fmt::Display::fmt(e, f),
            Error::NoncontiguousHeader => write!(f, "Non-contiguous header"),
            Error::MissingHeader => write!(f, "Missing header"),
            Error::InvalidPoW => write!(f, "Invalid proof of work"),
            Error::InvalidChainWork => write!(f, "Chain difficulty cannot decrease"),
            Error::InvalidByteSequence => write!(f, "Invalid sequence of bytes"),
            Error::ConfigError(ref e_str) => fmt::Display::fmt(e_str, f),
            Error::BlockchainHeight => write!(f, "Value is beyond the end of the blockchain"),
            Error::TimedOut => write!(f, "Request timed out"),
        }
    }
}

impl error::Error for Error {
    fn cause(&self) -> Option<&dyn error::Error> {
        match *self {
            Error::Io(ref e) => Some(e),
            Error::SocketNotConnectedToPeer => None,
            Error::SerializationError(ref e) => Some(e),
            Error::InvalidMessage(ref _msg) => None,
            Error::InvalidReply => None,
            Error::InvalidMagic => None,
            Error::UnhandledMessage(ref _msg) => None,
            Error::ConnectionBroken => None,
            Error::ConnectionError => None,
            Error::FilesystemError(ref e) => Some(e),
            Error::DBError(ref e) => Some(e),
            Error::HashError(ref e) => Some(e),
            Error::NoncontiguousHeader => None,
            Error::MissingHeader => None,
            Error::InvalidPoW => None,
            Error::InvalidChainWork => None,
            Error::InvalidByteSequence => None,
            Error::ConfigError(ref _e_str) => None,
            Error::BlockchainHeight => None,
            Error::TimedOut => None,
        }
    }
}

impl From<db_error> for Error {
    fn from(e: db_error) -> Error {
        Error::DBError(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BitcoinNetworkType {
    Mainnet,
    Testnet,
    Regtest,
}

#[derive(Debug, PartialEq, Clone, Eq, Serialize, Deserialize)]
pub struct BitcoinTxOutput {
    pub address: BitcoinAddress,
    pub units: u64,
}

/// Legacy Bitcoin address input type, based on scriptSig.
#[derive(Debug, PartialEq, Clone, Eq, Serialize, Deserialize)]
pub enum BitcoinInputType {
    Standard,
    SegwitP2SH,
}

/// Bitcoin tx input we can parse in 2.05 and earlier.
/// In 2.05 and earlier, we cared about being able to parse a scriptSig and witness.
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct BitcoinTxInputStructured {
    pub keys: Vec<BitcoinPublicKey>,
    pub num_required: usize,
    pub in_type: BitcoinInputType,
    pub tx_ref: (Txid, u32),
}

/// Bitcoin tx input we can parse in 2.1 and later.
/// In 2.1 and later, we don't care about being able to parse a scriptSig or witness.
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct BitcoinTxInputRaw {
    pub scriptSig: Vec<u8>,
    pub witness: Vec<Vec<u8>>,
    pub tx_ref: (Txid, u32),
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub enum BitcoinTxInput {
    Structured(BitcoinTxInputStructured),
    Raw(BitcoinTxInputRaw),
}

impl From<BitcoinTxInputStructured> for BitcoinTxInput {
    fn from(inp: BitcoinTxInputStructured) -> BitcoinTxInput {
        BitcoinTxInput::Structured(inp)
    }
}

impl From<BitcoinTxInputRaw> for BitcoinTxInput {
    fn from(inp: BitcoinTxInputRaw) -> BitcoinTxInput {
        BitcoinTxInput::Raw(inp)
    }
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct BitcoinTransaction {
    pub txid: Txid,
    pub vtxindex: u32,
    pub opcode: u8,
    pub data: Vec<u8>,
    /// how much BTC was sent to the data output
    pub data_amt: u64,
    pub inputs: Vec<BitcoinTxInput>,
    pub outputs: Vec<BitcoinTxOutput>,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct BitcoinBlock {
    pub block_height: u64,
    pub block_hash: BurnchainHeaderHash,
    pub parent_block_hash: BurnchainHeaderHash,
    pub txs: Vec<BitcoinTransaction>,
    pub timestamp: u64,
}

impl BitcoinBlock {
    pub fn new(
        height: u64,
        hash: &BurnchainHeaderHash,
        parent: &BurnchainHeaderHash,
        txs: Vec<BitcoinTransaction>,
        timestamp: u64,
    ) -> BitcoinBlock {
        BitcoinBlock {
            block_height: height,
            block_hash: hash.clone(),
            parent_block_hash: parent.clone(),
            txs,
            timestamp,
        }
    }
}
