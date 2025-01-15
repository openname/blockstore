use std::cmp;
use std::path::Path;

use clarity::types::sqlite::NO_PARAMS;
use clarity::vm::costs::ExecutionCost;
use clarity::vm::database::{ClaritySerializable, STXBalance};
use rusqlite::types::{FromSql, FromSqlError, ToSql};
use rusqlite::{
    params, Connection, Error as SqliteError, OpenFlags, OptionalExtension,
    Transaction as SqlTransaction,
};
use serde_json::Value as JsonValue;

use super::metrics::CostMetric;
use super::{EstimatorError, FeeEstimator, FeeRateEstimate};
use crate::chainstate::stacks::db::StacksEpochReceipt;
use crate::chainstate::stacks::events::TransactionOrigin;
use crate::chainstate::stacks::TransactionPayload;
use crate::util_lib::db::{
    sql_pragma, sqlite_open, table_exists, tx_begin_immediate_sqlite, u64_to_sql,
};

const SINGLETON_ROW_ID: i64 = 1;
const CREATE_TABLE: &str = "
CREATE TABLE scalar_fee_estimator (
    estimate_key NUMBER PRIMARY KEY,
    high NUMBER NOT NULL,
    middle NUMBER NOT NULL,
    low NUMBER NOT NULL
)";

/// This struct estimates fee rates by translating a transaction's `ExecutionCost`
/// into a scalar using `ExecutionCost::proportion_dot_product` and computing
/// the subsequent fee rate using the actual paid fee. The 5th, 50th and 95th
/// percentile fee rates for each block are used as the low, middle, and high
/// estimates. Estimates are updated via exponential decay windowing.
pub struct ScalarFeeRateEstimator<M: CostMetric> {
    db: Connection,
    /// how quickly does the current estimate decay
    /// compared to the newly received block estimate
    ///      new_estimate := (decay_rate) * old_estimate + (1 - decay_rate) * new_measure
    decay_rate: f64,
    metric: M,
}

impl<M: CostMetric> ScalarFeeRateEstimator<M> {
    /// Open a fee rate estimator at the given db path. Creates if not existent.
    pub fn open(p: &Path, metric: M) -> Result<Self, SqliteError> {
        let mut db = sqlite_open(
            p,
            OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_READ_WRITE,
            false,
        )?;

        // check if the db needs to be instantiated regardless of whether or not
        //  it was newly created: the db itself may be shared with other fee estimators,
        //  which would not have created the necessary table for this estimator.
        let tx = tx_begin_immediate_sqlite(&mut db)?;
        Self::instantiate_db(&tx)?;
        tx.commit()?;

        Ok(Self {
            db,
            metric,
            decay_rate: 0.5_f64,
        })
    }

    /// Check if the SQL database was already created. Necessary to avoid races if
    ///  different threads open an estimator at the same time.
    fn db_already_instantiated(tx: &SqlTransaction) -> Result<bool, SqliteError> {
        table_exists(tx, "scalar_fee_estimator")
    }

    fn instantiate_db(tx: &SqlTransaction) -> Result<(), SqliteError> {
        if !Self::db_already_instantiated(tx)? {
            tx.execute(CREATE_TABLE, NO_PARAMS)?;
        }

        Ok(())
    }

    fn update_estimate(&mut self, new_measure: FeeRateEstimate) {
        let next_estimate = match self.get_rate_estimates() {
            Ok(old_estimate) => {
                // compute the exponential windowing:
                // estimate = (a/b * old_estimate) + ((1 - a/b) * new_estimate)
                let prior_component = old_estimate.clone() * self.decay_rate;
                let next_component = new_measure.clone() * (1_f64 - self.decay_rate);
                let mut next_computed = prior_component + next_component;

                // because of integer math, we can end up with some edge effects
                // when the estimate is < decay_rate_fraction.1, so just saturate
                // on the low end at a rate of "1"
                next_computed.high = if next_computed.high >= 1f64 {
                    next_computed.high
                } else {
                    1f64
                };
                next_computed.middle = if next_computed.middle >= 1f64 {
                    next_computed.middle
                } else {
                    1f64
                };
                next_computed.low = if next_computed.low >= 1f64 {
                    next_computed.low
                } else {
                    1f64
                };

                next_computed
            }
            Err(EstimatorError::NoEstimateAvailable) => new_measure.clone(),
            Err(e) => {
                warn!("Error in fee estimator fetching current estimates"; "err" => ?e);
                return;
            }
        };

        debug!("Updating fee rate estimate for new block";
               "new_measure_high" => new_measure.high,
               "new_measure_middle" => new_measure.middle,
               "new_measure_low" => new_measure.low,
               "new_estimate_high" => next_estimate.high,
               "new_estimate_middle" => next_estimate.middle,
               "new_estimate_low" => next_estimate.low);

        let sql = "INSERT OR REPLACE INTO scalar_fee_estimator
                     (estimate_key, high, middle, low) VALUES (?, ?, ?, ?)";

        let tx = tx_begin_immediate_sqlite(&mut self.db).expect("SQLite failure");

        tx.execute(
            sql,
            params![
                SINGLETON_ROW_ID,
                next_estimate.high,
                next_estimate.middle,
                next_estimate.low,
            ],
        )
        .expect("SQLite failure");

        tx.commit().expect("SQLite failure");
    }
}

impl<M: CostMetric> FeeEstimator for ScalarFeeRateEstimator<M> {
    fn notify_block(
        &mut self,
        receipt: &StacksEpochReceipt,
        block_limit: &ExecutionCost,
    ) -> Result<(), EstimatorError> {
        let mut all_fee_rates: Vec<_> = receipt
            .tx_receipts
            .iter()
            .filter_map(|tx_receipt| {
                let (payload, fee, tx_size) = match tx_receipt.transaction {
                    TransactionOrigin::Stacks(ref tx) => {
                        Some((&tx.payload, tx.get_tx_fee(), tx.tx_len()))
                    }
                    TransactionOrigin::Burn(_) => None,
                }?;
                let scalar_cost = match payload {
                    TransactionPayload::TokenTransfer(_, _, _) => {
                        // TokenTransfers *only* contribute tx_len, and just have an empty ExecutionCost.
                        let stx_balance_len = STXBalance::LockedPoxThree {
                            amount_unlocked: 1,
                            amount_locked: 1,
                            unlock_height: 1,
                        }
                        .serialize()
                        .as_bytes()
                        .len() as u64;
                        self.metric.from_cost_and_len(
                            &ExecutionCost {
                                write_length: stx_balance_len,
                                write_count: 1,
                                read_length: 2 * stx_balance_len,
                                read_count: 2,
                                runtime: 4640, // taken from .costs-3
                            },
                            block_limit,
                            tx_size,
                        )
                    }
                    TransactionPayload::Coinbase(..) => {
                        // Coinbase txs are "free", so they don't factor into the fee market.
                        return None;
                    }
                    TransactionPayload::PoisonMicroblock(_, _)
                    | TransactionPayload::ContractCall(_)
                    | TransactionPayload::SmartContract(..)
                    | TransactionPayload::TenureChange(..) => {
                        // These transaction payload types all "work" the same: they have associated ExecutionCosts
                        // and contibute to the block length limit with their tx_len
                        self.metric.from_cost_and_len(
                            &tx_receipt.execution_cost,
                            block_limit,
                            tx_size,
                        )
                    }
                };
                let fee_rate = fee as f64
                    / if scalar_cost >= 1 {
                        scalar_cost as f64
                    } else {
                        1f64
                    };
                if fee_rate >= 1f64 && fee_rate.is_finite() {
                    Some(fee_rate)
                } else {
                    Some(1f64)
                }
            })
            .collect();
        all_fee_rates.sort_by(|a, b| {
            a.partial_cmp(b)
                .expect("BUG: Fee rates should be orderable: NaN and infinite values are filtered")
        });

        let measures_len = all_fee_rates.len();
        if measures_len > 0 {
            // use 5th, 50th, and 95th percentiles from block
            let highest_index = measures_len - cmp::max(1, measures_len / 20);
            let median_index = measures_len / 2;
            let lowest_index = measures_len / 20;
            let block_estimate = FeeRateEstimate {
                high: all_fee_rates[highest_index],
                middle: all_fee_rates[median_index],
                low: all_fee_rates[lowest_index],
            };

            self.update_estimate(block_estimate);
        }

        Ok(())
    }

    fn get_rate_estimates(&self) -> Result<FeeRateEstimate, EstimatorError> {
        let sql = "SELECT high, middle, low FROM scalar_fee_estimator WHERE estimate_key = ?";
        self.db
            .query_row(sql, params![SINGLETON_ROW_ID], |row| {
                let high: f64 = row.get(0)?;
                let middle: f64 = row.get(1)?;
                let low: f64 = row.get(2)?;
                Ok((high, middle, low))
            })
            .optional()
            .expect("SQLite failure")
            .map(|(high, middle, low)| FeeRateEstimate { high, middle, low })
            .ok_or_else(|| EstimatorError::NoEstimateAvailable)
    }
}
