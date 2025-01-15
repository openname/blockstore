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

use std::char::from_digit;
use std::fmt::Write;
use std::{fmt, mem};

use ripemd::Ripemd160;
use serde::de::{Deserialize, Error as de_Error};
use serde::Serialize;
use sha2::{Digest, Sha256, Sha512, Sha512_256};
use sha3::Keccak256;

use crate::types::StacksPublicKeyBuffer;
use crate::util::pair::*;
use crate::util::secp256k1::Secp256k1PublicKey;
use crate::util::uint::Uint256;
use crate::util::HexError;

// hash function for Merkle trees
pub trait MerkleHashFunc {
    fn empty() -> Self
    where
        Self: Sized;
    fn from_tagged_data(tag: u8, data: &[u8]) -> Self
    where
        Self: Sized;
    fn bits(&self) -> &[u8];
}

macro_rules! impl_serde_json_hex_string {
    ($name:ident, $len:expr) => {
        pub struct $name {}
        impl $name {
            pub fn json_serialize<S: serde::Serializer>(
                inst: &[u8; $len],
                s: S,
            ) -> Result<S::Ok, S::Error> {
                let hex_inst = to_hex(inst);
                s.serialize_str(&hex_inst.as_str())
            }

            pub fn json_deserialize<'de, D: serde::Deserializer<'de>>(
                d: D,
            ) -> Result<[u8; $len], D::Error> {
                let hex_inst = String::deserialize(d)?;
                let inst_bytes = hex_bytes(&hex_inst).map_err(de_Error::custom)?;

                match inst_bytes.len() {
                    $len => {
                        let mut byte_slice = [0u8; $len];
                        byte_slice.copy_from_slice(&inst_bytes);
                        Ok(byte_slice)
                    }
                    _ => Err(de_Error::custom(format!(
                        "Invalid hex string -- not {} bytes",
                        $len
                    ))),
                }
            }
        }
    };
}

impl_serde_json_hex_string!(Hash20, 20);
impl_serde_json_hex_string!(Hash32, 32);
impl_serde_json_hex_string!(Hash64, 64);

#[derive(Serialize, Deserialize)]
pub struct Hash160(
    #[serde(
        serialize_with = "Hash20::json_serialize",
        deserialize_with = "Hash20::json_deserialize"
    )]
    pub [u8; 20],
);
impl_array_newtype!(Hash160, u8, 20);
impl_array_hexstring_fmt!(Hash160);
impl_byte_array_newtype!(Hash160, u8, 20);
pub const HASH160_ENCODED_SIZE: u32 = 20;

#[derive(Serialize, Deserialize)]
pub struct Keccak256Hash(
    #[serde(
        serialize_with = "Hash32::json_serialize",
        deserialize_with = "Hash32::json_deserialize"
    )]
    pub [u8; 32],
);
impl_array_newtype!(Keccak256Hash, u8, 32);
impl_array_hexstring_fmt!(Keccak256Hash);
impl_byte_array_newtype!(Keccak256Hash, u8, 32);

#[derive(Serialize, Deserialize)]
pub struct Sha256Sum(
    #[serde(
        serialize_with = "Hash32::json_serialize",
        deserialize_with = "Hash32::json_deserialize"
    )]
    pub [u8; 32],
);
impl_array_newtype!(Sha256Sum, u8, 32);
impl_array_hexstring_fmt!(Sha256Sum);
impl_byte_array_newtype!(Sha256Sum, u8, 32);

impl Default for Sha256Sum {
    fn default() -> Self {
        Sha256Sum::zero()
    }
}

#[derive(Serialize, Deserialize)]
pub struct Sha512Sum(
    #[serde(
        serialize_with = "Hash64::json_serialize",
        deserialize_with = "Hash64::json_deserialize"
    )]
    pub [u8; 64],
);
impl_array_newtype!(Sha512Sum, u8, 64);
impl_array_hexstring_fmt!(Sha512Sum);
impl_byte_array_newtype!(Sha512Sum, u8, 64);

#[derive(Serialize, Deserialize)]
pub struct Sha512Trunc256Sum(
    #[serde(
        serialize_with = "Hash32::json_serialize",
        deserialize_with = "Hash32::json_deserialize"
    )]
    pub [u8; 32],
);
impl_array_newtype!(Sha512Trunc256Sum, u8, 32);
impl_array_hexstring_fmt!(Sha512Trunc256Sum);
impl_byte_array_newtype!(Sha512Trunc256Sum, u8, 32);

#[derive(Serialize, Deserialize)]
pub struct DoubleSha256(
    #[serde(
        serialize_with = "Hash32::json_serialize",
        deserialize_with = "Hash32::json_deserialize"
    )]
    pub [u8; 32],
);
impl_array_newtype!(DoubleSha256, u8, 32);
impl_array_hexstring_fmt!(DoubleSha256);
impl_byte_array_newtype!(DoubleSha256, u8, 32);
pub const DOUBLE_SHA256_ENCODED_SIZE: u32 = 32;

#[derive(Debug, PartialEq, Clone)]
#[repr(C)]
pub enum MerklePathOrder {
    Left = 0x02,
    Right = 0x03,
}

const MERKLE_PATH_LEAF_TAG: u8 = 0x00;
const MERKLE_PATH_NODE_TAG: u8 = 0x01;

impl Hash160 {
    pub fn from_sha256(sha256_hash: &[u8; 32]) -> Hash160 {
        let mut rmd = Ripemd160::new();
        let mut ret = [0u8; 20];
        rmd.update(sha256_hash);
        ret.copy_from_slice(rmd.finalize().as_slice());
        Hash160(ret)
    }

    /// Create a hash by hashing some data
    // (borrowed from Andrew Poelstra)
    pub fn from_data(data: &[u8]) -> Hash160 {
        let sha2_result = Sha256::digest(data);
        let ripe_160_result = Ripemd160::digest(sha2_result.as_slice());
        Hash160::from(ripe_160_result.as_slice())
    }

    pub fn from_node_public_key(pubkey: &Secp256k1PublicKey) -> Hash160 {
        Hash160::from_data(&pubkey.to_bytes_compressed())
    }

    pub fn from_node_public_key_buffer(pubkey_buf: &StacksPublicKeyBuffer) -> Hash160 {
        Hash160::from_data(pubkey_buf.as_bytes())
    }
}

impl Sha512Sum {
    pub fn from_data(data: &[u8]) -> Sha512Sum {
        Sha512Sum::from(Sha512::digest(data).as_slice())
    }
}

impl Sha512Trunc256Sum {
    pub fn from_data(data: &[u8]) -> Sha512Trunc256Sum {
        Sha512Trunc256Sum::from(Sha512_256::digest(data).as_slice())
    }
    pub fn from_hasher(hasher: Sha512_256) -> Sha512Trunc256Sum {
        Sha512Trunc256Sum::from(hasher.finalize().as_slice())
    }
}

impl MerkleHashFunc for Hash160 {
    fn empty() -> Hash160 {
        Hash160([0u8; 20])
    }

    fn from_tagged_data(tag: u8, data: &[u8]) -> Hash160 {
        let mut tmp = [0u8; 32];
        let mut sha2 = Sha256::new();
        sha2.update([tag]);
        sha2.update(data);
        tmp.copy_from_slice(sha2.finalize().as_slice());
        Hash160::from_sha256(&tmp)
    }

    fn bits(&self) -> &[u8] {
        &self.0
    }
}

impl MerkleHashFunc for Sha256Sum {
    fn empty() -> Sha256Sum {
        Sha256Sum([0u8; 32])
    }

    fn from_tagged_data(tag: u8, data: &[u8]) -> Sha256Sum {
        let mut tmp = [0u8; 32];

        let mut sha2 = Sha256::new();
        sha2.update([tag]);
        sha2.update(data);
        tmp.copy_from_slice(sha2.finalize().as_slice());

        Sha256Sum(tmp)
    }

    fn bits(&self) -> &[u8] {
        &self.0
    }
}

impl MerkleHashFunc for DoubleSha256 {
    fn empty() -> DoubleSha256 {
        DoubleSha256([0u8; 32])
    }

    fn from_tagged_data(tag: u8, data: &[u8]) -> DoubleSha256 {
        let mut tmp = [0u8; 32];
        let mut tmp2 = [0u8; 32];

        let mut sha2_1 = Sha256::new();
        sha2_1.update([tag]);
        sha2_1.update(data);
        tmp.copy_from_slice(sha2_1.finalize().as_slice());

        let mut sha2_2 = Sha256::new();
        sha2_2.update(tmp);
        tmp2.copy_from_slice(sha2_2.finalize().as_slice());

        DoubleSha256(tmp2)
    }

    fn bits(&self) -> &[u8] {
        &self.0
    }
}

impl MerkleHashFunc for Sha512Trunc256Sum {
    fn empty() -> Sha512Trunc256Sum {
        Sha512Trunc256Sum([0u8; 32])
    }

    fn from_tagged_data(tag: u8, data: &[u8]) -> Sha512Trunc256Sum {
        use sha2::Digest;
        let mut tmp = [0u8; 32];

        let mut sha2 = Sha512_256::new();
        sha2.update([tag]);
        sha2.update(data);
        tmp.copy_from_slice(sha2.finalize().as_slice());

        Sha512Trunc256Sum(tmp)
    }

    fn bits(&self) -> &[u8] {
        &self.0
    }
}

impl Keccak256Hash {
    pub fn from_data(data: &[u8]) -> Keccak256Hash {
        Keccak256Hash(Keccak256::digest(data).into())
    }
}

impl Sha256Sum {
    pub fn from_data(data: &[u8]) -> Sha256Sum {
        Sha256Sum(Sha256::digest(data).into())
    }
    pub fn zero() -> Sha256Sum {
        Sha256Sum([0u8; 32])
    }
}

impl DoubleSha256 {
    pub fn from_data(data: &[u8]) -> DoubleSha256 {
        let hashed = Sha256::digest(Sha256::digest(data));
        DoubleSha256(hashed.into())
    }

    /// Converts a hash to a little-endian Uint256
    #[inline]
    pub fn into_le(self) -> Uint256 {
        let DoubleSha256(data) = self;
        let mut ret: [u64; 4] = unsafe { mem::transmute(data) };
        for x in ret.iter_mut() {
            *x = x.to_le();
        }
        Uint256(ret)
    }

    /// Converts a hash to a big-endian Uint256
    #[inline]
    pub fn into_be(self) -> Uint256 {
        let DoubleSha256(mut data) = self;
        data.reverse();
        let mut ret: [u64; 4] = unsafe { mem::transmute(data) };
        for x in ret.iter_mut() {
            *x = x.to_be();
        }
        Uint256(ret)
    }

    /// Human-readable hex output
    pub fn le_hex_string(&self) -> String {
        let &DoubleSha256(data) = self;
        let mut ret = String::with_capacity(64);
        for item in data.iter().take(32) {
            ret.push(from_digit((*item / 0x10) as u32, 16).unwrap());
            ret.push(from_digit((*item & 0x0f) as u32, 16).unwrap());
        }
        ret
    }

    /// Human-readable hex output
    pub fn be_hex_string(&self) -> String {
        let &DoubleSha256(data) = self;
        let mut ret = String::with_capacity(64);
        for i in (0..32).rev() {
            ret.push(from_digit((data[i] / 0x10) as u32, 16).unwrap());
            ret.push(from_digit((data[i] & 0x0f) as u32, 16).unwrap());
        }
        ret
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MerkleTree<H: MerkleHashFunc> {
    // nodes[0] is the list of leaves
    // nodes[-1][0] is the root
    nodes: Vec<Vec<H>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MerklePathPoint<H: MerkleHashFunc> {
    pub order: MerklePathOrder,
    pub hash: H,
}

pub type MerklePath<H> = Vec<MerklePathPoint<H>>;

/// Merkle tree implementation with tagged nodes:
///
/// * A leaf hash is `H(0x00 + data)`
/// * A node hash is `H(0x01 + left.hash + right.hash)`
///
/// An empty tree has a root hash of `0x00000...00000`.
///
/// NOTE: This is consensus-critical code, because it is used to generate the transaction Merkle
/// tree roots in Stacks blocks.
impl<H> MerkleTree<H>
where
    H: MerkleHashFunc + Clone + PartialEq + fmt::Debug,
{
    pub fn empty() -> MerkleTree<H> {
        MerkleTree { nodes: vec![] }
    }

    pub fn new(data: &[Vec<u8>]) -> MerkleTree<H> {
        if data.is_empty() {
            return MerkleTree { nodes: vec![] };
        }

        let mut leaf_hashes: Vec<H> = data
            .iter()
            .map(|buf| MerkleTree::get_leaf_hash(&buf[..]))
            .collect();

        // force even number
        if leaf_hashes.len() % 2 != 0 {
            let dup = leaf_hashes[leaf_hashes.len() - 1].clone();
            leaf_hashes.push(dup);
        }

        let mut nodes = vec![];
        nodes.push(leaf_hashes);

        loop {
            // next row
            let i = nodes.len() - 1;
            let capacity = nodes[i].len().saturating_add(1) / 2;
            let mut row_hashes = Vec::with_capacity(capacity);

            for j in 0..(nodes[i].len() / 2) {
                let h = MerkleTree::get_node_hash(&nodes[i][2 * j], &nodes[i][2 * j + 1]);
                row_hashes.push(h);
            }

            if row_hashes.len() == 1 {
                // at root
                nodes.push(row_hashes);
                break;
            }

            // force even
            if row_hashes.len() % 2 != 0 {
                let dup = row_hashes[row_hashes.len() - 1].clone();
                row_hashes.push(dup);
            }
            nodes.push(row_hashes);
        }

        MerkleTree { nodes }
    }

    /// Get the leaf hash
    pub fn get_leaf_hash(leaf_data: &[u8]) -> H {
        H::from_tagged_data(MERKLE_PATH_LEAF_TAG, leaf_data)
    }

    /// Get a non-leaf hash
    pub fn get_node_hash(left: &H, right: &H) -> H {
        let iter = left.bits().iter();
        let iter = iter.chain(right.bits().iter());
        let buf = iter.copied().collect::<Vec<_>>();
        H::from_tagged_data(MERKLE_PATH_NODE_TAG, &buf)
    }

    /// Find a given hash in a merkle tree row
    fn find_hash_index(&self, hash: &H, row_index: usize) -> Option<usize> {
        if row_index >= self.nodes.len() {
            panic!(
                "Tried to index Merkle tree at height {} (>= {})",
                row_index,
                self.nodes.len()
            );
        }
        (0..self.nodes[row_index].len()).find(|&i| self.nodes[row_index][i] == *hash)
    }

    /// Given an index into the Merkle tree, find the pair of hashes
    /// that comprise a sibling pair.
    /// Panics if the row_index or hash_index values are invalid.  In particular:
    /// * row_index must be positive and less than the number of rows
    /// * hash_index must correspond to a hash in its row
    /// * if hash_index is even, then it must have a right sibling
    fn find_siblings(&self, row_index: usize, hash_index: usize) -> (H, H) {
        if row_index == self.nodes.len() - 1 {
            panic!("Tried to find sibling of root");
        }

        if row_index >= self.nodes.len() {
            panic!(
                "Tried to index Merkle tree at height {} (>= {})",
                row_index,
                self.nodes.len()
            );
        }
        if hash_index >= self.nodes[row_index].len() {
            panic!(
                "Tried to index Merkle tree at column {} (>= {}) in row {}",
                hash_index,
                self.nodes[row_index].len(),
                row_index
            );
        }

        if hash_index % 2 == 0 {
            if hash_index + 1 >= self.nodes[row_index].len() {
                panic!(
                    "Corrupt Merkle tree -- colunn {} is the last item in row {}",
                    hash_index, row_index
                );
            }

            // left sibling
            (
                self.nodes[row_index][hash_index].clone(),
                self.nodes[row_index][hash_index + 1].clone(),
            )
        } else {
            // right sibling
            (
                self.nodes[row_index][hash_index - 1].clone(),
                self.nodes[row_index][hash_index].clone(),
            )
        }
    }

    /// Get the Merkle root hash.
    /// will be all 0's if the tree is empty.
    pub fn root(&self) -> H {
        if !self.nodes.is_empty() {
            if !self.nodes[self.nodes.len() - 1].is_empty() {
                self.nodes[self.nodes.len() - 1][0].clone()
            } else {
                H::empty()
            }
        } else {
            H::empty()
        }
    }

    /// Get the path from the given data's leaf up to the root.
    /// will be None if the data isn't a leaf.
    pub fn path(&self, data: &[u8]) -> Option<MerklePath<H>> {
        let leaf_hash = MerkleTree::get_leaf_hash(data);
        let mut hash_index = self.find_hash_index(&leaf_hash, 0)?;

        let mut path: MerklePath<H> = Vec::with_capacity(self.nodes.len());

        let mut next_hash = leaf_hash;

        for i in 0..self.nodes.len() - 1 {
            let (left, right) = self.find_siblings(i, hash_index);
            if next_hash == left {
                // this is the left hash
                path.push(MerklePathPoint {
                    order: MerklePathOrder::Left,
                    hash: right.clone(),
                });
            } else {
                // this is the right hash
                path.push(MerklePathPoint {
                    order: MerklePathOrder::Right,
                    hash: left.clone(),
                });
            }

            next_hash = MerkleTree::get_node_hash(&left, &right);
            hash_index = self.find_hash_index(&next_hash, i + 1)?;
        }

        Some(path)
    }

    /// Verify a datum and its Merkle path against a Merkle root
    pub fn path_verify(data: &[u8], path: &MerklePath<H>, root: &H) -> bool {
        if path.is_empty() {
            // invalid path
            return false;
        }

        let mut hash_acc = MerkleTree::get_leaf_hash(data);
        for path_point in path {
            match path_point.order {
                MerklePathOrder::Left => {
                    hash_acc = MerkleTree::get_node_hash(&hash_acc, &path_point.hash);
                }
                MerklePathOrder::Right => {
                    hash_acc = MerkleTree::get_node_hash(&path_point.hash, &hash_acc);
                }
            }
        }

        hash_acc == *root
    }
}

// borrowed from Andrew Poelstra's rust-bitcoin library
/// Convert a hexadecimal-encoded string to its corresponding bytes
pub fn hex_bytes(s: &str) -> Result<Vec<u8>, HexError> {
    let mut v = vec![];
    let mut iter = s.chars().pair();
    // Do the parsing
    iter.by_ref()
        .try_fold((), |_, (f, s)| match (f.to_digit(16), s.to_digit(16)) {
            (None, _) => Err(HexError::BadCharacter(f)),
            (_, None) => Err(HexError::BadCharacter(s)),
            (Some(f), Some(s)) => {
                v.push((f * 0x10 + s) as u8);
                Ok(())
            }
        })?;
    // Check that there was no remainder
    match iter.remainder() {
        Some(_) => Err(HexError::BadLength(s.len())),
        None => Ok(v),
    }
}

/// Convert a binary-encoded string to its corresponding bytes
pub fn bin_bytes(s: &str) -> Result<Vec<u8>, HexError> {
    let mut v = Vec::with_capacity(s.len() / 8 + 1);
    let mut next = 0u8;
    for (i, c) in s.chars().rev().enumerate() {
        if c != '0' && c != '1' {
            return Err(HexError::BadCharacter(c));
        }
        if c == '1' {
            next |= 1 << (i % 8);
        }
        if i % 8 == 7 {
            v.push(next);
            next = 0;
        }
    }
    if s.len() % 8 != 0 {
        v.push(next);
    }
    v.reverse();
    Ok(v)
}

/// Convert a slice of u8 to a hex string
pub fn to_hex(s: &[u8]) -> String {
    let mut r = String::with_capacity(s.len() * 2);
    for b in s.iter() {
        write!(r, "{:02x}", b).unwrap();
    }
    r
}

/// Convert a slice of u8 into a binary string
pub fn to_bin(s: &[u8]) -> String {
    let mut r = String::with_capacity(s.len() * 8);
    for b in s.iter() {
        write!(r, "{:08b}", b).unwrap();
    }
    r
}

/// Convert a vec of u8 to a hex string
pub fn bytes_to_hex(s: &[u8]) -> String {
    to_hex(s)
}

#[cfg(test)]
mod test {
    use super::{bin_bytes, hex_bytes, to_bin, DoubleSha256, MerkleHashFunc, MerkleTree};

    struct MerkleTreeFixture {
        data: Vec<Vec<u8>>,
        res: Option<MerkleTree<DoubleSha256>>,
    }

    #[test]
    fn make_merkle_tree() {
        let fixtures = vec![
            MerkleTreeFixture {
                data: vec![],
                res: Some(MerkleTree { nodes: vec![] }),
            },
            MerkleTreeFixture {
                data: vec![
                    hex_bytes("0000000000000000000000000000000000000000000000000000000000000000").unwrap(),
                ],
                res: Some(MerkleTree {
                    nodes: vec![
                        vec![
                            DoubleSha256::from_vec(&hex_bytes("44cf874abb7d10b323d5f6bf5bd4a5f25e3fe3d27fc74d59d7c258f4e5ed35c4").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("44cf874abb7d10b323d5f6bf5bd4a5f25e3fe3d27fc74d59d7c258f4e5ed35c4").unwrap()).unwrap()
                        ],
                        vec![
                            DoubleSha256::from_vec(&hex_bytes("0486bee7283eb9a1251cf134e60635ea797ab54e5986b27c13ac83f03119d680").unwrap()).unwrap()
                        ]
                    ]
                })
            },
            MerkleTreeFixture {
                data: vec![
                    hex_bytes("0000000000000000000000000000000000000000000000000000000000000000").unwrap(),
                    hex_bytes("1111111111111111111111111111111111111111111111111111111111111111").unwrap(),
                ],
                res: Some(MerkleTree {
                    nodes: vec![
                        vec![
                            DoubleSha256::from_vec(&hex_bytes("44cf874abb7d10b323d5f6bf5bd4a5f25e3fe3d27fc74d59d7c258f4e5ed35c4").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("b7d2c0a06fc0bffb86fca086fe9ae87561bb4191b770d947f1f042387904405f").unwrap()).unwrap(),
                        ],
                        vec![
                            DoubleSha256::from_vec(&hex_bytes("5fb4b0c841e2d00964f6ddc2bc7c0eb75b3af02223b3900132744dfa8c22433f").unwrap()).unwrap(),
                        ],
                    ],
                })
            },
            MerkleTreeFixture {
                data: vec![
                    hex_bytes("0000000000000000000000000000000000000000000000000000000000000000").unwrap(),
                    hex_bytes("1111111111111111111111111111111111111111111111111111111111111111").unwrap(),
                    hex_bytes("2222222222222222222222222222222222222222222222222222222222222222").unwrap(),
                ],
                res: Some(MerkleTree {
                    nodes: vec![
                        vec![
                            DoubleSha256::from_vec(&hex_bytes("44cf874abb7d10b323d5f6bf5bd4a5f25e3fe3d27fc74d59d7c258f4e5ed35c4").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("b7d2c0a06fc0bffb86fca086fe9ae87561bb4191b770d947f1f042387904405f").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("a2737fd98f23cf619c3c1e7b85484ec864491c29aa8f5422c3e9e73c3213a79d").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("a2737fd98f23cf619c3c1e7b85484ec864491c29aa8f5422c3e9e73c3213a79d").unwrap()).unwrap(),
                        ],
                        vec![
                            DoubleSha256::from_vec(&hex_bytes("5fb4b0c841e2d00964f6ddc2bc7c0eb75b3af02223b3900132744dfa8c22433f").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("ae7c314ff379af325a26f408b6f883add2542a44f5c39313f545a42c25bad17c").unwrap()).unwrap(),
                        ],
                        vec![
                            DoubleSha256::from_vec(&hex_bytes("978d9d2ea33ce554e38fa49141f80e2cba770cdacc8d0da6605b4bbd31f50b3b").unwrap()).unwrap(),
                        ]
                    ]
                })
            },
            MerkleTreeFixture {
                data: vec![
                    hex_bytes("0000000000000000000000000000000000000000000000000000000000000000").unwrap(),
                    hex_bytes("1111111111111111111111111111111111111111111111111111111111111111").unwrap(),
                    hex_bytes("2222222222222222222222222222222222222222222222222222222222222222").unwrap(),
                    hex_bytes("3333333333333333333333333333333333333333333333333333333333333333").unwrap(),
                    hex_bytes("4444444444444444444444444444444444444444444444444444444444444444").unwrap(),
                ],
                res: Some(MerkleTree {
                    nodes: vec![
                        vec![
                            DoubleSha256::from_vec(&hex_bytes("44cf874abb7d10b323d5f6bf5bd4a5f25e3fe3d27fc74d59d7c258f4e5ed35c4").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("b7d2c0a06fc0bffb86fca086fe9ae87561bb4191b770d947f1f042387904405f").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("a2737fd98f23cf619c3c1e7b85484ec864491c29aa8f5422c3e9e73c3213a79d").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("9b1ab546065ba19b028bcac528162af25931c785e60d635db9038defbf022a4c").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("473effa680e4e10f28121cb8f8d34f2dbf6c8b89b2a3e59629180b1ea3d08849").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("473effa680e4e10f28121cb8f8d34f2dbf6c8b89b2a3e59629180b1ea3d08849").unwrap()).unwrap(),
                        ],
                        vec![
                            DoubleSha256::from_vec(&hex_bytes("5fb4b0c841e2d00964f6ddc2bc7c0eb75b3af02223b3900132744dfa8c22433f").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("cb985eb38b2184a9ebc0df8ea7b54579ffc25bc6a127e51a3e701b2ac0db73cc").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("2236b6e4c9f72a5d43ada53445afa045872663c1e674f8e7c2068e8377b224a6").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("2236b6e4c9f72a5d43ada53445afa045872663c1e674f8e7c2068e8377b224a6").unwrap()).unwrap(),
                        ],
                        vec![
                            DoubleSha256::from_vec(&hex_bytes("5f040e3625c217bba84f89a61c70cb954c848e035db28c0568a13c691f73fb73").unwrap()).unwrap(),
                            DoubleSha256::from_vec(&hex_bytes("9f8e10332f968166b526c6eea230d7f31d4f8f6cd2eb6d84b0c34320dc976b8b").unwrap()).unwrap(),
                        ],
                        vec![
                            DoubleSha256::from_vec(&hex_bytes("6695db0423ffd46dc936a35b454223c4ff663ceeaffbc30a970cf33c861e50a2").unwrap()).unwrap()
                        ]
                    ]
                })
            }
        ];

        for fixture in fixtures {
            let tree = MerkleTree::new(&fixture.data);

            assert_eq!(Some(tree.clone()), fixture.res);
            if fixture.res.is_some() {
                let nodes = fixture.res.unwrap().nodes;

                if !nodes.is_empty() {
                    assert_eq!(tree.root(), nodes[nodes.len() - 1][0]);
                } else {
                    assert_eq!(tree.root(), DoubleSha256::empty());
                }

                for d in fixture.data {
                    let path = tree.path(&d).unwrap();
                    assert_eq!(path.len(), tree.nodes.len() - 1);
                    assert!(MerkleTree::path_verify(&d, &path, &tree.root()));
                }

                if !nodes.is_empty() {
                    let no_path = tree.path(&hex_bytes("012345").unwrap());
                    assert!(no_path.is_none());
                }
            }
        }
    }

    #[test]
    fn test_bin_str_roundtrip() {
        assert_eq!(to_bin(&[42]), "00101010");
        assert_eq!(bin_bytes("00101010").unwrap(), vec![42]);
        assert_eq!(bin_bytes("101010").unwrap(), vec![42]);
        assert_eq!(bin_bytes("000101010").unwrap(), vec![0, 42]);
        assert_eq!(bin_bytes("1000101010").unwrap(), vec![2, 42]);

        assert_eq!(to_bin(&[255, 255]), "1111111111111111");
        assert_eq!(bin_bytes("1111111111111111").unwrap(), vec![255, 255]);

        assert_eq!(to_bin(&[127, 0, 0, 1]), "01111111000000000000000000000001");
        assert_eq!(
            bin_bytes("01111111000000000000000000000001").unwrap(),
            vec![127, 0, 0, 1]
        );
        assert_eq!(
            bin_bytes("1111111000000000000000000000001").unwrap(),
            vec![127, 0, 0, 1]
        );

        assert_eq!(bin_bytes("").unwrap().len(), 0);
        assert!(bin_bytes("2").is_err());
    }
}
