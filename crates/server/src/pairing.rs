//! Pairing offer/claim seam.
//!
//! The connection state machine accepts `pair_offer`/`pair_claim` after `hello_ok`
//! (and rejects any pairing frame after `join` with `bad_frame`). The pairing
//! exchange itself — pair codes with 300s TTL, the peer rendezvous, and registry
//! room creation under the 100-room quota — is owned by T8 behind this seam.

use dashmap::DashMap;

use clipboard_core::protocol::Frame;

use crate::room::{ConnectionId, OutboxHandle};

/// Live connection outboxes, keyed by connection id, so a pairing implementation can
/// reach the peer connection of an in-progress pairing.
pub type Connections = DashMap<ConnectionId, OutboxHandle>;

/// Handles pairing frames for connections in the pre-join `Ready` state.
pub trait PairingHandler: Send + Sync {
    /// Handles one `pair_offer`/`pair_claim` frame. Implementations reply through
    /// `outbox` and may reach the peer via `connections`. Returning `Err` sends the
    /// error frame and closes the connection.
    fn on_pair_frame(
        &self,
        connection_id: ConnectionId,
        frame: Frame,
        outbox: &OutboxHandle,
        connections: &Connections,
    ) -> Result<(), Box<Frame>>;
}

/// Default T6 implementation: pairing arrives with T8.
pub struct PairingUnavailable;

impl PairingHandler for PairingUnavailable {
    fn on_pair_frame(
        &self,
        connection_id: ConnectionId,
        frame: Frame,
        outbox: &OutboxHandle,
        connections: &Connections,
    ) -> Result<(), Box<Frame>> {
        let _ = (connection_id, outbox, connections);
        debug_assert!(matches!(
            frame,
            Frame::PairOffer { .. } | Frame::PairClaim { .. }
        ));
        Err(Box::new(Frame::Error {
            code: "pairing_unavailable".to_owned(),
            message: "pairing is not available on this relay".to_owned(),
        }))
    }
}
