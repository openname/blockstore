use std::cmp;
use std::cmp::Ordering;
use std::path::Path;

use clarity::types::sqlite::NO_PARAMS;
use clarity::vm::costs::ExecutionCost;
use rusqlite::types::{FromSql, FromSqlError, ToSql};
use rusqlite::{
    params, AndThenRows, Connection, Error as SqliteError, OpenFlags, OptionalExtension,
    Transaction as SqlTransaction,
};
use serde_json::Value as JsonValue;

use super::metrics::{CostMetric, PROPORTION_RESOLUTION};
use super::{EstimatorError, FeeEstimator, FeeRateEstimate};
use crate::chainstate::stacks::db::StacksEpochReceipt;
use crate::chainstate::stacks::events::TransactionOrigin;
use crate::chainstate::stacks::TransactionPayload;
use crate::cost_estimates::StacksTransactionReceipt;
use crate::util_lib::db::{
    sql_pragma, sqlite_open, table_exists, tx_begin_immediate_sqlite, u64_to_sql,
};

const CREATE_TABLE: &str = "
CREATE TABLE median_fee_estimator (
    measure_key INTEGER PRIMARY KEY AUTOINCREMENT,
    high NUMBER NOT NULL,
    middle NUMBER NOT NULL,
    low NUMBER NOT NULL
)";

const MINIMUM_TX_FEE_RATE: f64 = 1f64;

/// FeeRateEstimator with the following properties:
///
/// 1) We use a "weighted" percentile approach for calculating the percentile values. Described
///    below, larger transactions contribute more to the ranking than small transactions.
/// 2) Use "windowed" decay instead of exponential decay. This allows outliers to be forgotten
///    faster, and so reduces the influence of outliers.
/// 3) "Pad" the block, so that any unused spaces is considered to have an associated fee rate of
///    1f, the minimum. Ignoring the amount of empty space leads to over-estimates because it
///    ignores the fact that there was still space in the block.
pub struct WeightedMedianFeeRateEstimator<M: CostMetric> {
    db: Connection,
    /// We only look back `window_size` fee rates when averaging past estimates.
    window_size: u32,
    /// The weight of a "full block" in abstract scalar cost units. This is the weight of
    /// a block that is filled *one single* dimension.
    full_block_weight: u64,
    /// Use this cost metric in fee rate calculations.
    metric: M,
}

/// Convenience struct for passing around this pair.
#[derive(Debug)]
pub struct FeeRateAndWeight {
    pub fee_rate: f64,
    pub weight: u64,
}

impl<M: CostMetric> WeightedMedianFeeRateEstimator<M> {
    /// Open a fee rate estimator at the given db path. Creates if not existent.
    pub fn open(p: &Path, metric: M, window_size: u32) -> Result<Self, SqliteError> {
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
            window_size,
            full_block_weight: PROPORTION_RESOLUTION,
        })
    }

    /// Check if the SQL database was already created. Necessary to avoid races if
    ///  different threads open an estimator at the same time.
    fn db_already_instantiated(tx: &SqlTransaction) -> Result<bool, SqliteError> {
        table_exists(tx, "median_fee_estimator")
    }

    fn instantiate_db(tx: &SqlTransaction) -> Result<(), SqliteError> {
        if !Self::db_already_instantiated(tx)? {
            tx.execute(CREATE_TABLE, NO_PARAMS)?;
        }

        Ok(())
    }

    fn get_rate_estimates_from_sql(
        conn: &Connection,
        window_size: u32,
    ) -> Result<FeeRateEstimate, EstimatorError> {
        let sql =
            "SELECT high, middle, low FROM median_fee_estimator ORDER BY measure_key DESC LIMIT ?";
        let mut stmt = conn.prepare(sql).expect("SQLite failure");

        // shuttle high, low, middle estimates into these lists, and then sort and find median.
        let mut highs = Vec::with_capacity(window_size as usize);
        let mut mids = Vec::with_capacity(window_size as usize);
        let mut lows = Vec::with_capacity(window_size as usize);
        let results = stmt
            .query_and_then::<_, SqliteError, _, _>(params![window_size], |row| {
                let high: f64 = row.get("high")?;
                let middle: f64 = row.get("middle")?;
                let low: f64 = row.get("low")?;
                Ok((low, middle, high))
            })
            .expect("SQLite failure");

        for result in results {
            let (low, middle, high) = result.expect("SQLite failure");
            highs.push(high);
            mids.push(middle);
            lows.push(low);
        }

        if highs.is_empty() || mids.is_empty() || lows.is_empty() {
            return Err(EstimatorError::NoEstimateAvailable);
        }

        fn median(len: usize, l: Vec<f64>) -> f64 {
            if len % 2 == 1 {
                l[len / 2]
            } else {
                // note, measures_len / 2 - 1 >= 0, because
                //  len % 2 == 0 and emptiness is checked above
                (l[len / 2] + l[len / 2 - 1]) / 2f64
            }
        }

        // Sort our float arrays. For float values that do not compare easily,
        // treat them as equals.
        highs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        mids.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        lows.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));

        Ok(FeeRateEstimate {
            high: median(highs.len(), highs),
            middle: median(mids.len(), mids),
            low: median(lows.len(), lows),
        })
    }

    fn update_estimate(&mut self, new_measure: FeeRateEstimate) {
        let tx = tx_begin_immediate_sqlite(&mut self.db).expect("SQLite failure");
        let insert_sql = "INSERT INTO median_fee_estimator
                          (high, middle, low) VALUES (?, ?, ?)";
        let deletion_sql = "DELETE FROM median_fee_estimator
                            WHERE measure_key <= (
                               SELECT MAX(measure_key) - ?
                               FROM median_fee_estimator )";
        tx.execute(
            insert_sql,
            params![new_measure.high, new_measure.middle, new_measure.low,],
        )
        .expect("SQLite failure");
        tx.execute(deletion_sql, params![self.window_size])
            .expect("SQLite failure");

        let estimate = Self::get_rate_estimates_from_sql(&tx, self.window_size);
        tx.commit().expect("SQLite failure");
        if let Ok(next_estimate) = estimate {
            debug!("Updating fee rate estimate for new block";
                   "new_measure_high" => new_measure.high,
                   "new_measure_middle" => new_measure.middle,
                   "new_measure_low" => new_measure.low,
                   "new_estimate_high" => next_estimate.high,
                   "new_estimate_middle" => next_estimate.middle,
                   "new_estimate_low" => next_estimate.low);
        }
    }
}

impl<M: CostMetric> FeeEstimator for WeightedMedianFeeRateEstimator<M> {
    fn notify_block(
        &mut self,
        receipt: &StacksEpochReceipt,
        block_limit: &ExecutionCost,
    ) -> Result<(), EstimatorError> {
        // Calculate sorted fee rate for each transaction in the block.
        let mut working_fee_rates: Vec<FeeRateAndWeight> = receipt
            .tx_receipts
            .iter()
            .filter_map(|tx_receipt| {
                fee_rate_and_weight_from_receipt(&self.metric, tx_receipt, block_limit)
            })
            .collect();

        // If necessary, add the "minimum" fee rate to fill the block.
        maybe_add_minimum_fee_rate(&mut working_fee_rates, self.full_block_weight);

        // If fee rates non-empty, then compute an update.
        if !working_fee_rates.is_empty() {
            // Values must be sorted.
            working_fee_rates.sort_by(|a, b| {
                a.fee_rate
                    .partial_cmp(&b.fee_rate)
                    .unwrap_or(Ordering::Equal)
            });

            // Compute the estimate and update.
            let block_estimate = fee_rate_estimate_from_sorted_weighted_fees(&working_fee_rates);
            self.update_estimate(block_estimate);
        }

        Ok(())
    }

    fn get_rate_estimates(&self) -> Result<FeeRateEstimate, EstimatorError> {
        Self::get_rate_estimates_from_sql(&self.db, self.window_size)
    }
}

/// Computes a `FeeRateEstimate` based on `sorted_fee_rates` using a "weighted percentile" method
/// described in https://en.wikipedia.org/wiki/Percentile#Weighted_percentile
///
/// The percentiles computed are [0.05, 0.5, 0.95].
///
/// `sorted_fee_rates` must be non-empty.
pub fn fee_rate_estimate_from_sorted_weighted_fees(
    sorted_fee_rates: &[FeeRateAndWeight],
) -> FeeRateEstimate {
    assert!(!sorted_fee_rates.is_empty());

    let mut total_weight = 0f64;
    for rate_and_weight in sorted_fee_rates {
        total_weight += rate_and_weight.weight as f64;
    }

    assert!(total_weight > 0f64);

    let mut cumulative_weight = 0f64;
    let mut percentiles = Vec::new();
    for rate_and_weight in sorted_fee_rates {
        cumulative_weight += rate_and_weight.weight as f64;
        let percentile_n: f64 =
            (cumulative_weight - rate_and_weight.weight as f64 / 2f64) / total_weight;
        percentiles.push(percentile_n);
    }
    assert_eq!(percentiles.len(), sorted_fee_rates.len());

    let target_percentiles = vec![0.05, 0.5, 0.95];
    let mut fees_index = 0; // index into `sorted_fee_rates`
    let mut values_at_target_percentiles = Vec::new();
    for target_percentile in target_percentiles {
        while fees_index < percentiles.len() && percentiles[fees_index] < target_percentile {
            fees_index += 1;
        }
        let v = if fees_index == 0 {
            sorted_fee_rates[0].fee_rate
        } else if fees_index == percentiles.len() {
            sorted_fee_rates.last().unwrap().fee_rate
        } else {
            // Notation mimics https://en.wikipedia.org/wiki/Percentile#Weighted_percentile
            let vk = sorted_fee_rates[fees_index - 1].fee_rate;
            let vk1 = sorted_fee_rates[fees_index].fee_rate;
            let pk = percentiles[fees_index - 1];
            let pk1 = percentiles[fees_index];
            vk + (target_percentile - pk) / (pk1 - pk) * (vk1 - vk)
        };
        values_at_target_percentiles.push(v);
    }

    FeeRateEstimate {
        high: values_at_target_percentiles[2],
        middle: values_at_target_percentiles[1],
        low: values_at_target_percentiles[0],
    }
}

/// If the weights in `working_rates` do not add up to `full_block_weight`, add a new entry **in
/// place** that takes up the remaining space.
fn maybe_add_minimum_fee_rate(working_rates: &mut Vec<FeeRateAndWeight>, full_block_weight: u64) {
    let mut total_weight = 0u64;
    for rate_and_weight in working_rates.into_iter() {
        total_weight = match total_weight.checked_add(rate_and_weight.weight) {
            Some(result) => result,
            None => return,
        };
    }

    if total_weight < full_block_weight {
        let weight_remaining = full_block_weight - total_weight;
        working_rates.push(FeeRateAndWeight {
            fee_rate: MINIMUM_TX_FEE_RATE,
            weight: weight_remaining,
        })
    }
}

/// Depending on the type of the transaction, calculate fee rate and total cost.
///
/// Returns None if:
///   1) There is no fee rate for the tx.
///   2) Cacluated fee rate is infinite.
fn fee_rate_and_weight_from_receipt(
    metric: &dyn CostMetric,
    tx_receipt: &StacksTransactionReceipt,
    block_limit: &ExecutionCost,
) -> Option<FeeRateAndWeight> {
    let (payload, fee, tx_size) = match tx_receipt.transaction {
        TransactionOrigin::Stacks(ref tx) => Some((&tx.payload, tx.get_tx_fee(), tx.tx_len())),
        TransactionOrigin::Burn(_) => None,
    }?;
    let scalar_cost = match payload {
        TransactionPayload::TokenTransfer(..) => {
            // TokenTransfers *only* contribute tx_len, and just have an empty ExecutionCost.
            metric.from_len(tx_size)
        }
        TransactionPayload::Coinbase(..) => {
            // Coinbase txs are "free", so they don't factor into the fee market.
            return None;
        }
        TransactionPayload::PoisonMicroblock(..)
        | TransactionPayload::ContractCall(..)
        | TransactionPayload::SmartContract(..)
        | TransactionPayload::TenureChange(..) => {
            // These transaction payload types all "work" the same: they have associated ExecutionCosts
            // and contibute to the block length limit with their tx_len
            metric.from_cost_and_len(&tx_receipt.execution_cost, block_limit, tx_size)
        }
    };
    let denominator = cmp::max(scalar_cost, 1) as f64;
    let fee_rate = fee as f64 / denominator;

    if fee_rate.is_infinite() {
        warn!("fee_rate is infinite for {tx_receipt:?}");
        None
    } else {
        let effective_fee_rate = if fee_rate < MINIMUM_TX_FEE_RATE {
            MINIMUM_TX_FEE_RATE
        } else {
            fee_rate
        };
        Some(FeeRateAndWeight {
            fee_rate: effective_fee_rate,
            weight: scalar_cost,
        })
    }
}
