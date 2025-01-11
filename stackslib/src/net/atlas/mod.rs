// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2021 Stacks Open Internet Foundation
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

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use clarity::vm::types::{QualifiedContractIdentifier, SequenceData, TupleData, Value};
use lazy_static::lazy_static;
use regex::Regex;
use serde::de::{Deserialize, Error as de_Error};
use serde::ser::Serialize;
use stacks_common::codec::StacksMessageCodec;
use stacks_common::types::chainstate::{BlockHeaderHash, StacksBlockId};
use stacks_common::util::hash::{hex_bytes, to_hex, Hash160, MerkleHashFunc};

pub use self::db::AtlasDB;
pub use self::download::AttachmentsDownloader;
use crate::burnchains::Txid;
use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::burn::ConsensusHash;
use crate::util_lib::boot::boot_code_id;

/// Implements AtlasDB and associated API. Stores information about attachments and attachment
/// instances.
pub mod db;
/// Implements `AttachmentsDownloader`, which attempts to download the requested batch of
/// attachment instances from peers.
pub mod download;

pub const MAX_ATTACHMENT_INV_PAGES_PER_REQUEST: usize = 8;
pub const MAX_RETRY_DELAY: u64 = 600; // seconds
/// This is the maximum number of pending attachments batches allowed
///  in the synchronized channel before the coordinator will stall
///  waiting for attachments to be processed.
pub const ATTACHMENTS_CHANNEL_SIZE: usize = 5;

lazy_static! {
    pub static ref BNS_CHARS_REGEX: Regex = Regex::new("^([a-z0-9]|[-_])*$").unwrap();
}

const ATTACHMENTS_MAX_SIZE_MIN: u32 = 1_048_576;
const MAX_UNINSTANTIATED_ATTACHMENTS_MIN: u32 = 50_000;
const UNINSTANTIATED_ATTACHMENTS_EXPIRE_AFTER_MIN: u32 = 86_400;
const UNRESOLVED_ATTACHMENT_INSTANCES_EXPIRE_AFTER_MIN: u32 = 172_800;

#[derive(Debug, Clone, PartialEq)]
pub struct GetAttachmentResponse {
    pub attachment: Attachment,
}

impl Serialize for GetAttachmentResponse {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let hex_encoded = to_hex(&self.attachment.content[..]);
        s.serialize_str(hex_encoded.as_str())
    }
}

impl<'de> Deserialize<'de> for GetAttachmentResponse {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<GetAttachmentResponse, D::Error> {
        let payload = String::deserialize(d)?;
        let hex_encoded = payload.parse::<String>().map_err(de_Error::custom)?;
        let bytes = hex_bytes(&hex_encoded).map_err(de_Error::custom)?;
        let attachment = Attachment::new(bytes);
        Ok(GetAttachmentResponse { attachment })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetAttachmentsInvResponse {
    pub block_id: StacksBlockId,
    pub pages: Vec<AttachmentPage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentPage {
    pub index: u32,
    pub inventory: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct AtlasConfig {
    pub contracts: HashSet<QualifiedContractIdentifier>,
    pub attachments_max_size: u32,
    pub max_uninstantiated_attachments: u32,
    pub uninstantiated_attachments_expire_after: u32,
    pub unresolved_attachment_instances_expire_after: u32,
    pub genesis_attachments: Option<Vec<Attachment>>,
}

impl AtlasConfig {
    pub fn new(mainnet: bool) -> AtlasConfig {
        let mut contracts = HashSet::new();
        contracts.insert(boot_code_id("bns", mainnet));
        AtlasConfig {
            contracts,
            attachments_max_size: ATTACHMENTS_MAX_SIZE_MIN,
            max_uninstantiated_attachments: MAX_UNINSTANTIATED_ATTACHMENTS_MIN,
            uninstantiated_attachments_expire_after: UNINSTANTIATED_ATTACHMENTS_EXPIRE_AFTER_MIN,
            unresolved_attachment_instances_expire_after:
                UNRESOLVED_ATTACHMENT_INSTANCES_EXPIRE_AFTER_MIN,
            genesis_attachments: None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.attachments_max_size < ATTACHMENTS_MAX_SIZE_MIN {
            Err(format!(
                "Invalid value for `attachments_max_size`: {}. Expected {} or greater",
                self.attachments_max_size, ATTACHMENTS_MAX_SIZE_MIN
            ))
        } else if self.max_uninstantiated_attachments < MAX_UNINSTANTIATED_ATTACHMENTS_MIN {
            Err(format!(
                "Invalid value for `max_uninstantiated_attachments`: {}. Expected {} or greater",
                self.max_uninstantiated_attachments, MAX_UNINSTANTIATED_ATTACHMENTS_MIN
            ))
        } else if self.uninstantiated_attachments_expire_after
            < UNINSTANTIATED_ATTACHMENTS_EXPIRE_AFTER_MIN
        {
            Err(format!(
                "Invalid value for `uninstantiated_attachments_expire_after`: {}. Expected {} or greater",
                self.uninstantiated_attachments_expire_after, UNINSTANTIATED_ATTACHMENTS_EXPIRE_AFTER_MIN
            ))
        } else if self.unresolved_attachment_instances_expire_after
            < UNRESOLVED_ATTACHMENT_INSTANCES_EXPIRE_AFTER_MIN
        {
            Err(format!(
                "Invalid value for `unresolved_attachment_instances_expire_after`: {}. Expected {} or greater",
                self.unresolved_attachment_instances_expire_after, UNRESOLVED_ATTACHMENT_INSTANCES_EXPIRE_AFTER_MIN
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
/// Attachments are the content associated with an AttachmentInstance
pub struct Attachment {
    pub content: Vec<u8>,
}

impl Attachment {
    pub fn new(content: Vec<u8>) -> Attachment {
        Attachment { content }
    }

    pub fn hash(&self) -> Hash160 {
        Hash160::from_data(&self.content)
    }

    pub fn empty() -> Attachment {
        Attachment { content: vec![] }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
/// An attachment instance is a reference to atlas data: a commitment
/// to track the content that is the inverse of `content_hash`.
/// Attachment instances are created by atlas events issued by contracts
/// specified in a node's `AtlasConfig`.
pub struct AttachmentInstance {
    pub content_hash: Hash160,
    pub attachment_index: u32,
    pub stacks_block_height: u64,
    pub index_block_hash: StacksBlockId,
    pub metadata: String,
    pub contract_id: QualifiedContractIdentifier,
    pub tx_id: Txid,
    pub canonical_stacks_tip_height: Option<u64>,
}

impl AttachmentInstance {
    const ATTACHMENTS_INV_PAGE_SIZE: u32 = 64;

    pub fn try_new_from_value(
        value: &Value,
        contract_id: &QualifiedContractIdentifier,
        index_block_hash: StacksBlockId,
        stacks_block_height: u64,
        tx_id: Txid,
        canonical_stacks_tip_height: Option<u64>,
    ) -> Option<AttachmentInstance> {
        if let Value::Tuple(ref attachment) = value {
            if let Ok(Value::Tuple(ref attachment_data)) = attachment.get("attachment") {
                match (
                    attachment_data.get("hash"),
                    attachment_data.get("attachment-index"),
                ) {
                    (
                        Ok(Value::Sequence(SequenceData::Buffer(content_hash))),
                        Ok(Value::UInt(attachment_index)),
                    ) => {
                        let content_hash = if content_hash.data.is_empty() {
                            Hash160::empty()
                        } else {
                            match Hash160::from_bytes(&content_hash.data[..]) {
                                Some(content_hash) => content_hash,
                                _ => return None,
                            }
                        };
                        let metadata = match attachment_data.get("metadata") {
                            Ok(metadata) => {
                                let mut serialized = vec![];
                                metadata
                                    .consensus_serialize(&mut serialized)
                                    .expect("FATAL: invalid metadata");
                                to_hex(&serialized[..])
                            }
                            _ => String::new(),
                        };
                        let instance = AttachmentInstance {
                            index_block_hash,
                            content_hash,
                            attachment_index: *attachment_index as u32,
                            stacks_block_height,
                            metadata,
                            contract_id: contract_id.clone(),
                            tx_id,
                            canonical_stacks_tip_height,
                        };
                        return Some(instance);
                    }
                    _ => {}
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests;
