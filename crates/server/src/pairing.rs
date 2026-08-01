//! One-time pairing-code rendezvous.
//!
//! Codes are memory-only and connection-bound. A claim transitions its code exactly
//! once, commits the complete room registry before notification, then reserves both
//! connection FIFOs before enqueueing either `pair_peer` frame.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dashmap::DashMap;

use clipboard_core::crypto::{bundle_fp, room_id};
use clipboard_core::protocol::{Frame, PubBundle};

use crate::registry::{Registry, unix_time_ms};
use crate::room::{ConnectionId, OutboxHandle};

const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";

pub type Connections = DashMap<ConnectionId, OutboxHandle>;

#[derive(Clone, Debug)]
pub struct PairingConfig {
    pub offer_ttl: Duration,
    pub max_offers: usize,
    pub attempts_per_window: u32,
    pub rate_window: Duration,
}

impl Default for PairingConfig {
    fn default() -> Self {
        Self {
            offer_ttl: Duration::from_secs(300),
            max_offers: 100,
            attempts_per_window: 5,
            rate_window: Duration::from_secs(60),
        }
    }
}

pub trait PairingHandler: Send + Sync {
    fn on_pair_frame(
        &self,
        connection_id: ConnectionId,
        client_ip: IpAddr,
        hello_bundle: &PubBundle,
        frame: Frame,
        outbox: &OutboxHandle,
        connections: &Connections,
    ) -> Result<(), Box<Frame>>;

    fn on_disconnect(&self, connection_id: ConnectionId);
}

pub struct PairingUnavailable;

impl PairingHandler for PairingUnavailable {
    fn on_pair_frame(
        &self,
        connection_id: ConnectionId,
        client_ip: IpAddr,
        hello_bundle: &PubBundle,
        frame: Frame,
        outbox: &OutboxHandle,
        connections: &Connections,
    ) -> Result<(), Box<Frame>> {
        let _ = (connection_id, client_ip, hello_bundle, outbox, connections);
        debug_assert!(matches!(
            frame,
            Frame::PairOffer { .. } | Frame::PairClaim { .. }
        ));
        Err(error(
            "pairing_unavailable",
            "pairing is not available on this relay",
        ))
    }

    fn on_disconnect(&self, _connection_id: ConnectionId) {}
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum OfferState {
    Preparing,
    Published,
    Claimed,
    Terminal,
}

struct Offer {
    connection_id: ConnectionId,
    bundle: PubBundle,
    fingerprint: String,
    expires_at: Instant,
    state: OfferState,
}

struct RateEntry {
    window_start: Instant,
    attempts: u32,
}

#[derive(Default)]
struct PairingState {
    offers: HashMap<String, Offer>,
    rates: HashMap<IpAddr, RateEntry>,
}

struct PairContext<'a> {
    connection_id: ConnectionId,
    hello_bundle: &'a PubBundle,
    outbox: &'a OutboxHandle,
    connections: &'a Connections,
    now: Instant,
}

pub struct PairingRelay {
    registry: Arc<dyn Registry>,
    config: PairingConfig,
    state: Mutex<PairingState>,
}

impl PairingRelay {
    #[must_use]
    pub fn new(registry: Arc<dyn Registry>, config: PairingConfig) -> Self {
        Self {
            registry,
            config,
            state: Mutex::new(PairingState::default()),
        }
    }

    #[must_use]
    pub fn is_published(&self, code: &str) -> bool {
        self.state
            .lock()
            .expect("pairing mutex poisoned")
            .offers
            .get(code)
            .is_some_and(|offer| {
                offer.state == OfferState::Published && Instant::now() < offer.expires_at
            })
    }

    fn allow_attempt(&self, state: &mut PairingState, ip: IpAddr, now: Instant) -> bool {
        let entry = state.rates.entry(ip).or_insert(RateEntry {
            window_start: now,
            attempts: 0,
        });
        if now.duration_since(entry.window_start) >= self.config.rate_window {
            entry.window_start = now;
            entry.attempts = 0;
        }
        if entry.attempts >= self.config.attempts_per_window {
            return false;
        }
        entry.attempts += 1;
        true
    }

    fn on_offer(
        &self,
        state: &mut PairingState,
        context: &PairContext<'_>,
        code: String,
        bundle: PubBundle,
    ) -> Result<(), Box<Frame>> {
        if !valid_code(&code) {
            return Err(error(
                "bad_code",
                "pairing code must use the six-character alphabet",
            ));
        }
        if &bundle != context.hello_bundle {
            return Err(error("bad_auth", "pairing bundle does not match hello"));
        }
        state
            .offers
            .retain(|_, offer| context.now < offer.expires_at);
        if state.offers.contains_key(&code) {
            return Err(error("code_in_use", "pairing code collision"));
        }
        if state.offers.len() >= self.config.max_offers {
            return Err(error("server_full", "pairing offer limit exceeded"));
        }
        if !context.connections.contains_key(&context.connection_id) {
            return Err(error("peer_unavailable", "pairing connection is stale"));
        }
        state.offers.insert(
            code.clone(),
            Offer {
                connection_id: context.connection_id,
                fingerprint: bundle_fp(&bundle),
                bundle,
                expires_at: context.now + self.config.offer_ttl,
                state: OfferState::Preparing,
            },
        );
        if !context.outbox.send_frame(&Frame::PairOfferOk) {
            state.offers.get_mut(&code).expect("offer inserted").state = OfferState::Terminal;
            return Err(error("peer_unavailable", "offerer outbound queue is full"));
        }
        state.offers.get_mut(&code).expect("offer inserted").state = OfferState::Published;
        Ok(())
    }

    fn on_claim(
        &self,
        state: &mut PairingState,
        context: &PairContext<'_>,
        code: String,
        bundle: PubBundle,
    ) -> Result<(), Box<Frame>> {
        if !valid_code(&code) {
            return Err(error(
                "bad_code",
                "pairing code must use the six-character alphabet",
            ));
        }
        let Some(offer) = state.offers.get_mut(&code) else {
            return Err(error("code_expired", "pairing code is unavailable"));
        };
        if context.now >= offer.expires_at || offer.state != OfferState::Published {
            offer.state = OfferState::Terminal;
            return Err(error("code_expired", "pairing code is unavailable"));
        }
        offer.state = OfferState::Claimed;
        let offer_connection_id = offer.connection_id;
        let offer_bundle = offer.bundle.clone();
        let offer_fp = offer.fingerprint.clone();

        let fail = |state: &mut PairingState, code: &str, frame: Box<Frame>| {
            if let Some(offer) = state.offers.get_mut(code) {
                offer.state = OfferState::Terminal;
            }
            Err(frame)
        };
        if &bundle != context.hello_bundle {
            return fail(
                state,
                &code,
                error("bad_auth", "pairing bundle does not match hello"),
            );
        }
        let claim_fp = bundle_fp(&bundle);
        if claim_fp == offer_fp {
            return fail(
                state,
                &code,
                error("same_identity", "an identity cannot pair with itself"),
            );
        }
        let Some(offer_outbox) = context
            .connections
            .get(&offer_connection_id)
            .map(|entry| entry.clone())
        else {
            return fail(
                state,
                &code,
                error("peer_unavailable", "offerer disconnected"),
            );
        };
        let room = room_id(&offer_bundle, &bundle);
        let members = [offer_fp, claim_fp];
        if self
            .registry
            .commit_room(&room, &members, unix_time_ms())
            .is_err()
        {
            return fail(
                state,
                &code,
                error("registry_failed", "room registry commit failed"),
            );
        }

        let offer_peer = Frame::PairPeer {
            peer_pub_bundle: bundle,
        };
        let claim_peer = Frame::PairPeer {
            peer_pub_bundle: offer_bundle,
        };
        let Some(offer_reservation) = offer_outbox.try_reserve_frame(&offer_peer) else {
            return fail(
                state,
                &code,
                error("peer_unavailable", "offerer outbound queue is full"),
            );
        };
        let Some(claim_reservation) = context.outbox.try_reserve_frame(&claim_peer) else {
            drop(offer_reservation);
            return fail(
                state,
                &code,
                error("peer_unavailable", "claimer outbound queue is full"),
            );
        };
        offer_reservation.send();
        claim_reservation.send();
        state
            .offers
            .get_mut(&code)
            .expect("claimed offer remains")
            .state = OfferState::Terminal;
        Ok(())
    }
}

impl PairingHandler for PairingRelay {
    fn on_pair_frame(
        &self,
        connection_id: ConnectionId,
        client_ip: IpAddr,
        hello_bundle: &PubBundle,
        frame: Frame,
        outbox: &OutboxHandle,
        connections: &Connections,
    ) -> Result<(), Box<Frame>> {
        let now = Instant::now();
        let mut state = self.state.lock().expect("pairing mutex poisoned");
        if !self.allow_attempt(&mut state, client_ip, now) {
            if let Frame::PairClaim { code, .. } = &frame
                && let Some(offer) = state.offers.get_mut(code)
            {
                offer.state = OfferState::Terminal;
            }
            return Err(error("rate_limited", "pairing attempts exceeded"));
        }
        let context = PairContext {
            connection_id,
            hello_bundle,
            outbox,
            connections,
            now,
        };
        match frame {
            Frame::PairOffer { code, pub_bundle } => {
                self.on_offer(&mut state, &context, code, pub_bundle)
            }
            Frame::PairClaim { code, pub_bundle } => {
                self.on_claim(&mut state, &context, code, pub_bundle)
            }
            _ => Err(error("bad_frame", "expected a pairing frame")),
        }
    }

    fn on_disconnect(&self, connection_id: ConnectionId) {
        let mut state = self.state.lock().expect("pairing mutex poisoned");
        for offer in state.offers.values_mut() {
            if offer.connection_id == connection_id && offer.state != OfferState::Terminal {
                offer.state = OfferState::Terminal;
            }
        }
    }
}

fn valid_code(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|byte| CODE_ALPHABET.contains(&byte))
}

fn error(code: &str, message: &str) -> Box<Frame> {
    Box::new(Frame::Error {
        code: code.to_owned(),
        message: message.to_owned(),
    })
}
