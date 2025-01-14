// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2023 Stacks Open Internet Foundation
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

use std::fs::File;
use std::io::{BufRead, BufReader};

use regex::{Captures, Regex};
use stacks_common::types::net::PeerAddress;
use stacks_common::util::log;

use crate::net::Error as net_error;

// IPv4 prefix to ASN/org map entry
#[derive(Debug, Clone, PartialEq)]
pub struct ASEntry4 {
    pub prefix: u32,
    pub mask: u8,
    pub asn: u32,
    pub org: u32,
}

impl ASEntry4 {
    pub fn from_file(asn_file: &String) -> Result<Vec<ASEntry4>, net_error> {
        // each row in asn_file must be one of the following:
        // ^[:whitespace:]*([0-9]+.[0-9]+.[0-9]+.[0-9]+)/([0-9]+)[:whitespace:]+([0-9]+)[:whitespace:]*$
        // group 1 is the IP prefix
        // group 2 is the prefix length
        // group 3 is the AS number
        let file_handle = File::open(asn_file).map_err(|_e| net_error::FilesystemError)?;

        let mut line_cursor = BufReader::new(file_handle);
        ASEntry4::read_asn4_sequence(&mut line_cursor)
    }

    // read a sequence of ASEntry4 records
    fn read_asn4_sequence<R: BufRead>(fd: &mut R) -> Result<Vec<ASEntry4>, net_error> {
        let mut asn4 = vec![];

        let asn4_regex =
            Regex::new("^[ \t]*([0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+)/([0-9]+)[ \t]+([0-9]+)[ \t]*$")
                .unwrap();
        let asn4_whitespace_regex = Regex::new("^[ \t]*$|^[ \t]*#.+$").unwrap();
        let mut line_count = 0;
        let mut parsed = true;

        loop {
            let next_asn4_opt_res = ASEntry4::read_asn4(fd, &asn4_regex, &asn4_whitespace_regex);

            match next_asn4_opt_res {
                Ok(next_asn4_opt) => match next_asn4_opt {
                    None => {}
                    Some(asn4_rec) => {
                        asn4.push(asn4_rec);
                    }
                },
                Err(net_error::DeserializeError(msg)) => {
                    warn!("ASN4 parse error on line {}: {}", line_count, msg);
                    parsed = false;
                }
                Err(net_error::PermanentlyDrained) => {
                    // EOF
                    break;
                }
                Err(e) => {
                    return Err(e);
                }
            }

            line_count += 1;
        }
        if !parsed {
            return Err(net_error::DeserializeError(format!(
                "Failed to parse ASN4 sequence on line {}",
                line_count
            )));
        }

        asn4.sort_by(|a1, a2| a1.prefix.cmp(&a2.prefix));
        Ok(asn4)
    }

    // read one ASEntry4 record
    // Returns None on whitespace
    // Returns PermanentlyDrained on EOF
    fn read_asn4<R: BufRead>(
        fd: &mut R,
        asn4_regex: &Regex,
        asn4_whitespace_regex: &Regex,
    ) -> Result<Option<ASEntry4>, net_error> {
        let mut buf_full = String::new();
        let num_bytes = fd
            .read_line(&mut buf_full)
            .map_err(|_e| net_error::FilesystemError)?;

        if num_bytes == 0 {
            return Err(net_error::PermanentlyDrained);
        }

        // trim trailing newline
        let buf = buf_full.trim().to_string();

        // comment and/or whitespace?
        if asn4_whitespace_regex.is_match(&buf) {
            return Ok(None);
        }

        let caps = asn4_regex
            .captures(&buf)
            .ok_or(net_error::DeserializeError(
                "Line does not match ANS4 regex".to_string(),
            ))
            .map_err(|e| {
                debug!("Failed to read line \"{}\"", &buf);
                e
            })?;

        let prefix_octets_str = caps
            .get(1)
            .ok_or(net_error::DeserializeError(
                "Failed to read ANS4 prefix".to_string(),
            ))
            .map_err(|e| {
                debug!("Failed to get octets of \"{}\"", &buf);
                e
            })?
            .as_str();

        let prefix_mask_str = caps
            .get(2)
            .ok_or(net_error::DeserializeError(
                "Failed to read ASN4 prefix mask".to_string(),
            ))
            .map_err(|e| {
                debug!("Failed to get mask of \"{}\"", &buf);
                e
            })?
            .as_str();

        let asn_str = caps
            .get(3)
            .ok_or(net_error::DeserializeError(
                "Failed to read ASN ID".to_string(),
            ))
            .map_err(|e| {
                debug!("Failed to get ASN of \"{}\"", &buf);
                e
            })?
            .as_str();

        let prefix_octets_strs: Vec<&str> = prefix_octets_str.split('.').collect();
        if prefix_octets_strs.len() != 4 {
            debug!("Wrong number of octets in \"{}\"", &prefix_octets_str);
            return Err(net_error::DeserializeError(
                "Wrong number of octets".to_string(),
            ));
        }

        let mut prefix_octets: Vec<u8> = vec![];
        for octet_str in &prefix_octets_strs {
            let octet_opt = octet_str.parse::<u8>();
            if octet_opt.is_err() {
                debug!(
                    "Failed to parse octet \"{}\" in \"{}\"",
                    &octet_str, &prefix_octets_str
                );
                return Err(net_error::DeserializeError(
                    "Failed to parse octet".to_string(),
                ));
            }
            prefix_octets.push(octet_opt.unwrap());
        }

        let prefix = ((prefix_octets[0] as u32) << 24)
            | ((prefix_octets[1] as u32) << 16)
            | ((prefix_octets[2] as u32) << 8)
            | (prefix_octets[3] as u32);

        let mask_opt = prefix_mask_str.parse::<u8>();
        if mask_opt.is_err() {
            debug!("Failed to parse mask \"{}\"", &prefix_mask_str);
            return Err(net_error::DeserializeError(
                "Failed to parse ASN mask".to_string(),
            ));
        }
        let mask = mask_opt.unwrap();
        if mask < 8 || mask > 24 {
            debug!("Invalid mask \"{}\"", mask);
            return Err(net_error::DeserializeError(format!(
                "Invalid ASN mask {}",
                mask
            )));
        }

        let asn_opt = asn_str.parse::<u32>();
        if asn_opt.is_err() {
            debug!("Failed to parse ASN \"{}\"", asn_str);
            return Err(net_error::DeserializeError(
                "Failed to parse ASN".to_string(),
            ));
        }
        let asn = asn_opt.unwrap();

        Ok(Some(ASEntry4 {
            prefix,
            mask,
            asn,
            org: 0, // TODO
        }))
    }
}

#[cfg(test)]
mod test {
    use std::io;
    use std::io::BufRead;

    use stacks_common::util::log;

    use super::*;

    struct asn_fixture {
        text: String,
        result: Result<Vec<ASEntry4>, net_error>,
    }

    #[test]
    fn test_parse_asn4() {
        let tests = vec![
            asn_fixture {
                text: "1.0.0.0/8 1\n2.1.0.0/16 2\n".to_string(),
                result: Ok(vec![
                    ASEntry4 {
                        prefix: 0x01000000,
                        mask: 8,
                        asn: 1,
                        org: 0,
                    },
                    ASEntry4 {
                        prefix: 0x02010000,
                        mask: 16,
                        asn: 2,
                        org: 0
                    },
                ])
            },
            asn_fixture {
                text: "\n\n1.2.3.4/24 100\n    \n2.3.4.5/23 \t\t\t200\n  # this is a comment\n# so is this\n".to_string(),
                result: Ok(vec![
                    ASEntry4 {
                        prefix: 0x01020304,
                        mask: 24,
                        asn: 100,
                        org: 0,
                    },
                    ASEntry4 {
                        prefix: 0x02030405,
                        mask: 23,
                        asn: 200,
                        org: 0
                    },
                ]),
            },
            // invalid line
            asn_fixture {
                text: "1.2.3.4.5/24 100".to_string(),
                result: Err(net_error::DeserializeError("Failed to parse ASN4 sequence on line 1".to_string())),
            },
            // invalid prefix 
            asn_fixture {
                text: "257.0.0.0/8 100".to_string(),
                result: Err(net_error::DeserializeError("Failed to parse ASN4 sequence on line 1".to_string())),
            },
            // invalid mask 
            asn_fixture {
                text: "1.2.3.0/25 100".to_string(),
                result: Err(net_error::DeserializeError("Failed to parse ASN4 sequence on line 1".to_string())),
            },
            // invalid asn 
            asn_fixture {
                text: "1.2.3.0/24 4294967296".to_string(),
                result: Err(net_error::DeserializeError("Failed to parse ASN4 sequence on line 1".to_string())),
            },
        ];

        for test in &tests {
            let mut cur = io::Cursor::new(&test.text);
            let res = ASEntry4::read_asn4_sequence(&mut cur);
            assert_eq!(res, test.result);
        }
    }
}
