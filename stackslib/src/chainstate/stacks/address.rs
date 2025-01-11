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

use std::cmp::Ordering;
use std::io::prelude::*;
use std::io::{Read, Write};
use std::{fmt, io};

use clarity::vm::types::{PrincipalData, SequenceData, StandardPrincipalData, TupleData, Value};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use stacks_common::address::c32::{c32_address, c32_address_decode};
use stacks_common::address::{b58, public_keys_to_address_hash, AddressHashMode};
use stacks_common::codec::{read_next, write_next, Error as codec_error, StacksMessageCodec};
use stacks_common::deps_common::bitcoin::blockdata::opcodes::All as BtcOp;
use stacks_common::deps_common::bitcoin::blockdata::script::Builder as BtcScriptBuilder;
use stacks_common::deps_common::bitcoin::blockdata::transaction::TxOut;
use stacks_common::types::chainstate::{StacksAddress, STACKS_ADDRESS_ENCODED_SIZE};
use stacks_common::util::hash::{to_hex, Hash160, HASH160_ENCODED_SIZE};

use crate::burnchains::bitcoin::address::{
    legacy_address_type_to_version_byte, legacy_version_byte_to_address_type, to_b58_version_byte,
    to_c32_version_byte, BitcoinAddress, LegacyBitcoinAddress, LegacyBitcoinAddressType,
    SegwitBitcoinAddress,
};
use crate::burnchains::bitcoin::BitcoinTxOutput;
use crate::burnchains::{Address, PublicKey};
use crate::chainstate::stacks::{
    StacksPublicKey, C32_ADDRESS_VERSION_MAINNET_MULTISIG, C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
    C32_ADDRESS_VERSION_TESTNET_MULTISIG, C32_ADDRESS_VERSION_TESTNET_SINGLESIG,
};
use crate::net::Error as net_error;
use crate::util_lib::boot::boot_code_addr;

pub trait StacksAddressExtensions {
    fn to_b58(self) -> String;
    fn from_legacy_bitcoin_address(addr: &LegacyBitcoinAddress) -> StacksAddress;
    fn is_boot_code_addr(&self) -> bool;
}

// PoX 20-byte address types that do not have a StacksAddress representation
define_u8_enum!(PoxAddressType20 {
    // ADDRESS_VERSION_P2WPKH in pox-2.clar
    P2WPKH = 0x04
});

// PoX 32-byte address types that do not have a StacksAddress representation
define_u8_enum!(PoxAddressType32 {
    // ADDRESS_VERSION_P2WSH in pox-2.clar
    P2WSH = 0x05,
    // ADDRESS_VERSION_P2TR in pox-2.clar
    P2TR = 0x06
});

/// A PoX address as seen by the .pox and .pox-2 contracts.
/// Used by the sortition DB and chains coordinator to extract addresses from the PoX contract to
/// build the reward set and to validate block-commits.
/// Note that this comprises a larger set of possible addresses than StacksAddress
#[derive(Debug, PartialEq, PartialOrd, Ord, Clone, Hash, Eq, Serialize, Deserialize)]
pub enum PoxAddress {
    /// Represents a { version: (buff 1), hashbytes: (buff 20) } tuple that has a Stacks
    /// representation.  Not all 20-byte hashbyte addresses do (such as Bitcoin p2wpkh)
    /// The address hash mode is optional because if we decode a legacy bitcoin address, we won't
    /// be able to determine the hash mode since we can't distinguish segwit-p2sh from p2sh
    Standard(StacksAddress, Option<AddressHashMode>),
    /// Represents { version: (buff 1), hashbytes: (buff 20) } that does not have a Stacks
    /// representation.  This includes Bitcoin p2wpkh.
    /// Fields are (mainnet, address type ID, bytes)
    Addr20(bool, PoxAddressType20, [u8; 20]),
    /// Represents { version: (buff 1), hashbytes: (buff 32) } that does not have a Stacks
    /// representation.  This includes Bitcoin p2wsh and p2tr.
    /// Fields are (mainnet, address type ID, bytes)
    Addr32(bool, PoxAddressType32, [u8; 32]),
}

/// Serializes a PoxAddress as a B58 check encoded address or a bech32 address
pub fn pox_addr_b58_serialize<S: Serializer>(
    input: &PoxAddress,
    ser: S,
) -> Result<S::Ok, S::Error> {
    ser.serialize_str(&input.clone().to_b58())
}

/// Deserializes a PoxAddress from a B58 check encoded address or a bech32 address
pub fn pox_addr_b58_deser<'de, D: Deserializer<'de>>(deser: D) -> Result<PoxAddress, D::Error> {
    let string_repr = String::deserialize(deser)?;
    PoxAddress::from_b58(&string_repr)
        .ok_or_else(|| serde::de::Error::custom("Failed to decode PoxAddress from string"))
}

impl std::fmt::Display for PoxAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_db_string())
    }
}

impl PoxAddress {
    /// Obtain the address hash mode used for the PoX address, if applicable.  This identifies the
    /// address as p2pkh, p2sh, p2wpkh-p2sh, or p2wsh-p2sh
    #[cfg(any(test, feature = "testing"))]
    pub fn hashmode(&self) -> Option<AddressHashMode> {
        match *self {
            PoxAddress::Standard(_, hm) => hm.clone(),
            _ => None,
        }
    }

    /// Get the version byte representation of the hash mode.  Used only in testing, where the test
    /// knows that it will only use Bitcoin legacy addresses (i.e. so this method is infallable).
    #[cfg(any(test, feature = "testing"))]
    pub fn version(&self) -> u8 {
        self.hashmode()
            .expect("FATAL: tried to load the hashmode of a PoxAddress which has none known")
            as u8
    }

    /// Get the Hash160 portion of this address.  Only applies to legacy Bitcoin addresses.
    /// Used only in tests, and even then, only in ones that expect a legacy Bitcoin address.
    #[cfg(any(test, feature = "testing"))]
    pub fn hash160(&self) -> Hash160 {
        match *self {
            PoxAddress::Standard(addr, _) => addr.bytes.clone(),
            _ => panic!("Called hash160 on a non-standard PoX address"),
        }
    }

    /// Get the data portion of this address.  This does not include the address or witness
    /// version.
    pub fn bytes(&self) -> Vec<u8> {
        match *self {
            PoxAddress::Standard(addr, _) => addr.bytes.0.to_vec(),
            PoxAddress::Addr20(_, _, bytes) => bytes.to_vec(),
            PoxAddress::Addr32(_, _, bytes) => bytes.to_vec(),
        }
    }

    /// Try to convert a Clarity value representation of the PoX address into a
    /// PoxAddress::Standard.
    fn try_standard_from_pox_tuple(
        mainnet: bool,
        hashmode_u8: u8,
        hashbytes: &[u8],
    ) -> Option<PoxAddress> {
        let hashmode: AddressHashMode = hashmode_u8.try_into().ok()?;

        // this is a valid AddressHashMode, so there must be exactly 20 bytes
        if hashbytes.len() != 20 {
            return None;
        }

        let mut hashbytes_20 = [0u8; 20];
        hashbytes_20.copy_from_slice(&hashbytes[0..20]);
        let bytes = Hash160(hashbytes_20);

        let version = if mainnet {
            hashmode.to_version_mainnet()
        } else {
            hashmode.to_version_testnet()
        };

        Some(PoxAddress::Standard(
            StacksAddress { version, bytes },
            Some(hashmode),
        ))
    }

    /// Try to convert a Clarity value representation of the PoX address into a
    /// PoxAddress::Addr20.
    fn try_addr20_from_pox_tuple(
        mainnet: bool,
        hashmode_u8: u8,
        hashbytes: &[u8],
    ) -> Option<PoxAddress> {
        let addrtype = PoxAddressType20::from_u8(hashmode_u8)?;

        // this is a valid PoxAddressType20, so there must be exactly 20 bytes
        if hashbytes.len() != 20 {
            return None;
        }

        let mut hashbytes_20 = [0u8; 20];
        hashbytes_20.copy_from_slice(&hashbytes[0..20]);

        Some(PoxAddress::Addr20(mainnet, addrtype, hashbytes_20))
    }

    /// Try to convert a Clarity value representation of the PoX address into a
    /// PoxAddress::Addr32.
    fn try_addr32_from_pox_tuple(
        mainnet: bool,
        hashmode_u8: u8,
        hashbytes: &[u8],
    ) -> Option<PoxAddress> {
        let addrtype = PoxAddressType32::from_u8(hashmode_u8)?;

        // this is a valid PoxAddressType32, so there must be exactly 32 bytes
        if hashbytes.len() != 32 {
            return None;
        }

        let mut hashbytes_32 = [0u8; 32];
        hashbytes_32.copy_from_slice(&hashbytes[0..32]);

        Some(PoxAddress::Addr32(mainnet, addrtype, hashbytes_32))
    }

    /// Try to convert a Clarity value representation of the PoX address into a PoxAddress.
    /// `value` must be `{ version: (buff 1), hashbytes: (buff 32) }`
    pub fn try_from_pox_tuple(mainnet: bool, value: &Value) -> Option<PoxAddress> {
        let tuple_data = match value {
            Value::Tuple(data) => data.clone(),
            _ => {
                return None;
            }
        };

        let hashmode_value = tuple_data.get("version").ok()?.to_owned();

        let hashmode_u8 = match hashmode_value {
            Value::Sequence(SequenceData::Buffer(data)) => {
                if data.data.len() == 1 {
                    data.data[0]
                } else {
                    return None;
                }
            }
            _ => {
                return None;
            }
        };

        let hashbytes_value = tuple_data.get("hashbytes").ok()?.to_owned();
        let hashbytes_vec = match hashbytes_value {
            Value::Sequence(SequenceData::Buffer(data)) => data.data,
            _ => {
                return None;
            }
        };

        // try to decode
        if let Some(addr) =
            PoxAddress::try_standard_from_pox_tuple(mainnet, hashmode_u8, &hashbytes_vec)
        {
            return Some(addr);
        }
        if let Some(addr) =
            PoxAddress::try_addr20_from_pox_tuple(mainnet, hashmode_u8, &hashbytes_vec)
        {
            return Some(addr);
        }
        if let Some(addr) =
            PoxAddress::try_addr32_from_pox_tuple(mainnet, hashmode_u8, &hashbytes_vec)
        {
            return Some(addr);
        }
        None
    }

    /// Serialize this structure to a string that we can store in the sortition DB
    pub fn to_db_string(&self) -> String {
        serde_json::to_string(self).expect("FATAL: failed to serialize JSON value")
    }

    /// Decode a db string back into a PoxAddress
    pub fn from_db_string(db_string: &str) -> Option<PoxAddress> {
        serde_json::from_str(db_string).ok()?
    }

    /// Is this a burn address?
    pub fn is_burn(&self) -> bool {
        match *self {
            PoxAddress::Standard(ref addr, _) => addr.is_burn(),
            _ => false,
        }
    }

    /// What is the burnchain representation of this address?
    /// Used for comparing addresses from block-commits, where certain information (e.g. the hash
    /// mode) can't be used since it's not stored there.  The resulting string encodes all of the
    /// information that is present on the burnchain, and it does so in a _stable_ way.
    pub fn to_burnchain_repr(&self) -> String {
        match *self {
            PoxAddress::Standard(ref addr, _) => {
                format!("{:02x}-{}", &addr.version, &addr.bytes)
            }
            PoxAddress::Addr20(_, ref addrtype, ref addrbytes) => {
                format!("{:02x}-{}", addrtype.to_u8(), to_hex(addrbytes))
            }
            PoxAddress::Addr32(_, ref addrtype, ref addrbytes) => {
                format!("{:02x}-{}", addrtype.to_u8(), to_hex(addrbytes))
            }
        }
    }

    /// Make a standard burn address, i.e. as a legacy p2pkh address comprised of all 0's.
    /// NOTE: this is used to represent both PoB outputs, as well as to back-fill reward set data
    /// when storing a reward cycle's sortition for which there are no output slots.  This means
    /// that the behavior of this method is *consensus critical*
    pub fn standard_burn_address(mainnet: bool) -> PoxAddress {
        PoxAddress::Standard(
            StacksAddress::burn_address(mainnet),
            Some(AddressHashMode::SerializeP2PKH),
        )
    }

    /// Convert this PoxAddress into a Clarity value.
    /// Returns None if the address hash mode is not known (i.e. this only works for PoxAddresses
    /// constructed from a PoX tuple in the PoX contract).
    pub fn as_clarity_tuple(&self) -> Option<TupleData> {
        match *self {
            PoxAddress::Standard(ref addr, ref hm) => {
                let hm = match hm {
                    Some(hm) => hm,
                    None => {
                        return None;
                    }
                };
                let version = Value::buff_from_byte(*hm as u8);
                let hashbytes = Value::buff_from(Vec::from(addr.bytes.0.clone()))
                    .expect("FATAL: hash160 does not fit into a Clarity value");

                let tuple_data = TupleData::from_data(vec![
                    ("version".into(), version),
                    ("hashbytes".into(), hashbytes),
                ])
                .expect("FATAL: cannot encode PoxAddress::Standard as a Clarity tuple");

                Some(tuple_data)
            }
            PoxAddress::Addr20(ref _mainnet, ref addrtype, ref addrbytes) => {
                let version = Value::buff_from_byte(*addrtype as u8);
                let hashbytes = Value::buff_from(Vec::from(addrbytes.clone()))
                    .expect("FATAL: could not create a 20-byte buffer");

                let tuple_data = TupleData::from_data(vec![
                    ("version".into(), version),
                    ("hashbytes".into(), hashbytes),
                ])
                .expect("FATAL: Cannot fit PoxAddress::Addr20 as a Clarity tuple");

                Some(tuple_data)
            }
            PoxAddress::Addr32(ref _mainnet, ref addrtype, ref addrbytes) => {
                let version = Value::buff_from_byte(*addrtype as u8);
                let hashbytes = Value::buff_from(Vec::from(addrbytes.clone()))
                    .expect("FATAL: could not create a 32-byte buffer");

                let tuple_data = TupleData::from_data(vec![
                    ("version".into(), version),
                    ("hashbytes".into(), hashbytes),
                ])
                .expect("FATAL: Cannot fit PoxAddress::Addr32 as a Clarity tuple");

                Some(tuple_data)
            }
        }
    }

    /// Coerce a hash mode for this address if it is standard.
    ///
    /// WARNING
    /// The hash mode may not reflect the true nature of the address, since segwit-p2sh and p2sh
    /// are indistinguishable.  Use with caution.
    pub fn coerce_hash_mode(self) -> PoxAddress {
        match self {
            PoxAddress::Standard(addr, _) => {
                let hm = AddressHashMode::from_version(addr.version);
                PoxAddress::Standard(addr, Some(hm))
            }
            _ => self,
        }
    }

    /// Try to convert this into a standard StacksAddress.
    /// With Bitcoin, this means a legacy address
    pub fn try_into_stacks_address(self) -> Option<StacksAddress> {
        match self {
            PoxAddress::Standard(addr, _) => Some(addr),
            _ => None,
        }
    }

    /// Convert this PoxAddress into a base58check string
    pub fn to_b58(self) -> String {
        match self {
            PoxAddress::Standard(addr, _) => addr.to_b58(),
            PoxAddress::Addr20(mainnet, addrtype, addrbytes) => match addrtype {
                PoxAddressType20::P2WPKH => {
                    let btc_addr = SegwitBitcoinAddress::P2WPKH(mainnet, addrbytes);
                    btc_addr.to_bech32()
                }
            },
            PoxAddress::Addr32(mainnet, addrtype, addrbytes) => match addrtype {
                PoxAddressType32::P2WSH => {
                    let btc_addr = SegwitBitcoinAddress::P2WSH(mainnet, addrbytes);
                    btc_addr.to_bech32()
                }
                PoxAddressType32::P2TR => {
                    let btc_addr = SegwitBitcoinAddress::P2TR(mainnet, addrbytes);
                    btc_addr.to_bech32()
                }
            },
        }
    }

    // Convert from a B58 encoded bitcoin address
    pub fn from_b58(input: &str) -> Option<Self> {
        let btc_addr = BitcoinAddress::from_string(input)?;
        PoxAddress::try_from_bitcoin_output(&BitcoinTxOutput {
            address: btc_addr,
            units: 0,
        })
    }

    /// Convert this PoxAddress into a Bitcoin tx output
    pub fn to_bitcoin_tx_out(&self, value: u64) -> TxOut {
        match *self {
            PoxAddress::Standard(addr, _) => {
                // legacy Bitcoin address
                let btc_version = to_b58_version_byte(addr.version).expect(
                    "BUG: failed to decode Stacks version byte to legacy Bitcoin version byte",
                );
                let btc_addr_type = legacy_version_byte_to_address_type(btc_version)
                    .expect("BUG: failed to decode legacy Bitcoin version byte")
                    .0;
                match btc_addr_type {
                    LegacyBitcoinAddressType::PublicKeyHash => {
                        LegacyBitcoinAddress::to_p2pkh_tx_out(&addr.bytes, value)
                    }
                    LegacyBitcoinAddressType::ScriptHash => {
                        LegacyBitcoinAddress::to_p2sh_tx_out(&addr.bytes, value)
                    }
                }
            }
            PoxAddress::Addr20(_, ref addrtype, ref addrbytes) => match *addrtype {
                PoxAddressType20::P2WPKH => {
                    SegwitBitcoinAddress::to_p2wpkh_tx_out(addrbytes, value)
                }
            },
            PoxAddress::Addr32(_, ref addrtype, ref addrbytes) => match *addrtype {
                PoxAddressType32::P2WSH => SegwitBitcoinAddress::to_p2wsh_tx_out(addrbytes, value),
                PoxAddressType32::P2TR => SegwitBitcoinAddress::to_p2tr_tx_out(addrbytes, value),
            },
        }
    }

    /// Try instantiating a PoxAddress from a Bitcoin tx output
    pub fn try_from_bitcoin_output(o: &BitcoinTxOutput) -> Option<PoxAddress> {
        match &o.address {
            BitcoinAddress::Legacy(ref legacy_addr) => {
                let addr = StacksAddress::from_legacy_bitcoin_address(legacy_addr);
                let pox_addr = PoxAddress::Standard(addr, None);
                Some(pox_addr)
            }
            BitcoinAddress::Segwit(ref segwit_addr) => {
                if segwit_addr.is_p2wpkh() {
                    let mut bytes20 = [0u8; 20];
                    bytes20.copy_from_slice(&segwit_addr.bytes_ref()[0..20]);
                    Some(PoxAddress::Addr20(
                        segwit_addr.is_mainnet(),
                        PoxAddressType20::P2WPKH,
                        bytes20,
                    ))
                } else if segwit_addr.is_p2wsh() {
                    let mut bytes32 = [0u8; 32];
                    bytes32.copy_from_slice(&segwit_addr.bytes_ref()[0..32]);
                    Some(PoxAddress::Addr32(
                        segwit_addr.is_mainnet(),
                        PoxAddressType32::P2WSH,
                        bytes32,
                    ))
                } else if segwit_addr.is_p2tr() {
                    let mut bytes32 = [0u8; 32];
                    bytes32.copy_from_slice(&segwit_addr.bytes_ref()[0..32]);
                    Some(PoxAddress::Addr32(
                        segwit_addr.is_mainnet(),
                        PoxAddressType32::P2TR,
                        bytes32,
                    ))
                } else {
                    None
                }
            }
        }
    }

    /// Construct from hash mode and hash160
    #[cfg(any(test, feature = "testing"))]
    pub fn from_legacy(hash_mode: AddressHashMode, hash_bytes: Hash160) -> PoxAddress {
        PoxAddress::Standard(
            StacksAddress {
                version: hash_mode.to_version_testnet(),
                bytes: hash_bytes,
            },
            Some(hash_mode),
        )
    }
}

impl StacksAddressExtensions for StacksAddress {
    /// is this a boot code address, if the supplied address is mainnet or testnet,
    ///  it checks against the appropriate the boot code addr
    fn is_boot_code_addr(&self) -> bool {
        self == &boot_code_addr(self.is_mainnet())
    }

    /// Convert from a Bitcoin address
    /// WARNING: this does not distinguish between p2sh and segwit-p2sh
    fn from_legacy_bitcoin_address(addr: &LegacyBitcoinAddress) -> StacksAddress {
        let btc_version = legacy_address_type_to_version_byte(addr.addrtype, addr.network_id);

        // should not fail by construction
        let version = to_c32_version_byte(btc_version)
            .expect("Failed to decode Bitcoin version byte to Stacks version byte");
        StacksAddress {
            version,
            bytes: addr.bytes.clone(),
        }
    }

    fn to_b58(self) -> String {
        let StacksAddress { version, bytes } = self;
        let btc_version = to_b58_version_byte(version)
            // fallback to version
            .unwrap_or(version);
        let mut all_bytes = vec![btc_version];
        all_bytes.extend(bytes.0.iter());
        b58::check_encode_slice(&all_bytes)
    }
}

#[cfg(test)]
mod test {
    use clarity::vm::types::BuffData;
    use stacks_common::util::hash::*;
    use stacks_common::util::secp256k1::Secp256k1PublicKey as PubKey;

    use super::*;
    use crate::burnchains::bitcoin::BitcoinNetworkType;
    use crate::chainstate::stacks::*;
    use crate::net::codec::test::check_codec_and_corruption;
    use crate::net::codec::*;
    use crate::net::*;

    #[test]
    fn tx_stacks_address_codec() {
        let addr = StacksAddress {
            version: 1,
            bytes: Hash160([0xff; 20]),
        };
        let addr_bytes = vec![
            // version
            0x01, // bytes
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ];

        check_codec_and_corruption::<StacksAddress>(&addr, &addr_bytes);
    }

    #[test]
    fn tx_stacks_address_valid_p2pkh() {
        // p2pkh should accept compressed or uncompressed
        assert_eq!(StacksAddress::from_public_keys(1, &AddressHashMode::SerializeP2PKH, 1, &vec![PubKey::from_hex("04b7c7cbe36a1aed38c6324b143584a1e822bbf0c4435b102f0497ccb592baf8e964a5a270f9348285595b78855c3e33dc36708e34f9abdeeaad4d2977cb81e3a1").unwrap()]),
                   Some(StacksAddress { version: 1, bytes: Hash160::from_hex("560ee9d7f5694dd4dbeddf55eff16bcc05409fef").unwrap() }));

        assert_eq!(
            StacksAddress::from_public_keys(
                2,
                &AddressHashMode::SerializeP2PKH,
                1,
                &vec![PubKey::from_hex(
                    "03b7c7cbe36a1aed38c6324b143584a1e822bbf0c4435b102f0497ccb592baf8e9"
                )
                .unwrap()]
            ),
            Some(StacksAddress {
                version: 2,
                bytes: Hash160::from_hex("e3771b5724d9a8daca46052bab5d0f533cd1e619").unwrap()
            })
        );

        // should fail if we have too many signatures
        assert_eq!(
            StacksAddress::from_public_keys(
                2,
                &AddressHashMode::SerializeP2PKH,
                2,
                &vec![PubKey::from_hex(
                    "03b7c7cbe36a1aed38c6324b143584a1e822bbf0c4435b102f0497ccb592baf8e9"
                )
                .unwrap()]
            ),
            None
        )
    }

    #[test]
    fn tx_stacks_address_valid_p2wpkh() {
        // p2wpkh should accept only compressed keys
        assert_eq!(StacksAddress::from_public_keys(3, &AddressHashMode::SerializeP2WPKH, 1, &vec![PubKey::from_hex("04b7c7cbe36a1aed38c6324b143584a1e822bbf0c4435b102f0497ccb592baf8e964a5a270f9348285595b78855c3e33dc36708e34f9abdeeaad4d2977cb81e3a1").unwrap()]),
                  None);

        assert_eq!(
            StacksAddress::from_public_keys(
                4,
                &AddressHashMode::SerializeP2WPKH,
                1,
                &vec![PubKey::from_hex(
                    "02dd42250a8b45dd22c7f9dc7a8d387b6c7194ed3eff23c172c80bf8bee23c9047"
                )
                .unwrap()]
            ),
            Some(StacksAddress {
                version: 4,
                bytes: Hash160::from_hex("384d172898686fd0337fba27843add64cbe684f1").unwrap()
            })
        );
    }

    #[test]
    fn tx_stacks_address_valid_p2sh() {
        // p2sh may accept compressed or uncompressed
        assert_eq!(
            StacksAddress::from_public_keys(
                5,
                &AddressHashMode::SerializeP2SH,
                2,
                &vec![
                    PubKey::from_hex(
                        "02b30fafab3a12372c5d150d567034f37d60a91168009a779498168b0e9d8ec7f2"
                    )
                    .unwrap(),
                    PubKey::from_hex(
                        "03ce61f1d155738a5e434fc8a61c3e104f891d1ec71576e8ad85abb68b34670d35"
                    )
                    .unwrap(),
                    PubKey::from_hex(
                        "03ef2340518b5867b23598a9cf74611f8b98064f7d55cdb8c107c67b5efcbc5c77"
                    )
                    .unwrap()
                ]
            ),
            Some(StacksAddress {
                version: 5,
                bytes: Hash160::from_hex("b01162ecda72c57ed419f7966ec4e8dd7987c704").unwrap()
            })
        );

        assert_eq!(StacksAddress::from_public_keys(6, &AddressHashMode::SerializeP2SH, 2, &vec![PubKey::from_hex("04b30fafab3a12372c5d150d567034f37d60a91168009a779498168b0e9d8ec7f259fc6bc2f317febe245344d9e11912427cee095b64418719207ac502e8cff0ce").unwrap(),
                                                                                                PubKey::from_hex("04ce61f1d155738a5e434fc8a61c3e104f891d1ec71576e8ad85abb68b34670d35c61aec8a973b3b7d68c7325b03c1d18a82e88998b8307afeaa491c1e45e46255").unwrap(),
                                                                                                PubKey::from_hex("04ef2340518b5867b23598a9cf74611f8b98064f7d55cdb8c107c67b5efcbc5c771f112f919b00a6c6c5f51f7c63e1762fe9fac9b66ec75a053db7f51f4a52712b").unwrap()]),
                   Some(StacksAddress { version: 6, bytes: Hash160::from_hex("1003ab7fc0ba18a343da2818c560109c170cdcbb").unwrap() }));
    }

    #[test]
    fn tx_stacks_address_valid_p2wsh() {
        // p2wsh should accept only compressed keys
        assert_eq!(
            StacksAddress::from_public_keys(
                7,
                &AddressHashMode::SerializeP2WSH,
                2,
                &vec![
                    PubKey::from_hex(
                        "02b30fafab3a12372c5d150d567034f37d60a91168009a779498168b0e9d8ec7f2"
                    )
                    .unwrap(),
                    PubKey::from_hex(
                        "03ce61f1d155738a5e434fc8a61c3e104f891d1ec71576e8ad85abb68b34670d35"
                    )
                    .unwrap(),
                    PubKey::from_hex(
                        "03ef2340518b5867b23598a9cf74611f8b98064f7d55cdb8c107c67b5efcbc5c77"
                    )
                    .unwrap()
                ]
            ),
            Some(StacksAddress {
                version: 7,
                bytes: Hash160::from_hex("57130f08a480e7518c1d685e8bb88008d90a0a60").unwrap()
            })
        );

        assert_eq!(StacksAddress::from_public_keys(8, &AddressHashMode::SerializeP2PKH, 2, &vec![PubKey::from_hex("04b30fafab3a12372c5d150d567034f37d60a91168009a779498168b0e9d8ec7f259fc6bc2f317febe245344d9e11912427cee095b64418719207ac502e8cff0ce").unwrap(),
                                                                                                 PubKey::from_hex("04ce61f1d155738a5e434fc8a61c3e104f891d1ec71576e8ad85abb68b34670d35c61aec8a973b3b7d68c7325b03c1d18a82e88998b8307afeaa491c1e45e46255").unwrap(),
                                                                                                 PubKey::from_hex("04ef2340518b5867b23598a9cf74611f8b98064f7d55cdb8c107c67b5efcbc5c771f112f919b00a6c6c5f51f7c63e1762fe9fac9b66ec75a053db7f51f4a52712b").unwrap()]),
                   None);
    }

    fn make_pox_addr_raw(version: u8, bytes: Vec<u8>) -> Value {
        Value::Tuple(
            TupleData::from_data(vec![
                (
                    ClarityName::try_from("version".to_owned()).unwrap(),
                    Value::buff_from_byte(version),
                ),
                (
                    ClarityName::try_from("hashbytes".to_owned()).unwrap(),
                    Value::Sequence(SequenceData::Buffer(BuffData { data: bytes })),
                ),
            ])
            .unwrap(),
        )
    }

    #[test]
    fn test_try_from_pox_tuple() {
        assert_eq!(
            PoxAddress::try_from_pox_tuple(true, &make_pox_addr_raw(0x00, vec![0x01; 20])).unwrap(),
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
                    bytes: Hash160([0x01; 20])
                },
                Some(AddressHashMode::SerializeP2PKH)
            )
        );
        assert_eq!(
            PoxAddress::try_from_pox_tuple(false, &make_pox_addr_raw(0x00, vec![0x02; 20]))
                .unwrap(),
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_TESTNET_SINGLESIG,
                    bytes: Hash160([0x02; 20])
                },
                Some(AddressHashMode::SerializeP2PKH)
            )
        );
        assert_eq!(
            PoxAddress::try_from_pox_tuple(true, &make_pox_addr_raw(0x01, vec![0x03; 20])).unwrap(),
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_MULTISIG,
                    bytes: Hash160([0x03; 20])
                },
                Some(AddressHashMode::SerializeP2SH)
            )
        );
        assert_eq!(
            PoxAddress::try_from_pox_tuple(false, &make_pox_addr_raw(0x01, vec![0x04; 20]))
                .unwrap(),
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_TESTNET_MULTISIG,
                    bytes: Hash160([0x04; 20])
                },
                Some(AddressHashMode::SerializeP2SH)
            )
        );
        assert_eq!(
            PoxAddress::try_from_pox_tuple(true, &make_pox_addr_raw(0x02, vec![0x05; 20])).unwrap(),
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_MULTISIG,
                    bytes: Hash160([0x05; 20])
                },
                Some(AddressHashMode::SerializeP2WPKH)
            )
        );
        assert_eq!(
            PoxAddress::try_from_pox_tuple(false, &make_pox_addr_raw(0x02, vec![0x06; 20]))
                .unwrap(),
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_TESTNET_MULTISIG,
                    bytes: Hash160([0x06; 20])
                },
                Some(AddressHashMode::SerializeP2WPKH)
            )
        );
        assert_eq!(
            PoxAddress::try_from_pox_tuple(true, &make_pox_addr_raw(0x03, vec![0x07; 20])).unwrap(),
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_MULTISIG,
                    bytes: Hash160([0x07; 20])
                },
                Some(AddressHashMode::SerializeP2WSH)
            )
        );
        assert_eq!(
            PoxAddress::try_from_pox_tuple(false, &make_pox_addr_raw(0x03, vec![0x08; 20]))
                .unwrap(),
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_TESTNET_MULTISIG,
                    bytes: Hash160([0x08; 20])
                },
                Some(AddressHashMode::SerializeP2WSH)
            )
        );
        assert_eq!(
            PoxAddress::try_from_pox_tuple(true, &make_pox_addr_raw(0x04, vec![0x09; 20])).unwrap(),
            PoxAddress::Addr20(true, PoxAddressType20::P2WPKH, [0x09; 20]),
        );
        assert_eq!(
            PoxAddress::try_from_pox_tuple(false, &make_pox_addr_raw(0x04, vec![0x0a; 20]))
                .unwrap(),
            PoxAddress::Addr20(false, PoxAddressType20::P2WPKH, [0x0a; 20]),
        );
        assert_eq!(
            PoxAddress::try_from_pox_tuple(true, &make_pox_addr_raw(0x05, vec![0x0b; 32])).unwrap(),
            PoxAddress::Addr32(true, PoxAddressType32::P2WSH, [0x0b; 32]),
        );
        assert_eq!(
            PoxAddress::try_from_pox_tuple(false, &make_pox_addr_raw(0x05, vec![0x0c; 32]))
                .unwrap(),
            PoxAddress::Addr32(false, PoxAddressType32::P2WSH, [0x0c; 32]),
        );
        assert_eq!(
            PoxAddress::try_from_pox_tuple(true, &make_pox_addr_raw(0x06, vec![0x0d; 32])).unwrap(),
            PoxAddress::Addr32(true, PoxAddressType32::P2TR, [0x0d; 32]),
        );
        assert_eq!(
            PoxAddress::try_from_pox_tuple(false, &make_pox_addr_raw(0x06, vec![0x0e; 32]))
                .unwrap(),
            PoxAddress::Addr32(false, PoxAddressType32::P2TR, [0x0e; 32]),
        );

        // unsupported version
        assert!(
            PoxAddress::try_from_pox_tuple(true, &make_pox_addr_raw(0x07, vec![0x0e; 32]))
                .is_none()
        );
        // bad payload length
        assert!(
            PoxAddress::try_from_pox_tuple(true, &make_pox_addr_raw(0x06, vec![0x0e; 33]))
                .is_none()
        );
        assert!(
            PoxAddress::try_from_pox_tuple(true, &make_pox_addr_raw(0x06, vec![0x0e; 31]))
                .is_none()
        );
        assert!(
            PoxAddress::try_from_pox_tuple(true, &make_pox_addr_raw(0x04, vec![0x0e; 32]))
                .is_none()
        );
        assert!(
            PoxAddress::try_from_pox_tuple(true, &make_pox_addr_raw(0x05, vec![0x0e; 20]))
                .is_none()
        );
        assert!(PoxAddress::try_from_pox_tuple(true, &make_pox_addr_raw(0x06, vec![])).is_none());
        // not a tuple
        assert!(PoxAddress::try_from_pox_tuple(true, &Value::UInt(3)).is_none());
        // bad tuple
        assert!(PoxAddress::try_from_pox_tuple(
            true,
            &Value::Tuple(
                TupleData::from_data(vec![
                    (
                        ClarityName::try_from("version".to_owned()).unwrap(),
                        Value::Sequence(SequenceData::Buffer(BuffData {
                            data: vec![0x01, 0x02],
                        }))
                    ),
                    (
                        ClarityName::try_from("hashbytes".to_owned()).unwrap(),
                        Value::Sequence(SequenceData::Buffer(BuffData {
                            data: vec![0x0e; 20]
                        })),
                    ),
                ])
                .unwrap()
            )
        )
        .is_none());
        // bad tuple
        assert!(PoxAddress::try_from_pox_tuple(
            true,
            &Value::Tuple(
                TupleData::from_data(vec![
                    (
                        ClarityName::try_from("version".to_owned()).unwrap(),
                        Value::Sequence(SequenceData::Buffer(BuffData { data: vec![] }))
                    ),
                    (
                        ClarityName::try_from("hashbytes".to_owned()).unwrap(),
                        Value::Sequence(SequenceData::Buffer(BuffData {
                            data: vec![0x0e; 20]
                        })),
                    ),
                ])
                .unwrap()
            )
        )
        .is_none());
        // bad tuple
        assert!(PoxAddress::try_from_pox_tuple(
            true,
            &Value::Tuple(
                TupleData::from_data(vec![
                    (
                        ClarityName::try_from("version-nope".to_owned()).unwrap(),
                        Value::Sequence(SequenceData::Buffer(BuffData { data: vec![0x01] }))
                    ),
                    (
                        ClarityName::try_from("hashbytes".to_owned()).unwrap(),
                        Value::Sequence(SequenceData::Buffer(BuffData {
                            data: vec![0x0e; 20]
                        })),
                    ),
                ])
                .unwrap()
            )
        )
        .is_none());
        // bad tuple
        assert!(PoxAddress::try_from_pox_tuple(
            true,
            &Value::Tuple(
                TupleData::from_data(vec![
                    (
                        ClarityName::try_from("version".to_owned()).unwrap(),
                        Value::Sequence(SequenceData::Buffer(BuffData { data: vec![0x01] }))
                    ),
                    (
                        ClarityName::try_from("hashbytes-nope".to_owned()).unwrap(),
                        Value::Sequence(SequenceData::Buffer(BuffData {
                            data: vec![0x0e; 20]
                        })),
                    ),
                ])
                .unwrap()
            )
        )
        .is_none());
    }

    #[test]
    fn test_as_clarity_tuple() {
        assert_eq!(
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
                    bytes: Hash160([0x01; 20])
                },
                Some(AddressHashMode::SerializeP2PKH)
            )
            .as_clarity_tuple()
            .unwrap(),
            make_pox_addr_raw(0x00, vec![0x01; 20])
                .expect_tuple()
                .unwrap()
        );
        assert_eq!(
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_TESTNET_SINGLESIG,
                    bytes: Hash160([0x02; 20])
                },
                Some(AddressHashMode::SerializeP2PKH)
            )
            .as_clarity_tuple()
            .unwrap(),
            make_pox_addr_raw(0x00, vec![0x02; 20])
                .expect_tuple()
                .unwrap()
        );
        assert!(PoxAddress::Standard(
            StacksAddress {
                version: C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
                bytes: Hash160([0x01; 20])
            },
            None
        )
        .as_clarity_tuple()
        .is_none());
        assert!(PoxAddress::Standard(
            StacksAddress {
                version: C32_ADDRESS_VERSION_TESTNET_SINGLESIG,
                bytes: Hash160([0x02; 20])
            },
            None
        )
        .as_clarity_tuple()
        .is_none(),);

        assert_eq!(
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_MULTISIG,
                    bytes: Hash160([0x01; 20])
                },
                Some(AddressHashMode::SerializeP2SH)
            )
            .as_clarity_tuple()
            .unwrap(),
            make_pox_addr_raw(0x01, vec![0x01; 20])
                .expect_tuple()
                .unwrap()
        );
        assert_eq!(
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_TESTNET_MULTISIG,
                    bytes: Hash160([0x02; 20])
                },
                Some(AddressHashMode::SerializeP2SH)
            )
            .as_clarity_tuple()
            .unwrap(),
            make_pox_addr_raw(0x01, vec![0x02; 20])
                .expect_tuple()
                .unwrap()
        );
        assert!(PoxAddress::Standard(
            StacksAddress {
                version: C32_ADDRESS_VERSION_MAINNET_MULTISIG,
                bytes: Hash160([0x01; 20])
            },
            None
        )
        .as_clarity_tuple()
        .is_none());
        assert!(PoxAddress::Standard(
            StacksAddress {
                version: C32_ADDRESS_VERSION_TESTNET_MULTISIG,
                bytes: Hash160([0x02; 20])
            },
            None
        )
        .as_clarity_tuple()
        .is_none(),);

        assert_eq!(
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_MULTISIG,
                    bytes: Hash160([0x01; 20])
                },
                Some(AddressHashMode::SerializeP2WPKH)
            )
            .as_clarity_tuple()
            .unwrap(),
            make_pox_addr_raw(0x02, vec![0x01; 20])
                .expect_tuple()
                .unwrap()
        );
        assert_eq!(
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_TESTNET_MULTISIG,
                    bytes: Hash160([0x02; 20])
                },
                Some(AddressHashMode::SerializeP2WPKH)
            )
            .as_clarity_tuple()
            .unwrap(),
            make_pox_addr_raw(0x02, vec![0x02; 20])
                .expect_tuple()
                .unwrap()
        );
        assert!(PoxAddress::Standard(
            StacksAddress {
                version: C32_ADDRESS_VERSION_MAINNET_MULTISIG,
                bytes: Hash160([0x01; 20])
            },
            None
        )
        .as_clarity_tuple()
        .is_none());
        assert!(PoxAddress::Standard(
            StacksAddress {
                version: C32_ADDRESS_VERSION_TESTNET_MULTISIG,
                bytes: Hash160([0x02; 20])
            },
            None
        )
        .as_clarity_tuple()
        .is_none(),);

        assert_eq!(
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_MULTISIG,
                    bytes: Hash160([0x01; 20])
                },
                Some(AddressHashMode::SerializeP2WSH)
            )
            .as_clarity_tuple()
            .unwrap(),
            make_pox_addr_raw(0x03, vec![0x01; 20])
                .expect_tuple()
                .unwrap()
        );
        assert_eq!(
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_TESTNET_MULTISIG,
                    bytes: Hash160([0x02; 20])
                },
                Some(AddressHashMode::SerializeP2WSH)
            )
            .as_clarity_tuple()
            .unwrap(),
            make_pox_addr_raw(0x03, vec![0x02; 20])
                .expect_tuple()
                .unwrap()
        );
        assert!(PoxAddress::Standard(
            StacksAddress {
                version: C32_ADDRESS_VERSION_MAINNET_MULTISIG,
                bytes: Hash160([0x01; 20])
            },
            None
        )
        .as_clarity_tuple()
        .is_none());
        assert!(PoxAddress::Standard(
            StacksAddress {
                version: C32_ADDRESS_VERSION_TESTNET_MULTISIG,
                bytes: Hash160([0x02; 20])
            },
            None
        )
        .as_clarity_tuple()
        .is_none(),);

        assert_eq!(
            PoxAddress::Addr20(true, PoxAddressType20::P2WPKH, [0x09; 20])
                .as_clarity_tuple()
                .unwrap(),
            make_pox_addr_raw(0x04, vec![0x09; 20])
                .expect_tuple()
                .unwrap()
        );
        assert_eq!(
            PoxAddress::Addr20(false, PoxAddressType20::P2WPKH, [0x09; 20])
                .as_clarity_tuple()
                .unwrap(),
            make_pox_addr_raw(0x04, vec![0x09; 20])
                .expect_tuple()
                .unwrap()
        );

        assert_eq!(
            PoxAddress::Addr32(true, PoxAddressType32::P2WSH, [0x09; 32])
                .as_clarity_tuple()
                .unwrap(),
            make_pox_addr_raw(0x05, vec![0x09; 32])
                .expect_tuple()
                .unwrap()
        );
        assert_eq!(
            PoxAddress::Addr32(false, PoxAddressType32::P2WSH, [0x09; 32])
                .as_clarity_tuple()
                .unwrap(),
            make_pox_addr_raw(0x05, vec![0x09; 32])
                .expect_tuple()
                .unwrap()
        );

        assert_eq!(
            PoxAddress::Addr32(true, PoxAddressType32::P2TR, [0x09; 32])
                .as_clarity_tuple()
                .unwrap(),
            make_pox_addr_raw(0x06, vec![0x09; 32])
                .expect_tuple()
                .unwrap()
        );
        assert_eq!(
            PoxAddress::Addr32(false, PoxAddressType32::P2TR, [0x09; 32])
                .as_clarity_tuple()
                .unwrap(),
            make_pox_addr_raw(0x06, vec![0x09; 32])
                .expect_tuple()
                .unwrap()
        );
    }

    #[test]
    fn test_to_bitcoin_tx_out() {
        assert_eq!(
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
                    bytes: Hash160([0x01; 20])
                },
                Some(AddressHashMode::SerializeP2PKH)
            )
            .to_bitcoin_tx_out(123)
            .script_pubkey
            .into_bytes(),
            hex_bytes("76a914010101010101010101010101010101010101010188ac").unwrap()
        );
        assert_eq!(
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_MULTISIG,
                    bytes: Hash160([0x01; 20])
                },
                Some(AddressHashMode::SerializeP2PKH)
            )
            .to_bitcoin_tx_out(123)
            .script_pubkey
            .into_bytes(),
            hex_bytes("a914010101010101010101010101010101010101010187").unwrap()
        );
        assert_eq!(
            PoxAddress::Addr20(true, PoxAddressType20::P2WPKH, [0x01; 20])
                .to_bitcoin_tx_out(123)
                .script_pubkey
                .into_bytes(),
            hex_bytes("00140101010101010101010101010101010101010101").unwrap()
        );
        assert_eq!(
            PoxAddress::Addr32(true, PoxAddressType32::P2WSH, [0x01; 32])
                .to_bitcoin_tx_out(123)
                .script_pubkey
                .into_bytes(),
            hex_bytes("00200101010101010101010101010101010101010101010101010101010101010101")
                .unwrap()
        );
        assert_eq!(
            PoxAddress::Addr32(true, PoxAddressType32::P2TR, [0x01; 32])
                .to_bitcoin_tx_out(123)
                .script_pubkey
                .into_bytes(),
            hex_bytes("51200101010101010101010101010101010101010101010101010101010101010101")
                .unwrap()
        );
    }

    #[test]
    fn test_pox_addr_from_b58() {
        // representative test PoxAddresses
        let pox_addrs: Vec<PoxAddress> = vec![
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
                    bytes: Hash160([0x01; 20]),
                },
                Some(AddressHashMode::SerializeP2PKH),
            ),
            PoxAddress::Addr20(true, PoxAddressType20::P2WPKH, [0x01; 20]),
            PoxAddress::Addr20(false, PoxAddressType20::P2WPKH, [0x01; 20]),
            PoxAddress::Addr32(true, PoxAddressType32::P2WSH, [0x01; 32]),
            PoxAddress::Addr32(false, PoxAddressType32::P2WSH, [0x01; 32]),
            PoxAddress::Addr32(true, PoxAddressType32::P2TR, [0x01; 32]),
            PoxAddress::Addr32(false, PoxAddressType32::P2TR, [0x01; 32]),
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_MULTISIG,
                    bytes: Hash160([0x01; 20]),
                },
                Some(AddressHashMode::SerializeP2SH),
            ),
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
                    bytes: Hash160([0x01; 20]),
                },
                Some(AddressHashMode::SerializeP2SH),
            ),
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_MULTISIG,
                    bytes: Hash160([0x01; 20]),
                },
                Some(AddressHashMode::SerializeP2WSH),
            ),
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_MULTISIG,
                    bytes: Hash160([0x01; 20]),
                },
                Some(AddressHashMode::SerializeP2WPKH),
            ),
        ];
        for addr in pox_addrs.iter() {
            let addr_str = addr.clone().to_b58();
            let addr_parsed = PoxAddress::from_b58(&addr_str).unwrap();
            let mut addr_checked = addr.clone();
            if let PoxAddress::Standard(_, ref mut hash_mode) = addr_checked {
                hash_mode.take();
            }
            assert_eq!(&addr_parsed, &addr_checked);
        }
    }

    #[test]
    fn test_try_from_bitcoin_output() {
        assert_eq!(
            PoxAddress::try_from_bitcoin_output(&BitcoinTxOutput {
                address: BitcoinAddress::from_scriptpubkey(
                    BitcoinNetworkType::Mainnet,
                    &hex_bytes("76a914010101010101010101010101010101010101010188ac").unwrap()
                )
                .unwrap(),
                units: 123
            })
            .unwrap(),
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
                    bytes: Hash160([0x01; 20])
                },
                None
            )
        );
        assert_eq!(
            PoxAddress::try_from_bitcoin_output(&BitcoinTxOutput {
                address: BitcoinAddress::from_scriptpubkey(
                    BitcoinNetworkType::Mainnet,
                    &hex_bytes("a914010101010101010101010101010101010101010187").unwrap()
                )
                .unwrap(),
                units: 123
            })
            .unwrap(),
            PoxAddress::Standard(
                StacksAddress {
                    version: C32_ADDRESS_VERSION_MAINNET_MULTISIG,
                    bytes: Hash160([0x01; 20])
                },
                None
            )
        );
        assert_eq!(
            PoxAddress::try_from_bitcoin_output(&BitcoinTxOutput {
                address: BitcoinAddress::from_scriptpubkey(
                    BitcoinNetworkType::Mainnet,
                    &hex_bytes("00140101010101010101010101010101010101010101").unwrap()
                )
                .unwrap(),
                units: 123
            })
            .unwrap(),
            PoxAddress::Addr20(true, PoxAddressType20::P2WPKH, [0x01; 20])
        );
        assert_eq!(
            PoxAddress::try_from_bitcoin_output(&BitcoinTxOutput {
                address: BitcoinAddress::from_scriptpubkey(
                    BitcoinNetworkType::Mainnet,
                    &hex_bytes(
                        "00200101010101010101010101010101010101010101010101010101010101010101"
                    )
                    .unwrap()
                )
                .unwrap(),
                units: 123
            })
            .unwrap(),
            PoxAddress::Addr32(true, PoxAddressType32::P2WSH, [0x01; 32])
        );
        assert_eq!(
            PoxAddress::try_from_bitcoin_output(&BitcoinTxOutput {
                address: BitcoinAddress::from_scriptpubkey(
                    BitcoinNetworkType::Mainnet,
                    &hex_bytes(
                        "51200101010101010101010101010101010101010101010101010101010101010101"
                    )
                    .unwrap()
                )
                .unwrap(),
                units: 123
            })
            .unwrap(),
            PoxAddress::Addr32(true, PoxAddressType32::P2TR, [0x01; 32])
        );
    }
}
