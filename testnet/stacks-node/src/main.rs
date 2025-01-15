#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate stacks_common;

extern crate clarity;
extern crate stacks;

#[allow(unused_imports)]
#[macro_use(o, slog_log, slog_trace, slog_debug, slog_info, slog_warn, slog_error)]
extern crate slog;

pub use stacks_common::util;
use stacks_common::util::hash::hex_bytes;

pub mod monitoring;

pub mod burnchains;
pub mod event_dispatcher;
pub mod genesis_data;
pub mod globals;
pub mod keychain;
pub mod nakamoto_node;
pub mod neon_node;
pub mod node;
pub mod operations;
pub mod run_loop;
pub mod syncctl;
pub mod tenure;

use std::collections::HashMap;
use std::{env, panic, process};

use backtrace::Backtrace;
use pico_args::Arguments;
use stacks::chainstate::burn::db::sortdb::SortitionDB;
use stacks::chainstate::burn::operations::leader_block_commit::RewardSetInfo;
use stacks::chainstate::coordinator::{get_next_recipients, OnChainRewardSetProvider};
use stacks::chainstate::stacks::address::PoxAddress;
use stacks::chainstate::stacks::db::blocks::DummyEventDispatcher;
use stacks::chainstate::stacks::db::StacksChainState;
use stacks::config::chain_data::MinerStats;
pub use stacks::config::{Config, ConfigFile};
#[cfg(not(any(target_os = "macos", target_os = "windows", target_arch = "arm")))]
use tikv_jemallocator::Jemalloc;

pub use self::burnchains::{
    BitcoinRegtestController, BurnchainController, BurnchainTip, MocknetController,
};
pub use self::event_dispatcher::EventDispatcher;
pub use self::keychain::Keychain;
pub use self::node::{ChainTip, Node};
pub use self::run_loop::{helium, neon};
pub use self::tenure::Tenure;
use crate::neon_node::{BlockMinerThread, TipCandidate};
use crate::run_loop::boot_nakamoto;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_arch = "arm")))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

/// Implmentation of `pick_best_tip` CLI option
fn cli_pick_best_tip(config_path: &str, at_stacks_height: Option<u64>) -> TipCandidate {
    info!("Loading config at path {config_path}");
    let config = match ConfigFile::from_path(config_path) {
        Ok(config_file) => Config::from_config_file(config_file, true).unwrap(),
        Err(e) => {
            warn!("Invalid config file: {e}");
            process::exit(1);
        }
    };
    let burn_db_path = config.get_burn_db_file_path();
    let stacks_chainstate_path = config.get_chainstate_path_str();
    let burnchain = config.get_burnchain();
    let (mut chainstate, _) = StacksChainState::open(
        config.is_mainnet(),
        config.burnchain.chain_id,
        &stacks_chainstate_path,
        Some(config.node.get_marf_opts()),
    )
    .unwrap();
    let mut sortdb = SortitionDB::open(&burn_db_path, false, burnchain.pox_constants).unwrap();

    let max_depth = config.miner.max_reorg_depth;

    // There could be more than one possible chain tip. Go find them.
    let stacks_tips = BlockMinerThread::load_candidate_tips(
        &mut sortdb,
        &mut chainstate,
        max_depth,
        at_stacks_height,
    );

    BlockMinerThread::inner_pick_best_tip(stacks_tips, HashMap::new()).unwrap()
}

/// Implementation of `get_miner_spend` CLI option
#[allow(clippy::incompatible_msrv)]
fn cli_get_miner_spend(
    config_path: &str,
    mine_start: Option<u64>,
    at_burnchain_height: Option<u64>,
) -> u64 {
    info!("Loading config at path {config_path}");
    let config = match ConfigFile::from_path(config_path) {
        Ok(config_file) => Config::from_config_file(config_file, true).unwrap(),
        Err(e) => {
            warn!("Invalid config file: {e}");
            process::exit(1);
        }
    };
    let keychain = Keychain::default(config.node.seed.clone());
    let burn_db_path = config.get_burn_db_file_path();
    let stacks_chainstate_path = config.get_chainstate_path_str();
    let burnchain = config.get_burnchain();
    let (mut chainstate, _) = StacksChainState::open(
        config.is_mainnet(),
        config.burnchain.chain_id,
        &stacks_chainstate_path,
        Some(config.node.get_marf_opts()),
    )
    .unwrap();
    let mut sortdb =
        SortitionDB::open(&burn_db_path, true, burnchain.pox_constants.clone()).unwrap();
    let tip = if let Some(at_burnchain_height) = at_burnchain_height {
        let tip = SortitionDB::get_canonical_burn_chain_tip(sortdb.conn()).unwrap();
        let ih = sortdb.index_handle(&tip.sortition_id);
        ih.get_block_snapshot_by_height(at_burnchain_height)
            .unwrap()
            .unwrap()
    } else {
        SortitionDB::get_canonical_burn_chain_tip(sortdb.conn()).unwrap()
    };

    let no_dispatcher: Option<&DummyEventDispatcher> = None;
    let recipients = get_next_recipients(
        &tip,
        &mut chainstate,
        &mut sortdb,
        &burnchain,
        &OnChainRewardSetProvider(no_dispatcher),
        config.node.always_use_affirmation_maps,
    )
    .unwrap();

    let commit_outs = if !burnchain.is_in_prepare_phase(tip.block_height + 1) {
        RewardSetInfo::into_commit_outs(recipients, config.is_mainnet())
    } else {
        vec![PoxAddress::standard_burn_address(config.is_mainnet())]
    };

    let spend_amount = BlockMinerThread::get_mining_spend_amount(
        &config,
        &keychain,
        &burnchain,
        &sortdb,
        &commit_outs,
        mine_start.unwrap_or(tip.block_height),
        at_burnchain_height,
        |burn_block_height| {
            let sortdb =
                SortitionDB::open(&burn_db_path, true, burnchain.pox_constants.clone()).unwrap();
            let Some(miner_stats) = config.get_miner_stats() else {
                return 0.0;
            };
            let Ok(active_miners_and_commits) =
                MinerStats::get_active_miners(&sortdb, Some(burn_block_height))
                    .inspect_err(|e| warn!("Failed to get active miners: {e:?}"))
            else {
                return 0.0;
            };
            if active_miners_and_commits.is_empty() {
                warn!("No active miners detected; using config file burn_fee_cap");
                return 0.0;
            }

            let active_miners: Vec<_> = active_miners_and_commits
                .iter()
                .map(|(miner, _cmt)| miner.as_str())
                .collect();

            info!("Active miners: {active_miners:?}");

            let Ok(unconfirmed_block_commits) = miner_stats
                .get_unconfirmed_commits(burn_block_height + 1, &active_miners)
                .inspect_err(|e| warn!("Failed to find unconfirmed block-commits: {e}"))
            else {
                return 0.0;
            };

            let unconfirmed_miners_and_amounts: Vec<(String, u64)> = unconfirmed_block_commits
                .iter()
                .map(|cmt| (format!("{}", &cmt.apparent_sender), cmt.burn_fee))
                .collect();

            info!("Found unconfirmed block-commits: {unconfirmed_miners_and_amounts:?}");

            let (spend_dist, _total_spend) = MinerStats::get_spend_distribution(
                &active_miners_and_commits,
                &unconfirmed_block_commits,
                &commit_outs,
            );
            let win_probs = if config.miner.fast_rampup {
                // look at spends 6+ blocks in the future
                MinerStats::get_future_win_distribution(
                    &active_miners_and_commits,
                    &unconfirmed_block_commits,
                    &commit_outs,
                )
            } else {
                // look at the current spends
                let Ok(unconfirmed_burn_dist) = miner_stats
                    .get_unconfirmed_burn_distribution(
                        &burnchain,
                        &sortdb,
                        &active_miners_and_commits,
                        unconfirmed_block_commits,
                        &commit_outs,
                        at_burnchain_height,
                    )
                    .inspect_err(|e| warn!("Failed to get unconfirmed burn distribution: {e:?}"))
                else {
                    return 0.0;
                };

                MinerStats::burn_dist_to_prob_dist(&unconfirmed_burn_dist)
            };

            info!("Unconfirmed spend distribution: {spend_dist:?}");
            info!(
                "Unconfirmed win probabilities (fast_rampup={}): {win_probs:?}",
                config.miner.fast_rampup
            );

            let miner_addrs = BlockMinerThread::get_miner_addrs(&config, &keychain);
            let win_prob = miner_addrs
                .iter()
                .find_map(|x| win_probs.get(x))
                .copied()
                .unwrap_or(0.0);

            info!(
                "This miner's win probability at {} is {win_prob}",
                tip.block_height
            );
            win_prob
        },
        |_burn_block_height, _win_prob| {},
    );
    spend_amount
}

fn main() {
    panic::set_hook(Box::new(|panic_info| {
        error!("Process abort due to thread panic: {panic_info}");
        let bt = Backtrace::new();
        error!("Panic backtrace: {bt:?}");

        // force a core dump
        #[cfg(unix)]
        {
            let pid = process::id();
            eprintln!("Dumping core for pid {}", std::process::id());

            use libc::{kill, SIGQUIT};

            // *should* trigger a core dump, if you run `ulimit -c unlimited` first!
            unsafe { kill(pid.try_into().unwrap(), SIGQUIT) };
        }

        // just in case
        process::exit(1);
    }));

    let mut args = Arguments::from_env();
    let subcommand = args.subcommand().unwrap().unwrap_or_default();

    info!("{}", version());

    let mine_start: Option<u64> = args
        .opt_value_from_str("--mine-at-height")
        .expect("Failed to parse --mine-at-height argument");

    if let Some(mine_start) = mine_start {
        info!("Will begin mining once Stacks chain has synced to height >= {mine_start}");
    }

    let config_file = match subcommand.as_str() {
        "mocknet" => {
            args.finish();
            ConfigFile::mocknet()
        }
        "helium" => {
            args.finish();
            ConfigFile::helium()
        }
        "testnet" => {
            args.finish();
            ConfigFile::xenon()
        }
        "mainnet" => {
            args.finish();
            ConfigFile::mainnet()
        }
        "check-config" => {
            let config_path: String = args.value_from_str("--config").unwrap();
            args.finish();
            info!("Loading config at path {config_path}");
            let config_file = match ConfigFile::from_path(&config_path) {
                Ok(config_file) => {
                    debug!("Loaded config file: {config_file:?}");
                    config_file
                }
                Err(e) => {
                    warn!("Invalid config file: {e}");
                    process::exit(1);
                }
            };
            match Config::from_config_file(config_file, true) {
                Ok(_) => {
                    info!("Loaded config!");
                    process::exit(0);
                }
                Err(e) => {
                    warn!("Invalid config: {e}");
                    process::exit(1);
                }
            };
        }
        "start" => {
            let config_path: String = args.value_from_str("--config").unwrap();
            args.finish();
            info!("Loading config at path {config_path}");
            match ConfigFile::from_path(&config_path) {
                Ok(config_file) => config_file,
                Err(e) => {
                    warn!("Invalid config file: {e}");
                    process::exit(1);
                }
            }
        }
        "version" => {
            println!("{}", &version());
            return;
        }
        "key-for-seed" => {
            let seed = {
                let config_path: Option<String> = args.opt_value_from_str("--config").unwrap();
                if let Some(config_path) = config_path {
                    let conf = Config::from_config_file(
                        ConfigFile::from_path(&config_path).unwrap(),
                        true,
                    )
                    .unwrap();
                    args.finish();
                    conf.node.seed
                } else {
                    let free_args = args.finish();
                    let seed_hex = free_args
                        .first()
                        .expect("`wif-for-seed` must be passed either a config file via the `--config` flag or a hex seed string");
                    hex_bytes(seed_hex.to_str().unwrap())
                        .expect("Seed should be a hex encoded string")
                }
            };
            let keychain = Keychain::default(seed);
            println!(
                "Hex formatted secret key: {}",
                keychain.generate_op_signer().get_sk_as_hex()
            );
            println!(
                "WIF formatted secret key: {}",
                keychain.generate_op_signer().get_sk_as_wif()
            );
            return;
        }
        "pick-best-tip" => {
            let config_path: String = args.value_from_str("--config").unwrap();
            let at_stacks_height: Option<u64> =
                args.opt_value_from_str("--at-stacks-height").unwrap();
            args.finish();

            let best_tip = cli_pick_best_tip(&config_path, at_stacks_height);
            println!("Best tip is {best_tip:?}");
            process::exit(0);
        }
        "get-spend-amount" => {
            let config_path: String = args.value_from_str("--config").unwrap();
            let at_burnchain_height: Option<u64> =
                args.opt_value_from_str("--at-bitcoin-height").unwrap();
            args.finish();

            let spend_amount = cli_get_miner_spend(&config_path, mine_start, at_burnchain_height);
            println!("Will spend {spend_amount}");
            process::exit(0);
        }
        _ => {
            print_help();
            return;
        }
    };

    let conf = match Config::from_config_file(config_file, true) {
        Ok(conf) => conf,
        Err(e) => {
            warn!("Invalid config: {e}");
            process::exit(1);
        }
    };

    debug!("node configuration {:?}", &conf.node);
    debug!("burnchain configuration {:?}", &conf.burnchain);
    debug!("connection configuration {:?}", &conf.connection_options);

    let num_round: u64 = 0; // Infinite number of rounds

    if conf.burnchain.mode == "helium" || conf.burnchain.mode == "mocknet" {
        let mut run_loop = helium::RunLoop::new(conf);
        if let Err(e) = run_loop.start(num_round) {
            warn!("Helium runloop exited: {e}");
        }
    } else if conf.burnchain.mode == "neon"
        || conf.burnchain.mode == "nakamoto-neon"
        || conf.burnchain.mode == "xenon"
        || conf.burnchain.mode == "krypton"
        || conf.burnchain.mode == "mainnet"
    {
        let mut run_loop = boot_nakamoto::BootRunLoop::new(conf).unwrap();
        run_loop.start(None, 0);
    } else {
        println!("Burnchain mode '{}' not supported", conf.burnchain.mode);
    }
}

fn version() -> String {
    stacks::version_string("stacks-node", option_env!("STACKS_NODE_VERSION"))
}

fn print_help() {
    let argv: Vec<_> = env::args().collect();

    eprintln!(
        "\
{} <SUBCOMMAND>
Run a stacks-node.

USAGE:
stacks-node <SUBCOMMAND>

SUBCOMMANDS:

mainnet\t\tStart a node that will join and stream blocks from the public mainnet.

mocknet\t\tStart a node based on a fast local setup emulating a burnchain. Ideal for smart contract development.

helium\t\tStart a node based on a local setup relying on a local instance of bitcoind.
\t\tThe following bitcoin.conf is expected:
\t\t  chain=regtest
\t\t  disablewallet=0
\t\t  txindex=1
\t\t  server=1
\t\t  rpcuser=helium
\t\t  rpcpassword=helium

testnet\t\tStart a node that will join and stream blocks from the public testnet, relying on Bitcoin Testnet.

start\t\tStart a node with a config of your own. Can be used for joining a network, starting new chain, etc.
\t\tArguments:
\t\t  --config: path of the config (such as https://github.com/blockstack/stacks-blockchain/blob/master/sample/conf/testnet-follower-conf.toml).
\t\tExample:
\t\t  stacks-node start --config /path/to/config.toml

check-config\t\tValidates the config file without starting up the node. Uses same arguments as start subcommand.

version\t\tDisplay information about the current version and our release cycle.

key-for-seed\tOutput the associated secret key for a burnchain signer created with a given seed.
\t\tCan be passed a config file for the seed via the `--config <file>` option *or* by supplying the hex seed on
\t\tthe command line directly.

replay-mock-mining\tReplay mock mined blocks from <dir>
\t\tArguments:
\t\t  --path: path to directory of mock mined blocks
\t\t  --config: path to the config file

help\t\tDisplay this help.

OPTIONAL ARGUMENTS:

\t\t--mine-at-height=<height>: optional argument for a miner to not attempt mining until Stacks block has sync'ed to <height>

", argv[0]);
}

#[cfg(test)]
pub mod tests;
