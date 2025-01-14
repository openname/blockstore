// Rust Bitcoin Library
// Written in 2014 by
//   Andrew Poelstra <apoelstra@wpsoftware.net>
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

//! Opcodes
//!
//! Bitcoin's script uses a stack-based assembly language. This module defines
//! all of the opcodes
//!

#![allow(non_camel_case_types)]

// Heavy stick to translate between opcode types
use std::mem::transmute;

#[cfg(feature = "serde")]
use serde;

use crate::deps_common::bitcoin::network::encodable::{ConsensusDecodable, ConsensusEncodable};
use crate::deps_common::bitcoin::network::serialize::{self, SimpleDecoder, SimpleEncoder};

// Note: I am deliberately not implementing PartialOrd or Ord on the
//       opcode enum. If you want to check ranges of opcodes, etc.,
//       write an #[inline] helper function which casts to u8s.

/// A script Opcode
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum All {
    /// Push an empty array onto the stack
    OP_PUSHBYTES_0 = 0x0,
    /// Push the next byte as an array onto the stack
    OP_PUSHBYTES_1 = 0x01,
    /// Push the next 2 bytes as an array onto the stack
    OP_PUSHBYTES_2 = 0x02,
    /// Push the next 2 bytes as an array onto the stack
    OP_PUSHBYTES_3 = 0x03,
    /// Push the next 4 bytes as an array onto the stack
    OP_PUSHBYTES_4 = 0x04,
    /// Push the next 5 bytes as an array onto the stack
    OP_PUSHBYTES_5 = 0x05,
    /// Push the next 6 bytes as an array onto the stack
    OP_PUSHBYTES_6 = 0x06,
    /// Push the next 7 bytes as an array onto the stack
    OP_PUSHBYTES_7 = 0x07,
    /// Push the next 8 bytes as an array onto the stack
    OP_PUSHBYTES_8 = 0x08,
    /// Push the next 9 bytes as an array onto the stack
    OP_PUSHBYTES_9 = 0x09,
    /// Push the next 10 bytes as an array onto the stack
    OP_PUSHBYTES_10 = 0x0a,
    /// Push the next 11 bytes as an array onto the stack
    OP_PUSHBYTES_11 = 0x0b,
    /// Push the next 12 bytes as an array onto the stack
    OP_PUSHBYTES_12 = 0x0c,
    /// Push the next 13 bytes as an array onto the stack
    OP_PUSHBYTES_13 = 0x0d,
    /// Push the next 14 bytes as an array onto the stack
    OP_PUSHBYTES_14 = 0x0e,
    /// Push the next 15 bytes as an array onto the stack
    OP_PUSHBYTES_15 = 0x0f,
    /// Push the next 16 bytes as an array onto the stack
    OP_PUSHBYTES_16 = 0x10,
    /// Push the next 17 bytes as an array onto the stack
    OP_PUSHBYTES_17 = 0x11,
    /// Push the next 18 bytes as an array onto the stack
    OP_PUSHBYTES_18 = 0x12,
    /// Push the next 19 bytes as an array onto the stack
    OP_PUSHBYTES_19 = 0x13,
    /// Push the next 20 bytes as an array onto the stack
    OP_PUSHBYTES_20 = 0x14,
    /// Push the next 21 bytes as an array onto the stack
    OP_PUSHBYTES_21 = 0x15,
    /// Push the next 22 bytes as an array onto the stack
    OP_PUSHBYTES_22 = 0x16,
    /// Push the next 23 bytes as an array onto the stack
    OP_PUSHBYTES_23 = 0x17,
    /// Push the next 24 bytes as an array onto the stack
    OP_PUSHBYTES_24 = 0x18,
    /// Push the next 25 bytes as an array onto the stack
    OP_PUSHBYTES_25 = 0x19,
    /// Push the next 26 bytes as an array onto the stack
    OP_PUSHBYTES_26 = 0x1a,
    /// Push the next 27 bytes as an array onto the stack
    OP_PUSHBYTES_27 = 0x1b,
    /// Push the next 28 bytes as an array onto the stack
    OP_PUSHBYTES_28 = 0x1c,
    /// Push the next 29 bytes as an array onto the stack
    OP_PUSHBYTES_29 = 0x1d,
    /// Push the next 30 bytes as an array onto the stack
    OP_PUSHBYTES_30 = 0x1e,
    /// Push the next 31 bytes as an array onto the stack
    OP_PUSHBYTES_31 = 0x1f,
    /// Push the next 32 bytes as an array onto the stack
    OP_PUSHBYTES_32 = 0x20,
    /// Push the next 33 bytes as an array onto the stack
    OP_PUSHBYTES_33 = 0x21,
    /// Push the next 34 bytes as an array onto the stack
    OP_PUSHBYTES_34 = 0x22,
    /// Push the next 35 bytes as an array onto the stack
    OP_PUSHBYTES_35 = 0x23,
    /// Push the next 36 bytes as an array onto the stack
    OP_PUSHBYTES_36 = 0x24,
    /// Push the next 37 bytes as an array onto the stack
    OP_PUSHBYTES_37 = 0x25,
    /// Push the next 38 bytes as an array onto the stack
    OP_PUSHBYTES_38 = 0x26,
    /// Push the next 39 bytes as an array onto the stack
    OP_PUSHBYTES_39 = 0x27,
    /// Push the next 40 bytes as an array onto the stack
    OP_PUSHBYTES_40 = 0x28,
    /// Push the next 41 bytes as an array onto the stack
    OP_PUSHBYTES_41 = 0x29,
    /// Push the next 42 bytes as an array onto the stack
    OP_PUSHBYTES_42 = 0x2a,
    /// Push the next 43 bytes as an array onto the stack
    OP_PUSHBYTES_43 = 0x2b,
    /// Push the next 44 bytes as an array onto the stack
    OP_PUSHBYTES_44 = 0x2c,
    /// Push the next 45 bytes as an array onto the stack
    OP_PUSHBYTES_45 = 0x2d,
    /// Push the next 46 bytes as an array onto the stack
    OP_PUSHBYTES_46 = 0x2e,
    /// Push the next 47 bytes as an array onto the stack
    OP_PUSHBYTES_47 = 0x2f,
    /// Push the next 48 bytes as an array onto the stack
    OP_PUSHBYTES_48 = 0x30,
    /// Push the next 49 bytes as an array onto the stack
    OP_PUSHBYTES_49 = 0x31,
    /// Push the next 50 bytes as an array onto the stack
    OP_PUSHBYTES_50 = 0x32,
    /// Push the next 51 bytes as an array onto the stack
    OP_PUSHBYTES_51 = 0x33,
    /// Push the next 52 bytes as an array onto the stack
    OP_PUSHBYTES_52 = 0x34,
    /// Push the next 53 bytes as an array onto the stack
    OP_PUSHBYTES_53 = 0x35,
    /// Push the next 54 bytes as an array onto the stack
    OP_PUSHBYTES_54 = 0x36,
    /// Push the next 55 bytes as an array onto the stack
    OP_PUSHBYTES_55 = 0x37,
    /// Push the next 56 bytes as an array onto the stack
    OP_PUSHBYTES_56 = 0x38,
    /// Push the next 57 bytes as an array onto the stack
    OP_PUSHBYTES_57 = 0x39,
    /// Push the next 58 bytes as an array onto the stack
    OP_PUSHBYTES_58 = 0x3a,
    /// Push the next 59 bytes as an array onto the stack
    OP_PUSHBYTES_59 = 0x3b,
    /// Push the next 60 bytes as an array onto the stack
    OP_PUSHBYTES_60 = 0x3c,
    /// Push the next 61 bytes as an array onto the stack
    OP_PUSHBYTES_61 = 0x3d,
    /// Push the next 62 bytes as an array onto the stack
    OP_PUSHBYTES_62 = 0x3e,
    /// Push the next 63 bytes as an array onto the stack
    OP_PUSHBYTES_63 = 0x3f,
    /// Push the next 64 bytes as an array onto the stack
    OP_PUSHBYTES_64 = 0x40,
    /// Push the next 65 bytes as an array onto the stack
    OP_PUSHBYTES_65 = 0x41,
    /// Push the next 66 bytes as an array onto the stack
    OP_PUSHBYTES_66 = 0x42,
    /// Push the next 67 bytes as an array onto the stack
    OP_PUSHBYTES_67 = 0x43,
    /// Push the next 68 bytes as an array onto the stack
    OP_PUSHBYTES_68 = 0x44,
    /// Push the next 69 bytes as an array onto the stack
    OP_PUSHBYTES_69 = 0x45,
    /// Push the next 70 bytes as an array onto the stack
    OP_PUSHBYTES_70 = 0x46,
    /// Push the next 71 bytes as an array onto the stack
    OP_PUSHBYTES_71 = 0x47,
    /// Push the next 72 bytes as an array onto the stack
    OP_PUSHBYTES_72 = 0x48,
    /// Push the next 73 bytes as an array onto the stack
    OP_PUSHBYTES_73 = 0x49,
    /// Push the next 74 bytes as an array onto the stack
    OP_PUSHBYTES_74 = 0x4a,
    /// Push the next 75 bytes as an array onto the stack
    OP_PUSHBYTES_75 = 0x4b,
    /// Read the next byte as N; push the next N bytes as an array onto the stack
    OP_PUSHDATA1 = 0x4c,
    /// Read the next 2 bytes as N; push the next N bytes as an array onto the stack
    OP_PUSHDATA2 = 0x4d,
    /// Read the next 4 bytes as N; push the next N bytes as an array onto the stack
    OP_PUSHDATA4 = 0x4e,
    /// Push the array [0x81] onto the stack
    OP_PUSHNUM_NEG1 = 0x4f,
    /// Synonym for OP_RETURN
    OP_RESERVED = 0x50,
    /// Push the array [0x01] onto the stack
    OP_PUSHNUM_1 = 0x51,
    /// Push the array [0x02] onto the stack
    OP_PUSHNUM_2 = 0x52,
    /// Push the array [0x03] onto the stack
    OP_PUSHNUM_3 = 0x53,
    /// Push the array [0x04] onto the stack
    OP_PUSHNUM_4 = 0x54,
    /// Push the array [0x05] onto the stack
    OP_PUSHNUM_5 = 0x55,
    /// Push the array [0x06] onto the stack
    OP_PUSHNUM_6 = 0x56,
    /// Push the array [0x07] onto the stack
    OP_PUSHNUM_7 = 0x57,
    /// Push the array [0x08] onto the stack
    OP_PUSHNUM_8 = 0x58,
    /// Push the array [0x09] onto the stack
    OP_PUSHNUM_9 = 0x59,
    /// Push the array [0x0a] onto the stack
    OP_PUSHNUM_10 = 0x5a,
    /// Push the array [0x0b] onto the stack
    OP_PUSHNUM_11 = 0x5b,
    /// Push the array [0x0c] onto the stack
    OP_PUSHNUM_12 = 0x5c,
    /// Push the array [0x0d] onto the stack
    OP_PUSHNUM_13 = 0x5d,
    /// Push the array [0x0e] onto the stack
    OP_PUSHNUM_14 = 0x5e,
    /// Push the array [0x0f] onto the stack
    OP_PUSHNUM_15 = 0x5f,
    /// Push the array [0x10] onto the stack
    OP_PUSHNUM_16 = 0x60,
    /// Does nothing
    OP_NOP = 0x61,
    /// Synonym for OP_RETURN
    OP_VER = 0x62,
    /// Pop and execute the next statements if a nonzero element was popped
    OP_IF = 0x63,
    /// Pop and execute the next statements if a zero element was popped
    OP_NOTIF = 0x64,
    /// Fail the script unconditionally, does not even need to be executed
    OP_VERIF = 0x65,
    /// Fail the script unconditionally, does not even need to be executed
    OP_VERNOTIF = 0x66,
    /// Execute statements if those after the previous OP_IF were not, and vice-versa.
    /// If there is no previous OP_IF, this acts as a RETURN.
    OP_ELSE = 0x67,
    /// Pop and execute the next statements if a zero element was popped
    OP_ENDIF = 0x68,
    /// If the top value is zero or the stack is empty, fail; otherwise, pop the stack
    OP_VERIFY = 0x69,
    /// Fail the script immediately. (Must be executed.)
    OP_RETURN = 0x6a,
    /// Pop one element from the main stack onto the alt stack
    OP_TOALTSTACK = 0x6b,
    /// Pop one element from the alt stack onto the main stack
    OP_FROMALTSTACK = 0x6c,
    /// Drops the top two stack items
    OP_2DROP = 0x6d,
    /// Duplicates the top two stack items as AB -> ABAB
    OP_2DUP = 0x6e,
    /// Duplicates the two three stack items as ABC -> ABCABC
    OP_3DUP = 0x6f,
    /// Copies the two stack items of items two spaces back to
    /// the front, as xxAB -> ABxxAB
    OP_2OVER = 0x70,
    /// Moves the two stack items four spaces back to the front,
    /// as xxxxAB -> ABxxxx
    OP_2ROT = 0x71,
    /// Swaps the top two pairs, as ABCD -> CDAB
    OP_2SWAP = 0x72,
    /// Duplicate the top stack element unless it is zero
    OP_IFDUP = 0x73,
    /// Push the current number of stack items onto te stack
    OP_DEPTH = 0x74,
    /// Drops the top stack item
    OP_DROP = 0x75,
    /// Duplicates the top stack item
    OP_DUP = 0x76,
    /// Drops the second-to-top stack item
    OP_NIP = 0x77,
    /// Copies the second-to-top stack item, as xA -> AxA
    OP_OVER = 0x78,
    /// Pop the top stack element as N. Copy the Nth stack element to the top
    OP_PICK = 0x79,
    /// Pop the top stack element as N. Move the Nth stack element to the top
    OP_ROLL = 0x7a,
    /// Rotate the top three stack items, as [top next1 next2] -> [next2 top next1]
    OP_ROT = 0x7b,
    /// Swap the top two stack items
    OP_SWAP = 0x7c,
    /// Copy the top stack item to before the second item, as [top next] -> [top next top]
    OP_TUCK = 0x7d,
    /// Fail the script unconditionally, does not even need to be executed
    OP_CAT = 0x7e,
    /// Fail the script unconditionally, does not even need to be executed
    OP_SUBSTR = 0x7f,
    /// Fail the script unconditionally, does not even need to be executed
    OP_LEFT = 0x80,
    /// Fail the script unconditionally, does not even need to be executed
    OP_RIGHT = 0x81,
    /// Pushes the length of the top stack item onto the stack
    OP_SIZE = 0x82,
    /// Fail the script unconditionally, does not even need to be executed
    OP_INVERT = 0x83,
    /// Fail the script unconditionally, does not even need to be executed
    OP_AND = 0x84,
    /// Fail the script unconditionally, does not even need to be executed
    OP_OR = 0x85,
    /// Fail the script unconditionally, does not even need to be executed
    OP_XOR = 0x86,
    /// Pushes 1 if the inputs are exactly equal, 0 otherwise
    OP_EQUAL = 0x87,
    /// Returns success if the inputs are exactly equal, failure otherwise
    OP_EQUALVERIFY = 0x88,
    /// Synonym for OP_RETURN
    OP_RESERVED1 = 0x89,
    /// Synonym for OP_RETURN
    OP_RESERVED2 = 0x8a,
    /// Increment the top stack element in place
    OP_1ADD = 0x8b,
    /// Decrement the top stack element in place
    OP_1SUB = 0x8c,
    /// Fail the script unconditionally, does not even need to be executed
    OP_2MUL = 0x8d,
    /// Fail the script unconditionally, does not even need to be executed
    OP_2DIV = 0x8e,
    /// Multiply the top stack item by -1 in place
    OP_NEGATE = 0x8f,
    /// Absolute value the top stack item in place
    OP_ABS = 0x90,
    /// Map 0 to 1 and everything else to 0, in place
    OP_NOT = 0x91,
    /// Map 0 to 0 and everything else to 1, in place
    OP_0NOTEQUAL = 0x92,
    /// Pop two stack items and push their sum
    OP_ADD = 0x93,
    /// Pop two stack items and push the second minus the top
    OP_SUB = 0x94,
    /// Fail the script unconditionally, does not even need to be executed
    OP_MUL = 0x95,
    /// Fail the script unconditionally, does not even need to be executed
    OP_DIV = 0x96,
    /// Fail the script unconditionally, does not even need to be executed
    OP_MOD = 0x97,
    /// Fail the script unconditionally, does not even need to be executed
    OP_LSHIFT = 0x98,
    /// Fail the script unconditionally, does not even need to be executed
    OP_RSHIFT = 0x99,
    /// Pop the top two stack items and push 1 if both are nonzero, else push 0
    OP_BOOLAND = 0x9a,
    /// Pop the top two stack items and push 1 if either is nonzero, else push 0
    OP_BOOLOR = 0x9b,
    /// Pop the top two stack items and push 1 if both are numerically equal, else push 0
    OP_NUMEQUAL = 0x9c,
    /// Pop the top two stack items and return success if both are numerically equal, else return failure
    OP_NUMEQUALVERIFY = 0x9d,
    /// Pop the top two stack items and push 0 if both are numerically equal, else push 1
    OP_NUMNOTEQUAL = 0x9e,
    /// Pop the top two items; push 1 if the second is less than the top, 0 otherwise
    OP_LESSTHAN = 0x9f,
    /// Pop the top two items; push 1 if the second is greater than the top, 0 otherwise
    OP_GREATERTHAN = 0xa0,
    /// Pop the top two items; push 1 if the second is <= the top, 0 otherwise
    OP_LESSTHANOREQUAL = 0xa1,
    /// Pop the top two items; push 1 if the second is >= the top, 0 otherwise
    OP_GREATERTHANOREQUAL = 0xa2,
    /// Pop the top two items; push the smaller
    OP_MIN = 0xa3,
    /// Pop the top two items; push the larger
    OP_MAX = 0xa4,
    /// Pop the top three items; if the top is >= the second and < the third, push 1, otherwise push 0
    OP_WITHIN = 0xa5,
    /// Pop the top stack item and push its RIPEMD160 hash
    OP_RIPEMD160 = 0xa6,
    /// Pop the top stack item and push its SHA1 hash
    OP_SHA1 = 0xa7,
    /// Pop the top stack item and push its SHA256 hash
    OP_SHA256 = 0xa8,
    /// Pop the top stack item and push its RIPEMD(SHA256) hash
    OP_HASH160 = 0xa9,
    /// Pop the top stack item and push its SHA256(SHA256) hash
    OP_HASH256 = 0xaa,
    /// Ignore this and everything preceding when deciding what to sign when signature-checking
    OP_CODESEPARATOR = 0xab,
    /// https://en.bitcoin.it/wiki/OP_CHECKSIG pushing 1/0 for success/failure
    OP_CHECKSIG = 0xac,
    /// https://en.bitcoin.it/wiki/OP_CHECKSIG returning success/failure
    OP_CHECKSIGVERIFY = 0xad,
    /// Pop N, N pubkeys, M, M signatures, a dummy (due to bug in reference code), and verify that all M signatures are valid.
    /// Push 1 for "all valid", 0 otherwise
    OP_CHECKMULTISIG = 0xae,
    /// Like the above but return success/failure
    OP_CHECKMULTISIGVERIFY = 0xaf,
    /// Does nothing
    OP_NOP1 = 0xb0,
    /// Does nothing
    OP_NOP2 = 0xb1,
    /// Does nothing
    OP_NOP3 = 0xb2,
    /// Does nothing
    OP_NOP4 = 0xb3,
    /// Does nothing
    OP_NOP5 = 0xb4,
    /// Does nothing
    OP_NOP6 = 0xb5,
    /// Does nothing
    OP_NOP7 = 0xb6,
    /// Does nothing
    OP_NOP8 = 0xb7,
    /// Does nothing
    OP_NOP9 = 0xb8,
    /// Does nothing
    OP_NOP10 = 0xb9,
    // Every other opcode acts as OP_RETURN
    /// Synonym for OP_RETURN
    OP_RETURN_186 = 0xba,
    /// Synonym for OP_RETURN
    OP_RETURN_187 = 0xbb,
    /// Synonym for OP_RETURN
    OP_RETURN_188 = 0xbc,
    /// Synonym for OP_RETURN
    OP_RETURN_189 = 0xbd,
    /// Synonym for OP_RETURN
    OP_RETURN_190 = 0xbe,
    /// Synonym for OP_RETURN
    OP_RETURN_191 = 0xbf,
    /// Synonym for OP_RETURN
    OP_RETURN_192 = 0xc0,
    /// Synonym for OP_RETURN
    OP_RETURN_193 = 0xc1,
    /// Synonym for OP_RETURN
    OP_RETURN_194 = 0xc2,
    /// Synonym for OP_RETURN
    OP_RETURN_195 = 0xc3,
    /// Synonym for OP_RETURN
    OP_RETURN_196 = 0xc4,
    /// Synonym for OP_RETURN
    OP_RETURN_197 = 0xc5,
    /// Synonym for OP_RETURN
    OP_RETURN_198 = 0xc6,
    /// Synonym for OP_RETURN
    OP_RETURN_199 = 0xc7,
    /// Synonym for OP_RETURN
    OP_RETURN_200 = 0xc8,
    /// Synonym for OP_RETURN
    OP_RETURN_201 = 0xc9,
    /// Synonym for OP_RETURN
    OP_RETURN_202 = 0xca,
    /// Synonym for OP_RETURN
    OP_RETURN_203 = 0xcb,
    /// Synonym for OP_RETURN
    OP_RETURN_204 = 0xcc,
    /// Synonym for OP_RETURN
    OP_RETURN_205 = 0xcd,
    /// Synonym for OP_RETURN
    OP_RETURN_206 = 0xce,
    /// Synonym for OP_RETURN
    OP_RETURN_207 = 0xcf,
    /// Synonym for OP_RETURN
    OP_RETURN_208 = 0xd0,
    /// Synonym for OP_RETURN
    OP_RETURN_209 = 0xd1,
    /// Synonym for OP_RETURN
    OP_RETURN_210 = 0xd2,
    /// Synonym for OP_RETURN
    OP_RETURN_211 = 0xd3,
    /// Synonym for OP_RETURN
    OP_RETURN_212 = 0xd4,
    /// Synonym for OP_RETURN
    OP_RETURN_213 = 0xd5,
    /// Synonym for OP_RETURN
    OP_RETURN_214 = 0xd6,
    /// Synonym for OP_RETURN
    OP_RETURN_215 = 0xd7,
    /// Synonym for OP_RETURN
    OP_RETURN_216 = 0xd8,
    /// Synonym for OP_RETURN
    OP_RETURN_217 = 0xd9,
    /// Synonym for OP_RETURN
    OP_RETURN_218 = 0xda,
    /// Synonym for OP_RETURN
    OP_RETURN_219 = 0xdb,
    /// Synonym for OP_RETURN
    OP_RETURN_220 = 0xdc,
    /// Synonym for OP_RETURN
    OP_RETURN_221 = 0xdd,
    /// Synonym for OP_RETURN
    OP_RETURN_222 = 0xde,
    /// Synonym for OP_RETURN
    OP_RETURN_223 = 0xdf,
    /// Synonym for OP_RETURN
    OP_RETURN_224 = 0xe0,
    /// Synonym for OP_RETURN
    OP_RETURN_225 = 0xe1,
    /// Synonym for OP_RETURN
    OP_RETURN_226 = 0xe2,
    /// Synonym for OP_RETURN
    OP_RETURN_227 = 0xe3,
    /// Synonym for OP_RETURN
    OP_RETURN_228 = 0xe4,
    /// Synonym for OP_RETURN
    OP_RETURN_229 = 0xe5,
    /// Synonym for OP_RETURN
    OP_RETURN_230 = 0xe6,
    /// Synonym for OP_RETURN
    OP_RETURN_231 = 0xe7,
    /// Synonym for OP_RETURN
    OP_RETURN_232 = 0xe8,
    /// Synonym for OP_RETURN
    OP_RETURN_233 = 0xe9,
    /// Synonym for OP_RETURN
    OP_RETURN_234 = 0xea,
    /// Synonym for OP_RETURN
    OP_RETURN_235 = 0xeb,
    /// Synonym for OP_RETURN
    OP_RETURN_236 = 0xec,
    /// Synonym for OP_RETURN
    OP_RETURN_237 = 0xed,
    /// Synonym for OP_RETURN
    OP_RETURN_238 = 0xee,
    /// Synonym for OP_RETURN
    OP_RETURN_239 = 0xef,
    /// Synonym for OP_RETURN
    OP_RETURN_240 = 0xf0,
    /// Synonym for OP_RETURN
    OP_RETURN_241 = 0xf1,
    /// Synonym for OP_RETURN
    OP_RETURN_242 = 0xf2,
    /// Synonym for OP_RETURN
    OP_RETURN_243 = 0xf3,
    /// Synonym for OP_RETURN
    OP_RETURN_244 = 0xf4,
    /// Synonym for OP_RETURN
    OP_RETURN_245 = 0xf5,
    /// Synonym for OP_RETURN
    OP_RETURN_246 = 0xf6,
    /// Synonym for OP_RETURN
    OP_RETURN_247 = 0xf7,
    /// Synonym for OP_RETURN
    OP_RETURN_248 = 0xf8,
    /// Synonym for OP_RETURN
    OP_RETURN_249 = 0xf9,
    /// Synonym for OP_RETURN
    OP_RETURN_250 = 0xfa,
    /// Synonym for OP_RETURN
    OP_RETURN_251 = 0xfb,
    /// Synonym for OP_RETURN
    OP_RETURN_252 = 0xfc,
    /// Synonym for OP_RETURN
    OP_RETURN_253 = 0xfd,
    /// Synonym for OP_RETURN
    OP_RETURN_254 = 0xfe,
    /// Synonym for OP_RETURN
    OP_RETURN_255 = 0xff,
}

impl All {
    /// Classifies an Opcode into a broad class
    #[inline]
    pub fn classify(&self) -> Class {
        // 17 opcodes
        if *self == All::OP_VERIF
            || *self == All::OP_VERNOTIF
            || *self == All::OP_CAT
            || *self == All::OP_SUBSTR
            || *self == All::OP_LEFT
            || *self == All::OP_RIGHT
            || *self == All::OP_INVERT
            || *self == All::OP_AND
            || *self == All::OP_OR
            || *self == All::OP_XOR
            || *self == All::OP_2MUL
            || *self == All::OP_2DIV
            || *self == All::OP_MUL
            || *self == All::OP_DIV
            || *self == All::OP_MOD
            || *self == All::OP_LSHIFT
            || *self == All::OP_RSHIFT
        {
            Class::IllegalOp
        // 11 opcodes
        } else if *self == All::OP_NOP
            || (All::OP_NOP1 as u8 <= *self as u8 && *self as u8 <= All::OP_NOP10 as u8)
        {
            Class::NoOp
        // 75 opcodes
        } else if *self == All::OP_RESERVED
            || *self == All::OP_VER
            || *self == All::OP_RETURN
            || *self == All::OP_RESERVED1
            || *self == All::OP_RESERVED2
            || *self as u8 >= All::OP_RETURN_186 as u8
        {
            Class::ReturnOp
        // 1 opcode
        } else if *self == All::OP_PUSHNUM_NEG1 {
            Class::PushNum(-1)
        // 16 opcodes
        } else if All::OP_PUSHNUM_1 as u8 <= *self as u8 && *self as u8 <= All::OP_PUSHNUM_16 as u8
        {
            Class::PushNum(1 + *self as i32 - All::OP_PUSHNUM_1 as i32)
        // 76 opcodes
        } else if *self as u8 <= All::OP_PUSHBYTES_75 as u8 {
            Class::PushBytes(*self as u32)
        // 60 opcodes
        } else {
            Class::Ordinary(unsafe { transmute::<All, Ordinary>(*self) })
        }
    }
}

impl From<u8> for All {
    #[inline]
    fn from(b: u8) -> All {
        unsafe { transmute(b) }
    }
}

display_from_debug!(All);

impl<D: SimpleDecoder> ConsensusDecodable<D> for All {
    #[inline]
    fn consensus_decode(d: &mut D) -> Result<All, serialize::Error> {
        Ok(All::from(d.read_u8()?))
    }
}

impl<S: SimpleEncoder> ConsensusEncodable<S> for All {
    #[inline]
    fn consensus_encode(&self, s: &mut S) -> Result<(), serialize::Error> {
        s.emit_u8(*self as u8)
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for All {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// Empty stack is also FALSE
pub static OP_FALSE: All = All::OP_PUSHBYTES_0;
/// Number 1 is also TRUE
pub static OP_TRUE: All = All::OP_PUSHNUM_1;
/// check locktime verify
pub static OP_CLTV: All = All::OP_NOP2;
/// check sequence verify
pub static OP_CSV: All = All::OP_NOP3;

/// Broad categories of opcodes with similar behavior
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Class {
    /// Pushes the given number onto the stack
    PushNum(i32),
    /// Pushes the given number of bytes onto the stack
    PushBytes(u32),
    /// Fails the script if executed
    ReturnOp,
    /// Fails the script even if not executed
    IllegalOp,
    /// Does nothing
    NoOp,
    /// Any opcode not covered above
    Ordinary(Ordinary),
}

display_from_debug!(Class);

#[cfg(feature = "serde")]
impl serde::Serialize for Class {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

macro_rules! ordinary_opcode {
  ($($op:ident),*) => (
    #[repr(u8)]
    #[doc(hidden)]
    #[derive(Copy, Clone, PartialEq, Eq, Debug)]
    pub enum Ordinary {
      $( $op = All::$op as u8 ),*
    }
  );
}

// "Ordinary" opcodes -- should be 60 of these
ordinary_opcode! {
  // pushdata
  OP_PUSHDATA1, OP_PUSHDATA2, OP_PUSHDATA4,
  // control flow
  OP_IF, OP_NOTIF, OP_ELSE, OP_ENDIF, OP_VERIFY,
  // stack
  OP_TOALTSTACK, OP_FROMALTSTACK,
  OP_2DROP, OP_2DUP, OP_3DUP, OP_2OVER, OP_2ROT, OP_2SWAP,
  OP_DROP, OP_DUP, OP_NIP, OP_OVER, OP_PICK, OP_ROLL, OP_ROT, OP_SWAP, OP_TUCK,
  OP_IFDUP, OP_DEPTH, OP_SIZE,
  // equality
  OP_EQUAL, OP_EQUALVERIFY,
  // arithmetic
  OP_1ADD, OP_1SUB, OP_NEGATE, OP_ABS, OP_NOT, OP_0NOTEQUAL,
  OP_ADD, OP_SUB, OP_BOOLAND, OP_BOOLOR,
  OP_NUMEQUAL, OP_NUMEQUALVERIFY, OP_NUMNOTEQUAL, OP_LESSTHAN,
  OP_GREATERTHAN, OP_LESSTHANOREQUAL, OP_GREATERTHANOREQUAL,
  OP_MIN, OP_MAX, OP_WITHIN,
  // crypto
  OP_RIPEMD160, OP_SHA1, OP_SHA256, OP_HASH160, OP_HASH256,
  OP_CODESEPARATOR, OP_CHECKSIG, OP_CHECKSIGVERIFY,
  OP_CHECKMULTISIG, OP_CHECKMULTISIGVERIFY
}
