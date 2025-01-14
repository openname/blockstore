use std::env;

use clarity::vm::costs::ExecutionCost;
use clarity::vm::types::{PrincipalData, StandardPrincipalData};
use clarity::vm::Value;
use rand::seq::SliceRandom;
use rand::Rng;
use stacks_common::types::chainstate::{
    BlockHeaderHash, BurnchainHeaderHash, StacksAddress, StacksWorkScore,
};
use stacks_common::util::hash::{to_hex, Hash160, Sha512Trunc256Sum};

use crate::chainstate::burn::ConsensusHash;
use crate::chainstate::stacks::db::{StacksEpochReceipt, StacksHeaderInfo};
use crate::chainstate::stacks::events::StacksTransactionReceipt;
use crate::chainstate::stacks::{
    CoinbasePayload, StacksBlockHeader, StacksTransaction, TokenTransferMemo, TransactionAuth,
    TransactionContractCall, TransactionPayload, TransactionSpendingCondition, TransactionVersion,
};
use crate::core::StacksEpochId;
use crate::cost_estimates::fee_medians::{
    fee_rate_estimate_from_sorted_weighted_fees, FeeRateAndWeight, WeightedMedianFeeRateEstimator,
};
use crate::cost_estimates::metrics::{CostMetric, ProportionalDotProduct};
use crate::cost_estimates::tests::common::*;
use crate::cost_estimates::{EstimatorError, FeeEstimator, FeeRateEstimate};

/// Returns true iff `b` is within `0.1%` of `a`.
fn is_close_f64(a: f64, b: f64) -> bool {
    let error = (a - b).abs() / a.abs();
    error < 0.001
}

/// Returns `true` iff each value in `left` "close" to its counterpart in `right`.
fn is_close(left: FeeRateEstimate, right: FeeRateEstimate) -> bool {
    let is_ok = is_close_f64(left.high, right.high)
        && is_close_f64(left.middle, right.middle)
        && is_close_f64(left.low, right.low);
    if !is_ok {
        warn!("ExecutionCost's are not close. {:?} vs {:?}", left, right);
    }
    is_ok
}

fn instantiate_test_db<CM: CostMetric>(m: CM) -> WeightedMedianFeeRateEstimator<CM> {
    let mut path = env::temp_dir();
    let random_bytes = rand::thread_rng().gen::<[u8; 32]>();
    path.push(&format!("fee_db_{}.sqlite", &to_hex(&random_bytes)[0..8]));

    let window_size = 5;
    WeightedMedianFeeRateEstimator::open(&path, m, window_size)
        .expect("Test failure: could not open fee rate DB")
}

fn make_dummy_coinbase_tx() -> StacksTransaction {
    StacksTransaction::new(
        TransactionVersion::Mainnet,
        TransactionAuth::Standard(TransactionSpendingCondition::new_initial_sighash()),
        TransactionPayload::Coinbase(CoinbasePayload([0; 32]), None, None),
    )
}

fn make_dummy_cc_tx(fee: u64, execution_cost: &ExecutionCost) -> StacksTransactionReceipt {
    let mut tx = StacksTransaction::new(
        TransactionVersion::Mainnet,
        TransactionAuth::Standard(TransactionSpendingCondition::new_initial_sighash()),
        TransactionPayload::ContractCall(TransactionContractCall {
            address: StacksAddress::new(0, Hash160([0; 20])),
            contract_name: "cc-dummy".into(),
            function_name: "func-name".into(),
            function_args: vec![],
        }),
    );
    tx.set_tx_fee(fee);
    StacksTransactionReceipt::from_contract_call(
        tx,
        vec![],
        Value::okay(Value::Bool(true)).unwrap(),
        0,
        execution_cost.clone(),
    )
}

const block_limit: ExecutionCost = ExecutionCost {
    write_length: 100,
    write_count: 100,
    read_length: 100,
    read_count: 100,
    runtime: 100,
};

const tenth_operation_cost: ExecutionCost = ExecutionCost {
    write_length: 0,
    write_count: 0,
    read_length: 0,
    read_count: 0,
    runtime: 10,
};

const half_operation_cost: ExecutionCost = ExecutionCost {
    write_length: 0,
    write_count: 0,
    read_length: 0,
    read_count: 0,
    runtime: 50,
};

// The scalar cost of `make_dummy_cc_tx(_, &tenth_operation_cost)`.
const tenth_operation_cost_basis: u64 = 1164;

// The scalar cost of `make_dummy_cc_tx(_, &half_operation_cost)`.
const half_operation_cost_basis: u64 = 5164;

/// Tests that we have no estimate available until we `notify`.
#[test]
fn test_empty_fee_estimator() {
    let metric = ProportionalDotProduct::new(10_000);
    let estimator = instantiate_test_db(metric);
    assert_eq!(
        estimator
            .get_rate_estimates()
            .expect_err("Empty rate estimator should error."),
        EstimatorError::NoEstimateAvailable
    );
}

/// If we do not have any transactions in a block, we should fill the space
/// with a transaction with fee rate 1f. This means that, for a totally empty
/// block, the fee rate should be 1f.
#[test]
fn test_empty_block_returns_minimum() {
    let metric = ProportionalDotProduct::new(10_000);
    let mut estimator = instantiate_test_db(metric);

    let empty_block_receipt = make_block_receipt(vec![]);
    estimator
        .notify_block(&empty_block_receipt, &block_limit)
        .expect("Should be able to process an empty block");

    assert!(is_close(
        estimator
            .get_rate_estimates()
            .expect("Should be able to create estimate now"),
        FeeRateEstimate {
            high: 1f64,
            middle: 1f64,
            low: 1f64
        }
    ));
}

/// A block that is only a very small minority filled should reflect the paid value,
/// but be dominated by the padded fee rate.
#[test]
fn test_one_block_partially_filled() {
    let metric = ProportionalDotProduct::new(10_000);
    let mut estimator = instantiate_test_db(metric);

    let single_tx_receipt = make_block_receipt(vec![
        StacksTransactionReceipt::from_coinbase(make_dummy_coinbase_tx()),
        make_dummy_cc_tx(10 * tenth_operation_cost_basis, &tenth_operation_cost),
    ]);

    estimator
        .notify_block(&single_tx_receipt, &block_limit)
        .expect("Should be able to process block receipt");

    // The higher fee is 10, because of the operation paying 10f per cost.
    // The middle fee should be near 1, because the block is mostly empty, and dominated by the
    // minimum fee rate padding.
    // The lower fee is 1 because of the minimum fee rate padding.
    assert!(is_close(
        estimator
            .get_rate_estimates()
            .expect("Should be able to create estimate now"),
        FeeRateEstimate {
            high: 10.0f64,
            middle: 2.0475999999999996f64,
            low: 1f64
        }
    ));
}

/// A block that is mostly filled should create an estimate dominated by the transactions paid, and
/// the padding should only affect `low`.
#[test]
fn test_one_block_mostly_filled() {
    let metric = ProportionalDotProduct::new(10_000);
    let mut estimator = instantiate_test_db(metric);

    let single_tx_receipt = make_block_receipt(vec![
        StacksTransactionReceipt::from_coinbase(make_dummy_coinbase_tx()),
        make_dummy_cc_tx(10 * half_operation_cost_basis, &half_operation_cost),
        make_dummy_cc_tx(10 * tenth_operation_cost_basis, &tenth_operation_cost),
        make_dummy_cc_tx(10 * tenth_operation_cost_basis, &tenth_operation_cost),
        make_dummy_cc_tx(10 * tenth_operation_cost_basis, &tenth_operation_cost),
    ]);

    estimator
        .notify_block(&single_tx_receipt, &block_limit)
        .expect("Should be able to process block receipt");

    // The higher fee is 10, because that's what we paid.
    // The middle fee should be 10, because the block is mostly filled.
    // The lower fee is 1 because of the minimum fee rate padding.
    assert!(is_close(
        estimator
            .get_rate_estimates()
            .expect("Should be able to create estimate now"),
        FeeRateEstimate {
            high: 10.0f64,
            middle: 10.0f64,
            low: 1f64
        }
    ));
}

/// Tests the effect of adding blocks over time. We add five blocks with an easy to calculate
/// median.
///
/// We add 5 blocks with window size 5 so none should be forgotten.
#[test]
fn test_window_size_forget_nothing() {
    let metric = ProportionalDotProduct::new(10_000);
    let mut estimator = instantiate_test_db(metric);

    for i in 1..6 {
        let single_tx_receipt = make_block_receipt(vec![
            StacksTransactionReceipt::from_coinbase(make_dummy_coinbase_tx()),
            make_dummy_cc_tx(i * 10 * half_operation_cost_basis, &half_operation_cost),
            make_dummy_cc_tx(i * 10 * half_operation_cost_basis, &half_operation_cost),
        ]);

        estimator
            .notify_block(&single_tx_receipt, &block_limit)
            .expect("Should be able to process block receipt");
    }

    // The fee should be 30, because it's the median of [10, 20, .., 50].
    assert!(is_close(
        estimator
            .get_rate_estimates()
            .expect("Should be able to create estimate now"),
        FeeRateEstimate {
            high: 30f64,
            middle: 30f64,
            low: 30f64
        }
    ));
}

/// Tests the effect of adding blocks over time. We add five blocks with an easy to calculate
/// median.
///
/// We add 10 blocks with window size 5 so the first 5 should be forgotten.
#[test]
fn test_window_size_forget_something() {
    let metric = ProportionalDotProduct::new(10_000);
    let mut estimator = instantiate_test_db(metric);

    for i in 1..11 {
        let single_tx_receipt = make_block_receipt(vec![
            StacksTransactionReceipt::from_coinbase(make_dummy_coinbase_tx()),
            make_dummy_cc_tx(i * 10 * half_operation_cost_basis, &half_operation_cost),
            make_dummy_cc_tx(i * 10 * half_operation_cost_basis, &half_operation_cost),
        ]);

        estimator
            .notify_block(&single_tx_receipt, &block_limit)
            .expect("Should be able to process block receipt");
    }

    // The fee should be 80, because we forgot the first five estimates.
    assert!(is_close(
        estimator
            .get_rate_estimates()
            .expect("Should be able to create estimate now"),
        FeeRateEstimate {
            high: 80f64,
            middle: 80f64,
            low: 80f64
        }
    ));
}

#[test]
fn test_fee_rate_estimate_5_vs_95() {
    assert_eq!(
        fee_rate_estimate_from_sorted_weighted_fees(&[
            FeeRateAndWeight {
                fee_rate: 1f64,
                weight: 5u64,
            },
            FeeRateAndWeight {
                fee_rate: 10f64,
                weight: 95u64,
            },
        ]),
        FeeRateEstimate {
            high: 10.0f64,
            middle: 9.549999999999999f64,
            low: 1.45f64
        }
    );
}

#[test]
fn test_fee_rate_estimate_50_vs_50() {
    assert_eq!(
        fee_rate_estimate_from_sorted_weighted_fees(&[
            FeeRateAndWeight {
                fee_rate: 1f64,
                weight: 50u64,
            },
            FeeRateAndWeight {
                fee_rate: 10f64,
                weight: 50u64,
            },
        ]),
        FeeRateEstimate {
            high: 10.0f64,
            middle: 5.5f64,
            low: 1.0f64
        }
    );
}

#[test]
fn test_fee_rate_estimate_95_vs_5() {
    assert_eq!(
        fee_rate_estimate_from_sorted_weighted_fees(&[
            FeeRateAndWeight {
                fee_rate: 1f64,
                weight: 95u64,
            },
            FeeRateAndWeight {
                fee_rate: 10f64,
                weight: 5u64,
            },
        ]),
        FeeRateEstimate {
            high: 9.549999999999999f64,
            middle: 1.4500000000000004f64,
            low: 1.0f64
        }
    );
}

#[test]
fn test_fee_rate_estimate_20() {
    let mut pairs = vec![];
    for i in 1..21 {
        pairs.push(FeeRateAndWeight {
            fee_rate: 1f64 * i as f64,
            weight: 1u64,
        })
    }

    assert_eq!(
        fee_rate_estimate_from_sorted_weighted_fees(&pairs),
        FeeRateEstimate {
            high: 19.5f64,
            middle: 10.5f64,
            low: 1.5f64
        }
    );
}

#[test]
fn test_fee_rate_estimate_100() {
    let mut pairs = vec![];
    for i in 1..101 {
        pairs.push(FeeRateAndWeight {
            fee_rate: 1f64 * i as f64,
            weight: 1u64,
        })
    }

    assert_eq!(
        fee_rate_estimate_from_sorted_weighted_fees(&pairs),
        FeeRateEstimate {
            high: 95.5f64,
            middle: 50.5f64,
            low: 5.5f64
        }
    );
}
