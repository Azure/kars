use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use dashmap::DashMap;
use uuid::Uuid;
use chrono::{DateTime, Utc, Duration};

use crate::types::{Amid, IceCandidate};

/// ICE negotiation state for P2P connection establishment
#[derive(Debug, Clone)]
pub struct IceSession {
    pub id: Uuid,
    pub initiator: Amid,
    pub responder: Amid,
    pub initiator_sdp: Option<String>,
    pub responder_sdp: Option<String>,
    pub initiator_candidates: Vec<IceCandidate>,
    pub responder_candidates: Vec<IceCandidate>,
    pub state: IceState,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceState {
    /// Waiting for responder's answer
    OfferSent,
    /// Both sides have exchanged offers
    Negotiating,
    /// ICE candidates are being gathered
    GatheringCandidates,
    /// Connection check in progress
    Checking,
    /// P2P connection established
    Connected,
    /// Failed to establish P2P
    Failed,
    /// Session expired
    Expired,
}

/// Manages ICE negotiations for P2P upgrades
pub struct IceManager {
    /// Active ICE sessions by session ID
    sessions: DashMap<Uuid, IceSession>,
    /// Session lookup by peer pair (sorted AMIDs)
    peer_sessions: DashMap<(Amid, Amid), Uuid>,
    /// STUN server addresses
    stun_servers: Vec<String>,
    /// Session timeout
    session_timeout: Duration,
}

impl IceManager {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            peer_sessions: DashMap::new(),
            stun_servers: vec![
                "stun:stun.l.google.com:19302".to_string(),
                "stun:stun1.l.google.com:19302".to_string(),
                "stun:stun.agentmesh.online:3478".to_string(),
            ],
            session_timeout: Duration::seconds(30),
        }
    }

    /// Start a new ICE negotiation
    pub fn start_negotiation(
        &self,
        initiator: Amid,
        responder: Amid,
        offer_sdp: String,
        candidates: Vec<IceCandidate>,
    ) -> IceSession {
        let id = Uuid::new_v4();
        let now = Utc::now();

        let session = IceSession {
            id,
            initiator: initiator.clone(),
            responder: responder.clone(),
            initiator_sdp: Some(offer_sdp),
            responder_sdp: None,
            initiator_candidates: candidates,
            responder_candidates: Vec::new(),
            state: IceState::OfferSent,
            created_at: now,
            expires_at: now + self.session_timeout,
        };

        self.sessions.insert(id, session.clone());

        // Create sorted peer key
        let peer_key = if initiator < responder {
            (initiator, responder)
        } else {
            (responder, initiator)
        };
        self.peer_sessions.insert(peer_key, id);

        session
    }

    /// Handle an ICE answer
    pub fn handle_answer(
        &self,
        session_id: Uuid,
        answer_sdp: String,
        candidates: Vec<IceCandidate>,
    ) -> Option<IceSession> {
        self.sessions.get_mut(&session_id).map(|mut session| {
            session.responder_sdp = Some(answer_sdp);
            session.responder_candidates = candidates;
            session.state = IceState::Negotiating;
            session.clone()
        })
    }

    /// Add ICE candidates to a session
    pub fn add_candidates(
        &self,
        session_id: Uuid,
        from: &Amid,
        candidates: Vec<IceCandidate>,
    ) -> bool {
        if let Some(mut session) = self.sessions.get_mut(&session_id) {
            if from == &session.initiator {
                session.initiator_candidates.extend(candidates);
            } else if from == &session.responder {
                session.responder_candidates.extend(candidates);
            } else {
                return false;
            }

            if session.state == IceState::OfferSent || session.state == IceState::Negotiating {
                session.state = IceState::GatheringCandidates;
            }

            return true;
        }
        false
    }

    /// Mark a session as connected (P2P established)
    pub fn mark_connected(&self, session_id: Uuid) -> bool {
        if let Some(mut session) = self.sessions.get_mut(&session_id) {
            session.state = IceState::Connected;
            return true;
        }
        false
    }

    /// Mark a session as failed
    pub fn mark_failed(&self, session_id: Uuid) -> bool {
        if let Some(mut session) = self.sessions.get_mut(&session_id) {
            session.state = IceState::Failed;
            return true;
        }
        false
    }

    /// Get session by ID
    pub fn get_session(&self, session_id: Uuid) -> Option<IceSession> {
        self.sessions.get(&session_id).map(|s| s.clone())
    }

    /// Get session by peer pair
    pub fn get_session_by_peers(&self, amid1: &Amid, amid2: &Amid) -> Option<IceSession> {
        let peer_key = if amid1 < amid2 {
            (amid1.clone(), amid2.clone())
        } else {
            (amid2.clone(), amid1.clone())
        };

        self.peer_sessions
            .get(&peer_key)
            .and_then(|id| self.sessions.get(&id).map(|s| s.clone()))
    }

    /// Clean up expired sessions
    pub fn cleanup_expired(&self) -> usize {
        let now = Utc::now();
        let mut removed = 0;

        self.sessions.retain(|_, session| {
            if session.expires_at < now {
                // Also remove from peer_sessions
                let peer_key = if session.initiator < session.responder {
                    (session.initiator.clone(), session.responder.clone())
                } else {
                    (session.responder.clone(), session.initiator.clone())
                };
                self.peer_sessions.remove(&peer_key);
                removed += 1;
                false
            } else {
                true
            }
        });

        removed
    }

    /// Get STUN server list
    pub fn get_stun_servers(&self) -> &[String] {
        &self.stun_servers
    }
}

/// Generate a basic SDP offer for WebRTC data channel
pub fn generate_sdp_offer(local_ip: Option<&str>) -> String {
    // This is a simplified SDP template for data channel only
    // In production, this would be generated by a WebRTC library
    let ip = local_ip.unwrap_or("0.0.0.0");

    format!(
        r#"v=0
o=- {} 1 IN IP4 {}
s=AgentMesh P2P Session
t=0 0
a=group:BUNDLE 0
m=application 9 UDP/DTLS/SCTP webrtc-datachannel
c=IN IP4 {}
a=ice-ufrag:{}
a=ice-pwd:{}
a=fingerprint:sha-256 {}
a=setup:actpass
a=mid:0
a=sctp-port:5000
"#,
        Utc::now().timestamp(),
        ip,
        ip,
        generate_ice_ufrag(),
        generate_ice_pwd(),
        generate_fingerprint(),
    )
}

fn generate_ice_ufrag() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..8)
        .map(|_| rng.sample(rand::distributions::Alphanumeric) as char)
        .collect()
}

fn generate_ice_pwd() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..24)
        .map(|_| rng.sample(rand::distributions::Alphanumeric) as char)
        .collect()
}

fn generate_fingerprint() -> String {
    // Placeholder - in production this would be derived from the DTLS certificate
    "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ice_session_lifecycle() {
        let manager = IceManager::new();

        let initiator = "agent1".to_string();
        let responder = "agent2".to_string();

        // Start negotiation
        let session = manager.start_negotiation(
            initiator.clone(),
            responder.clone(),
            "offer_sdp".to_string(),
            vec![],
        );

        assert_eq!(session.state, IceState::OfferSent);

        // Handle answer
        let updated = manager.handle_answer(
            session.id,
            "answer_sdp".to_string(),
            vec![],
        );

        assert!(updated.is_some());
        assert_eq!(updated.unwrap().state, IceState::Negotiating);

        // Mark connected
        assert!(manager.mark_connected(session.id));

        let final_session = manager.get_session(session.id).unwrap();
        assert_eq!(final_session.state, IceState::Connected);
    }
}
