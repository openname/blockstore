# Stacks Mining

Stacks tokens (STX) are mined by transferring BTC via PoX. To run as a miner,
you should make sure to add the following config fields to your [config file](../sample/conf/mainnet-miner-conf.toml):

```toml
[node]
# Run as a miner
miner = True
# Bitcoin private key to spend
seed = "YOUR PRIVATE KEY"
# Run as a mock-miner, to test mining without spending BTC. Needs miner=True.
#mock_mining = True

[miner]
# Time to spend mining a Nakamoto block, in milliseconds.
nakamoto_attempt_time_ms = 20000

[burnchain]
# Maximum amount (in sats) of "burn commitment" to broadcast for the next block's leader election
burn_fee_cap = 20000
# Amount in sats per byte used to calculate the Bitcoin transaction fee (default: 50)
satoshis_per_byte = 50
# Amount of sats per byte to add when RBF'ing a Bitcoin tx  (default: 5)
rbf_fee_increment = 5
# Maximum percentage of satoshis_per_byte to allow in RBF fee (default: 150)
max_rbf = 150
```

NOTE: Ensuring that your miner can successfully use RBF (Replace-by-Fee) is
critical for reliable block production. If a miner fails to replace an outdated
block commit with a higher-fee transaction, it risks committing to an incorrect
tenure. This would prevent the miner from producing valid blocks during its
tenure, as it would be building on an invalid chain tip, causing the signers to
reject its blocks.

To avoid this, configure satoshis_per_byte, rbf_fee_increment, and max_rbf to
allow for at least three fee increments within the max_rbf limit. This helps
ensure that your miner can adjust its fees sufficiently to stay on the canonical
chain.

You can verify that your node is operating as a miner by checking its log output
to verify that it was able to find its Bitcoin UTXOs:

```bash
$ head -n 100 /path/to/your/node/logs | grep -i utxo
INFO [1630127492.031042] [testnet/stacks-node/src/run_loop/neon.rs:146] [main] Miner node: checking UTXOs at address: <redacted>
INFO [1630127492.062652] [testnet/stacks-node/src/run_loop/neon.rs:164] [main] UTXOs found - will run as a Miner node
```

## Configuring Cost and Fee Estimation

Fee and cost estimators can be configured via the config section `[fee_estimation]`:

```toml
[fee_estimation]
cost_estimator = naive_pessimistic
fee_estimator = fuzzed_weighted_median_fee_rate
fee_rate_fuzzer_fraction = 0.1
fee_rate_window_size = 5
cost_metric = proportion_dot_product
log_error = true
enabled = true
```

Fee and cost estimators observe transactions on the network and use the
observed costs of those transactions to build estimates for viable fee rates
and expected execution costs for transactions. Estimators and metrics can be
selected using the configuration fields above, though the default values are
the only options currently. `log_error` controls whether or not the INFO logger
will display information about the cost estimator accuracy as new costs are
observed. Setting `enabled = false` turns off the cost estimators. Cost estimators
are **not** consensus-critical components, but rather can be used by miners to
rank transactions in the mempool or client to determine appropriate fee rates
for transactions before broadcasting them.

The `fuzzed_weighted_median_fee_rate` uses a
median estimate from a window of the fees paid in the last `fee_rate_window_size` blocks.
Estimates are then randomly "fuzzed" using uniform random fuzz of size up to
`fee_rate_fuzzer_fraction` of the base estimate.

## Further Reading

- [stacksfoundation/miner-docs](https://github.com/stacksfoundation/miner-docs)
- [Mining Documentation](https://docs.stacks.co/stacks-in-depth/nodes-and-miners/mine-mainnet-stacks-tokens)
