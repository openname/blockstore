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

use sha2::{Digest, Sha256};

use crate::deps_common::bitcoin::blockdata::opcodes::All as btc_opcodes;
use crate::deps_common::bitcoin::blockdata::script::Builder;
use crate::types::PublicKey;
use crate::util::hash::Hash160;

pub mod b58;
pub mod c32;
#[cfg(test)]
pub mod c32_old;

pub const C32_ADDRESS_VERSION_MAINNET_SINGLESIG: u8 = 22; // P
pub const C32_ADDRESS_VERSION_MAINNET_MULTISIG: u8 = 20; // M
pub const C32_ADDRESS_VERSION_TESTNET_SINGLESIG: u8 = 26; // T
pub const C32_ADDRESS_VERSION_TESTNET_MULTISIG: u8 = 21; // N

#[derive(Debug)]
pub enum Error {
    InvalidCrockford32,
    InvalidVersion(u8),
    EmptyData,
    /// Invalid character encountered
    BadByte(u8),
    /// Checksum was not correct (expected, actual)
    BadChecksum(u32, u32),
    /// The length (in bytes) of the object was not correct
    /// Note that if the length is excessively long the provided length may be
    /// an estimate (and the checksum step may be skipped).
    InvalidLength(usize),
    /// Checked data was less than 4 bytes
    TooShort(usize),
    /// Any other error
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::InvalidCrockford32 => write!(f, "Invalid crockford 32 string"),
            Error::InvalidVersion(ref v) => write!(f, "Invalid version {}", v),
            Error::EmptyData => f.write_str("Empty data"),
            Error::BadByte(b) => write!(f, "invalid base58 character 0x{:x}", b),
            Error::BadChecksum(exp, actual) => write!(
                f,
                "base58ck checksum 0x{:x} does not match expected 0x{:x}",
                actual, exp
            ),
            Error::InvalidLength(ell) => write!(f, "length {} invalid for this base58 type", ell),
            Error::TooShort(_) => write!(f, "base58ck data not even long enough for a checksum"),
            Error::Other(ref s) => f.write_str(s),
        }
    }
}

impl error::Error for Error {
    fn cause(&self) -> Option<&dyn error::Error> {
        None
    }
    fn description(&self) -> &'static str {
        match *self {
            Error::InvalidCrockford32 => "Invalid crockford 32 string",
            Error::InvalidVersion(_) => "Invalid version",
            Error::EmptyData => "Empty data",
            Error::BadByte(_) => "invalid b58 character",
            Error::BadChecksum(_, _) => "invalid b58ck checksum",
            Error::InvalidLength(_) => "invalid length for b58 type",
            Error::TooShort(_) => "b58ck data less than 4 bytes",
            Error::Other(_) => "unknown b58 error",
        }
    }
}

/// Serialization modes for public keys to addresses.  These apply to Stacks addresses, which
/// correspond to legacy Bitcoin addresses -- legacy Bitcoin address can be converted directly
/// into a Stacks address, permitting a Bitcoin address to be represented directly on Stacks.
/// These *do not apply* to Bitcoin segwit addresses.
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, PartialOrd, Ord, Hash, Eq, Copy, Serialize, Deserialize)]
pub enum AddressHashMode {
    // We support four different modes due to legacy compatibility with Stacks v1 addresses:
    SerializeP2PKH = 0x00,  // hash160(public-key), same as bitcoin's p2pkh
    SerializeP2SH = 0x01,   // hash160(multisig-redeem-script), same as bitcoin's multisig p2sh
    SerializeP2WPKH = 0x02, // hash160(segwit-program-00(p2pkh)), same as bitcoin's p2sh-p2wpkh
    SerializeP2WSH = 0x03,  // hash160(segwit-program-00(public-keys)), same as bitcoin's p2sh-p2wsh
}

impl AddressHashMode {
    pub fn to_version_mainnet(&self) -> u8 {
        match *self {
            AddressHashMode::SerializeP2PKH => C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
            _ => C32_ADDRESS_VERSION_MAINNET_MULTISIG,
        }
    }

    pub fn to_version_testnet(&self) -> u8 {
        match *self {
            AddressHashMode::SerializeP2PKH => C32_ADDRESS_VERSION_TESTNET_SINGLESIG,
            _ => C32_ADDRESS_VERSION_TESTNET_MULTISIG,
        }
    }

    /// WARNING: this does not support segwit-p2sh!
    pub fn from_version(version: u8) -> AddressHashMode {
        match version {
            C32_ADDRESS_VERSION_TESTNET_SINGLESIG | C32_ADDRESS_VERSION_MAINNET_SINGLESIG => {
                AddressHashMode::SerializeP2PKH
            }
            _ => AddressHashMode::SerializeP2SH,
        }
    }
}

/// Given the u8 of an AddressHashMode, deduce the AddressHashNode
impl TryFrom<u8> for AddressHashMode {
    type Error = Error;

    fn try_from(value: u8) -> Result<AddressHashMode, Self::Error> {
        match value {
            x if x == AddressHashMode::SerializeP2PKH as u8 => Ok(AddressHashMode::SerializeP2PKH),
            x if x == AddressHashMode::SerializeP2SH as u8 => Ok(AddressHashMode::SerializeP2SH),
            x if x == AddressHashMode::SerializeP2WPKH as u8 => {
                Ok(AddressHashMode::SerializeP2WPKH)
            }
            x if x == AddressHashMode::SerializeP2WSH as u8 => Ok(AddressHashMode::SerializeP2WSH),
            _ => Err(Error::InvalidVersion(value)),
        }
    }
}

/// Internally, the Stacks blockchain encodes address the same as Bitcoin
/// single-sig address (p2pkh)
/// Get back the hash of the address
pub fn to_bits_p2pkh<K: PublicKey>(pubk: &K) -> Hash160 {
    Hash160::from_data(&pubk.to_bytes())
}

/// Internally, the Stacks blockchain encodes address the same as Bitcoin
/// multi-sig address (p2sh)
fn to_bits_p2sh<K: PublicKey>(num_sigs: usize, pubkeys: &Vec<K>) -> Hash160 {
    let mut bldr = Builder::new();
    bldr = bldr.push_int(num_sigs as i64);
    for pubk in pubkeys {
        bldr = bldr.push_slice(&pubk.to_bytes());
    }
    bldr = bldr.push_int(pubkeys.len() as i64);
    bldr = bldr.push_opcode(btc_opcodes::OP_CHECKMULTISIG);

    let script = bldr.into_script();
    Hash160::from_data(script.as_bytes())
}

/// Internally, the Stacks blockchain encodes address the same as Bitcoin
/// single-sig address over p2sh (p2h-p2wpkh)
fn to_bits_p2sh_p2wpkh<K: PublicKey>(pubk: &K) -> Hash160 {
    let key_hash = Hash160::from_data(&pubk.to_bytes());

    let bldr = Builder::new().push_int(0).push_slice(key_hash.as_bytes());

    let script = bldr.into_script();
    Hash160::from_data(script.as_bytes())
}

/// Internally, the Stacks blockchain encodes address the same as Bitcoin
/// multisig address over p2sh (p2sh-p2wsh)
fn to_bits_p2sh_p2wsh<K: PublicKey>(num_sigs: usize, pubkeys: &Vec<K>) -> Hash160 {
    let mut bldr = Builder::new();
    bldr = bldr.push_int(num_sigs as i64);
    for pubk in pubkeys {
        bldr = bldr.push_slice(&pubk.to_bytes());
    }
    bldr = bldr.push_int(pubkeys.len() as i64);
    bldr = bldr.push_opcode(btc_opcodes::OP_CHECKMULTISIG);

    let mut digest = Sha256::new();
    let mut d = [0u8; 32];

    digest.update(bldr.into_script().as_bytes());
    d.copy_from_slice(digest.finalize().as_slice());

    let ws = Builder::new().push_int(0).push_slice(&d).into_script();
    Hash160::from_data(ws.as_bytes())
}

/// Convert a number of required signatures and a list of public keys into a byte-vec to hash to an
/// address.  Validity of the hash_flag vis a vis the num_sigs and pubkeys will _NOT_ be checked.
/// This is a low-level method.  Consider using StacksAdress::from_public_keys() if you can.
pub fn public_keys_to_address_hash<K: PublicKey>(
    hash_flag: &AddressHashMode,
    num_sigs: usize,
    pubkeys: &Vec<K>,
) -> Hash160 {
    match *hash_flag {
        AddressHashMode::SerializeP2PKH => to_bits_p2pkh(&pubkeys[0]),
        AddressHashMode::SerializeP2SH => to_bits_p2sh(num_sigs, pubkeys),
        AddressHashMode::SerializeP2WPKH => to_bits_p2sh_p2wpkh(&pubkeys[0]),
        AddressHashMode::SerializeP2WSH => to_bits_p2sh_p2wsh(num_sigs, pubkeys),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::util::hash::*;
    use crate::util::secp256k1::Secp256k1PublicKey as PubKey;

    struct PubkeyFixture {
        keys: Vec<PubKey>,
        num_required: usize,
        segwit: bool,
        result: Vec<u8>,
    }

    #[test]
    fn test_public_keys_to_address_hash() {
        let pubkey_fixtures = vec![
            PubkeyFixture {
                // script pubkey for p2pkh
                keys: vec![
                    PubKey::from_hex("040fadbbcea0ff3b05f03195b41cd991d7a0af8bd38559943aec99cbdaf0b22cc806b9a4f07579934774cc0c155e781d45c989f94336765e88a66d91cfb9f060b0").unwrap(),
                ],
                num_required: 1,
                segwit: false,
                result: hex_bytes("395f3643cea07ec4eec73b4d9a973dcce56b9bf1").unwrap().to_vec()
            },
            PubkeyFixture {
                // script pubkey for multisig p2sh
                keys: vec![
                    PubKey::from_hex("040fadbbcea0ff3b05f03195b41cd991d7a0af8bd38559943aec99cbdaf0b22cc806b9a4f07579934774cc0c155e781d45c989f94336765e88a66d91cfb9f060b0").unwrap(),
                    PubKey::from_hex("04c77f262dda02580d65c9069a8a34c56bd77325bba4110b693b90216f5a3edc0bebc8ce28d61aa86b414aa91ecb29823b11aeed06098fcd97fee4bc73d54b1e96").unwrap(),
                ],
                num_required: 2,
                segwit: false,
                result: hex_bytes("fd3a5e9f5ba311ce6122765f0af8da7488e25d3a").unwrap().to_vec(),
            },
            PubkeyFixture {
                // script pubkey for p2sh-p2wpkh
                keys: vec![
                    PubKey::from_hex("020fadbbcea0ff3b05f03195b41cd991d7a0af8bd38559943aec99cbdaf0b22cc8").unwrap(),
                ],
                num_required: 1,
                segwit: true,
                result: hex_bytes("0ac7ad046fe22c794dd923b3be14b2e668e50c42").unwrap().to_vec(),
            },
            PubkeyFixture {
                // script pubkey for multisig p2sh-p2wsh
                keys: vec![
                    PubKey::from_hex("020fadbbcea0ff3b05f03195b41cd991d7a0af8bd38559943aec99cbdaf0b22cc8").unwrap(),
                    PubKey::from_hex("02c77f262dda02580d65c9069a8a34c56bd77325bba4110b693b90216f5a3edc0b").unwrap(),
                ],
                num_required: 2,
                segwit: true,
                result: hex_bytes("3e02fa83ac2fae11fd6703b91e7c94ad393052e2").unwrap().to_vec(),
            },
        ];

        for pubkey_fixture in pubkey_fixtures {
            let hash_mode = if !pubkey_fixture.segwit {
                if pubkey_fixture.num_required == 1 {
                    AddressHashMode::SerializeP2PKH
                } else {
                    AddressHashMode::SerializeP2SH
                }
            } else if pubkey_fixture.num_required == 1 {
                AddressHashMode::SerializeP2WPKH
            } else {
                AddressHashMode::SerializeP2WSH
            };

            let result_hash = public_keys_to_address_hash(
                &hash_mode,
                pubkey_fixture.num_required,
                &pubkey_fixture.keys,
            );
            let result = result_hash.as_bytes().to_vec();

            assert_eq!(result, pubkey_fixture.result);
        }
    }
}
