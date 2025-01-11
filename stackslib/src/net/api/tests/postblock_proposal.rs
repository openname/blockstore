// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2024 Stacks Open Internet Foundation
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

use std::cell::RefCell;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};

use clarity::types::chainstate::{StacksPrivateKey, TrieHash};
use clarity::util::secp256k1::MessageSignature;
use clarity::util::vrf::VRFProof;
use clarity::vm::ast::ASTRules;
use clarity::vm::types::{PrincipalData, QualifiedContractIdentifier, StacksAddressExtensions};
use clarity::vm::{ClarityName, ContractName, Value};
use mempool::{MemPoolDB, MemPoolEventDispatcher, ProposalCallbackReceiver};
use postblock_proposal::{NakamotoBlockProposal, ValidateRejectCode};
use stacks_common::bitvec::BitVec;
use stacks_common::types::chainstate::{ConsensusHash, StacksAddress};
use stacks_common::types::net::PeerHost;
use stacks_common::types::{Address, StacksEpochId};
use stacks_common::util::hash::{hex_bytes, Hash160, MerkleTree, Sha512Trunc256Sum};

use super::TestRPC;
use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::burn::BlockSnapshot;
use crate::chainstate::nakamoto::miner::NakamotoBlockBuilder;
use crate::chainstate::nakamoto::{NakamotoBlock, NakamotoBlockHeader, NakamotoChainState};
use crate::chainstate::stacks::db::StacksChainState;
use crate::chainstate::stacks::miner::{BlockBuilder, BlockLimitFunction};
use crate::chainstate::stacks::test::{make_codec_test_block, make_codec_test_nakamoto_block};
use crate::chainstate::stacks::{
    CoinbasePayload, StacksBlockHeader, StacksTransactionSigner, TenureChangeCause,
    TenureChangePayload, TokenTransferMemo, TransactionAnchorMode, TransactionAuth,
    TransactionPayload, TransactionPostConditionMode, TransactionVersion,
};
use crate::core::BLOCK_LIMIT_MAINNET_21;
use crate::net::api::*;
use crate::net::connection::ConnectionOptions;
use crate::net::httpcore::{
    HttpRequestContentsExtensions, RPCRequestHandler, StacksHttp, StacksHttpRequest,
};
use crate::net::relay::Relayer;
use crate::net::test::TestEventObserver;
use crate::net::{ProtocolFamily, TipRequest};

#[test]
fn test_try_parse_request() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 33333);
    let mut http = StacksHttp::new(addr.clone(), &ConnectionOptions::default());

    let block = make_codec_test_nakamoto_block(StacksEpochId::Epoch30, &StacksPrivateKey::new());
    let proposal = NakamotoBlockProposal {
        block: block.clone(),
        chain_id: 0x80000000,
    };
    let mut request = StacksHttpRequest::new_for_peer(
        addr.into(),
        "POST".into(),
        "/v3/block_proposal".into(),
        HttpRequestContents::new().payload_json(serde_json::to_value(proposal).unwrap()),
    )
    .expect("failed to construct request");
    let bytes = request.try_serialize().unwrap();

    let (parsed_preamble, offset) = http.read_preamble(&bytes).unwrap();
    let mut handler =
        postblock_proposal::RPCBlockProposalRequestHandler::new(Some("password".into()));

    // missing authorization header
    let bad_request = http.handle_try_parse_request(
        &mut handler,
        &parsed_preamble.expect_request(),
        &bytes[offset..],
    );
    match bad_request {
        Err(crate::net::Error::Http(crate::net::http::Error::Http(err_code, message))) => {
            assert_eq!(err_code, 401);
            assert_eq!(message, "Unauthorized");
        }
        _ => panic!("expected error"),
    }

    // add the authorization header
    request.add_header("authorization".into(), "password".into());
    let bytes = request.try_serialize().unwrap();
    let (parsed_preamble, offset) = http.read_preamble(&bytes).unwrap();
    let mut parsed_request = http
        .handle_try_parse_request(
            &mut handler,
            &parsed_preamble.expect_request(),
            &bytes[offset..],
        )
        .unwrap();

    assert_eq!(
        handler.block_proposal,
        Some(NakamotoBlockProposal {
            block,
            chain_id: 0x80000000
        })
    );

    // parsed request consumes headers that would not be in a constructed request
    parsed_request.clear_headers();
    // but the authorization header should still be there
    parsed_request.add_header("authorization".into(), "password".into());
    let (preamble, contents) = parsed_request.destruct();

    assert_eq!(&preamble, request.preamble());

    handler.restart();
    assert!(handler.auth.is_some());
    assert!(handler.block_proposal.is_none());
}

struct ProposalObserver {
    results: Mutex<
        Vec<Result<postblock_proposal::BlockValidateOk, postblock_proposal::BlockValidateReject>>,
    >,
    condvar: Condvar,
}

impl ProposalObserver {
    fn new() -> Self {
        Self {
            results: Mutex::new(vec![]),
            condvar: Condvar::new(),
        }
    }
}

impl ProposalCallbackReceiver for ProposalObserver {
    fn notify_proposal_result(
        &self,
        result: Result<
            postblock_proposal::BlockValidateOk,
            postblock_proposal::BlockValidateReject,
        >,
    ) {
        let mut results = self.results.lock().unwrap();
        results.push(result);
        self.condvar.notify_one();
    }
}

struct ProposalTestObserver {
    pub proposal_observer: Arc<Mutex<ProposalObserver>>,
}

impl ProposalTestObserver {
    fn new() -> Self {
        Self {
            proposal_observer: Arc::new(Mutex::new(ProposalObserver::new())),
        }
    }
}

impl ProposalCallbackReceiver for Arc<Mutex<ProposalObserver>> {
    fn notify_proposal_result(
        &self,
        result: Result<
            postblock_proposal::BlockValidateOk,
            postblock_proposal::BlockValidateReject,
        >,
    ) {
        let observer = self.lock().unwrap();
        observer.notify_proposal_result(result);
    }
}

impl MemPoolEventDispatcher for ProposalTestObserver {
    fn get_proposal_callback_receiver(&self) -> Option<Box<dyn mempool::ProposalCallbackReceiver>> {
        Some(Box::new(Arc::clone(&self.proposal_observer)))
    }

    fn mempool_txs_dropped(
        &self,
        txids: Vec<Txid>,
        new_txid: Option<Txid>,
        reason: mempool::MemPoolDropReason,
    ) {
    }

    fn mined_block_event(
        &self,
        target_burn_height: u64,
        block: &crate::chainstate::stacks::StacksBlock,
        block_size_bytes: u64,
        consumed: &ExecutionCost,
        confirmed_microblock_cost: &ExecutionCost,
        tx_results: Vec<crate::chainstate::stacks::miner::TransactionEvent>,
    ) {
    }

    fn mined_microblock_event(
        &self,
        microblock: &StacksMicroblock,
        tx_results: Vec<crate::chainstate::stacks::miner::TransactionEvent>,
        anchor_block_consensus_hash: ConsensusHash,
        anchor_block: BlockHeaderHash,
    ) {
    }

    fn mined_nakamoto_block_event(
        &self,
        target_burn_height: u64,
        block: &crate::chainstate::nakamoto::NakamotoBlock,
        block_size_bytes: u64,
        consumed: &ExecutionCost,
        tx_results: Vec<crate::chainstate::stacks::miner::TransactionEvent>,
    ) {
    }
}

#[test]
#[ignore]
fn test_try_make_response() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 33333);
    let test_observer = TestEventObserver::new();
    let mut rpc_test = TestRPC::setup_nakamoto(function_name!(), &test_observer);
    let mut requests = vec![];

    let tip =
        SortitionDB::get_canonical_burn_chain_tip(&rpc_test.peer_1.sortdb.as_ref().unwrap().conn())
            .unwrap();

    let (stacks_tip_ch, stacks_tip_bhh) = SortitionDB::get_canonical_stacks_chain_tip_hash(
        rpc_test.peer_1.sortdb.as_ref().unwrap().conn(),
    )
    .unwrap();
    let stacks_tip = StacksBlockId::new(&stacks_tip_ch, &stacks_tip_bhh);

    let miner_privk = &rpc_test.peer_1.miner.nakamoto_miner_key();

    let mut good_block = {
        let chainstate = rpc_test.peer_1.chainstate();
        let parent_stacks_header =
            NakamotoChainState::get_block_header(chainstate.db(), &stacks_tip)
                .unwrap()
                .unwrap();

        let proof_bytes = hex_bytes("9275df67a68c8745c0ff97b48201ee6db447f7c93b23ae24cdc2400f52fdb08a1a6ac7ec71bf9c9c76e96ee4675ebff60625af28718501047bfd87b810c2d2139b73c23bd69de66360953a642c2a330a").unwrap();
        let proof = VRFProof::from_bytes(&proof_bytes[..].to_vec()).unwrap();

        let privk = StacksPrivateKey::from_hex(
            "6d430bb91222408e7706c9001cfaeb91b08c2be6d5ac95779ab52c6b431950e001",
        )
        .unwrap();

        let stx_address = StacksAddress {
            version: 1,
            bytes: Hash160([0xff; 20]),
        };
        let payload = TransactionPayload::TokenTransfer(
            stx_address.into(),
            123,
            TokenTransferMemo([0u8; 34]),
        );

        let auth = TransactionAuth::from_p2pkh(miner_privk).unwrap();
        let addr = auth.origin().address_testnet();
        let mut tx = StacksTransaction::new(TransactionVersion::Testnet, auth, payload);
        tx.chain_id = 0x80000000;
        tx.auth.set_origin_nonce(36);
        tx.set_post_condition_mode(TransactionPostConditionMode::Allow);
        tx.set_tx_fee(300);
        let mut tx_signer = StacksTransactionSigner::new(&tx);
        tx_signer.sign_origin(miner_privk).unwrap();
        let tx = tx_signer.get_tx().unwrap();

        let mut builder = NakamotoBlockBuilder::new(
            &parent_stacks_header,
            &parent_stacks_header.consensus_hash,
            26000,
            None,
            None,
            8,
            None,
        )
        .unwrap();

        rpc_test
            .peer_1
            .with_db_state(
                |sort_db: &mut SortitionDB,
                 chainstate: &mut StacksChainState,
                 _: &mut Relayer,
                 _: &mut MemPoolDB| {
                    let burn_dbconn = sort_db.index_handle_at_tip();
                    let mut miner_tenure_info = builder
                        .load_tenure_info(chainstate, &burn_dbconn, None)
                        .unwrap();
                    let mut tenure_tx = builder
                        .tenure_begin(&burn_dbconn, &mut miner_tenure_info)
                        .unwrap();
                    builder.try_mine_tx_with_len(
                        &mut tenure_tx,
                        &tx,
                        tx.tx_len(),
                        &BlockLimitFunction::NO_LIMIT_HIT,
                        ASTRules::PrecheckSize,
                    );
                    let block = builder.mine_nakamoto_block(&mut tenure_tx);
                    Ok(block)
                },
            )
            .unwrap()
    };

    // Increment the timestamp by 1 to ensure it is different from the previous block
    good_block.header.timestamp += 1;
    rpc_test.peer_1.miner.sign_nakamoto_block(&mut good_block);

    // post the valid block proposal
    let proposal = NakamotoBlockProposal {
        block: good_block.clone(),
        chain_id: 0x80000000,
    };

    let mut request = StacksHttpRequest::new_for_peer(
        rpc_test.peer_1.to_peer_host(),
        "POST".into(),
        "/v3/block_proposal".into(),
        HttpRequestContents::new().payload_json(serde_json::to_value(proposal).unwrap()),
    )
    .expect("failed to construct request");
    request.add_header("authorization".into(), "password".into());
    requests.push(request);

    // Set the timestamp to a value in the past (but NOT BEFORE timeout)
    let mut early_time_block = good_block.clone();
    early_time_block.header.timestamp -= 400;
    rpc_test
        .peer_1
        .miner
        .sign_nakamoto_block(&mut early_time_block);

    // post the invalid block proposal
    let proposal = NakamotoBlockProposal {
        block: early_time_block,
        chain_id: 0x80000000,
    };

    let mut request = StacksHttpRequest::new_for_peer(
        rpc_test.peer_1.to_peer_host(),
        "POST".into(),
        "/v3/block_proposal".into(),
        HttpRequestContents::new().payload_json(serde_json::to_value(proposal).unwrap()),
    )
    .expect("failed to construct request");
    request.add_header("authorization".into(), "password".into());
    requests.push(request);

    // Set the timestamp to a value in the future
    let mut late_time_block = good_block.clone();
    late_time_block.header.timestamp += 20000;
    rpc_test
        .peer_1
        .miner
        .sign_nakamoto_block(&mut late_time_block);

    // post the invalid block proposal
    let proposal = NakamotoBlockProposal {
        block: late_time_block,
        chain_id: 0x80000000,
    };

    let mut request = StacksHttpRequest::new_for_peer(
        rpc_test.peer_1.to_peer_host(),
        "POST".into(),
        "/v3/block_proposal".into(),
        HttpRequestContents::new().payload_json(serde_json::to_value(proposal).unwrap()),
    )
    .expect("failed to construct request");
    request.add_header("authorization".into(), "password".into());
    requests.push(request);

    // Set the timestamp to a value in the past (BEFORE the timeout)
    let mut stale_block = good_block.clone();
    stale_block.header.timestamp -= 10000;
    rpc_test.peer_1.miner.sign_nakamoto_block(&mut stale_block);

    // post the invalid block proposal
    let proposal = NakamotoBlockProposal {
        block: stale_block,
        chain_id: 0x80000000,
    };

    let mut request = StacksHttpRequest::new_for_peer(
        rpc_test.peer_1.to_peer_host(),
        "POST".into(),
        "/v3/block_proposal".into(),
        HttpRequestContents::new().payload_json(serde_json::to_value(proposal).unwrap()),
    )
    .expect("failed to construct request");
    request.add_header("authorization".into(), "password".into());
    requests.push(request);

    // execute the requests
    let observer = ProposalTestObserver::new();
    let proposal_observer = Arc::clone(&observer.proposal_observer);

    info!("Run requests with observer");
    let responses = rpc_test.run_with_observer(requests, Some(&observer));

    for response in responses.iter().take(3) {
        assert_eq!(response.preamble().status_code, 202);
    }
    let response = &responses[3];
    assert_eq!(response.preamble().status_code, 422);

    // Wait for the results of all 3 PROCESSED requests
    let start = std::time::Instant::now();
    loop {
        info!("Wait for results to be non-empty");
        if proposal_observer
            .lock()
            .unwrap()
            .results
            .lock()
            .unwrap()
            .len()
            < 3
        {
            std::thread::sleep(std::time::Duration::from_secs(1));
        } else {
            break;
        }
        assert!(
            start.elapsed().as_secs() < 60,
            "Timed out waiting for results"
        );
    }

    let observer = proposal_observer.lock().unwrap();
    let mut results = observer.results.lock().unwrap();

    let result = results.remove(0);
    match result {
        Ok(postblock_proposal::BlockValidateOk {
            signer_signature_hash,
            cost,
            size,
            validation_time_ms,
        }) => {
            assert_eq!(
                signer_signature_hash,
                good_block.header.signer_signature_hash()
            );
            assert_eq!(cost, ExecutionCost::ZERO);
            assert_eq!(size, 180);
            assert!(validation_time_ms > 0 && validation_time_ms < 60000);
        }
        _ => panic!("expected ok"),
    }

    let result = results.remove(0);
    match result {
        Ok(_) => panic!("expected error"),
        Err(postblock_proposal::BlockValidateReject {
            reason_code,
            reason,
            ..
        }) => {
            assert_eq!(reason_code, ValidateRejectCode::InvalidBlock);
            assert_eq!(reason, "Block timestamp is not greater than parent block");
        }
    }

    let result = results.remove(0);
    match result {
        Ok(_) => panic!("expected error"),
        Err(postblock_proposal::BlockValidateReject {
            reason_code,
            reason,
            ..
        }) => {
            assert_eq!(reason_code, ValidateRejectCode::InvalidBlock);
            assert_eq!(reason, "Block timestamp is too far into the future");
        }
    }
}
