# Profiling Tools

This document describes several techniques to profile (i.e. find performance bottlenecks) the stacks-node mining loop, including:

- configuring debug logging,
- setting up a mock mining node,
- recording inbound transactions,
- mining on top of a past block,
- generating flame graphs, and
- profiling sqlite queries.

Note that all bash commands in this document are run from the [stacks-core repository](https://github.com/stacks-network/stacks-core) root directory.

## Logging tips

Validating the config file using `stacks-node check-config`:

```
$ cargo run -r -p stacks-node --bin stacks-node check-config --config ./sample/conf/mainnet-follower-conf.toml
INFO [1661276562.220137] [testnet/stacks-node/src/main.rs:82] [main] stacks-node 0.1.0 (:, release build, linux [x86_64])
INFO [1661276562.220363] [testnet/stacks-node/src/main.rs:115] [main] Loading config at path ./sample/conf/mainnet-follower-conf.toml
INFO [1661276562.233071] [testnet/stacks-node/src/main.rs:128] [main] Valid config!
```

Enabling debug logging using environment variable `STACKS_LOG_DEBUG=1`:

```
$ STACKS_LOG_DEBUG=1 cargo run -r -p stacks-node --bin stacks-node check-config --config ./sample/conf/mainnet-follower-conf.toml
INFO [1661276562.220137] [testnet/stacks-node/src/main.rs:82] [main] stacks-node 0.1.0 (tip-mine:c90476aa8a+, release build, macos [aarch64])
INFO [1661276562.220363] [testnet/stacks-node/src/main.rs:115] [main] Loading config at path ./sample/conf/mainnet-follower-conf.toml
DEBG [1661276562.222450] [testnet/stacks-node/src/main.rs:118] [main] Loaded config file: ConfigFile { burnchain: Some(BurnchainConfigFile { chain: Some("bitcoin"), burn_fee_cap: Some(1), mode: Some("mainnet"), commit_anchor_block_within: None, peer_host: Some("localhost"), peer_port: Some(8333), rpc_port: Some(8332), rpc_ssl: None, username: Some("btcuser"), password: Some("btcpass"), timeout: None, magic_bytes: None, local_mining_public_key: None, process_exit_at_block_height: None, poll_time_secs: None, satoshis_per_byte: None, leader_key_tx_estimated_size: None, block_commit_tx_estimated_size: None, rbf_fee_increment: None, max_rbf: None, epochs: None }), node: Some(NodeConfigFile { name: None, seed: None, deny_nodes: None, working_dir: Some("/Users/igor/w/stacks-work/working_dir"), rpc_bind: Some("0.0.0.0:20443"), p2p_bind: Some("0.0.0.0:20444"), p2p_address: None, data_url: None, bootstrap_node: Some("02196f005965cebe6ddc3901b7b1cc1aa7a88f305bb8c5893456b8f9a605923893@seed.mainnet.hiro.so:20444"), local_peer_seed: None, miner: Some(true), mock_mining: Some(true), mine_microblocks: None, microblock_frequency: None, max_microblocks: None, wait_time_for_microblocks: None, prometheus_bind: None, marf_cache_strategy: None, marf_defer_hashing: None, pox_sync_sample_secs: None, use_test_genesis_chainstate: None }), ustx_balance: None, events_observer: Some([EventObserverConfigFile { endpoint: "localhost:3700", events_keys: ["*"] }]), connection_options: None, fee_estimation: None, miner: None }
INFO [1661276562.233071] [testnet/stacks-node/src/main.rs:128] [main] Valid config!
```

Enabling json logging using environment variable `STACKS_LOG_JSON=1` and feature flag `slog_json`:

```
$ STACKS_LOG_JSON=1 cargo run -F slog_json -r -p stacks-node --bin stacks-node check-config --config ./sample/conf/mainnet-follower-conf.toml
{"msg":"stacks-node 0.1.0 (tip-mine:c90476aa8a+, release build, macos [aarch64])","level":"INFO","ts":"2022-08-23T12:44:28.072462-05:00","thread":"main","line":82,"file":"testnet/stacks-node/src/main.rs"}
{"msg":"Loading config at path ./sample/conf/mainnet-follower-conf.toml","level":"INFO","ts":"2022-08-23T12:44:28.074238-05:00","thread":"main","line":115,"file":"testnet/stacks-node/src/main.rs"}
{"msg":"Valid config!","level":"INFO","ts":"2022-08-23T12:44:28.089960-05:00","thread":"main","line":128,"file":"testnet/stacks-node/src/main.rs"}
```

## Setting up the working directory

First, let's set up the various directory locations:

```
$ export STACKS_DIR=~/stacks
$ export STACKS_WORKING_DIR=$STACKS_DIR/working
$ export STACKS_SNAPSHOT_DIR=$STACKS_DIR/snapshot
```

## Setting up the mock mining node

Download and extract an archived snapshot of mainnet working directory, provided by Hiro.

```
$ wget -P $STACKS_DIR https://archive.hiro.so/mainnet/stacks-blockchain/mainnet-stacks-blockchain-latest.tar.gz
$ tar xzvf $STACKS_DIR/mainnet-stacks-blockchain-latest.tar.gz -C $STACKS_DIR
```

We'll be using the `stacks-node` config file available at:

`sample/conf/mainnet-mockminer-conf.toml`

Note that, for convenience, the `stacks-node` binary uses the environment variable `$STACKS_WORKING_DIR` to override the working directory location in the config file.

```
$ cargo run -r -p stacks-node --bin stacks-node start --config ./sample/conf/mainnet-mockminer-conf.toml
```

The `stacks-node` process will receive blocks starting from the latest block available in the Hiro archive.

Check the latest tip height of our node.

```
$ curl -s 127.0.0.1:20443/v2/info | jq .stacks_tip_height
```

Compare our node's tip height to a public node's tip height to check when our node is fully synchronized.

```
$ curl -s seed-0.mainnet.stacks.co:20443/v2/info | jq .stacks_tip_height
```

Once the node is synchronized, terminate the `stacks-node` process so we can setup event recording.

## Recording blockchain events

Run `stacks-events` to receive and archive events:

```
$ cargo run -r -p stacks-node --bin stacks-events | tee $STACKS_DIR/events.log
```

Run `stacks-node` with an event observer:

```
$ STACKS_EVENT_OBSERVER=localhost:3700 cargo run -r -p stacks-node --bin stacks-node start --config ./sample/conf/mainnet-mockminer-conf.toml
```

You should see output from `stacks-events` in `events.logs` similar to:

```
$ tail -F $STACKS_DIR/events.log
{"path":"drop_mempool_tx","payload":{"dropped_txids":["0x6f78047f15ac3309153fc34be94ed8895111304336aec1ff106b7de051021e17, ..., "ts":"2022-08-12T05:03:08.577Z"}
```

## Historical Mining

Discover the first recorded block height:

```
$ cat $STACKS_DIR/events.log | egrep new_block | head -1 | jq .payload.block_height
```

Discover a lower bound number of recorded transactions. This is a lower bound because each line in the events file is a list of transactions.

```
$ cat $STACKS_DIR/events.log | egrep new_mempool_tx | wc -l
```

Make a snapshot of the working directory:

```
$ cp -r $STACKS_WORKING_DIR $STACKS_SNAPSHOT_DIR
```

Run the `tip-mine` benchmark:

```
$ export STACKS_TIP_MINE_BLOCK_HEIGHT=71294
$ export STACKS_TIP_MINE_NUM_TXS=100
$ cargo run -F disable-costs -r --bin stacks-inspect tip-mine $STACKS_SNAPSHOT_DIR $STACKS_DIR/events.log $STACKS_TIP_MINE_BLOCK_HEIGHT $STACKS_TIP_MINE_NUM_TXS
INFO [1661221745.316390] [src/main.rs:1383] [main] Clearing mempool
INFO [1661221745.316638] [src/main.rs:1405] [main] Found stacks_chain_tip with height 71296
INFO [1661221745.316651] [src/main.rs:1406] [main] Mining off parent block with height 71294
INFO [1661221745.316657] [src/main.rs:1408] [main] Submitting up to 0 transactions to the mempool
INFO [1661221745.393176] [src/main.rs:1424] [main] Found target block height 71294
INFO [1661221745.393213] [src/main.rs:1426] [main] Found new_block height 71294 parsed_tx_count 0 submit_tx_count 0
INFO [1661221745.394649] [src/main.rs:1441] [main] Reached mine_max_txns 0
INFO [1661221745.394665] [src/main.rs:1463] [main] Parsed 0 transactions
INFO [1661221745.394669] [src/main.rs:1464] [main] Submitted 0 transactions into the mempool
INFO [1661221745.394673] [src/main.rs:1466] [main] Mining a block
...
INFO [1661274285.417171] [src/chainstate/stacks/miner.rs:1628] [main] Miner: mined anchored block 4a64e0a4012acb6748a08784876c23f6f61aba08b7c826db5b57832935278f33 height 71295 with 87 txs, parent block f0f0caa2afaae75417f14fe2fad1e3fd52b0169e66cb045b4954b9ab78611f31, parent microblock 48ba93f3cc3cf88e44fe27ba58bd75d33e92d7e99b04a83240cfa90bd0767273 (7), state root = b84dcee8b48a77030682eb95340ffcc20cb76087587048c9c3d6c42be8fd22d4
Successfully mined block @ height = 71295 off of bd4fa09ece02e7fd53493c96bd69b89155058f7b28d4a659d87d89644208f41e (96cc06519e670eefb674aa2e9cfe0cfae103d4da/f0f0caa2afaae75417f14fe2fad1e3fd52b0169e66cb045b4954b9ab78611f31) in 7310ms.
Block 4a64e0a4012acb6748a08784876c23f6f61aba08b7c826db5b57832935278f33: 3227082 uSTX, 31587 bytes, cost ExecutionCost { write_length: 84090, write_count: 1170, read_length: 20381499, read_count: 7529, runtime: 103717315 }
```

In this run, `tip-mine` mined a block with 87 transactions.

Alternatively, you can run `cargo build` separately from the target binary `stacks-inspect` to avoid re-building and speed up profiling:

```
$ cargo build -F disable-costs -r --bin stacks-inspect
$ ./target/release/stacks-inspect tip-mine $STACKS_SNAPSHOT_DIR $STACKS_DIR/events.log $STACKS_TIP_MINE_BLOCK_HEIGHT $STACKS_TIP_MINE_NUM_TXS
```

## Profiling using Perf and Dtrace

Let's use the [flamegraph-rs](https://github.com/flamegraph-rs/flamegraph) package to generate flame graphs.

```
$ cargo install flamegraph
```

### Mac / Dtrace

flamegraph-rs uses [dtrace](https://en.wikipedia.org/wiki/DTrace) for profiling on Mac.

Build `stacks-inspect` using the feature `disable-costs` to disable the block cost limits:

```
$ cargo build -F disable-costs -r --bin stacks-inspect
```

Generate a flame graph:

```
$ flamegraph --root -o perf.svg -e cpu-clock --min-width 1 --deterministic -- ./target/release/stacks-inspect tip-mine $STACKS_SNAPSHOT_DIR $STACKS_DIR/events.log $STACKS_TIP_MINE_BLOCK_HEIGHT $STACKS_TIP_MINE_NUM_TXS
```

You can open the flame graph using a browser:

```
$ open perf.svg
```

### Debian / Perf

flamegraph-rs uses [perf](https://perf.wiki.kernel.org/index.php/Main_Page) for profiling on Linux.

The Linux performance tool `perf` has a performance bug which has been fixed. If you experience slow flame graph generation, try to build perf locally:

#### Build perf locally

Background on the `perf` performance bug: https://eighty-twenty.org/2021/09/09/perf-addr2line-speed-improvement

Find out your kernel version:

```
$ uname -a
Linux localhost 5.15.0-25-generic #26~16.04.1-Ubuntu SMP Tue Oct 1 16:30:39 UTC 2018 x86_64 x86_64 x86_64 GNU/Linux
```

Install dependencies, clone the linux kernel source, checkout the version tag matching your kernel version and build perf:

```
$ sudo apt install -y git libzstd-dev libunwind-dev libcap-dev libdw-dev libdwarf-dev libbfd-dev libelf-dev systemtap-sdt-dev binutils-dev libnuma-dev libiberty-dev bison flex
$ git clone https://github.com/torvalds/linux.git
$ git checkout v5.15
$ cd linux/tools/perf && make
```

#### Running perf

Grant kernel permissions to perf:

```
$ sudo sed -i "$ a kernel.perf_event_paranoid = -1" /etc/sysctl.conf
$ sudo sed -i "$ a kernel.kptr_restrict = 0" /etc/sysctl.conf
$ sysctl --system
```

Note that you need to uncomment the following in `.cargo/config.toml` (see [flamegraph-rs](https://github.com/flamegraph-rs/flamegraph) for details)

```
[target.x86_64-unknown-linux-gnu]
linker = "/usr/bin/clang"
rustflags = ["-Clink-arg=-fuse-ld=lld", "-Clink-arg=-Wl,--no-rosegment"]
```

Build `stacks-inspect` using the feature `disable-costs` to disable the block cost limits:

```
$ cargo build -F disable-costs -r --bin stacks-inspect
```

Generate a flame graph using the locally built `perf` binary:

```
$ PERF=~/linux/tools/perf/perf flamegraph --cmd "record -F 97 --call-graph dwarf,65528 -g -e cpu-clock" -o perf.svg --min-width 0.5 --deterministic -- ./target/release/stacks-inspect tip-mine $STACKS_SNAPSHOT_DIR $STACKS_DIR/events.log $STACKS_TIP_MINE_BLOCK_HEIGHT $STACKS_TIP_MINE_NUM_TXS
```

Output flame graph is in `perf.svg`.

## Profiling SQLite queries

Set the environment variable `STACKS_LOG_DEBUG=1` and use the cargo feature `profile-sqlite`:

```
$ STACKS_LOG_DEBUG=1 cargo run -F profile-sqlite,disable-costs -r --bin stacks-inspect try-mine $STACKS_WORKING_DIR
...
DEBG [1661217664.809057] [src/util_lib/db.rs:666] [main] sqlite trace profile {"millis":1,"query":"SELECT value FROM data_table WHERE key = ?"}
...
```
