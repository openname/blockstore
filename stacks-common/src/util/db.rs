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

use std::backtrace::Backtrace;
use std::sync::{LazyLock, Mutex};
use std::thread;
use std::time::Instant;

use hashbrown::HashMap;
use rand::{thread_rng, Rng};
use rusqlite::Connection;

use crate::util::sleep_ms;

/// Keep track of DB locks, for deadlock debugging
///  - **key:** `rusqlite::Connection` debug print
///  - **value:** Lock holder (thread name + timestamp)
///
/// This uses a `Mutex` inside of `LazyLock` because:
///  - Using `Mutex` alone, it can't be statically initialized because `HashMap::new()` isn't `const`
///  - Using `LazyLock` alone doesn't allow interior mutability
static LOCK_TABLE: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Generate timestanps for use in `LOCK_TABLE`
/// `Instant` is preferable to `SystemTime` because it uses `CLOCK_MONOTONIC` and is not affected by NTP adjustments
static LOCK_TABLE_TIMER: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Call when using an operation which locks a database
/// Updates `LOCK_TABLE`
pub fn update_lock_table(conn: &Connection) {
    let timestamp = LOCK_TABLE_TIMER.elapsed().as_millis();
    // The debug format for `Connection` includes the path
    let k = format!("{conn:?}");
    let v = format!("{:?}@{timestamp}", thread::current().name());
    LOCK_TABLE.lock().unwrap().insert(k, v);
}

/// Called by `rusqlite` if we are waiting too long on a database lock
/// If called too many times, will assume a deadlock and panic
pub fn tx_busy_handler(run_count: i32) -> bool {
    const AVG_SLEEP_TIME_MS: u64 = 100;

    // Every ~5min, report an error with a backtrace
    //   5min * 60s/min * 1_000ms/s / 100ms
    const ERROR_COUNT: u32 = 3_000;

    // First, check if this is taking unreasonably long. If so, it's probably a deadlock
    let run_count = run_count.unsigned_abs();
    if run_count > 0 && run_count % ERROR_COUNT == 0 {
        error!("Deadlock detected. Waited 5 minutes (estimated) for database lock.";
            "run_count" => run_count,
            "backtrace" => ?Backtrace::capture()
        );
        for (k, v) in LOCK_TABLE.lock().unwrap().iter() {
            error!("Database '{k}' last locked by {v}");
        }
    }

    let mut sleep_time_ms = 2u64.saturating_pow(run_count);
    sleep_time_ms = sleep_time_ms.saturating_add(thread_rng().gen_range(0..sleep_time_ms));

    if sleep_time_ms > AVG_SLEEP_TIME_MS {
        let jitter = 10;
        sleep_time_ms =
            thread_rng().gen_range((AVG_SLEEP_TIME_MS - jitter)..(AVG_SLEEP_TIME_MS + jitter));
    }

    let msg = format!("Database is locked; sleeping {sleep_time_ms}ms and trying again");
    if run_count > 10 && run_count % 10 == 0 {
        warn!("{msg}";
            "run_count" => run_count,
            "backtrace" => ?Backtrace::capture()
        );
    } else {
        debug!("{msg}");
    }

    sleep_ms(sleep_time_ms);
    true
}
