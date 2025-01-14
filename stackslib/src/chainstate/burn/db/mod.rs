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

use std::{error, fmt};

use clarity::vm::types::PrincipalData;
use rusqlite::{Connection, Error as sqlite_error, Row};
use serde_json::Error as serde_error;
use stacks_common::types::chainstate::{
    BlockHeaderHash, BurnchainHeaderHash, StacksAddress, TrieHash, VRFSeed,
};
use stacks_common::util::hash::{hex_bytes, Hash160, Sha512Trunc256Sum};
use stacks_common::util::secp256k1::MessageSignature;
use stacks_common::util::vrf::*;

use crate::burnchains::bitcoin::address::BitcoinAddress;
use crate::burnchains::{Address, Txid};
use crate::chainstate::burn::{ConsensusHash, OpsHash, SortitionHash};
use crate::chainstate::stacks::address::PoxAddress;
use crate::chainstate::stacks::StacksPublicKey;
use crate::util_lib::db;
use crate::util_lib::db::{Error as db_error, FromColumn};

pub mod processing;
pub mod sortdb;

pub type DBConn = Connection;

impl_byte_array_from_column!(Txid);
impl_byte_array_from_column_only!(ConsensusHash);
impl_byte_array_from_column_only!(Hash160);
impl_byte_array_from_column_only!(BlockHeaderHash);
impl_byte_array_from_column_only!(VRFSeed);
impl_byte_array_from_column!(OpsHash);
impl_byte_array_from_column_only!(BurnchainHeaderHash);
impl_byte_array_from_column!(SortitionHash);
impl_byte_array_from_column_only!(Sha512Trunc256Sum);
impl_byte_array_from_column_only!(VRFProof);
impl_byte_array_from_column_only!(TrieHash);
impl_byte_array_from_column_only!(MessageSignature);

impl FromColumn<VRFPublicKey> for VRFPublicKey {
    fn from_column(row: &Row, column_name: &str) -> Result<VRFPublicKey, db_error> {
        let pubkey_hex: String = row.get_unwrap(column_name);
        match VRFPublicKey::from_hex(&pubkey_hex) {
            Some(pubk) => Ok(pubk),
            None => Err(db_error::ParseError),
        }
    }
}

impl FromColumn<StacksAddress> for StacksAddress {
    fn from_column(row: &Row, column_name: &str) -> Result<Self, db_error> {
        let address_str: String = row.get_unwrap(column_name);
        match Self::from_string(&address_str) {
            Some(a) => Ok(a),
            None => Err(db_error::ParseError),
        }
    }
}

impl FromColumn<PrincipalData> for PrincipalData {
    fn from_column(row: &Row, column_name: &str) -> Result<Self, db_error> {
        let address_str: String = row.get_unwrap(column_name);
        Self::parse(&address_str).map_err(|_| db_error::ParseError)
    }
}

impl FromColumn<PoxAddress> for PoxAddress {
    fn from_column(row: &Row, column_name: &str) -> Result<Self, db_error> {
        let address_str: String = row.get_unwrap(column_name);
        match Self::from_db_string(&address_str) {
            Some(a) => Ok(a),
            None => Err(db_error::ParseError),
        }
    }
}

impl FromColumn<BitcoinAddress> for BitcoinAddress {
    fn from_column(row: &Row, column_name: &str) -> Result<Self, db_error> {
        let address_str: String = row.get_unwrap(column_name);
        match Self::from_string(&address_str) {
            Some(a) => Ok(a),
            None => Err(db_error::ParseError),
        }
    }
}
