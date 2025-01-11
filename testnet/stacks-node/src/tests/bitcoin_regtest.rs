use std::env;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use clarity::vm::costs::ExecutionCost;
use stacks::chainstate::burn::operations::BlockstackOperationType::{
    LeaderBlockCommit, LeaderKeyRegister,
};
use stacks::chainstate::stacks::StacksPrivateKey;
use stacks::config::InitialBalance;
use stacks::core::StacksEpochId;
use stacks_common::util::hash::hex_bytes;

use super::PUBLISH_CONTRACT;
use crate::burnchains::bitcoin_regtest_controller::BitcoinRPCRequest;
use crate::helium::RunLoop;
use crate::tests::to_addr;
use crate::Config;

#[derive(Debug, thiserror::Error)]
pub enum BitcoinCoreError {
    #[error("bitcoind spawn failed: {0}")]
    SpawnFailed(String),
    #[error("bitcoind stop failed: {0}")]
    StopFailed(String),
}

type BitcoinResult<T> = Result<T, BitcoinCoreError>;

pub struct BitcoinCoreController {
    bitcoind_process: Option<Child>,
    config: Config,
}

impl BitcoinCoreController {
    pub fn new(config: Config) -> BitcoinCoreController {
        BitcoinCoreController {
            bitcoind_process: None,
            config,
        }
    }

    fn add_rpc_cli_args(&self, command: &mut Command) {
        command.arg(format!("-rpcport={}", self.config.burnchain.rpc_port));

        if let (Some(username), Some(password)) = (
            &self.config.burnchain.username,
            &self.config.burnchain.password,
        ) {
            command
                .arg(format!("-rpcuser={username}"))
                .arg(format!("-rpcpassword={password}"));
        }
    }

    pub fn start_bitcoind(&mut self) -> BitcoinResult<()> {
        std::fs::create_dir_all(self.config.get_burnchain_path_str()).unwrap();

        let mut command = Command::new("bitcoind");
        command
            .stdout(Stdio::piped())
            .arg("-regtest")
            .arg("-nodebug")
            .arg("-nodebuglogfile")
            .arg("-rest")
            .arg("-txindex=1")
            .arg("-server=1")
            .arg("-listenonion=0")
            .arg("-rpcbind=127.0.0.1")
            .arg(format!("-port={}", self.config.burnchain.peer_port))
            .arg(format!("-datadir={}", self.config.get_burnchain_path_str()));

        self.add_rpc_cli_args(&mut command);

        eprintln!("bitcoind spawn: {command:?}");

        let mut process = match command.spawn() {
            Ok(child) => child,
            Err(e) => return Err(BitcoinCoreError::SpawnFailed(format!("{e:?}"))),
        };

        let mut out_reader = BufReader::new(process.stdout.take().unwrap());

        let mut line = String::new();
        while let Ok(bytes_read) = out_reader.read_line(&mut line) {
            if bytes_read == 0 {
                return Err(BitcoinCoreError::SpawnFailed(
                    "Bitcoind closed before spawning network".into(),
                ));
            }
            if line.contains("Done loading") {
                break;
            }
        }

        eprintln!("bitcoind startup finished");

        self.bitcoind_process = Some(process);

        Ok(())
    }

    pub fn stop_bitcoind(&mut self) -> Result<(), BitcoinCoreError> {
        if self.bitcoind_process.take().is_some() {
            let payload = BitcoinRPCRequest {
                method: "stop".to_string(),
                params: vec![],
                id: "stacks".to_string(),
                jsonrpc: "2.0".to_string(),
            };

            let res = BitcoinRPCRequest::send(&self.config, payload)
                .map_err(|e| BitcoinCoreError::StopFailed(format!("{e:?}")))?;

            if let Some(err) = res.get("error") {
                if !err.is_null() {
                    return Err(BitcoinCoreError::StopFailed(format!("{err}")));
                }
            } else {
                return Err(BitcoinCoreError::StopFailed(format!(
                    "Invalid response: {res:?}"
                )));
            }
        }
        Ok(())
    }

    pub fn kill_bitcoind(&mut self) {
        if let Some(mut bitcoind_process) = self.bitcoind_process.take() {
            bitcoind_process.kill().unwrap();
        }
    }
}

impl Drop for BitcoinCoreController {
    fn drop(&mut self) {
        self.kill_bitcoind();
    }
}

const BITCOIND_INT_TEST_COMMITS: u64 = 11000;

#[test]
#[ignore]
fn bitcoind_integration_test() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }
    bitcoind_integration(false);
}

#[test]
#[ignore]
fn bitcoind_integration_test_segwit() {
    if env::var("BITCOIND_TEST") != Ok("1".into()) {
        return;
    }
    bitcoind_integration(true);
}

fn bitcoind_integration(segwit_flag: bool) {
    let mut conf = super::new_test_conf();
    conf.burnchain.commit_anchor_block_within = 2000;
    conf.burnchain.burn_fee_cap = BITCOIND_INT_TEST_COMMITS;
    conf.burnchain.mode = "helium".to_string();
    conf.burnchain.peer_host = "127.0.0.1".to_string();
    conf.burnchain.rpc_port = 18443;
    conf.burnchain.username = Some("helium-node".to_string());
    conf.burnchain.password = Some("secret".to_string());
    conf.burnchain.local_mining_public_key = Some("04ee0b1602eb18fef7986887a7e8769a30c9df981d33c8380d255edef003abdcd243a0eb74afdf6740e6c423e62aec631519a24cf5b1d62bf8a3e06ddc695dcb77".to_string());

    conf.miner.first_attempt_time_ms = i64::MAX as u64;
    conf.miner.subsequent_attempt_time_ms = i64::MAX as u64;
    conf.miner.segwit = segwit_flag;

    conf.initial_balances.push(InitialBalance {
        address: to_addr(
            &StacksPrivateKey::from_hex(
                "043ff5004e3d695060fa48ac94c96049b8c14ef441c50a184a6a3875d2a000f3",
            )
            .unwrap(),
        )
        .into(),
        amount: 1000,
    });
    conf.initial_balances.push(InitialBalance {
        address: to_addr(
            &StacksPrivateKey::from_hex(
                "b1cf9cee5083f421c84d7cb53be5edf2801c3c78d63d53917aee0bdc8bd160ee01",
            )
            .unwrap(),
        )
        .into(),
        amount: 1000,
    });

    // Setup up a bitcoind controller
    let mut controller = BitcoinCoreController::new(conf.clone());
    // Start bitcoind
    let _res = controller.start_bitcoind();

    let num_rounds = 6;
    let mut run_loop = RunLoop::new(conf.clone());

    run_loop
        .callbacks
        .on_burn_chain_initialized(|burnchain_controller| {
            burnchain_controller.bootstrap_chain(2001);
        });

    // In this serie of tests, the callback is fired post-burnchain-sync, pre-stacks-sync
    run_loop
        .callbacks
        .on_new_burn_chain_state(|round, burnchain_tip, chain_tip| {
            let block = &burnchain_tip.block_snapshot;
            let expected_total_burn = BITCOIND_INT_TEST_COMMITS * (round + 1);
            assert_eq!(block.total_burn, expected_total_burn);
            assert!(block.sortition);
            assert_eq!(block.num_sortitions, round + 1);
            assert_eq!(block.block_height, round + 2003);
            let leader_key = "f888e0cab5c16de8edf72b544a189ece5c0b95cd9178606c970789ac71d17bb4";

            match round {
                0 => {
                    let state_transition = &burnchain_tip.state_transition;
                    assert!(state_transition.accepted_ops.len() == 1);
                    assert!(state_transition.consumed_leader_keys.len() == 1);

                    for op in &state_transition.accepted_ops {
                        match op {
                            LeaderKeyRegister(_op) => {
                                unreachable!();
                            }
                            LeaderBlockCommit(op) => {
                                assert_eq!(
                                    burnchain_tip.state_transition.consumed_leader_keys[0]
                                        .public_key
                                        .to_hex(),
                                    leader_key
                                );
                                assert!(op.parent_block_ptr == 0);
                                assert!(op.parent_vtxindex == 0);
                                assert_eq!(op.burn_fee, BITCOIND_INT_TEST_COMMITS);
                            }
                            _ => panic!("Unexpected operation"),
                        }
                    }
                }
                1 => {
                    let state_transition = &burnchain_tip.state_transition;
                    assert!(state_transition.accepted_ops.len() == 1);
                    assert!(state_transition.consumed_leader_keys.len() == 1);

                    for op in &state_transition.accepted_ops {
                        match op {
                            LeaderKeyRegister(_op) => {
                                unreachable!();
                            }
                            LeaderBlockCommit(op) => {
                                assert_eq!(
                                    burnchain_tip.state_transition.consumed_leader_keys[0]
                                        .public_key
                                        .to_hex(),
                                    leader_key
                                );
                                assert_eq!(op.parent_block_ptr, 2003);
                                assert_eq!(op.burn_fee, BITCOIND_INT_TEST_COMMITS);
                            }
                            _ => panic!("Unexpected operation"),
                        }
                    }

                    assert!(
                        burnchain_tip.block_snapshot.parent_burn_header_hash
                            == chain_tip.metadata.burn_header_hash
                    );
                }
                2 => {
                    let state_transition = &burnchain_tip.state_transition;
                    assert!(state_transition.accepted_ops.len() == 1);
                    assert!(state_transition.consumed_leader_keys.len() == 1);

                    for op in &state_transition.accepted_ops {
                        match op {
                            LeaderKeyRegister(_op) => {
                                unreachable!();
                            }
                            LeaderBlockCommit(op) => {
                                assert_eq!(
                                    burnchain_tip.state_transition.consumed_leader_keys[0]
                                        .public_key
                                        .to_hex(),
                                    leader_key
                                );
                                assert_eq!(op.parent_block_ptr, 2004);
                                assert_eq!(op.burn_fee, BITCOIND_INT_TEST_COMMITS);
                            }
                            _ => panic!("Unexpected operation"),
                        }
                    }

                    assert!(
                        burnchain_tip.block_snapshot.parent_burn_header_hash
                            == chain_tip.metadata.burn_header_hash
                    );
                }
                3 => {
                    let state_transition = &burnchain_tip.state_transition;
                    assert!(state_transition.accepted_ops.len() == 1);
                    assert!(state_transition.consumed_leader_keys.len() == 1);

                    for op in &state_transition.accepted_ops {
                        match op {
                            LeaderKeyRegister(_op) => {
                                unreachable!();
                            }
                            LeaderBlockCommit(op) => {
                                assert_eq!(
                                    burnchain_tip.state_transition.consumed_leader_keys[0]
                                        .public_key
                                        .to_hex(),
                                    leader_key
                                );
                                assert_eq!(op.parent_block_ptr, 2005);
                                assert_eq!(op.burn_fee, BITCOIND_INT_TEST_COMMITS);
                            }
                            _ => panic!("Unexpected operation"),
                        }
                    }

                    assert!(
                        burnchain_tip.block_snapshot.parent_burn_header_hash
                            == chain_tip.metadata.burn_header_hash
                    );
                }
                4 => {
                    let state_transition = &burnchain_tip.state_transition;
                    assert!(state_transition.accepted_ops.len() == 1);
                    assert!(state_transition.consumed_leader_keys.len() == 1);

                    for op in &state_transition.accepted_ops {
                        match op {
                            LeaderKeyRegister(_op) => {
                                unreachable!();
                            }
                            LeaderBlockCommit(op) => {
                                assert_eq!(
                                    burnchain_tip.state_transition.consumed_leader_keys[0]
                                        .public_key
                                        .to_hex(),
                                    leader_key
                                );
                                assert_eq!(op.parent_block_ptr, 2006);
                                assert_eq!(op.burn_fee, BITCOIND_INT_TEST_COMMITS);
                            }
                            _ => panic!("Unexpected operation"),
                        }
                    }

                    assert!(
                        burnchain_tip.block_snapshot.parent_burn_header_hash
                            == chain_tip.metadata.burn_header_hash
                    );
                }
                5 => {
                    let state_transition = &burnchain_tip.state_transition;
                    assert!(state_transition.accepted_ops.len() == 1);
                    assert!(state_transition.consumed_leader_keys.len() == 1);

                    for op in &state_transition.accepted_ops {
                        match op {
                            LeaderKeyRegister(_op) => {
                                unreachable!();
                            }
                            LeaderBlockCommit(op) => {
                                assert_eq!(
                                    burnchain_tip.state_transition.consumed_leader_keys[0]
                                        .public_key
                                        .to_hex(),
                                    leader_key
                                );
                                assert_eq!(op.parent_block_ptr, 2007);
                                assert_eq!(op.burn_fee, BITCOIND_INT_TEST_COMMITS);
                            }
                            _ => panic!("Unexpected operation"),
                        }
                    }

                    assert!(
                        burnchain_tip.block_snapshot.parent_burn_header_hash
                            == chain_tip.metadata.burn_header_hash
                    );
                }
                _ => {}
            }
        });

    // Use tenure's hook for submitting transactions
    run_loop.callbacks.on_new_tenure(|round, _burnchain_tip, chain_tip, tenure| {
        let mut chainstate_copy = tenure.open_chainstate();
            let sortdb = tenure.open_fake_sortdb();

        match round {
            1 => {
                // On round 1, publish the KV contract
                // $ cat /tmp/out.clar
                // (define-map store { key: (string-ascii 32) } { value: (string-ascii 32) })
                // (define-public (get-value (key (string-ascii 32)))
                //     (begin
                //         (print (concat "Getting key " key))
                //         (match (map-get? store { key: key })
                //             entry (ok (get value entry))
                //             (err 0))))
                // (define-public (set-value (key (string-ascii 32)) (value (string-ascii 32)))
                //     (begin
                //         (print (concat "Setting key " key))
                //         (map-set store { key: key } { value: value })
                //         (ok 'true)))
                // ./blockstack-cli --testnet publish 043ff5004e3d695060fa48ac94c96049b8c14ef441c50a184a6a3875d2a000f3 0 0 store /tmp/out.clar
                let header_hash = chain_tip.block.block_hash();
                let consensus_hash = chain_tip.metadata.consensus_hash;
                tenure.mem_pool.submit_raw(&mut chainstate_copy, &sortdb, &consensus_hash, &header_hash, PUBLISH_CONTRACT.to_owned(), &ExecutionCost::max_value(),
                                &StacksEpochId::Epoch20,).unwrap();
            },
            2 => {
                // On round 2, publish a "get:foo" transaction
                // ./blockstack-cli --testnet contract-call 043ff5004e3d695060fa48ac94c96049b8c14ef441c50a184a6a3875d2a000f3 10 1 STGT7GSMZG7EA0TS6MVSKT5JC1DCDFGZWJJZXN8A store get-value -e \"foo\"
                let header_hash = chain_tip.block.block_hash();
                let consensus_hash = chain_tip.metadata.consensus_hash;
                let get_foo = "8080000000040021a3c334fc0ee50359353799e8b2605ac6be1fe40000000000000001000000000000000a0100b7ff8b6c20c427b4f4f09c1ad7e50027e2b076b2ddc0ab55e64ef5ea3771dd4763a79bc5a2b1a79b72ce03dd146ccf24b84942d675a815819a8b85aa8065dfaa030200000000021a21a3c334fc0ee50359353799e8b2605ac6be1fe40573746f7265096765742d76616c7565000000010d00000003666f6f";
                tenure.mem_pool.submit_raw(&mut chainstate_copy, &sortdb, &consensus_hash, &header_hash,hex_bytes(get_foo).unwrap().to_vec(), &ExecutionCost::max_value(),
                                &StacksEpochId::Epoch20,).unwrap();
            },
            3 => {
                // On round 3, publish a "set:foo=bar" transaction
                // ./blockstack-cli --testnet contract-call 043ff5004e3d695060fa48ac94c96049b8c14ef441c50a184a6a3875d2a000f3 10 2 STGT7GSMZG7EA0TS6MVSKT5JC1DCDFGZWJJZXN8A store set-value -e \"foo\" -e \"bar\"
                let header_hash = chain_tip.block.block_hash();
                let consensus_hash = chain_tip.metadata.consensus_hash;
                let set_foo_bar = "8080000000040021a3c334fc0ee50359353799e8b2605ac6be1fe40000000000000002000000000000000a010142a01caf6a32b367664869182f0ebc174122a5a980937ba259d44cc3ebd280e769a53dd3913c8006ead680a6e1c98099fcd509ce94b0a4e90d9f4603b101922d030200000000021a21a3c334fc0ee50359353799e8b2605ac6be1fe40573746f7265097365742d76616c7565000000020d00000003666f6f0d00000003626172";
                tenure.mem_pool.submit_raw(&mut chainstate_copy, &sortdb, &consensus_hash, &header_hash,hex_bytes(set_foo_bar).unwrap().to_vec(), &ExecutionCost::max_value(),
                                &StacksEpochId::Epoch20,).unwrap();
            },
            4 => {
                // On round 4, publish a "get:foo" transaction
                // ./blockstack-cli --testnet contract-call 043ff5004e3d695060fa48ac94c96049b8c14ef441c50a184a6a3875d2a000f3 10 3 STGT7GSMZG7EA0TS6MVSKT5JC1DCDFGZWJJZXN8A store get-value -e \"foo\"
                let header_hash = chain_tip.block.block_hash();
                let consensus_hash = chain_tip.metadata.consensus_hash;
                let get_foo = "8080000000040021a3c334fc0ee50359353799e8b2605ac6be1fe40000000000000003000000000000000a010046c2c1c345231443fef9a1f64fccfef3e1deacc342b2ab5f97612bb3742aa799038b20aea456789aca6b883e52f84a31adfee0bc2079b740464877af8f2f87d2030200000000021a21a3c334fc0ee50359353799e8b2605ac6be1fe40573746f7265096765742d76616c7565000000010d00000003666f6f";
                tenure.mem_pool.submit_raw(&mut chainstate_copy, &sortdb, &consensus_hash, &header_hash,hex_bytes(get_foo).unwrap().to_vec(), &ExecutionCost::max_value(),
                                &StacksEpochId::Epoch20,).unwrap();
            },
            5 => {
                // On round 5, publish a stacks transaction
                // ./blockstack-cli --testnet token-transfer b1cf9cee5083f421c84d7cb53be5edf2801c3c78d63d53917aee0bdc8bd160ee01 10 0 ST195Q2HPXY576N4CT2A0R94D7DRYSX54A5X3YZTH 1000
                let header_hash = chain_tip.block.block_hash();
                let consensus_hash = chain_tip.metadata.consensus_hash;
                let transfer_1000_stx = "80800000000400b71a091b4b8b7661a661c620966ab6573bc2dcd30000000000000000000000000000000a0000393810832bacd44cfc4024980876135de6b95429bdb610d5ce96a92c9ee9bfd81ec77ea0f1748c8515fc9a1589e51d8b92bf028e3e84ade1249682c05271d5b803020000000000051a525b8a36ef8a73548cd0940c248d3b71ecf4a45100000000000003e800000000000000000000000000000000000000000000000000000000000000000000";
                tenure.mem_pool.submit_raw(&mut chainstate_copy, &sortdb, &consensus_hash, &header_hash,hex_bytes(transfer_1000_stx).unwrap().to_vec(), &ExecutionCost::max_value(),
                                &StacksEpochId::Epoch20,).unwrap();
            },
            _ => {}
        };
    });

    // Use block's hook for asserting expectations
    // In this serie of tests, the callback is fired post-burnchain-sync, post-stacks-sync
    run_loop.callbacks.on_new_stacks_chain_state(
        |round, burnchain_tip, chain_tip, _chain_state, _burn_dbconn| {
            match round {
                0 => {
                    // Inspecting the chain at round 0.
                    // - Chain length should be 1.
                    assert!(chain_tip.metadata.stacks_block_height == 1);

                    // Block #1 should only have 0 txs
                    assert!(chain_tip.block.txs.len() == 1);

                    assert!(
                        chain_tip.block.header.block_hash()
                            == burnchain_tip.block_snapshot.winning_stacks_block_hash
                    );
                }
                1 => {
                    // Inspecting the chain at round 1.
                    // - Chain length should be 2.
                    assert!(chain_tip.metadata.stacks_block_height == 2);

                    // Block #2 should only have 2 txs
                    assert!(chain_tip.block.txs.len() == 2);

                    assert!(
                        chain_tip.block.header.block_hash()
                            == burnchain_tip.block_snapshot.winning_stacks_block_hash
                    );
                }
                2 => {
                    // Inspecting the chain at round 2.
                    // - Chain length should be 3.
                    assert!(chain_tip.metadata.stacks_block_height == 3);

                    // Block #3 should only have 2 txs
                    assert!(chain_tip.block.txs.len() == 2);

                    assert!(
                        chain_tip.block.header.block_hash()
                            == burnchain_tip.block_snapshot.winning_stacks_block_hash
                    );
                }
                3 => {
                    // Inspecting the chain at round 3.
                    // - Chain length should be 4.
                    assert!(chain_tip.metadata.stacks_block_height == 4);

                    // Block #4 should only have 2 txs
                    assert!(chain_tip.block.txs.len() == 2);

                    assert!(
                        chain_tip.block.header.block_hash()
                            == burnchain_tip.block_snapshot.winning_stacks_block_hash
                    );
                }
                4 => {
                    // Inspecting the chain at round 4.
                    // - Chain length should be 5.
                    assert!(chain_tip.metadata.stacks_block_height == 5);

                    // Block #5 should only have 2 txs
                    assert!(chain_tip.block.txs.len() == 2);

                    assert!(
                        chain_tip.block.header.block_hash()
                            == burnchain_tip.block_snapshot.winning_stacks_block_hash
                    );
                }
                5 => {
                    // Inspecting the chain at round 5.
                    // - Chain length should be 6.
                    assert!(chain_tip.metadata.stacks_block_height == 6);

                    // Block #6 should only have 2 txs
                    assert!(chain_tip.block.txs.len() == 2);

                    assert!(
                        chain_tip.block.header.block_hash()
                            == burnchain_tip.block_snapshot.winning_stacks_block_hash
                    );
                }
                _ => {}
            }
        },
    );
    run_loop.start(num_rounds).unwrap();

    controller.kill_bitcoind();
}
