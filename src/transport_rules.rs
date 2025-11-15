/// Transport Rule Engine for RDP Multitransport
///
/// This module implements a transparent rule-based system for managing:
/// - UDP tunnel lifecycle (establishment, ready, active, closing)
/// - Soft-Sync negotiation and channel switching
/// - Transport routing decisions (TCP vs UDP)
///
/// The goal is to eliminate complex state machine logic scattered across the codebase
/// and replace it with clear, testable rules.
///
/// ## Integration with rdp.rs
///
/// ```text
/// 1. Initialize:
///    let mut transport_rules = TransportRules::new();
///
/// 2. When MultitransportRequest received:
///    transport_rules.register_tunnel(tunnel_id, tunnel_type);
///    transport_rules.transition_tunnel(tunnel_id, TunnelState::Requested);
///
/// 3. As UDP handshake progresses:
///    UdpTransportEvent::Connected → TunnelState::Connected
///    UdpTransportEvent::HandshakeComplete → TunnelState::TlsReady
///    UdpTransportEvent::TunnelEstablished → TunnelState::Established
///
/// 4. When Soft-Sync completed:
///    UdpTransportEvent::SoftSyncCompleted → TunnelState::SoftSyncComplete
///
/// 5. For incoming tunnel data:
///    let transport = transport_rules.route_incoming_tunnel_data(tunnel_id);
///    drdynvc.process(&data, transport.to_context());
///
/// 6. For outgoing channel data (future):
///    match transport_rules.route_channel(channel_id) {
///        TransportRoute::Tcp => send_via_tcp(msg),
///        TransportRoute::Udp(id) => send_via_udp(id, msg),
///    }
/// ```
///
/// ## Rules
///
/// **RULE 1**: Tunnel can carry DVC data only after `SoftSyncComplete`
/// **RULE 2**: Channels use TCP until explicitly switched via Soft-Sync
/// **RULE 3**: Incoming tunnel data uses UDP context only if tunnel is `SoftSyncComplete`
/// **RULE 4**: Outgoing responses inherit request transport (handled by DRDYNVC)

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

// Re-export TransportContext for external use
pub use ironrdp::svc::TransportContext;

/// Unique identifier for a tunnel (the request_id from MultitransportRequest)
pub type TunnelId = u32;

/// Unique identifier for a DVC channel
pub type ChannelId = u32;

/// Tunnel type as defined by MS-RDPEMT (0x00000001 = UDPFECR reliable)
pub type TunnelType = u32;

/// State of a UDP tunnel
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelState {
    /// Initial state: MultitransportRequest received from server
    Requested,
    /// UDP socket connected, handshake (SYN/SYN+ACK) complete
    Connected,
    /// TLS/DTLS handshake complete (if Enhanced RDP Security)
    TlsReady,
    /// Tunnel CREATE request/response complete, ready to carry data
    Established,
    /// Soft-Sync negotiation completed for this tunnel
    SoftSyncComplete,
    /// Tunnel is closing or closed
    Closed,
}

/// Represents a UDP tunnel and its lifecycle state
#[derive(Debug, Clone)]
pub struct Tunnel {
    pub id: TunnelId,
    pub tunnel_type: TunnelType,
    pub state: TunnelState,
    pub established_at: Option<Instant>,
    pub soft_sync_at: Option<Instant>,
    /// Channels that have been switched to this tunnel via Soft-Sync
    pub channels: BTreeSet<ChannelId>,
}

impl Tunnel {
    pub fn new(id: TunnelId, tunnel_type: TunnelType) -> Self {
        Self {
            id,
            tunnel_type,
            state: TunnelState::Requested,
            established_at: None,
            soft_sync_at: None,
            channels: BTreeSet::new(),
        }
    }

    /// Advance tunnel to next state
    pub fn transition(&mut self, new_state: TunnelState) {
        let old_state = self.state;
        self.state = new_state;
        
        match new_state {
            TunnelState::Established => {
                self.established_at = Some(Instant::now());
                info!("🔧 Tunnel {} transitioned: {:?} → {:?}", self.id, old_state, new_state);
            }
            TunnelState::SoftSyncComplete => {
                self.soft_sync_at = Some(Instant::now());
                info!("✅ Tunnel {} transitioned: {:?} → {:?} ({} channels)", 
                      self.id, old_state, new_state, self.channels.len());
            }
            _ => {
                debug!("🔄 Tunnel {} transitioned: {:?} → {:?}", self.id, old_state, new_state);
            }
        }
    }

    /// Check if tunnel is ready to carry DVC data (after Soft-Sync)
    pub fn can_carry_dvc_data(&self) -> bool {
        matches!(self.state, TunnelState::SoftSyncComplete)
    }

    /// Check if tunnel can participate in Soft-Sync negotiation
    pub fn can_soft_sync(&self) -> bool {
        matches!(self.state, TunnelState::Established)
    }
}

/// Channel routing information
#[derive(Debug, Clone)]
pub struct ChannelRoute {
    pub channel_id: ChannelId,
    pub channel_name: String,
    /// If Some, channel has been switched to UDP tunnel via Soft-Sync
    pub udp_tunnel: Option<TunnelId>,
    /// When the channel was switched to UDP
    pub switched_at: Option<Instant>,
}

impl ChannelRoute {
    pub fn new(channel_id: ChannelId, channel_name: String) -> Self {
        Self {
            channel_id,
            channel_name,
            udp_tunnel: None,
            switched_at: None,
        }
    }

    /// Check if channel should use TCP
    pub fn uses_tcp(&self) -> bool {
        self.udp_tunnel.is_none()
    }

    /// Check if channel should use UDP tunnel
    pub fn uses_udp(&self, tunnel_id: TunnelId) -> bool {
        self.udp_tunnel == Some(tunnel_id)
    }

    /// Switch channel to UDP tunnel
    pub fn switch_to_udp(&mut self, tunnel_id: TunnelId) {
        self.udp_tunnel = Some(tunnel_id);
        self.switched_at = Some(Instant::now());
        info!("🔀 Channel {} ('{}') switched to UDP tunnel {}", 
              self.channel_id, self.channel_name, tunnel_id);
    }
}

/// Transport Rule Engine
///
/// Central authority for all transport routing decisions
pub struct TransportRules {
    tunnels: BTreeMap<TunnelId, Tunnel>,
    channels: BTreeMap<ChannelId, ChannelRoute>,
    /// Timeout for Soft-Sync to complete after tunnel establishment
    soft_sync_timeout: Duration,
}

impl TransportRules {
    pub fn new() -> Self {
        Self {
            tunnels: BTreeMap::new(),
            channels: BTreeMap::new(),
            soft_sync_timeout: Duration::from_secs(10),
        }
    }

    /// Register a new tunnel (when MultitransportRequest received)
    pub fn register_tunnel(&mut self, id: TunnelId, tunnel_type: TunnelType) {
        let tunnel = Tunnel::new(id, tunnel_type);
        info!("📝 Registered tunnel id={} type=0x{:08x}", id, tunnel_type);
        self.tunnels.insert(id, tunnel);
    }

    /// Register a DVC channel
    pub fn register_channel(&mut self, channel_id: ChannelId, channel_name: String) {
        let route = ChannelRoute::new(channel_id, channel_name.clone());
        debug!("📝 Registered channel id={} name='{}'", channel_id, channel_name);
        self.channels.insert(channel_id, route);
    }

    /// Update tunnel state
    pub fn transition_tunnel(&mut self, tunnel_id: TunnelId, new_state: TunnelState) {
        if let Some(tunnel) = self.tunnels.get_mut(&tunnel_id) {
            tunnel.transition(new_state);
        } else {
            warn!("⚠️  Attempted to transition unknown tunnel {}", tunnel_id);
        }
    }

    /// Process Soft-Sync request from server
    ///
    /// Returns: List of (channel_id, tunnel_id) pairs that should be switched
    pub fn process_soft_sync_request(
        &mut self,
        tunnel_type: TunnelType,
        channel_ids: &[ChannelId],
    ) -> Vec<(ChannelId, TunnelId)> {
        info!("🔄 Processing Soft-Sync request: type=0x{:08x}, {} channels", 
              tunnel_type, channel_ids.len());

        // Find tunnel with matching type that's ready for Soft-Sync
        let tunnel_id = self.tunnels.iter()
            .find(|(_, t)| t.tunnel_type == tunnel_type && t.can_soft_sync())
            .map(|(id, _)| *id);

        let Some(tunnel_id) = tunnel_id else {
            warn!("⚠️  No tunnel available for Soft-Sync type=0x{:08x}", tunnel_type);
            return Vec::new();
        };

        let mut switched = Vec::new();

        // Switch requested channels to UDP
        for &channel_id in channel_ids {
            if let Some(route) = self.channels.get_mut(&channel_id) {
                route.switch_to_udp(tunnel_id);
                switched.push((channel_id, tunnel_id));

                // Track channel in tunnel
                if let Some(tunnel) = self.tunnels.get_mut(&tunnel_id) {
                    tunnel.channels.insert(channel_id);
                }
            } else {
                warn!("⚠️  Soft-Sync requested unknown channel {}", channel_id);
            }
        }

        // Mark tunnel as Soft-Sync complete
        if !switched.is_empty() {
            self.transition_tunnel(tunnel_id, TunnelState::SoftSyncComplete);
        }

        info!("✅ Soft-Sync complete: {} channels switched to tunnel {}", 
              switched.len(), tunnel_id);

        switched
    }

    /// Determine which transport to use for a channel
    ///
    /// RULE: Use UDP if:
    /// 1. Channel has been switched via Soft-Sync, AND
    /// 2. The tunnel is in SoftSyncComplete state
    /// Otherwise: Use TCP
    pub fn route_channel(&self, channel_id: ChannelId) -> TransportRoute {
        let Some(route) = self.channels.get(&channel_id) else {
            // Unknown channel defaults to TCP
            return TransportRoute::Tcp;
        };

        let Some(tunnel_id) = route.udp_tunnel else {
            // Channel not switched to UDP
            return TransportRoute::Tcp;
        };

        let Some(tunnel) = self.tunnels.get(&tunnel_id) else {
            // Tunnel doesn't exist anymore, fall back to TCP
            warn!("⚠️  Channel {} routed to non-existent tunnel {}, using TCP", 
                  channel_id, tunnel_id);
            return TransportRoute::Tcp;
        };

        if tunnel.can_carry_dvc_data() {
            TransportRoute::Udp(tunnel_id)
        } else {
            // Tunnel not ready yet, use TCP
            debug!("🔄 Tunnel {} not ready (state={:?}), using TCP for channel {}", 
                   tunnel_id, tunnel.state, channel_id);
            TransportRoute::Tcp
        }
    }

    /// Determine which transport to use for incoming DVC data
    ///
    /// RULE: When processing data from a tunnel:
    /// - If tunnel has completed Soft-Sync, tag responses with UdpTunnel(id)
    /// - Otherwise, tag with Tcp (during handshake phase)
    pub fn route_incoming_tunnel_data(&self, tunnel_id: TunnelId) -> TransportRoute {
        let Some(tunnel) = self.tunnels.get(&tunnel_id) else {
            return TransportRoute::Tcp;
        };

        if tunnel.can_carry_dvc_data() {
            TransportRoute::Udp(tunnel_id)
        } else {
            TransportRoute::Tcp
        }
    }

    /// Check if any tunnels have timed out waiting for Soft-Sync
    pub fn check_soft_sync_timeouts(&self) -> Vec<TunnelId> {
        let mut timed_out = Vec::new();
        let now = Instant::now();

        for (&id, tunnel) in &self.tunnels {
            if tunnel.state == TunnelState::Established {
                if let Some(established_at) = tunnel.established_at {
                    if now.duration_since(established_at) > self.soft_sync_timeout {
                        timed_out.push(id);
                    }
                }
            }
        }

        timed_out
    }

    /// Get tunnel state
    pub fn get_tunnel(&self, tunnel_id: TunnelId) -> Option<&Tunnel> {
        self.tunnels.get(&tunnel_id)
    }

    /// Get all tunnels in SoftSyncComplete state
    pub fn active_tunnels(&self) -> impl Iterator<Item = (&TunnelId, &Tunnel)> {
        self.tunnels.iter()
            .filter(|(_, t)| t.state == TunnelState::SoftSyncComplete)
    }

    /// Get statistics
    pub fn stats(&self) -> TransportStats {
        let tcp_channels = self.channels.values().filter(|r| r.uses_tcp()).count();
        let udp_channels = self.channels.len() - tcp_channels;
        let active_tunnels = self.active_tunnels().count();

        TransportStats {
            total_tunnels: self.tunnels.len(),
            active_tunnels,
            total_channels: self.channels.len(),
            tcp_channels,
            udp_channels,
        }
    }
}

impl Default for TransportRules {
    fn default() -> Self {
        Self::new()
    }
}

/// Transport routing decision
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportRoute {
    Tcp,
    Udp(TunnelId),
}

impl TransportRoute {
    /// Convert to ironrdp TransportContext
    pub fn to_context(self) -> TransportContext {
        match self {
            TransportRoute::Tcp => TransportContext::Tcp,
            TransportRoute::Udp(id) => TransportContext::UdpTunnel(id),
        }
    }
}

/// Transport statistics
#[derive(Debug, Clone, Copy)]
pub struct TransportStats {
    pub total_tunnels: usize,
    pub active_tunnels: usize,
    pub total_channels: usize,
    pub tcp_channels: usize,
    pub udp_channels: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tunnel_lifecycle() {
        let mut rules = TransportRules::new();
        
        // Register tunnel
        rules.register_tunnel(1, 0x00000001);
        
        // Initially in Requested state
        assert_eq!(rules.get_tunnel(1).unwrap().state, TunnelState::Requested);
        assert!(!rules.get_tunnel(1).unwrap().can_soft_sync());
        
        // Progress through states
        rules.transition_tunnel(1, TunnelState::Connected);
        rules.transition_tunnel(1, TunnelState::TlsReady);
        rules.transition_tunnel(1, TunnelState::Established);
        
        // Can now participate in Soft-Sync
        assert!(rules.get_tunnel(1).unwrap().can_soft_sync());
        assert!(!rules.get_tunnel(1).unwrap().can_carry_dvc_data());
    }

    #[test]
    fn test_soft_sync_switches_channels() {
        let mut rules = TransportRules::new();
        
        // Setup: tunnel established, channels registered
        rules.register_tunnel(1, 0x00000001);
        rules.transition_tunnel(1, TunnelState::Established);
        rules.register_channel(10, "gfx".to_string());
        rules.register_channel(11, "input".to_string());
        
        // Process Soft-Sync
        let switched = rules.process_soft_sync_request(0x00000001, &[10, 11]);
        
        assert_eq!(switched.len(), 2);
        assert!(rules.get_tunnel(1).unwrap().can_carry_dvc_data());
        assert_eq!(rules.get_tunnel(1).unwrap().channels.len(), 2);
    }

    #[test]
    fn test_routing_rules() {
        let mut rules = TransportRules::new();
        
        // Setup
        rules.register_tunnel(1, 0x00000001);
        rules.register_channel(10, "gfx".to_string());
        
        // Before Soft-Sync: TCP
        assert_eq!(rules.route_channel(10), TransportRoute::Tcp);
        
        // After tunnel established but before Soft-Sync: still TCP
        rules.transition_tunnel(1, TunnelState::Established);
        assert_eq!(rules.route_channel(10), TransportRoute::Tcp);
        
        // After Soft-Sync: UDP
        rules.process_soft_sync_request(0x00000001, &[10]);
        assert_eq!(rules.route_channel(10), TransportRoute::Udp(1));
    }

    #[test]
    fn test_unknown_channel_defaults_to_tcp() {
        let rules = TransportRules::new();
        assert_eq!(rules.route_channel(999), TransportRoute::Tcp);
    }
}
