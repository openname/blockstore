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

//! This module (`c32_old`) is only here to test compatibility with the new `c32`
//! module. It will be removed in the next network upgrade.

use sha2::{Digest, Sha256};

use super::Error;

const C32_CHARACTERS: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn c32_encode(input_bytes: &[u8]) -> String {
    let c32_chars: &[u8] = C32_CHARACTERS.as_bytes();

    let mut result = vec![];
    let mut carry = 0;
    let mut carry_bits = 0;

    for current_value in input_bytes.iter().rev() {
        let low_bits_to_take = 5 - carry_bits;
        let low_bits = current_value & ((1 << low_bits_to_take) - 1);
        let c32_value = (low_bits << carry_bits) + carry;
        result.push(c32_chars[c32_value as usize]);
        carry_bits = (8 + carry_bits) - 5;
        carry = current_value >> (8 - carry_bits);

        if carry_bits >= 5 {
            let c32_value = carry & ((1 << 5) - 1);
            result.push(c32_chars[c32_value as usize]);
            carry_bits -= 5;
            carry >>= 5;
        }
    }

    if carry_bits > 0 {
        result.push(c32_chars[carry as usize]);
    }

    // remove leading zeros from c32 encoding
    while let Some(v) = result.pop() {
        if v != c32_chars[0] {
            result.push(v);
            break;
        }
    }

    // add leading zeros from input.
    for current_value in input_bytes.iter() {
        if *current_value == 0 {
            result.push(c32_chars[0]);
        } else {
            break;
        }
    }

    let result: Vec<u8> = result.into_iter().rev().collect();
    String::from_utf8(result).unwrap()
}

fn c32_normalize(input_str: &str) -> String {
    let norm_str: String = input_str
        .to_uppercase()
        .replace('O', "0")
        .replace(['L', 'I'], "1");
    norm_str
}

fn c32_decode(input_str: &str) -> Result<Vec<u8>, Error> {
    // must be ASCII
    if !input_str.is_ascii() {
        return Err(Error::InvalidCrockford32);
    }

    let mut result = vec![];
    let mut carry: u16 = 0;
    let mut carry_bits = 0; // can be up to 5

    let iter_c32_digits_opts: Vec<Option<usize>> = c32_normalize(input_str)
        .chars()
        .rev()
        .map(|x| C32_CHARACTERS.find(x))
        .collect();

    let iter_c32_digits: Vec<usize> = iter_c32_digits_opts
        .iter()
        .filter_map(|x| x.as_ref())
        .copied()
        .collect();

    if iter_c32_digits.len() != iter_c32_digits_opts.len() {
        // at least one char was None
        return Err(Error::InvalidCrockford32);
    }

    for current_5bit in iter_c32_digits {
        carry += (current_5bit as u16) << carry_bits;
        carry_bits += 5;

        if carry_bits >= 8 {
            result.push((carry & ((1 << 8) - 1)) as u8);
            carry_bits -= 8;
            carry >>= 8;
        }
    }

    if carry_bits > 0 {
        result.push(carry as u8);
    }

    // remove leading zeros from Vec<u8> encoding
    while let Some(v) = result.pop() {
        if v != 0 {
            result.push(v);
            break;
        }
    }

    // add leading zeros from input.
    for current_value in input_str.chars() {
        if current_value == '0' {
            result.push(0);
        } else {
            break;
        }
    }

    result.reverse();
    Ok(result)
}

fn double_sha256_checksum(data: &[u8]) -> Vec<u8> {
    let mut sha2 = Sha256::new();
    let mut tmp = [0u8; 32];
    let mut tmp_2 = [0u8; 32];

    sha2.update(data);
    tmp.copy_from_slice(sha2.finalize().as_slice());

    let mut sha2_2 = Sha256::new();
    sha2_2.update(tmp);
    tmp_2.copy_from_slice(sha2_2.finalize().as_slice());

    tmp_2[0..4].to_vec()
}

fn c32_check_encode(version: u8, data: &[u8]) -> Result<String, Error> {
    if version >= 32 {
        return Err(Error::InvalidVersion(version));
    }

    let mut check_data = vec![version];
    check_data.extend_from_slice(data);
    let checksum = double_sha256_checksum(&check_data);

    let mut encoding_data = data.to_vec();
    encoding_data.extend_from_slice(&checksum);

    // working with ascii strings is awful.
    let mut c32_string = c32_encode(&encoding_data).into_bytes();
    let version_char = C32_CHARACTERS.as_bytes()[version as usize];
    c32_string.insert(0, version_char);

    Ok(String::from_utf8(c32_string).unwrap())
}

fn c32_check_decode(check_data_unsanitized: &str) -> Result<(u8, Vec<u8>), Error> {
    // must be ASCII
    if !check_data_unsanitized.is_ascii() {
        return Err(Error::InvalidCrockford32);
    }

    if check_data_unsanitized.len() < 2 {
        return Err(Error::InvalidCrockford32);
    }

    let check_data = c32_normalize(check_data_unsanitized);
    let (version, data) = check_data.split_at(1);

    let data_sum_bytes = c32_decode(data)?;
    if data_sum_bytes.len() < 5 {
        return Err(Error::InvalidCrockford32);
    }

    let (data_bytes, expected_sum) = data_sum_bytes.split_at(data_sum_bytes.len() - 4);

    let mut check_data = c32_decode(version)?;
    check_data.extend_from_slice(data_bytes);

    let computed_sum = double_sha256_checksum(&check_data);
    if computed_sum != expected_sum {
        let computed_sum_u32 = (computed_sum[0] as u32)
            | ((computed_sum[1] as u32) << 8)
            | ((computed_sum[2] as u32) << 16)
            | ((computed_sum[3] as u32) << 24);

        let expected_sum_u32 = (expected_sum[0] as u32)
            | ((expected_sum[1] as u32) << 8)
            | ((expected_sum[2] as u32) << 16)
            | ((expected_sum[3] as u32) << 24);

        return Err(Error::BadChecksum(computed_sum_u32, expected_sum_u32));
    }

    let version = check_data[0];
    let data = data_bytes.to_vec();
    Ok((version, data))
}

pub fn c32_address_decode(c32_address_str: &str) -> Result<(u8, Vec<u8>), Error> {
    if !c32_address_str.is_ascii() || c32_address_str.len() <= 5 {
        Err(Error::InvalidCrockford32)
    } else {
        c32_check_decode(&c32_address_str[1..])
    }
}

pub fn c32_address(version: u8, data: &[u8]) -> Result<String, Error> {
    let c32_string = c32_check_encode(version, data)?;
    Ok(format!("S{}", c32_string))
}
