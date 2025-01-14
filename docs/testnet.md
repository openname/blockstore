# Stacks testnet

[`testnet-follower-conf.toml`](../sample/conf/testnet-follower-conf.toml) is a configuration file that you can use for setting genesis balances or configuring event observers. You can grant an address an initial account balance by adding the following entries:

```
[[ustx_balance]]
address = "ST2VHM28V9E5QCRD6C73215KAPSBKQGPWTEE5CMQT"
amount = 100000000
```

The `address` field is the Stacks testnet address, and the `amount` field is the
number of microSTX to grant to it in the genesis block. The addresses of the
private keys used in the tutorial below are already added.

## Encode and sign transactions

Here, we have generated a keypair that will be used for signing the upcoming transactions:

```bash
cargo run --bin blockstack-cli generate-sk --testnet

# Sample output
# {
#  secretKey: "b8d99fd45da58038d630d9855d3ca2466e8e0f89d3894c4724f0efc9ff4b51f001",
#  publicKey: "02781d2d3a545afdb7f6013a8241b9e400475397516a0d0f76863c6742210539b5",
#  stacksAddress: "ST2ZRX0K27GW0SP3GJCEMHD95TQGJMKB7G9Y0X1MH"
# }
```

This keypair is already registered in the [`testnet-follower-conf.toml`](../sample/conf/testnet-follower-conf.toml) file, so it can be used as presented here.

We will interact with the following simple contract `kv-store`. In our examples, we will assume this contract is saved locally to `./kv-store.clar`:

```scheme
(define-map store { key: (string-ascii 32) } { value: (string-ascii 32) })

(define-public (get-value (key (string-ascii 32)))
    (match (map-get? store { key: key })
        entry (ok (get value entry))
        (err 0)))

(define-public (set-value (key (string-ascii 32)) (value (string-ascii 32)))
    (begin
        (map-set store { key: key } { value: value })
        (ok true)))
```

We want to publish this contract on chain, then issue some transactions that interact with it by setting some keys and getting some values, so we can observe read and writes.

Our first step is to generate and sign, using your private key, the transaction that will publish the contract `kv-store`.
To do that, we will use the subcommand:

```bash
cargo run --bin blockstack-cli publish --help
```

With the following arguments:

```bash
cargo run --bin blockstack-cli publish b8d99fd45da58038d630d9855d3ca2466e8e0f89d3894c4724f0efc9ff4b51f001 515 0 kv-store ./kv-store.clar --testnet
```

The `515` is the transaction fee, denominated in microSTX. Right now, the
testnet requires one microSTX per byte minimum, and this transaction should be
less than 515 bytes.
The third argument `0` is a nonce, that must be increased monotonically with each new transaction.

This command will output the **binary format** of the transaction. In our case, we want to pipe this output and dump it to a file that will be used later in this tutorial.

```bash
cargo run --bin blockstack-cli publish b8d99fd45da58038d630d9855d3ca2466e8e0f89d3894c4724f0efc9ff4b51f001 515 0 kv-store ./kv-store.clar --testnet | xxd -r -p > tx1.bin
```

## Publish your contract

Assuming that the testnet is running, we can publish our `kv-store` contract.

In another terminal (or file explorer), you can move the `tx1.bin` generated earlier, to the mempool:

```bash
curl -X POST -H "Content-Type: application/octet-stream" --data-binary @./tx1.bin http://localhost:20443/v2/transactions
```

In the terminal window running the testnet, you can observe the state machine's reactions.

## Reading from / Writing to the contract

Now that our contract has been published on chain, let's try to submit some read / write transactions.
We will start by trying to read the value associated with the key `foo`.

To do that, we will use the subcommand:

```bash
cargo run --bin blockstack-cli contract-call --help
```

With the following arguments:

```bash
cargo run --bin blockstack-cli contract-call b8d99fd45da58038d630d9855d3ca2466e8e0f89d3894c4724f0efc9ff4b51f001 500 1 ST2ZRX0K27GW0SP3GJCEMHD95TQGJMKB7G9Y0X1MH kv-store get-value -e \"foo\" --testnet | xxd -r -p > tx2.bin
```

`contract-call` generates and signs a contract-call transaction.

We can submit the transaction by moving it to the mempool path:

```bash
curl -X POST -H "Content-Type: application/octet-stream" --data-binary @./tx2.bin http://localhost:20443/v2/transactions
```

Similarly, we can generate a transaction that would be setting the key `foo` to the value `bar`:

```bash
cargo run --bin blockstack-cli contract-call b8d99fd45da58038d630d9855d3ca2466e8e0f89d3894c4724f0efc9ff4b51f001 500 2 ST2ZRX0K27GW0SP3GJCEMHD95TQGJMKB7G9Y0X1MH kv-store set-value -e \"foo\" -e \"bar\" --testnet | xxd -r -p > tx3.bin
```

And submit it by moving it to the mempool path:

```bash
curl -X POST -H "Content-Type: application/octet-stream" --data-binary @./tx3.bin http://localhost:20443/v2/transactions
```

Finally, we can issue a third transaction, reading the key `foo` again, for ensuring that the previous transaction has successfully updated the state machine:

```bash
cargo run --bin blockstack-cli contract-call b8d99fd45da58038d630d9855d3ca2466e8e0f89d3894c4724f0efc9ff4b51f001 500 3 ST2ZRX0K27GW0SP3GJCEMHD95TQGJMKB7G9Y0X1MH kv-store get-value -e \"foo\" --testnet | xxd -r -p > tx4.bin
```

And submit this last transaction by moving it to the mempool path:

```bash
curl -X POST -H "Content-Type: application/octet-stream" --data-binary @./tx4.bin http://localhost:20443/v2/transactions
```

Congratulations, you can now [write your own smart contracts with Clarity](https://docs.stacks.co/core/smart/overview.html).
