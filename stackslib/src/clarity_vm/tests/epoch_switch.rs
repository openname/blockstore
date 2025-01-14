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

use clarity::vm::analysis::AnalysisDatabase;
use clarity::vm::costs::ExecutionCost;
use clarity::vm::database::{
    BurnStateDB, ClarityBackingStore, ClarityDatabase, HeadersDB, SqliteConnection,
    NULL_BURN_STATE_DB, NULL_HEADER_DB,
};
use clarity::vm::errors::{InterpreterResult, RuntimeErrorType};
use clarity::vm::test_util::{TEST_BURN_STATE_DB, TEST_HEADER_DB};
use rand::{thread_rng, RngCore};
use rusqlite::{Connection, OptionalExtension};
use stacks_common::types::chainstate::{
    BlockHeaderHash, BurnchainHeaderHash, SortitionId, StacksAddress, StacksBlockId, VRFSeed,
};
use stacks_common::util::hash::to_hex;

use crate::burnchains::PoxConstants;
use crate::chainstate::burn::db::sortdb::{
    SortitionDB, SortitionDBConn, SortitionHandleConn, SortitionHandleTx,
};
use crate::chainstate::stacks::db::{MinerPaymentSchedule, StacksHeaderInfo};
use crate::chainstate::stacks::index::{ClarityMarfTrieId, MarfTrieId, TrieMerkleProof};
use crate::core::{
    StacksEpoch, StacksEpochId, PEER_VERSION_EPOCH_1_0, PEER_VERSION_EPOCH_2_0,
    PEER_VERSION_EPOCH_2_05, STACKS_EPOCH_MAX,
};
use crate::util_lib::db::{DBConn, FromRow};

fn test_burnstatedb_epoch(
    burnstatedb: &dyn BurnStateDB,
    height_start: u32,
    height_end: u32,
    epoch_20_height: u32,
    epoch_2_05_height: u32,
    epoch_21_height: u32,
) {
    for height in height_start..height_end {
        debug!("Get epoch for block height {}", height);
        let cur_epoch = burnstatedb.get_stacks_epoch(height).unwrap();

        if height < epoch_20_height {
            assert_eq!(cur_epoch.epoch_id, StacksEpochId::Epoch10);
        } else if height < epoch_2_05_height {
            assert_eq!(cur_epoch.epoch_id, StacksEpochId::Epoch20);
        } else if height < epoch_21_height {
            assert_eq!(cur_epoch.epoch_id, StacksEpochId::Epoch2_05);
        } else {
            assert_eq!(cur_epoch.epoch_id, StacksEpochId::Epoch21);
        }
    }
}

#[test]
fn test_vm_epoch_switch() {
    use crate::burnchains::PoxConstants;
    use crate::chainstate::burn::db::sortdb::tests::test_append_snapshot;

    let mut rng = rand::thread_rng();
    let mut buf = [0u8; 32];
    rng.fill_bytes(&mut buf);
    let db_path_dir = format!(
        "/tmp/stacks-node-tests/unit-tests-sortdb/db-{}",
        to_hex(&buf)
    );

    let mut db = SortitionDB::connect(
        &db_path_dir,
        3,
        &BurnchainHeaderHash([0u8; 32]),
        0,
        &vec![
            StacksEpoch {
                epoch_id: StacksEpochId::Epoch10,
                start_height: 0,
                end_height: 8,
                block_limit: ExecutionCost::max_value(),
                network_epoch: PEER_VERSION_EPOCH_1_0,
            },
            StacksEpoch {
                epoch_id: StacksEpochId::Epoch20,
                start_height: 8,
                end_height: 12,
                block_limit: ExecutionCost::max_value(),
                network_epoch: PEER_VERSION_EPOCH_2_0,
            },
            StacksEpoch {
                epoch_id: StacksEpochId::Epoch2_05,
                start_height: 12,
                end_height: 16,
                block_limit: ExecutionCost::max_value(),
                network_epoch: PEER_VERSION_EPOCH_2_05,
            },
            StacksEpoch {
                epoch_id: StacksEpochId::Epoch21,
                start_height: 16,
                end_height: STACKS_EPOCH_MAX,
                block_limit: ExecutionCost::max_value(),
                network_epoch: PEER_VERSION_EPOCH_2_05,
            },
        ],
        PoxConstants::test_default(),
        None,
        true,
    )
    .unwrap();

    let mut cur_snapshot = SortitionDB::get_canonical_burn_chain_tip(db.conn()).unwrap();
    let start_height = cur_snapshot.block_height as u32;
    let mut end_height = 0;
    for i in 0..20 {
        cur_snapshot =
            test_append_snapshot(&mut db, BurnchainHeaderHash([((i + 1) as u8); 32]), &[]);
        end_height = cur_snapshot.block_height as u32;
    }

    // impl BurnStateDB for SortitionHandleConn
    {
        let burndb = db.index_handle_at_tip();
        test_burnstatedb_epoch(&burndb, start_height, end_height, 8, 12, 16);
    }

    // impl BurnStateDB for SortitionHandleTx
    {
        let tip = SortitionDB::get_canonical_burn_chain_tip(db.conn()).unwrap();
        let burntx = db.tx_handle_begin(&tip.sortition_id).unwrap();
        test_burnstatedb_epoch(&burntx, start_height, end_height, 8, 12, 16);
    }
}
