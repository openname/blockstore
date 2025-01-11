use clarity::vm::ast::build_ast;
use clarity::vm::test_util::{TEST_BURN_STATE_DB, TEST_HEADER_DB};
use clarity::vm::tests::test_clarity_versions;
use clarity::vm::types::QualifiedContractIdentifier;
use clarity::vm::version::ClarityVersion;
#[cfg(test)]
use rstest::rstest;
#[cfg(test)]
use rstest_reuse::{self, *};
use stacks_common::consts::CHAIN_ID_TESTNET;
use stacks_common::types::chainstate::StacksBlockId;
use stacks_common::types::StacksEpochId;

use crate::chainstate::stacks::index::ClarityMarfTrieId;
use crate::clarity_vm::clarity::ClarityInstance;
use crate::clarity_vm::database::marf::MarfedKV;

fn dependency_edge_counting_runtime(
    iters: usize,
    version: ClarityVersion,
    epoch: StacksEpochId,
) -> u64 {
    let mut progn = "(define-private (a0) 1)".to_string();
    for i in 1..iters {
        progn.push_str(&format!("\n(define-private (a{}) (begin", i));
        for x in 0..i {
            progn.push_str(&format!(" (a{}) ", x));
        }
        progn.push_str("))");
    }

    let marf = MarfedKV::temporary();
    let mut clarity_instance = ClarityInstance::new(false, CHAIN_ID_TESTNET, marf);

    clarity_instance
        .begin_test_genesis_block(
            &StacksBlockId::sentinel(),
            &StacksBlockId([0; 32]),
            &TEST_HEADER_DB,
            &TEST_BURN_STATE_DB,
        )
        .commit_block();

    let mut cost_track = clarity_instance
        .begin_block(
            &StacksBlockId([0; 32]),
            &StacksBlockId([1; 32]),
            &TEST_HEADER_DB,
            &TEST_BURN_STATE_DB,
        )
        .commit_block();

    build_ast(
        &QualifiedContractIdentifier::transient(),
        &progn,
        &mut cost_track,
        version,
        epoch,
    )
    .unwrap();

    cost_track.get_total().runtime
}

#[apply(test_clarity_versions)]
fn test_edge_counting_runtime(#[case] version: ClarityVersion, #[case] epoch: StacksEpochId) {
    let ratio_4_8 = dependency_edge_counting_runtime(8, version, epoch)
        / dependency_edge_counting_runtime(4, version, epoch);
    let ratio_8_16 = dependency_edge_counting_runtime(16, version, epoch)
        / dependency_edge_counting_runtime(8, version, epoch);

    // this really is just testing for the non-linearity
    //   in the runtime cost assessment (because the edge count in the dependency graph is going up O(n^2)).
    assert!(ratio_8_16 > ratio_4_8);
}
