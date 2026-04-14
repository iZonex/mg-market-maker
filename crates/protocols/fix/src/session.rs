//! FIX 4.4 session engine.
//!
//! Pure synchronous state machine that turns:
//!
//! - inbound FIX messages → outbound responses + app-level deliveries
//! - the passage of time → heartbeats, test requests, watchdog disconnects
//!
//! The engine owns no I/O and no clock. Callers drive it by feeding
//! decoded [`Message`]s into [`FixSession::on_message`] and periodically
//! calling [`FixSession::tick`] with the current [`Instant`]. That
//! design keeps the session fully deterministic for tests — no tokio
//! time helpers, no real `Instant::now()` calls inside.
//!
//! The session covers the subset of FIX 4.4 session messages we actually
//! need for MM venues: Logon (A), Heartbeat (0), TestRequest (1),
//! ResendRequest (2), SequenceReset (4), Logout (5). Business messages
//! are delivered upward via [`SessionAction::DeliverApp`].

use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use crate::message::Message;
use crate::tags;

// Non-standard session-level tag numbers we reference below. Pulled
// inline instead of into `tags.rs` because they are only used in the
// session engine, not by the message codec.
const TAG_BEGIN_SEQ_NO: u32 = 7;
const TAG_END_SEQ_NO: u32 = 16;
const TAG_NEW_SEQ_NO: u32 = 36;
const TAG_POSS_DUP_FLAG: u32 = 43;
const TAG_GAP_FILL_FLAG: u32 = 123;
const TAG_RESET_SEQ_NUM_FLAG: u32 = 141;

/// Persistent send/receive sequence numbers.
///
/// Production implementations back this with a file so the session can
/// resume after a process crash. Tests use the in-memory impl.
pub trait SeqNumStore: Send {
    fn next_send(&mut self) -> u64;
    fn peek_send(&self) -> u64;
    fn expected_recv(&self) -> u64;
    fn advance_recv(&mut self, to: u64);
    fn reset(&mut self);
}

/// Non-persistent store used by tests and as a starting point for
/// fresh sessions.
#[derive(Debug, Clone)]
pub struct InMemorySeqStore {
    next_send: u64,
    expected_recv: u64,
}

impl InMemorySeqStore {
    pub fn new() -> Self {
        Self {
            next_send: 1,
            expected_recv: 1,
        }
    }
}

impl Default for InMemorySeqStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SeqNumStore for InMemorySeqStore {
    fn next_send(&mut self) -> u64 {
        let n = self.next_send;
        self.next_send += 1;
        n
    }

    fn peek_send(&self) -> u64 {
        self.next_send
    }

    fn expected_recv(&self) -> u64 {
        self.expected_recv
    }

    fn advance_recv(&mut self, to: u64) {
        self.expected_recv = to;
    }

    fn reset(&mut self) {
        self.next_send = 1;
        self.expected_recv = 1;
    }
}

/// High-level session lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// No TCP connection or connection dropped.
    Disconnected,
    /// Logon sent locally; waiting for peer's Logon acknowledgement.
    LogonSent,
    /// Both sides authenticated; application messages may flow.
    LoggedIn,
    /// Logout sent locally; waiting for peer's Logout response before
    /// closing the socket.
    LogoutSent,
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub sender_comp_id: String,
    pub target_comp_id: String,
    pub heartbeat_secs: u32,
    /// Set `ResetSeqNumFlag(141)=Y` on Logon and reset the store.
    pub reset_on_logon: bool,
}

/// Something the session wants the I/O driver to do.
#[derive(Debug)]
pub enum SessionAction {
    /// Write these encoded bytes to the transport.
    Send(Vec<u8>),
    /// Deliver this app-level message to the consumer above the
    /// session.
    DeliverApp(Message),
    /// Tear down the transport with the given human-readable reason.
    Disconnect(String),
}

pub struct FixSession<S: SeqNumStore> {
    config: SessionConfig,
    store: S,
    state: SessionState,
    last_recv_at: Option<Instant>,
    last_send_at: Option<Instant>,
    test_req_id_counter: u64,
    outstanding_test_req_id: Option<String>,
    outstanding_test_req_sent_at: Option<Instant>,
}

impl<S: SeqNumStore> FixSession<S> {
    pub fn new(config: SessionConfig, store: S) -> Self {
        Self {
            config,
            store,
            state: SessionState::Disconnected,
            last_recv_at: None,
            last_send_at: None,
            test_req_id_counter: 0,
            outstanding_test_req_id: None,
            outstanding_test_req_sent_at: None,
        }
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// Build and emit the Logon (35=A) frame that starts the session.
    pub fn start_logon(&mut self, now: Instant, sending_time: &str) -> SessionAction {
        if self.config.reset_on_logon {
            self.store.reset();
        }
        let mut msg = Message::logon(self.config.heartbeat_secs);
        if self.config.reset_on_logon {
            msg.set(TAG_RESET_SEQ_NUM_FLAG, "Y");
        }
        self.state = SessionState::LogonSent;
        self.send_session_msg(msg, now, sending_time)
    }

    /// Build a graceful Logout request.
    pub fn start_logout(&mut self, now: Instant, sending_time: &str) -> SessionAction {
        self.state = SessionState::LogoutSent;
        self.send_session_msg(Message::new("5"), now, sending_time)
    }

    /// Called by the I/O driver when an inbound message has been
    /// decoded. Returns zero or more actions the driver must perform
    /// (typically send-then-deliver, or disconnect).
    pub fn on_message(
        &mut self,
        msg: Message,
        now: Instant,
        sending_time: &str,
    ) -> Vec<SessionAction> {
        self.last_recv_at = Some(now);
        let mut out = Vec::new();

        // --- Sequence number check ---
        let msg_seq: u64 = msg
            .get(tags::MSG_SEQ_NUM)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let expected = self.store.expected_recv();
        let is_poss_dup = msg.get(TAG_POSS_DUP_FLAG) == Some("Y");

        if msg_seq > expected {
            // Gap — request a resend and do not process this message yet.
            let mut rr = Message::new("2");
            rr.set(TAG_BEGIN_SEQ_NO, expected.to_string());
            rr.set(TAG_END_SEQ_NO, "0"); // 0 = open-ended
            out.push(self.send_session_msg(rr, now, sending_time));
            return out;
        }

        if msg_seq < expected {
            if is_poss_dup {
                // Stale PossDup replay — silently drop. FIX 4.4 Vol 2
                // §"Session Protocol" → "Possible Duplicate": "When
                // MsgSeqNum is lower than expected, and PossDupFlag=Y,
                // the message should be discarded without generating
                // an error". Do NOT advance expected_recv, do NOT
                // deliver to the application layer.
                return out;
            }
            out.push(SessionAction::Disconnect(format!(
                "inbound MsgSeqNum {msg_seq} < expected {expected}"
            )));
            return out;
        }

        // --- Dispatch by MsgType ---
        match msg.msg_type() {
            "A" => {
                // Logon response.
                if self.state == SessionState::LogonSent {
                    self.state = SessionState::LoggedIn;
                    self.store.advance_recv(expected + 1);
                } else {
                    out.push(SessionAction::Disconnect(
                        "unexpected Logon in current state".into(),
                    ));
                }
            }
            "0" => {
                // Heartbeat.
                self.store.advance_recv(expected + 1);
                if let Some(tid) = self.outstanding_test_req_id.clone() {
                    if msg.get(tags::TEST_REQ_ID) == Some(tid.as_str()) {
                        self.outstanding_test_req_id = None;
                        self.outstanding_test_req_sent_at = None;
                    }
                }
            }
            "1" => {
                // TestRequest — echo the TestReqID back in a Heartbeat.
                self.store.advance_recv(expected + 1);
                let test_req_id = msg.get(tags::TEST_REQ_ID).unwrap_or("").to_string();
                let mut hb = Message::heartbeat();
                if !test_req_id.is_empty() {
                    hb.set(tags::TEST_REQ_ID, test_req_id);
                }
                out.push(self.send_session_msg(hb, now, sending_time));
            }
            "2" => {
                // ResendRequest. We do not buffer outbound history, so
                // we respond with a SequenceReset(4) GapFill covering
                // the requested range. Real venues that rely on actual
                // replay would reject this, but for our MM workflow the
                // loss of an unacked app message is usually recoverable
                // by reconciling open orders, so gap-fill is acceptable.
                self.store.advance_recv(expected + 1);
                let begin: u64 = msg
                    .get(TAG_BEGIN_SEQ_NO)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let end: u64 = msg
                    .get(TAG_END_SEQ_NO)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let target = end.max(begin) + 1;
                let mut sr = Message::new("4");
                sr.set(TAG_GAP_FILL_FLAG, "Y");
                sr.set(TAG_NEW_SEQ_NO, target.to_string());
                out.push(self.send_session_msg(sr, now, sending_time));
            }
            "4" => {
                // SequenceReset — advance our expected_recv.
                let new_seq: u64 = msg
                    .get(TAG_NEW_SEQ_NO)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(expected + 1);
                self.store.advance_recv(new_seq);
            }
            "5" => {
                // Logout.
                self.store.advance_recv(expected + 1);
                if self.state != SessionState::LogoutSent {
                    out.push(self.send_session_msg(Message::new("5"), now, sending_time));
                }
                self.state = SessionState::Disconnected;
                out.push(SessionAction::Disconnect("peer logout".into()));
            }
            _ => {
                // Application-level message — advance seq and deliver.
                self.store.advance_recv(expected + 1);
                out.push(SessionAction::DeliverApp(msg));
            }
        }

        out
    }

    /// Periodic tick — call roughly every 250ms. Emits heartbeats,
    /// test requests, or a disconnect when the watchdog fires.
    pub fn tick(&mut self, now: Instant, sending_time: &str) -> Vec<SessionAction> {
        let mut out = Vec::new();
        if self.state != SessionState::LoggedIn {
            return out;
        }
        let hb = Duration::from_secs(self.config.heartbeat_secs as u64);

        // 1. Send a heartbeat if we've been silent for HB seconds.
        let silent_for = self
            .last_send_at
            .map(|t| now.duration_since(t))
            .unwrap_or(hb);
        if silent_for >= hb {
            out.push(self.send_session_msg(Message::heartbeat(), now, sending_time));
        }

        // 2. Send a TestRequest if the peer has been silent for > 1.2 × HB.
        let recv_silent_for = self
            .last_recv_at
            .map(|t| now.duration_since(t))
            .unwrap_or_default();
        if recv_silent_for > hb.mul_f32(1.2) && self.outstanding_test_req_id.is_none() {
            self.test_req_id_counter += 1;
            let tid = format!("TR-{}", self.test_req_id_counter);
            let tr = Message::test_request(&tid);
            self.outstanding_test_req_id = Some(tid);
            self.outstanding_test_req_sent_at = Some(now);
            out.push(self.send_session_msg(tr, now, sending_time));
        }

        // 3. Disconnect if a TestRequest went unanswered for HB.
        if let Some(sent_at) = self.outstanding_test_req_sent_at {
            if now.duration_since(sent_at) > hb {
                self.state = SessionState::Disconnected;
                out.push(SessionAction::Disconnect("peer heartbeat timeout".into()));
            }
        }

        out
    }

    /// Send an application-level message. Refuses if not logged in.
    pub fn send_app(
        &mut self,
        msg: Message,
        now: Instant,
        sending_time: &str,
    ) -> Result<SessionAction> {
        if self.state != SessionState::LoggedIn {
            return Err(anyhow!("FIX session not logged in"));
        }
        Ok(self.send_session_msg(msg, now, sending_time))
    }

    fn send_session_msg(
        &mut self,
        msg: Message,
        now: Instant,
        sending_time: &str,
    ) -> SessionAction {
        let seq = self.store.next_send();
        let bytes = msg.encode(
            &self.config.sender_comp_id,
            &self.config.target_comp_id,
            seq,
            sending_time,
        );
        self.last_send_at = Some(now);
        SessionAction::Send(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "20240101-00:00:00.000";

    fn config() -> SessionConfig {
        SessionConfig {
            sender_comp_id: "CLIENT".into(),
            target_comp_id: "VENUE".into(),
            heartbeat_secs: 30,
            reset_on_logon: false,
        }
    }

    fn mk_session() -> FixSession<InMemorySeqStore> {
        FixSession::new(config(), InMemorySeqStore::new())
    }

    fn decode(bytes: &[u8]) -> Message {
        Message::decode(bytes).expect("decode failed")
    }

    fn take_send(actions: Vec<SessionAction>) -> Vec<u8> {
        for a in actions {
            if let SessionAction::Send(b) = a {
                return b;
            }
        }
        panic!("no Send action");
    }

    #[test]
    fn start_logon_emits_logon_and_moves_state() {
        let mut s = mk_session();
        let t0 = Instant::now();
        let action = s.start_logon(t0, T);
        assert_eq!(s.state(), &SessionState::LogonSent);
        let bytes = match action {
            SessionAction::Send(b) => b,
            _ => panic!("expected Send"),
        };
        let msg = decode(&bytes);
        assert_eq!(msg.msg_type(), "A");
        assert_eq!(msg.get(tags::HEART_BT_INT), Some("30"));
    }

    #[test]
    fn logon_ack_advances_to_logged_in() {
        let mut s = mk_session();
        let t0 = Instant::now();
        let _ = s.start_logon(t0, T);

        let ack = Message::logon(30);
        // Build an inbound Logon ack with MsgSeqNum = 1 (peer's first).
        let ack_bytes = ack.encode("VENUE", "CLIENT", 1, T);
        let actions = s.on_message(decode(&ack_bytes), t0, T);
        assert!(actions.is_empty());
        assert_eq!(s.state(), &SessionState::LoggedIn);
    }

    #[test]
    fn test_request_is_echoed_as_heartbeat_with_same_id() {
        let mut s = mk_session();
        let t0 = Instant::now();
        let _ = s.start_logon(t0, T);
        let _ = s.on_message(
            decode(&Message::logon(30).encode("VENUE", "CLIENT", 1, T)),
            t0,
            T,
        );

        let mut tr = Message::test_request("ABC");
        // Mutate MsgSeqNum via internal fields: encode then decode with
        // a specific seq.
        let tr_bytes = tr.encode("VENUE", "CLIENT", 2, T);
        let actions = s.on_message(decode(&tr_bytes), t0, T);
        let sent = take_send(actions);
        let hb = decode(&sent);
        assert_eq!(hb.msg_type(), "0");
        assert_eq!(hb.get(tags::TEST_REQ_ID), Some("ABC"));
        // Touch tr to silence the unused-mut warning.
        let _ = &mut tr;
    }

    #[test]
    fn heartbeat_tick_after_silence() {
        let mut s = mk_session();
        let t0 = Instant::now();
        let _ = s.start_logon(t0, T);
        let _ = s.on_message(
            decode(&Message::logon(30).encode("VENUE", "CLIENT", 1, T)),
            t0,
            T,
        );

        // Jump forward 31 seconds — tick should emit a Heartbeat.
        let t31 = t0 + Duration::from_secs(31);
        let actions = s.tick(t31, T);
        let sent = take_send(actions);
        assert_eq!(decode(&sent).msg_type(), "0");
    }

    #[test]
    fn test_request_watchdog_after_long_silence_from_peer() {
        let mut s = mk_session();
        let t0 = Instant::now();
        let _ = s.start_logon(t0, T);
        let _ = s.on_message(
            decode(&Message::logon(30).encode("VENUE", "CLIENT", 1, T)),
            t0,
            T,
        );

        // 37s passes — more than 1.2 × 30. tick should emit a
        // TestRequest.
        let t37 = t0 + Duration::from_secs(37);
        let actions = s.tick(t37, T);
        // Expect at least one Send; one of them should be a TestRequest.
        let mut saw_test_request = false;
        for a in actions {
            if let SessionAction::Send(b) = a {
                let m = decode(&b);
                if m.msg_type() == "1" {
                    saw_test_request = true;
                }
            }
        }
        assert!(saw_test_request, "no TestRequest emitted after silence");
    }

    #[test]
    fn watchdog_disconnects_if_test_request_unanswered() {
        let mut s = mk_session();
        let t0 = Instant::now();
        let _ = s.start_logon(t0, T);
        let _ = s.on_message(
            decode(&Message::logon(30).encode("VENUE", "CLIENT", 1, T)),
            t0,
            T,
        );

        // Trigger test request at t=37.
        let t37 = t0 + Duration::from_secs(37);
        let _ = s.tick(t37, T);
        // No answer from peer by t=70 (37 + 33 > 30s HB grace).
        let t70 = t0 + Duration::from_secs(70);
        let actions = s.tick(t70, T);
        let mut saw_disconnect = false;
        for a in actions {
            if matches!(a, SessionAction::Disconnect(_)) {
                saw_disconnect = true;
            }
        }
        assert!(saw_disconnect);
        assert_eq!(s.state(), &SessionState::Disconnected);
    }

    #[test]
    fn app_message_delivered_upward() {
        let mut s = mk_session();
        let t0 = Instant::now();
        let _ = s.start_logon(t0, T);
        let _ = s.on_message(
            decode(&Message::logon(30).encode("VENUE", "CLIENT", 1, T)),
            t0,
            T,
        );

        // ExecutionReport (35=8), peer seq = 2.
        let mut er = Message::new("8");
        er.set(tags::CL_ORD_ID, "order-1");
        let er_bytes = er.encode("VENUE", "CLIENT", 2, T);
        let actions = s.on_message(decode(&er_bytes), t0, T);
        let mut delivered = None;
        for a in actions {
            if let SessionAction::DeliverApp(m) = a {
                delivered = Some(m);
            }
        }
        let m = delivered.expect("no DeliverApp");
        assert_eq!(m.msg_type(), "8");
        assert_eq!(m.get(tags::CL_ORD_ID), Some("order-1"));
    }

    #[test]
    fn gap_triggers_resend_request() {
        let mut s = mk_session();
        let t0 = Instant::now();
        let _ = s.start_logon(t0, T);
        let _ = s.on_message(
            decode(&Message::logon(30).encode("VENUE", "CLIENT", 1, T)),
            t0,
            T,
        );

        // Peer skips seq 2 and sends seq 5 → gap of 3 messages.
        let mut er = Message::new("8");
        er.set(tags::CL_ORD_ID, "jumped");
        let er_bytes = er.encode("VENUE", "CLIENT", 5, T);
        let actions = s.on_message(decode(&er_bytes), t0, T);
        let sent = take_send(actions);
        let rr = decode(&sent);
        assert_eq!(rr.msg_type(), "2");
        assert_eq!(rr.get(TAG_BEGIN_SEQ_NO), Some("2"));
        // ExecutionReport was NOT delivered since we hold for resend.
    }

    #[test]
    fn peer_logout_triggers_echo_and_disconnect() {
        let mut s = mk_session();
        let t0 = Instant::now();
        let _ = s.start_logon(t0, T);
        let _ = s.on_message(
            decode(&Message::logon(30).encode("VENUE", "CLIENT", 1, T)),
            t0,
            T,
        );

        let logout = Message::new("5");
        let logout_bytes = logout.encode("VENUE", "CLIENT", 2, T);
        let actions = s.on_message(decode(&logout_bytes), t0, T);

        let mut saw_send = false;
        let mut saw_disconnect = false;
        for a in actions {
            match a {
                SessionAction::Send(b) => {
                    assert_eq!(decode(&b).msg_type(), "5");
                    saw_send = true;
                }
                SessionAction::Disconnect(_) => saw_disconnect = true,
                _ => {}
            }
        }
        assert!(saw_send && saw_disconnect);
        assert_eq!(s.state(), &SessionState::Disconnected);
    }

    #[test]
    fn send_app_before_logon_fails() {
        let mut s = mk_session();
        let t0 = Instant::now();
        let mut m = Message::new("D");
        m.set(tags::CL_ORD_ID, "x");
        assert!(s.send_app(m, t0, T).is_err());
    }

    #[test]
    fn reset_on_logon_sets_141_and_resets_store() {
        let mut cfg = config();
        cfg.reset_on_logon = true;
        let mut store = InMemorySeqStore::new();
        store.next_send();
        store.next_send();
        assert_eq!(store.peek_send(), 3);
        let mut s = FixSession::new(cfg, store);
        let t0 = Instant::now();
        let action = s.start_logon(t0, T);
        let sent = match action {
            SessionAction::Send(b) => b,
            _ => panic!("expected Send"),
        };
        let m = decode(&sent);
        assert_eq!(m.msg_type(), "A");
        assert_eq!(m.get(TAG_RESET_SEQ_NUM_FLAG), Some("Y"));
        // The store was reset, so the first sent msg carries seq 1.
        assert_eq!(m.get(tags::MSG_SEQ_NUM), Some("1"));
    }

    #[test]
    fn poss_dup_with_stale_seq_is_silently_dropped() {
        // Per FIX 4.4, a PossDupFlag=Y message with MsgSeqNum lower
        // than expected must be discarded: not delivered upward, not
        // used to advance expected_recv, and not cause a disconnect.
        //
        // Regression: prior code fell through to the dispatch match
        // and delivered the stale app message, corrupting the sequence
        // state by advancing past a duplicate.
        let mut s = mk_session();
        let t0 = Instant::now();
        let _ = s.start_logon(t0, T);
        let _ = s.on_message(
            decode(&Message::logon(30).encode("VENUE", "CLIENT", 1, T)),
            t0,
            T,
        );
        // Consume an ExecutionReport at seq=2 normally.
        let mut er = Message::new("8");
        er.set(tags::CL_ORD_ID, "first");
        let bytes_first = er.encode("VENUE", "CLIENT", 2, T);
        let _ = s.on_message(decode(&bytes_first), t0, T);
        let recv_after_first = s.store().expected_recv();
        assert_eq!(recv_after_first, 3);

        // Now peer retransmits seq=2 with PossDupFlag=Y (a replay).
        let mut dup = Message::new("8");
        dup.set(tags::CL_ORD_ID, "first");
        dup.set(TAG_POSS_DUP_FLAG, "Y");
        let dup_bytes = dup.encode("VENUE", "CLIENT", 2, T);
        let actions = s.on_message(decode(&dup_bytes), t0, T);

        // No disconnect, no DeliverApp, no Send — absolute silence.
        assert!(
            actions.is_empty(),
            "expected empty action list for stale PossDup, got {actions:?}"
        );
        // expected_recv must NOT have advanced.
        assert_eq!(
            s.store().expected_recv(),
            recv_after_first,
            "stale PossDup must not advance expected_recv"
        );
        assert_eq!(s.state(), &SessionState::LoggedIn);
    }

    #[test]
    fn seq_num_lower_than_expected_without_poss_dup_disconnects() {
        let mut s = mk_session();
        let t0 = Instant::now();
        let _ = s.start_logon(t0, T);
        let _ = s.on_message(
            decode(&Message::logon(30).encode("VENUE", "CLIENT", 1, T)),
            t0,
            T,
        );

        // Send seq 1 again (already consumed) with no PossDup → error.
        let mut m = Message::new("8");
        m.set(tags::CL_ORD_ID, "stale");
        let bytes = m.encode("VENUE", "CLIENT", 1, T);
        let actions = s.on_message(decode(&bytes), t0, T);
        assert!(actions
            .iter()
            .any(|a| matches!(a, SessionAction::Disconnect(_))));
    }
}
