// Rust Bitcoin Library
// Written in 2014 by
//     Andrew Poelstra <apoelstra@wpsoftware.net>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the CC0 Public Domain Dedication
// along with this software.
// If not, see <http://creativecommons.org/publicdomain/zero/1.0/>.
//

//! Script
//!
//! Scripts define Bitcoin's digital signature scheme: a signature is formed
//! from a script (the second half of which is defined by a coin to be spent,
//! and the first half provided by the spending transaction), and is valid
//! iff the script leaves `TRUE` on the stack after being evaluated.
//! Bitcoin's script is a stack-based assembly language similar in spirit to
//! Forth.
//!
//! This module provides the structures and functions needed to support scripts.
//!

use std::mem::size_of;
use std::{error, fmt};

use sha2::{Digest, Sha256};

use crate::deps_common::bitcoin::blockdata::opcodes;
use crate::deps_common::bitcoin::network::encodable::{ConsensusDecodable, ConsensusEncodable};
use crate::deps_common::bitcoin::network::serialize::{self, SimpleDecoder, SimpleEncoder};
// careful...
use crate::deps_common::bitcoin::util::hash::Hash160;

#[derive(Clone, Default, PartialOrd, Ord, PartialEq, Eq, Hash)]
/// A Bitcoin script
pub struct Script(Box<[u8]>);

impl fmt::Debug for Script {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut index = 0;

        f.write_str("Script(")?;
        while index < self.0.len() {
            let opcode = opcodes::All::from(self.0[index]);
            index += 1;

            let data_len = if let opcodes::Class::PushBytes(n) = opcode.classify() {
                n as usize
            } else {
                match opcode {
                    opcodes::All::OP_PUSHDATA1 => {
                        if self.0.len() < index + 1 {
                            f.write_str("<unexpected end>")?;
                            break;
                        }
                        match read_uint(&self.0[index..], 1) {
                            Ok(n) => {
                                index += 1;
                                n
                            }
                            Err(_) => {
                                f.write_str("<bad length>")?;
                                break;
                            }
                        }
                    }
                    opcodes::All::OP_PUSHDATA2 => {
                        if self.0.len() < index + 2 {
                            f.write_str("<unexpected end>")?;
                            break;
                        }
                        match read_uint(&self.0[index..], 2) {
                            Ok(n) => {
                                index += 2;
                                n
                            }
                            Err(_) => {
                                f.write_str("<bad length>")?;
                                break;
                            }
                        }
                    }
                    opcodes::All::OP_PUSHDATA4 => {
                        if self.0.len() < index + 4 {
                            f.write_str("<unexpected end>")?;
                            break;
                        }
                        match read_uint(&self.0[index..], 4) {
                            Ok(n) => {
                                index += 4;
                                n
                            }
                            Err(_) => {
                                f.write_str("<bad length>")?;
                                break;
                            }
                        }
                    }
                    _ => 0,
                }
            };

            if index > 1 {
                f.write_str(" ")?;
            }
            // Write the opcode
            if opcode == opcodes::All::OP_PUSHBYTES_0 {
                f.write_str("OP_0")?;
            } else {
                write!(f, "{:?}", opcode)?;
            }
            // Write any pushdata
            if data_len > 0 {
                f.write_str(" ")?;
                if index + data_len <= self.0.len() {
                    for ch in &self.0[index..index + data_len] {
                        write!(f, "{:02x}", ch)?;
                    }
                    index += data_len;
                } else {
                    f.write_str("<push past end>")?;
                    break;
                }
            }
        }
        f.write_str(")")
    }
}

impl fmt::Display for Script {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl fmt::LowerHex for Script {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for &ch in self.0.iter() {
            write!(f, "{:02x}", ch)?;
        }
        Ok(())
    }
}

impl fmt::UpperHex for Script {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for &ch in self.0.iter() {
            write!(f, "{:02X}", ch)?;
        }
        Ok(())
    }
}

#[derive(PartialEq, Eq, Debug, Clone)]
/// An object which can be used to construct a script piece by piece
pub struct Builder(Vec<u8>);
display_from_debug!(Builder);

/// Ways that a script might fail. Not everything is split up as
/// much as it could be; patches welcome if more detailed errors
/// would help you.
#[derive(PartialEq, Eq, Debug, Clone)]
pub enum Error {
    /// Something did a non-minimal push; for more information see
    /// `https://github.com/bitcoin/bips/blob/master/bip-0062.mediawiki#Push_operators`
    NonMinimalPush,
    /// Some opcode expected a parameter, but it was missing or truncated
    EarlyEndOfScript,
    /// Tried to read an array off the stack as a number when it was more than 4 bytes
    NumericOverflow,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::NonMinimalPush => write!(f, "non-minimal datapush"),
            Error::EarlyEndOfScript => write!(f, "unexpected end of script"),
            Error::NumericOverflow => {
                write!(f, "numeric overflow (number on stack larger than 4 bytes)")
            }
        }
    }
}

impl error::Error for Error {
    fn cause(&self) -> Option<&dyn error::Error> {
        None
    }
}

/// Helper to encode an integer in script format
fn build_scriptint(n: i64) -> Vec<u8> {
    if n == 0 {
        return vec![];
    }

    let neg = n < 0;

    let mut abs = n.unsigned_abs() as usize;
    let mut v = Vec::with_capacity(size_of::<usize>() + 1);
    while abs > 0xFF {
        v.push((abs & 0xFF) as u8);
        abs >>= 8;
    }
    // If the number's value causes the sign bit to be set, we need an extra
    // byte to get the correct value and correct sign bit
    if abs & 0x80 != 0 {
        v.push(abs as u8);
        v.push(if neg { 0x80u8 } else { 0u8 });
    }
    // Otherwise we just set the sign bit ourselves
    else {
        abs |= if neg { 0x80 } else { 0 };
        v.push(abs as u8);
    }
    v
}

/// Helper to decode an integer in script format
/// Notice that this fails on overflow: the result is the same as in
/// bitcoind, that only 4-byte signed-magnitude values may be read as
/// numbers. They can be added or subtracted (and a long time ago,
/// multiplied and divided), and this may result in numbers which
/// can't be written out in 4 bytes or less. This is ok! The number
/// just can't be read as a number again.
/// This is a bit crazy and subtle, but it makes sense: you can load
/// 32-bit numbers and do anything with them, which back when mult/div
/// was allowed, could result in up to a 64-bit number. We don't want
/// overflow since that's suprising --- and we don't want numbers that
/// don't fit in 64 bits (for efficiency on modern processors) so we
/// simply say, anything in excess of 32 bits is no longer a number.
/// This is basically a ranged type implementation.
pub fn read_scriptint(v: &[u8]) -> Result<i64, Error> {
    let len = v.len();
    if len == 0 {
        return Ok(0);
    }
    if len > 4 {
        return Err(Error::NumericOverflow);
    }

    let (mut ret, sh) = v
        .iter()
        .fold((0, 0), |(acc, sh), n| (acc + ((*n as i64) << sh), sh + 8));
    if v[len - 1] & 0x80 != 0 {
        ret &= (1 << (sh - 1)) - 1;
        ret = -ret;
    }
    Ok(ret)
}

/// This is like "`read_scriptint` then map 0 to false and everything
/// else as true", except that the overflow rules don't apply.
#[inline]
pub fn read_scriptbool(v: &[u8]) -> bool {
    !(v.is_empty()
        || ((v[v.len() - 1] == 0 || v[v.len() - 1] == 0x80)
            && v.iter().rev().skip(1).all(|&w| w == 0)))
}

/// Read a script-encoded unsigned integer
pub fn read_uint(data: &[u8], size: usize) -> Result<usize, Error> {
    if data.len() < size {
        Err(Error::EarlyEndOfScript)
    } else {
        let mut ret = 0;
        for (i, item) in data.iter().take(size).enumerate() {
            ret += (*item as usize) << (i * 8);
        }
        Ok(ret)
    }
}

impl Script {
    /// Creates a new empty script
    pub fn new() -> Script {
        Script(vec![].into_boxed_slice())
    }

    /// The length in bytes of the script
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the script is the empty script
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the script data
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Returns a copy of the script data
    pub fn to_bytes(&self) -> Vec<u8> {
        self.0.clone().into_vec()
    }

    /// Convert the script into a byte vector
    pub fn into_bytes(self) -> Vec<u8> {
        self.0.into_vec()
    }

    /// Compute the P2SH output corresponding to this redeem script
    pub fn to_p2sh(&self) -> Script {
        Builder::new()
            .push_opcode(opcodes::All::OP_HASH160)
            .push_slice(&Hash160::from_data(&self.0)[..])
            .push_opcode(opcodes::All::OP_EQUAL)
            .into_script()
    }

    /// Compute the P2WSH output corresponding to this witnessScript (aka the "witness redeem
    /// script")
    pub fn to_v0_p2wsh(&self) -> Script {
        let mut tmp = [0; 32];
        let mut sha2 = Sha256::new();
        sha2.update(&self.0);
        tmp.copy_from_slice(sha2.finalize().as_slice());
        Builder::new().push_int(0).push_slice(&tmp).into_script()
    }

    /// Checks whether a script pubkey is a p2sh output
    #[inline]
    pub fn is_p2sh(&self) -> bool {
        self.0.len() == 23
            && self.0[0] == opcodes::All::OP_HASH160 as u8
            && self.0[1] == opcodes::All::OP_PUSHBYTES_20 as u8
            && self.0[22] == opcodes::All::OP_EQUAL as u8
    }

    /// Checks whether a script pubkey is a p2pkh output
    #[inline]
    pub fn is_p2pkh(&self) -> bool {
        self.0.len() == 25
            && self.0[0] == opcodes::All::OP_DUP as u8
            && self.0[1] == opcodes::All::OP_HASH160 as u8
            && self.0[2] == opcodes::All::OP_PUSHBYTES_20 as u8
            && self.0[23] == opcodes::All::OP_EQUALVERIFY as u8
            && self.0[24] == opcodes::All::OP_CHECKSIG as u8
    }

    /// Checks whether a script pubkey is a p2pkh output
    #[inline]
    pub fn is_p2pk(&self) -> bool {
        self.0.len() == 67
            && self.0[0] == opcodes::All::OP_PUSHBYTES_65 as u8
            && self.0[66] == opcodes::All::OP_CHECKSIG as u8
    }

    /// Checks whether a script pubkey is a p2wsh output
    #[inline]
    pub fn is_v0_p2wsh(&self) -> bool {
        self.0.len() == 34
            && self.0[0] == opcodes::All::OP_PUSHBYTES_0 as u8
            && self.0[1] == opcodes::All::OP_PUSHBYTES_32 as u8
    }

    /// Checks whether a script pubkey is a p2wpkh output
    #[inline]
    pub fn is_v0_p2wpkh(&self) -> bool {
        self.0.len() == 22
            && self.0[0] == opcodes::All::OP_PUSHBYTES_0 as u8
            && self.0[1] == opcodes::All::OP_PUSHBYTES_20 as u8
    }

    /// Check if this is an OP_RETURN output
    pub fn is_op_return(&self) -> bool {
        !self.0.is_empty() && (opcodes::All::from(self.0[0]) == opcodes::All::OP_RETURN)
    }

    /// Whether a script can be proven to have no satisfying input
    pub fn is_provably_unspendable(&self) -> bool {
        !self.0.is_empty()
            && (opcodes::All::from(self.0[0]).classify() == opcodes::Class::ReturnOp
                || opcodes::All::from(self.0[0]).classify() == opcodes::Class::IllegalOp)
    }

    /// Iterate over the script in the form of `Instruction`s, which are an enum covering
    /// opcodes, datapushes and errors. At most one error will be returned and then the
    /// iterator will end. To instead iterate over the script as sequence of bytes, treat
    /// it as a slice using `script[..]` or convert it to a vector using `into_bytes()`.
    pub fn iter(&self, enforce_minimal: bool) -> Instructions {
        Instructions {
            data: &self.0[..],
            enforce_minimal,
        }
    }
}

/// Creates a new script from an existing vector
impl From<Vec<u8>> for Script {
    fn from(v: Vec<u8>) -> Script {
        Script(v.into_boxed_slice())
    }
}

impl_index_newtype!(Script, u8);

/// A "parsed opcode" which allows iterating over a Script in a more sensible way
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Instruction<'a> {
    /// Push a bunch of data
    PushBytes(&'a [u8]),
    /// Some non-push opcode
    Op(opcodes::All),
    /// An opcode we were unable to parse
    Error(Error),
}

/// Iterator over a script returning parsed opcodes
pub struct Instructions<'a> {
    data: &'a [u8],
    enforce_minimal: bool,
}

impl<'a> Iterator for Instructions<'a> {
    type Item = Instruction<'a>;

    fn next(&mut self) -> Option<Instruction<'a>> {
        if self.data.is_empty() {
            return None;
        }

        match opcodes::All::from(self.data[0]).classify() {
            opcodes::Class::PushBytes(n) => {
                let n = n as usize;
                if self.data.len() < n + 1 {
                    self.data = &[]; // Kill iterator so that it does not return an infinite stream of errors
                    return Some(Instruction::Error(Error::EarlyEndOfScript));
                }
                if self.enforce_minimal
                    && n == 1
                    && (self.data[1] == 0x81 || (self.data[1] > 0 && self.data[1] <= 16))
                {
                    self.data = &[];
                    return Some(Instruction::Error(Error::NonMinimalPush));
                }
                let ret = Some(Instruction::PushBytes(&self.data[1..n + 1]));
                self.data = &self.data[n + 1..];
                ret
            }
            opcodes::Class::Ordinary(opcodes::Ordinary::OP_PUSHDATA1) => {
                if self.data.len() < 2 {
                    self.data = &[];
                    return Some(Instruction::Error(Error::EarlyEndOfScript));
                }
                let n = match read_uint(&self.data[1..], 1) {
                    Ok(n) => n,
                    Err(e) => {
                        self.data = &[];
                        return Some(Instruction::Error(e));
                    }
                };
                if self.data.len() < n + 2 {
                    self.data = &[];
                    return Some(Instruction::Error(Error::EarlyEndOfScript));
                }
                if self.enforce_minimal && n < 76 {
                    self.data = &[];
                    return Some(Instruction::Error(Error::NonMinimalPush));
                }
                let ret = Some(Instruction::PushBytes(&self.data[2..n + 2]));
                self.data = &self.data[n + 2..];
                ret
            }
            opcodes::Class::Ordinary(opcodes::Ordinary::OP_PUSHDATA2) => {
                if self.data.len() < 3 {
                    self.data = &[];
                    return Some(Instruction::Error(Error::EarlyEndOfScript));
                }
                let n = match read_uint(&self.data[1..], 2) {
                    Ok(n) => n,
                    Err(e) => {
                        self.data = &[];
                        return Some(Instruction::Error(e));
                    }
                };
                if self.enforce_minimal && n < 0x100 {
                    self.data = &[];
                    return Some(Instruction::Error(Error::NonMinimalPush));
                }
                if self.data.len() < n + 3 {
                    self.data = &[];
                    return Some(Instruction::Error(Error::EarlyEndOfScript));
                }
                let ret = Some(Instruction::PushBytes(&self.data[3..n + 3]));
                self.data = &self.data[n + 3..];
                ret
            }
            opcodes::Class::Ordinary(opcodes::Ordinary::OP_PUSHDATA4) => {
                if self.data.len() < 5 {
                    self.data = &[];
                    return Some(Instruction::Error(Error::EarlyEndOfScript));
                }
                let n = match read_uint(&self.data[1..], 4) {
                    Ok(n) => n,
                    Err(e) => {
                        self.data = &[];
                        return Some(Instruction::Error(e));
                    }
                };
                if self.enforce_minimal && n < 0x10000 {
                    self.data = &[];
                    return Some(Instruction::Error(Error::NonMinimalPush));
                }
                if self.data.len() < n + 5 {
                    self.data = &[];
                    return Some(Instruction::Error(Error::EarlyEndOfScript));
                }
                let ret = Some(Instruction::PushBytes(&self.data[5..n + 5]));
                self.data = &self.data[n + 5..];
                ret
            }
            // Everything else we can push right through
            _ => {
                let ret = Some(Instruction::Op(opcodes::All::from(self.data[0])));
                self.data = &self.data[1..];
                ret
            }
        }
    }
}

impl Builder {
    /// Creates a new empty script
    pub fn new() -> Builder {
        Builder(vec![])
    }

    /// The length in bytes of the script
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the script is the empty script
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Adds instructions to push an integer onto the stack. Integers are
    /// encoded as little-endian signed-magnitude numbers, but there are
    /// dedicated opcodes to push some small integers.
    pub fn push_int(mut self, data: i64) -> Builder {
        // We can special-case -1, 1-16
        if data == -1 || (1..=16).contains(&data) {
            self.0.push((data - 1 + opcodes::OP_TRUE as i64) as u8);
            self
        }
        // We can also special-case zero
        else if data == 0 {
            self.0.push(opcodes::OP_FALSE as u8);
            self
        }
        // Otherwise encode it as data
        else {
            self.push_scriptint(data)
        }
    }

    /// Adds instructions to push an integer onto the stack, using the explicit
    /// encoding regardless of the availability of dedicated opcodes.
    pub fn push_scriptint(self, data: i64) -> Builder {
        self.push_slice(&build_scriptint(data))
    }

    /// Adds instructions to push some arbitrary data onto the stack
    pub fn push_slice(mut self, data: &[u8]) -> Builder {
        // Start with a PUSH opcode
        match data.len() as u64 {
            n if n < opcodes::Ordinary::OP_PUSHDATA1 as u64 => {
                self.0.push(n as u8);
            }
            n if n < 0x100 => {
                self.0.push(opcodes::Ordinary::OP_PUSHDATA1 as u8);
                self.0.push(n as u8);
            }
            n if n < 0x10000 => {
                self.0.push(opcodes::Ordinary::OP_PUSHDATA2 as u8);
                self.0.push((n % 0x100) as u8);
                self.0.push((n / 0x100) as u8);
            }
            n if n < 0x100000000 => {
                self.0.push(opcodes::Ordinary::OP_PUSHDATA4 as u8);
                self.0.push((n % 0x100) as u8);
                self.0.push(((n / 0x100) % 0x100) as u8);
                self.0.push(((n / 0x10000) % 0x100) as u8);
                self.0.push((n / 0x1000000) as u8);
            }
            _ => panic!("tried to put a 4bn+ sized object into a script!"),
        }
        // Then push the acraw
        self.0.extend(data.iter().cloned());
        self
    }

    /// Adds a single opcode to the script
    pub fn push_opcode(mut self, data: opcodes::All) -> Builder {
        self.0.push(data as u8);
        self
    }

    /// Converts the `Builder` into an unmodifiable `Script`
    pub fn into_script(self) -> Script {
        Script(self.0.into_boxed_slice())
    }
}

/// Adds an individual opcode to the script
impl Default for Builder {
    fn default() -> Builder {
        Builder(vec![])
    }
}

/// Creates a new script from an existing vector
impl From<Vec<u8>> for Builder {
    fn from(v: Vec<u8>) -> Builder {
        Builder(v)
    }
}

impl_index_newtype!(Builder, u8);

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Script {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use std::fmt::{self, Formatter};

        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Script;

            fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
                formatter.write_str("a script")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let v: Vec<u8> = crate::util::hash::hex_bytes(v).map_err(E::custom)?;
                Ok(Script::from(v))
            }

            fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str(v)
            }

            fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str(&v)
            }
        }

        deserializer.deserialize_str(Visitor)
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Script {
    /// User-facing serialization for `Script`.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!("{:x}", self))
    }
}

// Network serialization
impl<S: SimpleEncoder> ConsensusEncodable<S> for Script {
    #[inline]
    fn consensus_encode(&self, s: &mut S) -> Result<(), serialize::Error> {
        self.0.consensus_encode(s)
    }
}

impl<D: SimpleDecoder> ConsensusDecodable<D> for Script {
    #[inline]
    fn consensus_decode(d: &mut D) -> Result<Script, serialize::Error> {
        Ok(Script(ConsensusDecodable::consensus_decode(d)?))
    }
}

#[cfg(test)]
mod test {
    use super::{build_scriptint, *};
    use crate::deps_common::bitcoin::blockdata::opcodes;
    use crate::deps_common::bitcoin::network::serialize::{deserialize, serialize};
    use crate::util::hash::hex_bytes as hex_decode;

    #[test]
    fn script() {
        let mut comp = vec![];
        let mut script = Builder::new();
        assert_eq!(&script[..], &comp[..]);

        // small ints
        script = script.push_int(1);
        comp.push(81u8);
        assert_eq!(&script[..], &comp[..]);
        script = script.push_int(0);
        comp.push(0u8);
        assert_eq!(&script[..], &comp[..]);
        script = script.push_int(4);
        comp.push(84u8);
        assert_eq!(&script[..], &comp[..]);
        script = script.push_int(-1);
        comp.push(79u8);
        assert_eq!(&script[..], &comp[..]);
        // forced scriptint
        script = script.push_scriptint(4);
        comp.extend([1u8, 4].iter().cloned());
        assert_eq!(&script[..], &comp[..]);
        // big ints
        script = script.push_int(17);
        comp.extend([1u8, 17].iter().cloned());
        assert_eq!(&script[..], &comp[..]);
        script = script.push_int(10000);
        comp.extend([2u8, 16, 39].iter().cloned());
        assert_eq!(&script[..], &comp[..]);
        // notice the sign bit set here, hence the extra zero/128 at the end
        script = script.push_int(10000000);
        comp.extend([4u8, 128, 150, 152, 0].iter().cloned());
        assert_eq!(&script[..], &comp[..]);
        script = script.push_int(-10000000);
        comp.extend([4u8, 128, 150, 152, 128].iter().cloned());
        assert_eq!(&script[..], &comp[..]);

        // data
        script = script.push_slice("NRA4VR".as_bytes());
        comp.extend([6u8, 78, 82, 65, 52, 86, 82].iter().cloned());
        assert_eq!(&script[..], &comp[..]);

        // opcodes
        script = script.push_opcode(opcodes::All::OP_CHECKSIG);
        comp.push(0xACu8);
        assert_eq!(&script[..], &comp[..]);
        script = script.push_opcode(opcodes::All::OP_CHECKSIG);
        comp.push(0xACu8);
        assert_eq!(&script[..], &comp[..]);
    }

    #[test]
    fn script_builder() {
        // from txid 3bb5e6434c11fb93f64574af5d116736510717f2c595eb45b52c28e31622dfff which was in my mempool when I wrote the test
        let script = Builder::new()
            .push_opcode(opcodes::All::OP_DUP)
            .push_opcode(opcodes::All::OP_HASH160)
            .push_slice(&hex_decode("16e1ae70ff0fa102905d4af297f6912bda6cce19").unwrap())
            .push_opcode(opcodes::All::OP_EQUALVERIFY)
            .push_opcode(opcodes::All::OP_CHECKSIG)
            .into_script();
        assert_eq!(
            &format!("{:x}", script),
            "76a91416e1ae70ff0fa102905d4af297f6912bda6cce1988ac"
        );
    }

    #[test]
    fn script_serialize() {
        let hex_script = hex_decode("6c493046022100f93bb0e7d8db7bd46e40132d1f8242026e045f03a0efe71bbb8e3f475e970d790221009337cd7f1f929f00cc6ff01f03729b069a7c21b59b1736ddfee5db5946c5da8c0121033b9b137ee87d5a812d6f506efdd37f0affa7ffc310711c06c7f3e097c9447c52").unwrap();
        let script: Result<Script, _> = deserialize(&hex_script);
        assert!(script.is_ok());
        assert_eq!(serialize(&script.unwrap()).ok(), Some(hex_script));
    }

    #[test]
    fn scriptint_round_trip() {
        assert_eq!(build_scriptint(-1), vec![0x81]);
        assert_eq!(build_scriptint(255), vec![255, 0]);
        assert_eq!(build_scriptint(256), vec![0, 1]);
        assert_eq!(build_scriptint(257), vec![1, 1]);
        assert_eq!(build_scriptint(511), vec![255, 1]);
        for &i in [
            10,
            100,
            255,
            256,
            1000,
            10000,
            25000,
            200000,
            5000000,
            1000000000,
            (1 << 31) - 1,
            -((1 << 31) - 1),
        ]
        .iter()
        {
            assert_eq!(Ok(i), read_scriptint(&build_scriptint(i)));
            assert_eq!(Ok(-i), read_scriptint(&build_scriptint(-i)));
        }
        assert!(read_scriptint(&build_scriptint(1 << 31)).is_err());
        assert!(read_scriptint(&build_scriptint(-(1 << 31))).is_err());
    }

    #[test]
    fn provably_unspendable_test() {
        // p2pk
        assert!(!hex_script!("410446ef0102d1ec5240f0d061a4246c1bdef63fc3dbab7733052fbbf0ecd8f41fc26bf049ebb4f9527f374280259e7cfa99c48b0e3f39c51347a19a5819651503a5ac").is_provably_unspendable());
        assert!(!hex_script!("4104ea1feff861b51fe3f5f8a3b12d0f4712db80e919548a80839fc47c6a21e66d957e9c5d8cd108c7a2d2324bad71f9904ac0ae7336507d785b17a2c115e427a32fac").is_provably_unspendable());
        // p2pkhash
        assert!(
            !hex_script!("76a914ee61d57ab51b9d212335b1dba62794ac20d2bcf988ac")
                .is_provably_unspendable()
        );
        assert!(
            hex_script!("6aa9149eb21980dc9d413d8eac27314938b9da920ee53e87")
                .is_provably_unspendable()
        );
    }

    #[test]
    fn op_return_test() {
        assert!(hex_script!("6aa9149eb21980dc9d413d8eac27314938b9da920ee53e87").is_op_return());
        assert!(!hex_script!("76a914ee61d57ab51b9d212335b1dba62794ac20d2bcf988ac").is_op_return());
        assert!(!hex_script!("").is_op_return());
    }

    #[test]
    #[cfg(feature = "serde")]
    fn script_json_serialize() {
        use serde_json;

        let original = hex_script!("827651a0698faaa9a8a7a687");
        let json_value = serde_json::to_value(&original).unwrap();
        assert_eq!(
            serde_json::to_vec(&json_value).unwrap(),
            b"\"827651a0698faaa9a8a7a687\""
        );
        assert_eq!(json_value.to_string(), "\"827651a0698faaa9a8a7a687\"");
        let des = serde_json::from_value(json_value).unwrap();
        assert_eq!(original, des);
    }

    #[test]
    fn script_debug_display() {
        assert_eq!(format!("{:?}", hex_script!("6363636363686868686800")),
                   "Script(OP_IF OP_IF OP_IF OP_IF OP_IF OP_ENDIF OP_ENDIF OP_ENDIF OP_ENDIF OP_ENDIF OP_0)");
        assert_eq!(format!("{}", hex_script!("6363636363686868686800")),
                   "Script(OP_IF OP_IF OP_IF OP_IF OP_IF OP_ENDIF OP_ENDIF OP_ENDIF OP_ENDIF OP_ENDIF OP_0)");
        assert_eq!(format!("{}", hex_script!("2102715e91d37d239dea832f1460e91e368115d8ca6cc23a7da966795abad9e3b699ac")),
                   "Script(OP_PUSHBYTES_33 02715e91d37d239dea832f1460e91e368115d8ca6cc23a7da966795abad9e3b699 OP_CHECKSIG)");
        // Elements Alpha peg-out transaction with some signatures removed for brevity. Mainly to test PUSHDATA1
        assert_eq!(format!("{}", hex_script!("0047304402202457e78cc1b7f50d0543863c27de75d07982bde8359b9e3316adec0aec165f2f02200203fd331c4e4a4a02f48cf1c291e2c0d6b2f7078a784b5b3649fca41f8794d401004cf1552103244e602b46755f24327142a0517288cebd159eccb6ccf41ea6edf1f601e9af952103bbbacc302d19d29dbfa62d23f37944ae19853cf260c745c2bea739c95328fcb721039227e83246bd51140fe93538b2301c9048be82ef2fb3c7fc5d78426ed6f609ad210229bf310c379b90033e2ecb07f77ecf9b8d59acb623ab7be25a0caed539e2e6472103703e2ed676936f10b3ce9149fa2d4a32060fb86fa9a70a4efe3f21d7ab90611921031e9b7c6022400a6bb0424bbcde14cff6c016b91ee3803926f3440abf5c146d05210334667f975f55a8455d515a2ef1c94fdfa3315f12319a14515d2a13d82831f62f57ae")),
                   "Script(OP_0 OP_PUSHBYTES_71 304402202457e78cc1b7f50d0543863c27de75d07982bde8359b9e3316adec0aec165f2f02200203fd331c4e4a4a02f48cf1c291e2c0d6b2f7078a784b5b3649fca41f8794d401 OP_0 OP_PUSHDATA1 552103244e602b46755f24327142a0517288cebd159eccb6ccf41ea6edf1f601e9af952103bbbacc302d19d29dbfa62d23f37944ae19853cf260c745c2bea739c95328fcb721039227e83246bd51140fe93538b2301c9048be82ef2fb3c7fc5d78426ed6f609ad210229bf310c379b90033e2ecb07f77ecf9b8d59acb623ab7be25a0caed539e2e6472103703e2ed676936f10b3ce9149fa2d4a32060fb86fa9a70a4efe3f21d7ab90611921031e9b7c6022400a6bb0424bbcde14cff6c016b91ee3803926f3440abf5c146d05210334667f975f55a8455d515a2ef1c94fdfa3315f12319a14515d2a13d82831f62f57ae)");
    }

    #[test]
    fn script_p2sh_p2p2k_template() {
        // random outputs I picked out of the mempool
        assert!(hex_script!("76a91402306a7c23f3e8010de41e9e591348bb83f11daa88ac").is_p2pkh());
        assert!(!hex_script!("76a91402306a7c23f3e8010de41e9e591348bb83f11daa88ac").is_p2sh());
        assert!(!hex_script!("76a91402306a7c23f3e8010de41e9e591348bb83f11daa88ad").is_p2pkh());
        assert!(!hex_script!("").is_p2pkh());
        assert!(hex_script!("a914acc91e6fef5c7f24e5c8b3f11a664aa8f1352ffd87").is_p2sh());
        assert!(!hex_script!("a914acc91e6fef5c7f24e5c8b3f11a664aa8f1352ffd87").is_p2pkh());
        assert!(!hex_script!("a314acc91e6fef5c7f24e5c8b3f11a664aa8f1352ffd87").is_p2sh());
    }

    #[test]
    fn p2sh_p2wsh_conversion() {
        // Test vectors taken from Core tests/data/script_tests.json
        // bare p2wsh
        let redeem_script = hex_script!("410479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8ac");
        let expected_witout =
            hex_script!("0020b95237b48faaa69eb078e1170be3b5cbb3fddf16d0a991e14ad274f7b33a4f64");
        assert!(redeem_script.to_v0_p2wsh().is_v0_p2wsh());
        assert_eq!(redeem_script.to_v0_p2wsh(), expected_witout);

        // p2sh
        let redeem_script = hex_script!("0479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8");
        let expected_p2shout = hex_script!("a91491b24bf9f5288532960ac687abb035127b1d28a587");
        assert!(redeem_script.to_p2sh().is_p2sh());
        assert_eq!(redeem_script.to_p2sh(), expected_p2shout);

        // p2sh-p2wsh
        let redeem_script = hex_script!("410479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8ac");
        let expected_witout =
            hex_script!("0020b95237b48faaa69eb078e1170be3b5cbb3fddf16d0a991e14ad274f7b33a4f64");
        let expected_out = hex_script!("a914f386c2ba255cc56d20cfa6ea8b062f8b5994551887");
        assert!(redeem_script.to_p2sh().is_p2sh());
        assert!(redeem_script.to_p2sh().to_v0_p2wsh().is_v0_p2wsh());
        assert_eq!(redeem_script.to_v0_p2wsh(), expected_witout);
        assert_eq!(redeem_script.to_v0_p2wsh().to_p2sh(), expected_out);
    }

    #[test]
    fn test_iterator() {
        let zero = hex_script!("00");
        let zeropush = hex_script!("0100");

        let nonminimal = hex_script!("4c0169b2"); // PUSHDATA1 for no reason
        let minimal = hex_script!("0169b2"); // minimal
        let nonminimal_alt = hex_script!("026900b2"); // non-minimal number but minimal push (should be OK)

        let v_zero: Vec<Instruction> = zero.iter(true).collect();
        let v_zeropush: Vec<Instruction> = zeropush.iter(true).collect();

        let v_min: Vec<Instruction> = minimal.iter(true).collect();
        let v_nonmin: Vec<Instruction> = nonminimal.iter(true).collect();
        let v_nonmin_alt: Vec<Instruction> = nonminimal_alt.iter(true).collect();
        let slop_v_min: Vec<Instruction> = minimal.iter(false).collect();
        let slop_v_nonmin: Vec<Instruction> = nonminimal.iter(false).collect();
        let slop_v_nonmin_alt: Vec<Instruction> = nonminimal_alt.iter(false).collect();

        assert_eq!(v_zero, vec![Instruction::PushBytes(&[]),]);
        assert_eq!(v_zeropush, vec![Instruction::PushBytes(&[0]),]);

        assert_eq!(
            v_min,
            vec![
                Instruction::PushBytes(&[105]),
                Instruction::Op(opcodes::All::OP_NOP3),
            ]
        );

        assert_eq!(v_nonmin, vec![Instruction::Error(Error::NonMinimalPush),]);

        assert_eq!(
            v_nonmin_alt,
            vec![
                Instruction::PushBytes(&[105, 0]),
                Instruction::Op(opcodes::All::OP_NOP3),
            ]
        );

        assert_eq!(v_min, slop_v_min);
        assert_eq!(v_min, slop_v_nonmin);
        assert_eq!(v_nonmin_alt, slop_v_nonmin_alt);
    }

    #[test]
    fn script_ord() {
        let script_1 = Builder::new().push_slice(&[1, 2, 3, 4]).into_script();
        let script_2 = Builder::new().push_int(10).into_script();
        let script_3 = Builder::new().push_int(15).into_script();
        let script_4 = Builder::new()
            .push_opcode(opcodes::All::OP_RETURN)
            .into_script();

        assert!(script_1 < script_2);
        assert!(script_2 < script_3);
        assert!(script_3 < script_4);

        assert!(script_1 <= script_1);
        assert!(script_1 >= script_1);

        assert!(script_4 > script_3);
        assert!(script_3 > script_2);
        assert!(script_2 > script_1);
    }
}
