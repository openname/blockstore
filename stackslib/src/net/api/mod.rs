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

use clarity::vm::costs::ExecutionCost;
use stacks_common::codec::read_next;
use stacks_common::types::chainstate::{
    BlockHeaderHash, BurnchainHeaderHash, ConsensusHash, SortitionId, StacksBlockId,
};
use stacks_common::util::hash::Hash160;
use stacks_common::util::HexError;

use crate::burnchains::Txid;
use crate::chainstate::stacks::{StacksMicroblock, StacksTransaction};
use crate::core::mempool;
use crate::cost_estimates::FeeRateEstimate;
use crate::net::atlas::GetAttachmentResponse;
use crate::net::http::{
    Error, HttpRequest, HttpRequestContents, HttpRequestPreamble, HttpResponse,
    HttpResponseContents, HttpResponsePayload, HttpResponsePreamble,
};
use crate::net::httpcore::{StacksHttp, StacksHttpRequest, StacksHttpResponse};
use crate::net::Error as NetError;
use crate::stacks_common::codec::StacksMessageCodec;

pub mod callreadonly;
pub mod get_tenures_fork_info;
pub mod getaccount;
pub mod getattachment;
pub mod getattachmentsinv;
pub mod getblock;
pub mod getblock_v3;
pub mod getblockbyheight;
pub mod getclaritymarfvalue;
pub mod getclaritymetadata;
pub mod getconstantval;
pub mod getcontractabi;
pub mod getcontractsrc;
pub mod getdatavar;
pub mod getheaders;
pub mod getinfo;
pub mod getistraitimplemented;
pub mod getmapentry;
pub mod getmicroblocks_confirmed;
pub mod getmicroblocks_indexed;
pub mod getmicroblocks_unconfirmed;
pub mod getneighbors;
pub mod getpoxinfo;
pub mod getsigner;
pub mod getsortition;
pub mod getstackerdbchunk;
pub mod getstackerdbmetadata;
pub mod getstackers;
pub mod getstxtransfercost;
pub mod gettenure;
pub mod gettenureinfo;
pub mod gettenuretip;
pub mod gettransaction_unconfirmed;
pub mod liststackerdbreplicas;
pub mod postblock;
pub mod postblock_proposal;
#[warn(unused_imports)]
pub mod postblock_v3;
pub mod postfeerate;
pub mod postmempoolquery;
pub mod postmicroblock;
pub mod poststackerdbchunk;
pub mod posttransaction;

#[cfg(test)]
mod tests;

impl StacksHttp {
    /// Register all RPC methods.
    /// Put your new RPC method handlers here.
    pub fn register_rpc_methods(&mut self) {
        self.register_rpc_endpoint(callreadonly::RPCCallReadOnlyRequestHandler::new(
            self.maximum_call_argument_size,
            self.read_only_call_limit.clone(),
        ));
        self.register_rpc_endpoint(getaccount::RPCGetAccountRequestHandler::new());
        self.register_rpc_endpoint(getattachment::RPCGetAttachmentRequestHandler::new());
        self.register_rpc_endpoint(getattachmentsinv::RPCGetAttachmentsInvRequestHandler::new());
        self.register_rpc_endpoint(getblock::RPCBlocksRequestHandler::new());
        self.register_rpc_endpoint(getblock_v3::RPCNakamotoBlockRequestHandler::new());
        self.register_rpc_endpoint(getblockbyheight::RPCNakamotoBlockByHeightRequestHandler::new());
        self.register_rpc_endpoint(getclaritymarfvalue::RPCGetClarityMarfRequestHandler::new());
        self.register_rpc_endpoint(getclaritymetadata::RPCGetClarityMetadataRequestHandler::new());
        self.register_rpc_endpoint(getconstantval::RPCGetConstantValRequestHandler::new());
        self.register_rpc_endpoint(getcontractabi::RPCGetContractAbiRequestHandler::new());
        self.register_rpc_endpoint(getcontractsrc::RPCGetContractSrcRequestHandler::new());
        self.register_rpc_endpoint(getdatavar::RPCGetDataVarRequestHandler::new());
        self.register_rpc_endpoint(getheaders::RPCHeadersRequestHandler::new());
        self.register_rpc_endpoint(getinfo::RPCPeerInfoRequestHandler::new());
        self.register_rpc_endpoint(
            getistraitimplemented::RPCGetIsTraitImplementedRequestHandler::new(),
        );
        self.register_rpc_endpoint(getmapentry::RPCGetMapEntryRequestHandler::new());
        self.register_rpc_endpoint(
            getmicroblocks_confirmed::RPCMicroblocksConfirmedRequestHandler::new(),
        );
        self.register_rpc_endpoint(
            getmicroblocks_indexed::RPCMicroblocksIndexedRequestHandler::new(),
        );
        self.register_rpc_endpoint(
            getmicroblocks_unconfirmed::RPCMicroblocksUnconfirmedRequestHandler::new(),
        );
        self.register_rpc_endpoint(getneighbors::RPCNeighborsRequestHandler::new());
        self.register_rpc_endpoint(getstxtransfercost::RPCGetStxTransferCostRequestHandler::new());
        self.register_rpc_endpoint(getstackerdbchunk::RPCGetStackerDBChunkRequestHandler::new());
        self.register_rpc_endpoint(getpoxinfo::RPCPoxInfoRequestHandler::new());
        self.register_rpc_endpoint(
            getstackerdbmetadata::RPCGetStackerDBMetadataRequestHandler::new(),
        );
        self.register_rpc_endpoint(getstackers::GetStackersRequestHandler::default());
        self.register_rpc_endpoint(getsortition::GetSortitionHandler::new());
        self.register_rpc_endpoint(gettenure::RPCNakamotoTenureRequestHandler::new());
        self.register_rpc_endpoint(gettenureinfo::RPCNakamotoTenureInfoRequestHandler::new());
        self.register_rpc_endpoint(gettenuretip::RPCNakamotoTenureTipRequestHandler::new());
        self.register_rpc_endpoint(get_tenures_fork_info::GetTenuresForkInfo::default());
        self.register_rpc_endpoint(
            gettransaction_unconfirmed::RPCGetTransactionUnconfirmedRequestHandler::new(),
        );
        self.register_rpc_endpoint(getsigner::GetSignerRequestHandler::default());
        self.register_rpc_endpoint(
            liststackerdbreplicas::RPCListStackerDBReplicasRequestHandler::new(),
        );
        self.register_rpc_endpoint(postblock::RPCPostBlockRequestHandler::new());
        self.register_rpc_endpoint(postblock_proposal::RPCBlockProposalRequestHandler::new(
            self.auth_token.clone(),
        ));
        self.register_rpc_endpoint(postblock_v3::RPCPostBlockRequestHandler::new(
            self.auth_token.clone(),
        ));
        self.register_rpc_endpoint(postfeerate::RPCPostFeeRateRequestHandler::new());
        self.register_rpc_endpoint(postmempoolquery::RPCMempoolQueryRequestHandler::new());
        self.register_rpc_endpoint(postmicroblock::RPCPostMicroblockRequestHandler::new());
        self.register_rpc_endpoint(poststackerdbchunk::RPCPostStackerDBChunkRequestHandler::new());
        self.register_rpc_endpoint(posttransaction::RPCPostTransactionRequestHandler::new());
    }
}

/// Helper conversion for NetError to Error
impl From<NetError> for Error {
    fn from(e: NetError) -> Error {
        match e {
            NetError::Http(e) => e,
            x => Error::AppError(format!("{x:?}")),
        }
    }
}

/// This module serde encodes and decodes optional byte fields in RPC
/// responses as Some(String) where the String is a `0x` prefixed
/// hex string.
pub mod prefix_opt_hex {
    pub fn serialize<S: serde::Serializer, T: std::fmt::LowerHex>(
        val: &Option<T>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match val {
            Some(ref some_val) => {
                let val_str = format!("0x{some_val:x}");
                s.serialize_some(&val_str)
            }
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>, T: super::HexDeser>(
        d: D,
    ) -> Result<Option<T>, D::Error> {
        let opt_inst_str: Option<String> = serde::Deserialize::deserialize(d)?;
        let Some(inst_str) = opt_inst_str else {
            return Ok(None);
        };
        let Some(hex_str) = inst_str.get(2..) else {
            return Err(serde::de::Error::invalid_length(
                inst_str.len(),
                &"at least length 2 string",
            ));
        };
        let val = T::try_from(hex_str).map_err(serde::de::Error::custom)?;
        Ok(Some(val))
    }
}

/// This module serde encodes and decodes byte fields in RPC
/// responses as a String where the String is a `0x` prefixed
/// hex string.
pub mod prefix_hex {
    pub fn serialize<S: serde::Serializer, T: std::fmt::LowerHex>(
        val: &T,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("0x{val:x}"))
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>, T: super::HexDeser>(
        d: D,
    ) -> Result<T, D::Error> {
        let inst_str: String = serde::Deserialize::deserialize(d)?;
        let Some(hex_str) = inst_str.get(2..) else {
            return Err(serde::de::Error::invalid_length(
                inst_str.len(),
                &"at least length 2 string",
            ));
        };
        T::try_from(hex_str).map_err(serde::de::Error::custom)
    }
}

pub trait HexDeser: Sized {
    fn try_from(hex: &str) -> Result<Self, HexError>;
}

macro_rules! impl_hex_deser {
    ($thing:ident) => {
        impl HexDeser for $thing {
            fn try_from(hex: &str) -> Result<Self, HexError> {
                $thing::from_hex(hex)
            }
        }
    };
}

impl_hex_deser!(BurnchainHeaderHash);
impl_hex_deser!(StacksBlockId);
impl_hex_deser!(SortitionId);
impl_hex_deser!(ConsensusHash);
impl_hex_deser!(BlockHeaderHash);
impl_hex_deser!(Hash160);
