// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2024 Stacks Open Internet Foundation
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

/// You are going to need `ulimit -n` to be 4096 for these tests.
/// In Linux, the default is 1024.
use std::collections::{HashMap, HashSet};

use clarity::vm::types::{QualifiedContractIdentifier, StandardPrincipalData};
use rand::prelude::*;
use rand::thread_rng;
use rlimit;

use crate::core::PEER_VERSION_TESTNET;
use crate::net::db::*;
use crate::net::test::*;
use crate::net::*;
use crate::util_lib::test::*;

fn setup_rlimit_nofiles() {
    info!("Attempt to set nofile rlimit to 4096 (required for these tests to run)");
    assert!(rlimit::Resource::NOFILE.get().is_ok());
    let (slimit, hlimit) = rlimit::getrlimit(rlimit::Resource::NOFILE).unwrap();
    rlimit::setrlimit(rlimit::Resource::NOFILE, 4096.max(slimit), hlimit).unwrap();
    info!("Successfully set nofile rlimit to 4096");
}

fn stacker_db_id(i: usize) -> QualifiedContractIdentifier {
    QualifiedContractIdentifier::new(
        StandardPrincipalData(0x01, [i as u8; 20]),
        format!("db-{}", i).as_str().into(),
    )
}

fn make_stacker_db_ids(i: usize) -> Vec<QualifiedContractIdentifier> {
    let mut dbs = vec![];
    for j in 0..i {
        dbs.push(stacker_db_id(j));
    }
    dbs
}

fn setup_peer_config(
    i: usize,
    port_base: u16,
    neighbor_count: usize,
    peer_count: usize,
) -> TestPeerConfig {
    let mut conf = TestPeerConfig::from_port(port_base + (2 * i as u16));
    conf.connection_opts.num_neighbors = neighbor_count as u64;
    conf.connection_opts.soft_num_neighbors = neighbor_count as u64;

    conf.connection_opts.num_clients = 256;
    conf.connection_opts.soft_num_clients = 128;

    conf.connection_opts.max_http_clients = 1000;
    conf.connection_opts.max_neighbors_of_neighbor = neighbor_count as u64;

    conf.connection_opts.max_clients_per_host = MAX_NEIGHBORS_DATA_LEN as u64;
    conf.connection_opts.soft_max_clients_per_host = peer_count as u64;

    conf.connection_opts.max_neighbors_per_host = MAX_NEIGHBORS_DATA_LEN as u64;
    conf.connection_opts.soft_max_neighbors_per_host = (neighbor_count / 2) as u64;
    conf.connection_opts.soft_max_neighbors_per_org = (neighbor_count / 2) as u64;

    conf.connection_opts.walk_interval = 0;

    conf.connection_opts.disable_inv_sync = true;
    conf.connection_opts.disable_block_download = true;

    let j = i as u32;
    conf.burnchain.peer_version = PEER_VERSION_TESTNET | (j << 16) | (j << 8) | j; // different non-major versions for each peer

    // even-number peers support stacker DBs.
    // odd-number peers do not
    if i % 2 == 0 {
        conf.services = (ServiceFlags::RELAY as u16)
            | (ServiceFlags::RPC as u16)
            | (ServiceFlags::STACKERDB as u16);
        conf.stacker_dbs = make_stacker_db_ids(i);
    } else {
        conf.services = (ServiceFlags::RELAY as u16) | (ServiceFlags::RPC as u16);
        conf.stacker_dbs = vec![];
    }

    conf
}

/// Arrange 15 peers into a ring topology, and verify that each peer learns of each other peer over
/// time.  Peers are always allowed, so always peered with.
#[test]
#[ignore]
fn test_walk_ring_allow_15() {
    setup_rlimit_nofiles();
    with_timeout(600, || {
        // all initial peers are allowed
        let mut peer_configs = vec![];
        let peer_count: usize = 15;
        let neighbor_count: usize = 3;

        for i in 0..peer_count {
            let mut conf = setup_peer_config(i, 32800, neighbor_count, peer_count);

            conf.allowed = -1; // always allowed
            conf.denied = 0;

            conf.connection_opts.timeout = 100000;
            conf.connection_opts.handshake_timeout = 100000;
            conf.connection_opts.disable_natpunch = true; // breaks allow checks

            peer_configs.push(conf);
        }

        test_walk_ring(&mut peer_configs);
    })
}

/// Arrange 15 peers into a ring topology, and verify that each peer learns of each other peer over
/// time.  No peer is always-allowed, and all walks are allowed.
#[test]
#[ignore]
fn test_walk_ring_15_plain() {
    setup_rlimit_nofiles();
    with_timeout(600, || {
        // initial peers are neither white- nor denied
        let mut peer_configs = vec![];
        let peer_count: usize = 15;
        let neighbor_count: usize = 3;

        for i in 0..peer_count {
            let mut conf = setup_peer_config(i, 32900, neighbor_count, peer_count);

            conf.allowed = 0;
            conf.denied = 0;

            peer_configs.push(conf);
        }

        test_walk_ring(&mut peer_configs);
    })
}

/// Arrange 15 peers into a ring topology, and verify that each peer learns of each other peer over
/// time.  No inbound walks, but pingback walks are allowed.
#[test]
#[ignore]
fn test_walk_ring_15_pingback() {
    setup_rlimit_nofiles();
    with_timeout(600, || {
        // initial peers are neither white- nor denied
        let mut peer_configs = vec![];
        let peer_count: usize = 15;
        let neighbor_count: usize = 3;

        for i in 0..peer_count {
            let mut conf = setup_peer_config(i, 32950, neighbor_count, peer_count);

            conf.allowed = 0;
            conf.denied = 0;
            conf.connection_opts.disable_pingbacks = false;
            conf.connection_opts.disable_inbound_walks = true;

            peer_configs.push(conf);
        }

        test_walk_ring_pingback(&mut peer_configs);
    })
}

/// Arrange 15 peers into a ring topology, and verify that each peer learns of each other peer over
/// time.  Puts one peer in a different AS to bias the neighbor walk towards it.
#[test]
#[ignore]
fn test_walk_ring_15_org_biased() {
    setup_rlimit_nofiles();
    with_timeout(600, || {
        // one outlier peer has a different org than the others.
        use std::env;

        // ::33000 is in AS 1
        env::set_var("BLOCKSTACK_NEIGHBOR_TEST_33000", "1");

        let mut peer_configs = vec![];
        let peer_count: usize = 15;
        let neighbor_count: usize = 3;

        for i in 0..peer_count {
            let mut conf = setup_peer_config(i, 33000, neighbor_count, peer_count);

            conf.allowed = 0;
            conf.denied = 0;
            if i == 0 {
                conf.asn = 1;
                conf.org = 1;
            } else {
                conf.asn = 0;
                conf.org = 0;
            }

            peer_configs.push(conf);
        }

        // all peers see peer ::33000 as having ASN and Org ID 1
        let peer_0 = peer_configs[0].to_neighbor();

        let peers = test_walk_ring(&mut peer_configs);

        for i in 1..peer_count {
            match PeerDB::get_peer(
                peers[i].network.peerdb.conn(),
                peer_0.addr.network_id,
                &peer_0.addr.addrbytes,
                peer_0.addr.port,
            )
            .unwrap()
            {
                Some(p) => {
                    assert_eq!(p.asn, 1);
                    assert_eq!(p.org, 1);
                }
                None => {}
            }
        }

        // no peer pruned peer ::33000
        for i in 1..peer_count {
            match peers[i].network.prune_inbound_counts.get(&peer_0.addr) {
                None => {}
                Some(count) => {
                    assert_eq!(*count, 0);
                }
            }
        }
    })
}

fn test_walk_ring_ex(peer_configs: &mut Vec<TestPeerConfig>, test_pingback: bool) -> Vec<TestPeer> {
    // arrange neighbors into a "ring" topology, where
    // neighbor N is connected to neighbor (N-1)%NUM_NEIGHBORS and (N+1)%NUM_NEIGHBORS.
    // If test_pingback is true, then neighbor N is only connected to (N+1)%NUM_NEIGHBORS
    let mut peers = vec![];

    let peer_count = peer_configs.len();

    for i in 0..peer_count {
        let n = (i + 1) % peer_count;
        let neighbor = peer_configs[n].to_neighbor();
        peer_configs[i].add_neighbor(&neighbor);
    }

    if !test_pingback {
        for i in 1..peer_count + 1 {
            let p = i - 1;
            let neighbor = peer_configs[p].to_neighbor();
            peer_configs[i % peer_count].add_neighbor(&neighbor);
        }
    }

    for i in 0..peer_count {
        let p = TestPeer::new(peer_configs[i].clone());
        peers.push(p);
    }

    run_topology_test(&mut peers);

    // no nacks or handshake-rejects
    for i in 0..peer_count {
        for (_, convo) in peers[i].network.peers.iter() {
            assert!(
                *convo
                    .stats
                    .msg_rx_counts
                    .get(&StacksMessageID::Nack)
                    .unwrap_or(&0)
                    == 0
            );
            assert!(
                *convo
                    .stats
                    .msg_rx_counts
                    .get(&StacksMessageID::HandshakeReject)
                    .unwrap_or(&0)
                    == 0
            );
        }
    }

    peers
}

fn test_walk_ring(peer_configs: &mut Vec<TestPeerConfig>) -> Vec<TestPeer> {
    test_walk_ring_ex(peer_configs, false)
}

fn test_walk_ring_pingback(peer_configs: &mut Vec<TestPeerConfig>) -> Vec<TestPeer> {
    test_walk_ring_ex(peer_configs, true)
}

/// Arrange 15 peers into a line topology, and verify that each peer learns of each other peer over
/// time.  All peers are whitelisted to one another.
#[test]
#[ignore]
fn test_walk_line_allowed_15() {
    setup_rlimit_nofiles();
    with_timeout(600, || {
        let mut peer_configs = vec![];
        let peer_count: usize = 15;
        let neighbor_count: usize = 3;

        for i in 0..peer_count {
            let mut conf = setup_peer_config(i, 33100, neighbor_count, peer_count);

            conf.allowed = -1;
            conf.denied = 0;

            conf.connection_opts.timeout = 100000;
            conf.connection_opts.handshake_timeout = 100000;
            conf.connection_opts.disable_natpunch = true; // breaks allow checks

            peer_configs.push(conf);
        }

        test_walk_line(&mut peer_configs);
    })
}

/// Arrange 15 peers into a line topology, and verify that each peer learns of each other peer over
/// time.  No peers are whitelisted to one another, and all walk types are allowed.
#[test]
#[ignore]
fn test_walk_line_15_plain() {
    setup_rlimit_nofiles();
    with_timeout(600, || {
        // initial peers are neither white- nor denied
        let mut peer_configs = vec![];
        let peer_count: usize = 15;
        let neighbor_count: usize = 3;

        for i in 0..peer_count {
            let mut conf = setup_peer_config(i, 33200, neighbor_count, peer_count);

            conf.allowed = 0;
            conf.denied = 0;

            peer_configs.push(conf);
        }

        test_walk_line(&mut peer_configs);
    })
}

/// Arrange 15 peers into a line topology, and verify that each peer learns of each other peer over
/// time.  One peer is in a different AS.
#[test]
#[ignore]
fn test_walk_line_15_org_biased() {
    setup_rlimit_nofiles();
    with_timeout(600, || {
        // one outlier peer has a different org than the others.
        use std::env;

        // ::33300 is in AS 1
        env::set_var("BLOCKSTACK_NEIGHBOR_TEST_33300", "1");

        let mut peer_configs = vec![];
        let peer_count: usize = 15;
        let neighbor_count: usize = 3; // make this a little bigger to speed this test up
        for i in 0..peer_count {
            let mut conf = setup_peer_config(i, 33300, neighbor_count, peer_count);

            conf.allowed = 0;
            conf.denied = 0;
            if i == 0 {
                conf.asn = 1;
                conf.org = 1;
            } else {
                conf.asn = 0;
                conf.org = 0;
            }

            peer_configs.push(conf);
        }
        // all peers see peer ::33300 as having ASN and Org ID 1
        let peer_0 = peer_configs[0].to_neighbor();

        let peers = test_walk_line(&mut peer_configs);

        for i in 1..peer_count {
            match PeerDB::get_peer(
                peers[i].network.peerdb.conn(),
                peer_0.addr.network_id,
                &peer_0.addr.addrbytes,
                peer_0.addr.port,
            )
            .unwrap()
            {
                Some(p) => {
                    assert_eq!(p.asn, 1);
                    assert_eq!(p.org, 1);
                }
                None => {}
            }
        }

        // no peer pruned peer ::33300
        for i in 1..peer_count {
            match peers[i].network.prune_inbound_counts.get(&peer_0.addr) {
                None => {}
                Some(count) => {
                    assert_eq!(*count, 0);
                }
            }
        }
    })
}

/// Arrange 15 peers into a line topology, and verify that each peer learns of each other peer over
/// time.  No inbound walks allowed; only pingback walks.
#[test]
#[ignore]
fn test_walk_line_15_pingback() {
    setup_rlimit_nofiles();
    with_timeout(600, || {
        // initial peers are neither white- nor denied
        let mut peer_configs = vec![];
        let peer_count: usize = 15;
        let neighbor_count: usize = 3;

        for i in 0..peer_count {
            let mut conf = setup_peer_config(i, 33350, neighbor_count, peer_count);

            conf.allowed = 0;
            conf.denied = 0;
            conf.connection_opts.disable_pingbacks = false;
            conf.connection_opts.disable_inbound_walks = true;

            peer_configs.push(conf);
        }

        test_walk_line_pingback(&mut peer_configs);
    })
}

fn test_walk_line(peer_configs: &mut Vec<TestPeerConfig>) -> Vec<TestPeer> {
    test_walk_line_ex(peer_configs, false)
}

fn test_walk_line_pingback(peer_configs: &mut Vec<TestPeerConfig>) -> Vec<TestPeer> {
    test_walk_line_ex(peer_configs, true)
}

fn test_walk_line_ex(peer_configs: &mut Vec<TestPeerConfig>, pingback_test: bool) -> Vec<TestPeer> {
    // arrange neighbors into a "line" topology.
    // If pingback_test is true, then the topology is unidirectional:
    //
    // 0 ---> 1 ---> 2 ---> ... ---> peer_count
    //
    // If pingback_test is false, then the topology is bidirectional
    //
    // 0 <--> 1 <--> 2 <--> ... <--> peer_count
    //
    // all initial peers are allowed
    let mut peers = vec![];

    let peer_count = peer_configs.len();
    for i in 0..peer_count - 1 {
        let n = i + 1;
        let neighbor = peer_configs[n].to_neighbor();
        peer_configs[i].add_neighbor(&neighbor);
    }

    if !pingback_test {
        for i in 1..peer_count {
            let p = i - 1;
            let neighbor = peer_configs[p].to_neighbor();
            peer_configs[i].add_neighbor(&neighbor);
        }
    }

    for i in 0..peer_count {
        let p = TestPeer::new(peer_configs[i].clone());
        peers.push(p);
    }

    run_topology_test(&mut peers);

    // no nacks or handshake-rejects
    for i in 0..peer_count {
        for (_, convo) in peers[i].network.peers.iter() {
            assert!(
                *convo
                    .stats
                    .msg_rx_counts
                    .get(&StacksMessageID::Nack)
                    .unwrap_or(&0)
                    == 0
            );
            assert!(
                *convo
                    .stats
                    .msg_rx_counts
                    .get(&StacksMessageID::HandshakeReject)
                    .unwrap_or(&0)
                    == 0
            );
        }
    }

    peers
}

/// Arrange 15 peers into a star topology, and verify that each peer learns of each other peer over
/// time.  All peers whitelist each other.
#[test]
#[ignore]
fn test_walk_star_allowed_15() {
    setup_rlimit_nofiles();
    with_timeout(600, || {
        let mut peer_configs = vec![];
        let peer_count: usize = 15;
        let neighbor_count: usize = 3;
        for i in 0..peer_count {
            let mut conf = setup_peer_config(i, 33400, neighbor_count, peer_count);

            conf.allowed = -1; // always allowed
            conf.denied = 0;

            conf.connection_opts.timeout = 100000;
            conf.connection_opts.handshake_timeout = 100000;
            conf.connection_opts.disable_natpunch = true; // breaks allow checks

            peer_configs.push(conf);
        }

        test_walk_star(&mut peer_configs);
    })
}

/// Arrange 15 peers into a star topology, and verify that each peer learns of each other peer over
/// time.  No peers whitelist each other, and all walk types are alloweed.
#[test]
#[ignore]
fn test_walk_star_15_plain() {
    setup_rlimit_nofiles();
    with_timeout(600, || {
        let mut peer_configs = vec![];
        let peer_count: usize = 15;
        let neighbor_count: usize = 3;
        for i in 0..peer_count {
            let mut conf = setup_peer_config(i, 33500, neighbor_count, peer_count);

            conf.allowed = 0;
            conf.denied = 0;

            peer_configs.push(conf);
        }

        test_walk_star(&mut peer_configs);
    })
}

/// Arrange 15 peers into a star topology, and verify that each peer learns of each other peer over
/// time.  No peers whitelist each other, and inbound walks (but not pingbacks) are disabled.
#[test]
#[ignore]
fn test_walk_star_15_pingback() {
    setup_rlimit_nofiles();
    with_timeout(600, || {
        let mut peer_configs = vec![];
        let peer_count: usize = 15;
        let neighbor_count: usize = 3;
        for i in 0..peer_count {
            let mut conf = setup_peer_config(i, 33550, neighbor_count, peer_count);

            conf.allowed = 0;
            conf.denied = 0;
            conf.connection_opts.disable_pingbacks = false;
            conf.connection_opts.disable_inbound_walks = true;
            conf.connection_opts.soft_max_neighbors_per_org = peer_count as u64;

            peer_configs.push(conf);
        }

        test_walk_star_pingback(&mut peer_configs);
    })
}

/// Arrange 15 peers into a star topology, and verify that each peer learns of each other peer over
/// time.  One peer is in a separate AS.
#[test]
#[ignore]
fn test_walk_star_15_org_biased() {
    setup_rlimit_nofiles();
    with_timeout(600, || {
        // one outlier peer has a different org than the others.
        use std::env;

        // ::33600 is in AS 1
        env::set_var("BLOCKSTACK_NEIGHBOR_TEST_33600", "1");

        let mut peer_configs = vec![];
        let peer_count: usize = 15;
        let neighbor_count: usize = 3;
        for i in 0..peer_count {
            let mut conf = setup_peer_config(i, 33600, neighbor_count, peer_count);

            conf.allowed = 0;
            conf.denied = 0;
            if i == 0 {
                conf.asn = 1;
                conf.org = 1;
            } else {
                conf.asn = 0;
                conf.org = 0;
            }

            peer_configs.push(conf);
        }
        // all peers see peer ::33600 as having ASN and Org ID 1
        let peer_0 = peer_configs[0].to_neighbor();

        let peers = test_walk_star(&mut peer_configs);

        for i in 1..peer_count {
            match PeerDB::get_peer(
                peers[i].network.peerdb.conn(),
                peer_0.addr.network_id,
                &peer_0.addr.addrbytes,
                peer_0.addr.port,
            )
            .unwrap()
            {
                Some(p) => {
                    assert_eq!(p.asn, 1);
                    assert_eq!(p.org, 1);
                }
                None => {}
            }
        }

        // no peer pruned peer ::33600
        for i in 1..peer_count {
            match peers[i].network.prune_inbound_counts.get(&peer_0.addr) {
                None => {}
                Some(count) => {
                    assert_eq!(*count, 0);
                }
            }
        }
    })
}

fn test_walk_star(peer_configs: &mut Vec<TestPeerConfig>) -> Vec<TestPeer> {
    test_walk_star_ex(peer_configs, false)
}

fn test_walk_star_pingback(peer_configs: &mut Vec<TestPeerConfig>) -> Vec<TestPeer> {
    test_walk_star_ex(peer_configs, true)
}

fn test_walk_star_ex(peer_configs: &mut Vec<TestPeerConfig>, pingback_test: bool) -> Vec<TestPeer> {
    // arrange neighbors into a "star" topology.
    // If pingback_test is true, then initial connections are unidirectional -- each neighbor (except
    // for 0) only knows about 0.  Neighbor 0 knows about no one.
    // If pingback_test is false, then initial connections are bidirectional.

    let mut peers = vec![];
    let peer_count = peer_configs.len();

    for i in 1..peer_count {
        let neighbor = peer_configs[i].to_neighbor();
        let hub = peer_configs[0].to_neighbor();
        if !pingback_test {
            peer_configs[0].add_neighbor(&neighbor);
        }

        peer_configs[i].add_neighbor(&hub);
    }

    for i in 0..peer_count {
        let p = TestPeer::new(peer_configs[i].clone());
        peers.push(p);
    }

    run_topology_test(&mut peers);

    // no nacks or handshake-rejects
    for i in 0..peer_count {
        for (_, convo) in peers[i].network.peers.iter() {
            assert!(
                *convo
                    .stats
                    .msg_rx_counts
                    .get(&StacksMessageID::Nack)
                    .unwrap_or(&0)
                    == 0
            );
            assert!(
                *convo
                    .stats
                    .msg_rx_counts
                    .get(&StacksMessageID::HandshakeReject)
                    .unwrap_or(&0)
                    == 0
            );
        }
    }

    peers
}

fn test_walk_inbound_line(peer_configs: &mut Vec<TestPeerConfig>) -> Vec<TestPeer> {
    // arrange neighbors into a two-tiered "line" topology, where even-numbered neighbors are
    // "NAT'ed" but connected to both the predecessor and successor odd neighbors.  Odd
    // numbered neighbors are not connected to anyone.  The first and last even-numbered
    // neighbor is only connected to its successor and predecessor, respectively.
    //
    //    1     3     5
    //   ^ ^   ^ ^   ^ ^
    //  /   \ /   \ /   \   ... etc ...
    // 0     2     4     6
    //
    // The goal of this test is that odd-numbered neighbors all learn about each other

    let mut peers = vec![];
    let peer_count = peer_configs.len();

    for i in 0..peer_count {
        if i % 2 == 0 {
            if i > 0 {
                let predecessor = peer_configs[i - 1].to_neighbor();
                peer_configs[i].add_neighbor(&predecessor);
            }
            if i + 1 < peer_count {
                let successor = peer_configs[i + 1].to_neighbor();
                peer_configs[i].add_neighbor(&successor);
            }
        }
    }

    for i in 0..peer_count {
        let p = TestPeer::new(peer_configs[i].clone());
        peers.push(p);
    }

    run_topology_test_ex(
        &mut peers,
        |peers: &[TestPeer]| {
            let mut done = true;
            for i in 0..peer_count {
                // only check "public" peers
                if i % 2 != 0 {
                    let all_neighbors =
                        PeerDB::get_all_peers(peers[i].network.peerdb.conn()).unwrap();
                    if (all_neighbors.len() as u64) < ((peer_count / 2 - 1) as u64) {
                        let nk = peers[i].config.to_neighbor().addr;
                        test_debug!(
                            "waiting for public peer {:?} to fill up its frontier: {}",
                            &nk,
                            all_neighbors.len()
                        );
                        done = false;
                    }
                }
            }
            done
        },
        true,
    );

    // no nacks or handshake-rejects
    for i in 0..peer_count {
        for (_, convo) in peers[i].network.peers.iter() {
            assert!(
                *convo
                    .stats
                    .msg_rx_counts
                    .get(&StacksMessageID::Nack)
                    .unwrap_or(&0)
                    == 0
            );
            assert!(
                *convo
                    .stats
                    .msg_rx_counts
                    .get(&StacksMessageID::HandshakeReject)
                    .unwrap_or(&0)
                    == 0
            );
        }
    }

    peers
}

/// Arrange 15 peers into an alternating line topology, and verify that each peer learns of each
/// other peer over time.  Odd peers have no outbound neighbors initially, but share one or two
/// inbound peers.
#[test]
#[ignore]
fn test_walk_inbound_line_15() {
    setup_rlimit_nofiles();
    with_timeout(600, || {
        let mut peer_configs = vec![];
        let peer_count: usize = 15;
        let neighbor_count: usize = 15; // make this test go faster

        for i in 0..peer_count {
            let mut conf = setup_peer_config(i, 33250, neighbor_count, peer_count);

            conf.allowed = 0;
            conf.denied = 0;
            conf.connection_opts.disable_pingbacks = true;
            conf.connection_opts.disable_inbound_walks = false;
            conf.connection_opts.walk_inbound_ratio = 2;
            // basically, don't timeout (so public nodes can ask non-public inbound nodes about
            // neighbors indefinitely)
            conf.connection_opts.connect_timeout = 60000;
            conf.connection_opts.timeout = 60000;
            conf.connection_opts.handshake_timeout = 60000;
            conf.connection_opts.soft_max_neighbors_per_org = (neighbor_count + 1) as u64;
            conf.connection_opts.soft_max_neighbors_per_host = (neighbor_count + 1) as u64;

            peer_configs.push(conf);
        }

        test_walk_inbound_line(&mut peer_configs);
    })
}

fn dump_peers(peers: &[TestPeer]) {
    test_debug!("\n=== PEER DUMP ===");
    for i in 0..peers.len() {
        let mut neighbor_index = vec![];
        let mut outbound_neighbor_index = vec![];
        for j in 0..peers.len() {
            let stats_opt = peers[i]
                .network
                .get_neighbor_stats(&peers[j].to_neighbor().addr);
            match stats_opt {
                Some(stats) => {
                    neighbor_index.push(j);
                    if stats.outbound {
                        outbound_neighbor_index.push(j);
                    }
                }
                None => {}
            }
        }

        let all_neighbors = PeerDB::get_all_peers(peers[i].network.peerdb.conn()).unwrap();
        let num_allowed = all_neighbors.iter().fold(0, |mut sum, ref n2| {
            sum += if n2.allowed < 0 { 1 } else { 0 };
            sum
        });
        test_debug!("Neighbor {} (all={}, outbound={}) (total neighbors = {}, total allowed = {}): outbound={:?} all={:?}", i, neighbor_index.len(), outbound_neighbor_index.len(), all_neighbors.len(), num_allowed, &outbound_neighbor_index, &neighbor_index);
    }
    test_debug!("\n");
}

fn dump_peer_histograms(peers: &[TestPeer]) {
    let mut outbound_hist: HashMap<usize, usize> = HashMap::new();
    let mut inbound_hist: HashMap<usize, usize> = HashMap::new();
    let mut all_hist: HashMap<usize, usize> = HashMap::new();
    for i in 0..peers.len() {
        let mut neighbor_index = vec![];
        let mut inbound_neighbor_index = vec![];
        let mut outbound_neighbor_index = vec![];
        for j in 0..peers.len() {
            let stats_opt = peers[i]
                .network
                .get_neighbor_stats(&peers[j].to_neighbor().addr);
            match stats_opt {
                Some(stats) => {
                    neighbor_index.push(j);
                    if stats.outbound {
                        outbound_neighbor_index.push(j);
                    } else {
                        inbound_neighbor_index.push(j);
                    }
                }
                None => {}
            }
        }
        for inbound in inbound_neighbor_index.iter() {
            if inbound_hist.contains_key(inbound) {
                let c = inbound_hist.get(inbound).unwrap().to_owned();
                inbound_hist.insert(*inbound, c + 1);
            } else {
                inbound_hist.insert(*inbound, 1);
            }
        }
        for outbound in outbound_neighbor_index.iter() {
            if outbound_hist.contains_key(outbound) {
                let c = outbound_hist.get(outbound).unwrap().to_owned();
                outbound_hist.insert(*outbound, c + 1);
            } else {
                outbound_hist.insert(*outbound, 1);
            }
        }
        for n in neighbor_index.iter() {
            if all_hist.contains_key(n) {
                let c = all_hist.get(n).unwrap().to_owned();
                all_hist.insert(*n, c + 1);
            } else {
                all_hist.insert(*n, 1);
            }
        }
    }

    test_debug!("=== PEER HISTOGRAM ===");
    for i in 0..peers.len() {
        test_debug!(
            "Neighbor {}: #in={} #out={} #all={}",
            i,
            inbound_hist.get(&i).unwrap_or(&0),
            outbound_hist.get(&i).unwrap_or(&0),
            all_hist.get(&i).unwrap_or(&0)
        );
    }
    test_debug!("\n");
}

fn run_topology_test(peers: &mut Vec<TestPeer>) {
    run_topology_test_ex(peers, |_| false, false)
}

fn run_topology_test_ex<F>(
    peers: &mut Vec<TestPeer>,
    mut finished_check: F,
    use_finished_check: bool,
) where
    F: FnMut(&[TestPeer]) -> bool,
{
    let peer_count = peers.len();

    let mut initial_allowed: HashMap<NeighborKey, Vec<NeighborKey>> = HashMap::new();
    let mut initial_denied: HashMap<NeighborKey, Vec<NeighborKey>> = HashMap::new();

    for i in 0..peer_count {
        // turn off components we don't need
        peers[i].config.connection_opts.disable_inv_sync = true;
        peers[i].config.connection_opts.disable_block_download = true;
        let nk = peers[i].config.to_neighbor().addr.clone();
        for j in 0..peers[i].config.initial_neighbors.len() {
            let initial = &peers[i].config.initial_neighbors[j];
            if initial.allowed < 0 {
                if !initial_allowed.contains_key(&nk) {
                    initial_allowed.insert(nk.clone(), vec![]);
                }
                initial_allowed
                    .get_mut(&nk)
                    .unwrap()
                    .push(initial.addr.clone());
            }
            if initial.denied < 0 {
                if !initial_denied.contains_key(&nk) {
                    initial_denied.insert(nk.clone(), vec![]);
                }
                initial_denied
                    .get_mut(&nk)
                    .unwrap()
                    .push(initial.addr.clone());
            }
        }
    }

    for i in 0..peer_count {
        peers[i].connect_initial().unwrap();
    }

    // go until each neighbor knows about each other neighbor
    let mut finished = false;
    let mut count = 0;
    while !finished {
        finished = true;
        let mut peer_counts = 0;
        let mut random_order = vec![0usize; peer_count];
        for i in 0..peer_count {
            random_order[i] = i;
        }
        let mut rng = thread_rng();
        random_order.shuffle(&mut rng);

        debug!("Random order = {:?}", &random_order);
        for i in random_order.into_iter() {
            let _ = peers[i].step_with_ibd(false);
            let nk = peers[i].config.to_neighbor().addr;
            debug!("Step peer {:?}", &nk);

            // allowed peers are still connected
            match initial_allowed.get(&nk) {
                Some(ref peer_list) => {
                    for pnk in peer_list.iter() {
                        if !peers[i].network.events.contains_key(&pnk.clone()) {
                            error!(
                                "{:?}: Perma-allowed peer {:?} not connected anymore",
                                &nk, &pnk
                            );
                            assert!(false);
                        }
                    }
                }
                None => {}
            };

            // denied peers are never connected
            match initial_denied.get(&nk) {
                Some(ref peer_list) => {
                    for pnk in peer_list.iter() {
                        if peers[i].network.events.contains_key(&pnk.clone()) {
                            error!("{:?}: Perma-denied peer {:?} connected", &nk, &pnk);
                            assert!(false);
                        }
                    }
                }
                None => {}
            };

            // all ports are unique in the p2p socket table
            let mut ports: HashSet<u16> = HashSet::new();
            for k in peers[i].network.events.keys() {
                if ports.contains(&k.port) {
                    error!("duplicate port {} from {:?}", k.port, k);
                    assert!(false);
                }
                ports.insert(k.port);
            }

            // done?
            let now_finished = if use_finished_check {
                finished_check(&peers)
            } else {
                let mut done = true;
                let all_neighbors = PeerDB::get_all_peers(peers[i].network.peerdb.conn()).unwrap();
                peer_counts += all_neighbors.len();
                test_debug!("Peer {} ({}) has {} neighbors", i, &nk, all_neighbors.len());

                if (all_neighbors.len() as u64) < ((peer_count - 1) as u64) {
                    test_debug!(
                        "waiting for {:?} to fill up its frontier: {} < {}",
                        &nk,
                        all_neighbors.len(),
                        peer_count - 1
                    );
                    done = false;
                } else {
                    test_debug!(
                        "not waiting for {:?} to fill up its frontier: {} >= {}",
                        &nk,
                        all_neighbors.len(),
                        peer_count - 1
                    );
                }
                done
            };

            finished = finished && now_finished;
        }

        count += 1;

        test_debug!(
            "Network convergence rate: {}%",
            (100.0 * (peer_counts as f64)) / ((peer_count * peer_count) as f64),
        );

        if finished {
            break;
        }

        test_debug!("Finished walking the network {} times", count);
        dump_peers(&peers);
        dump_peer_histograms(&peers);
    }

    test_debug!("Converged after {} calls to network.run()", count);
    dump_peers(&peers);
    dump_peer_histograms(&peers);

    // each peer learns each other peer's stacker DBs
    for (i, peer) in peers.iter().enumerate() {
        if i % 2 != 0 {
            continue;
        }
        let mut expected_dbs = PeerDB::get_local_peer(peer.network.peerdb.conn())
            .unwrap()
            .stacker_dbs;
        expected_dbs.sort();
        for (j, other_peer) in peers.iter().enumerate() {
            if i == j {
                continue;
            }

            let all_neighbors = PeerDB::get_all_peers(other_peer.network.peerdb.conn()).unwrap();

            if (all_neighbors.len() as u64) < ((peer_count - 1) as u64) {
                // this is a simulated-NAT'ed node -- it won't learn about other NAT'ed nodes'
                // DBs
                continue;
            }

            // what does the other peer see as this peer's stacker DBs?
            let mut other_peer_dbs = other_peer
                .network
                .peerdb
                .get_peer_stacker_dbs(&peer.config.to_neighbor())
                .unwrap();
            other_peer_dbs.sort();

            if j % 2 == 0 {
                test_debug!(
                    "Compare stacker DBs of {} vs {}",
                    &peer.config.to_neighbor(),
                    &other_peer.config.to_neighbor()
                );
                assert_eq!(expected_dbs, other_peer_dbs);
            } else {
                // this peer doesn't support Stacker DBs
                assert_eq!(other_peer_dbs, vec![]);
            }
        }
    }
}
