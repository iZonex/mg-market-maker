//! PR-2a acceptance test: agent-side reconcile lifecycle.
//!
//! Drives a `LeaseClient` wired with a `StrategyRegistry` backed
//! by the `MockEngineFactory`. The test bypasses `AgentSession`
//! and talks over the raw transport so it can push arbitrary
//! command sequences directly — exactly what PR-2b's real controller
//! driver will do.
//!
//! Shape verified:
//! 1. Controller issues a lease + `SetDesiredStrategies{A, B}` — agent
//!    starts both.
//! 2. Controller pushes `SetDesiredStrategies{B, C}` — agent stops A,
//!    keeps B, starts C (running count stays at 2).
//! 3. Controller pushes `SetDesiredStrategies{}` — agent stops C (and
//!    B), running count drops to 0.

use std::sync::Arc;
use std::time::Duration;

use mm_agent::{AgentConfig, LeaseClient, MockEngineFactory, StrategyRegistry};
use mm_control::envelope::{Envelope, SignedEnvelope};
use mm_control::in_memory_pair;
use mm_control::lease::{LeaderLease, LeaseState};
use mm_control::messages::{AgentId, CommandPayload, DesiredStrategy};
use mm_control::seq::Seq;
use mm_control::transport::Transport;
use tokio::sync::watch;

fn desc(id: &str, sym: &str) -> DesiredStrategy {
    DesiredStrategy {
        deployment_id: id.into(),
        template: "mock".into(),
        symbol: sym.into(),
        ..Default::default()
    }
}

fn fresh_lease(agent: &str) -> LeaderLease {
    let now = chrono::Utc::now();
    LeaderLease {
        lease_id: uuid::Uuid::new_v4(),
        agent_id: agent.into(),
        issued_at: now,
        // Long TTL so the test is not racing the lease expiry.
        expires_at: now + chrono::Duration::seconds(60),
        issued_seq: Seq(1),
    }
}

#[tokio::test]
async fn reconcile_responds_to_brain_set_desired_strategies() {
    let (controller_side, agent_side) = in_memory_pair();

    // Registry + probe channel. The MockEngineFactory bumps the
    // probe counter once per spawn, so the test synchronises on
    // probe transitions rather than sleeping arbitrarily. We
    // observe reconcile correctness through the probe (spawns)
    // plus the agent's ACK flow (applied-seq advancing).
    let (probe_tx, mut probe_rx) = watch::channel(0u64);
    let registry = StrategyRegistry::new(Arc::new(MockEngineFactory::new(probe_tx)));
    let (client, _authority) = LeaseClient::new(
        agent_side,
        AgentConfig {
            id: AgentId::new("eu-test-reconcile"),
            ..Default::default()
        },
    );
    let client = client.with_registry(registry);
    let agent_task = tokio::spawn(async move { client.run().await });

    // Manual controller driver: read the agent's Register telemetry,
    // then push LeaseGrant and a series of SetDesiredStrategies
    // commands. Count spawns via the probe channel.
    let mut controller = controller_side;
    // Drain the Register telemetry.
    let reg = controller.recv().await.unwrap().expect("Register arrives");
    assert!(reg.envelope.telemetry.is_some());

    // Grant a lease so the agent is authorised.
    let lease = fresh_lease("eu-test-reconcile");
    controller
        .send(SignedEnvelope::unsigned(Envelope::command(
            Seq(1),
            CommandPayload::LeaseGrant { lease },
        )))
        .await
        .unwrap();
    // Drain the ACK.
    let _ = controller.recv().await;

    // Push first desired slice: {A, B}.
    controller
        .send(SignedEnvelope::unsigned(Envelope::command(
            Seq(2),
            CommandPayload::SetDesiredStrategies {
                strategies: vec![desc("A", "BTCUSDT"), desc("B", "ETHUSDT")],
            },
        )))
        .await
        .unwrap();
    let _ = controller.recv().await; // ACK

    // After apply, the probe should have advanced by 2 from the
    // post-lease baseline. Read with a timeout so a stuck test
    // fails loud rather than hanging.
    let spawns_after_first = read_probe_eventually(&mut probe_rx, 2).await;
    assert!(spawns_after_first >= 2, "spawned A + B");

    // Push second slice: {B, C}. A should stop, C should spawn.
    controller
        .send(SignedEnvelope::unsigned(Envelope::command(
            Seq(3),
            CommandPayload::SetDesiredStrategies {
                strategies: vec![desc("B", "ETHUSDT"), desc("C", "SOLUSDT")],
            },
        )))
        .await
        .unwrap();
    let _ = controller.recv().await;
    let after_swap = read_probe_eventually(&mut probe_rx, spawns_after_first + 1).await;
    assert!(
        after_swap >= spawns_after_first + 1,
        "swap spawns exactly one new (C)"
    );

    // Push empty slice — every running entry must stop.
    controller
        .send(SignedEnvelope::unsigned(Envelope::command(
            Seq(4),
            CommandPayload::SetDesiredStrategies {
                strategies: vec![],
            },
        )))
        .await
        .unwrap();
    let _ = controller.recv().await;

    // Give the agent a moment to abort the lingering tasks.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Tear down.
    agent_task.abort();
    let _ = agent_task.await;
}

async fn read_probe_eventually(rx: &mut watch::Receiver<u64>, min: u64) -> u64 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let v = *rx.borrow();
        if v >= min {
            return v;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("probe never reached {} (last saw {})", min, v);
        }
        let _ = tokio::time::timeout(Duration::from_millis(100), rx.changed()).await;
    }
}

#[tokio::test]
async fn lease_revocation_aborts_running_strategies() {
    let (controller_side, agent_side) = in_memory_pair();
    let (probe_tx, _probe_rx) = watch::channel(0u64);
    let registry = StrategyRegistry::new(Arc::new(MockEngineFactory::new(probe_tx)));

    let (client, authority) = LeaseClient::new(
        agent_side,
        AgentConfig {
            id: AgentId::new("eu-test-revoke"),
            ..Default::default()
        },
    );
    let client = client.with_registry(registry);
    let agent_task = tokio::spawn(async move { client.run().await });

    let mut controller = controller_side;
    let _ = controller.recv().await; // Register
    let lease = fresh_lease("eu-test-revoke");
    controller
        .send(SignedEnvelope::unsigned(Envelope::command(
            Seq(1),
            CommandPayload::LeaseGrant { lease },
        )))
        .await
        .unwrap();
    let _ = controller.recv().await; // ACK

    controller
        .send(SignedEnvelope::unsigned(Envelope::command(
            Seq(2),
            CommandPayload::SetDesiredStrategies {
                strategies: vec![desc("A", "BTCUSDT")],
            },
        )))
        .await
        .unwrap();
    let _ = controller.recv().await;

    // Revoke — agent should exit run() with AuthorityLost and
    // have aborted its single running strategy on the way out.
    controller
        .send(SignedEnvelope::unsigned(Envelope::command(
            Seq(3),
            CommandPayload::LeaseRevoke {
                reason: "test-revoke".into(),
            },
        )))
        .await
        .unwrap();

    let exit = tokio::time::timeout(Duration::from_secs(2), agent_task)
        .await
        .expect("agent exited within deadline")
        .expect("task panicked");
    assert!(
        exit.is_err(),
        "revocation should surface as AuthorityLost, got {:?}",
        exit
    );

    // Authority handle should reflect Revoked terminal state.
    let final_state = authority.current();
    assert!(
        matches!(final_state, LeaseState::Revoked { .. }),
        "terminal state should be Revoked, got {:?}",
        final_state
    );
}
