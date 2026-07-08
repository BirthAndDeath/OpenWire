use std::time::Duration;

use futures::StreamExt;
use libp2p::{Multiaddr, PeerId};
use libp2p_pathranker::node::smart::SmartNode;

/// 两个 MemoryTransport 节点：连接建立。
#[tokio::test]
async fn test_two_nodes_connect() {
    let mut node_a = SmartNode::new_test().await;
    let mut node_b = SmartNode::new_test().await;

    let b_addr: Multiaddr = format!("/memory/{}", rand::random::<u64>()).parse().unwrap();
    let b_id = *node_b.swarm.local_peer_id();

    node_b.listen(b_addr.clone()).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let listen_addr = node_b.listeners().next().cloned().unwrap_or(b_addr);
    node_a.swarm.dial(listen_addr).unwrap();

    // 轮询直到连接建立或超时
    for _ in 0..50 {
        tokio::select! {
            biased;
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            event = node_a.swarm.select_next_some() => {
                node_a.handle_swarm_event(event);
                if node_a.swarm.is_connected(&b_id) {
                    break;
                }
            }
            event = node_b.swarm.select_next_some() => {
                node_b.handle_swarm_event(event);
            }
        }
    }

    assert!(node_a.swarm.is_connected(&b_id), "nodes should connect via memory transport");
}

/// 评分查询：建立连接后发送查询，协议不应 panic。
#[tokio::test]
async fn test_score_query_sends_safely() {
    let mut node_a = SmartNode::new_test().await;
    let mut node_b = SmartNode::new_test().await;

    let b_addr: Multiaddr = format!("/memory/{}", rand::random::<u64>()).parse().unwrap();
    let b_id = *node_b.swarm.local_peer_id();

    node_b.listen(b_addr.clone()).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let listen_addr = node_b.listeners().next().cloned().unwrap_or(b_addr);
    node_a.swarm.dial(listen_addr).unwrap();

    // 建立连接
    for _ in 0..50 {
        tokio::select! {
            biased;
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            event = node_a.swarm.select_next_some() => {
                node_a.handle_swarm_event(event);
            }
            event = node_b.swarm.select_next_some() => {
                node_b.handle_swarm_event(event);
            }
        }
        if node_a.swarm.is_connected(&b_id) {
            break;
        }
    }

    assert!(node_a.swarm.is_connected(&b_id), "nodes should connect");

    // 发送评分查询并轮询（不应 panic）
    node_a.query_neighbor_scores(PeerId::random());
    tokio::time::sleep(Duration::from_millis(200)).await;
    for _ in 0..30 {
        tokio::select! {
            biased;
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            event = node_b.swarm.select_next_some() => {
                node_b.handle_swarm_event(event);
            }
            event = node_a.swarm.select_next_some() => {
                node_a.handle_swarm_event(event);
            }
        }
    }
}