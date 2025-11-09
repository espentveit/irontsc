use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use core::any::TypeId;
use core::fmt;

use ironrdp_core::{impl_as_any, Decode as _, DecodeResult, ReadCursor};
use ironrdp_pdu::{self as pdu, decode_err, encode_err, pdu_other_err};
use ironrdp_svc::{
    ChannelFlags, CompressionCondition, SvcClientProcessor, SvcMessage, SvcProcessor, TransportContext,
};
use pdu::gcc::ChannelName;
use pdu::PduResult;
use tracing::{debug, warn};

use crate::pdu::{
    CapabilitiesResponsePdu, CapsVersion, ClosePdu, CreateResponsePdu, CreationStatus,
    DrdynvcClientPdu, DrdynvcServerPdu, SoftSyncRequestPdu, SoftSyncResponsePdu,
};
use crate::{encode_dvc_messages, DvcProcessor, DynamicChannelSet, DynamicVirtualChannel};

pub trait DvcClientProcessor: DvcProcessor {}

/// DRDYNVC Static Virtual Channel (the Remote Desktop Protocol: Dynamic Virtual Channel Extension)
///
/// It adds support for dynamic virtual channels (DVC).
pub struct DrdynvcClient {
    dynamic_channels: DynamicChannelSet,
    /// Indicates whether the capability request/response handshake has been completed.
    cap_handshake_done: bool,
    /// Indicates whether soft-sync to UDP has been completed for a tunnel
    soft_sync_completed_tunnel: Option<u32>,
    /// Maps channel IDs to their UDP tunnel type (if they've been switched via Soft-Sync)
    /// Channels not in this map use TCP transport
    udp_channels: BTreeMap<u32, u32>,
}

impl fmt::Debug for DrdynvcClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DrdynvcClient([")?;

        for (i, channel) in self.dynamic_channels.values().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", channel.channel_name())?;
        }

        write!(f, "])")
    }
}

impl DrdynvcClient {
    pub const NAME: ChannelName = ChannelName::from_static(b"drdynvc\0");

    pub fn new() -> Self {
        Self {
            dynamic_channels: DynamicChannelSet::new(),
            cap_handshake_done: false,
            soft_sync_completed_tunnel: None,
            udp_channels: BTreeMap::new(),
        }
    }

    // FIXME(#61): it's likely we want to enable adding dynamic channels at any point during the session (message passing? other approach?)

    #[must_use]
    pub fn with_dynamic_channel<T>(mut self, channel: T) -> Self
    where
        T: DvcProcessor + 'static,
    {
        self.dynamic_channels.insert(channel);
        self
    }

    pub fn attach_dynamic_channel<T>(&mut self, channel: T)
    where
        T: DvcProcessor + 'static,
    {
        self.dynamic_channels.insert(channel);
    }

    /// Returns the tunnel type if soft-sync has been completed
    pub fn soft_sync_completed(&self) -> Option<u32> {
        self.soft_sync_completed_tunnel
    }

    /// Process DVC data with transport context.
    /// This allows the processor to tag response messages with the appropriate transport
    /// (TCP or UDP tunnel) so they can be routed correctly.
    pub fn process_with_transport(
        &mut self,
        payload: &[u8],
        transport: ironrdp_svc::TransportContext,
    ) -> PduResult<Vec<SvcMessage>> {
        let pdu = decode_dvc_message(payload).map_err(|e| decode_err!(e))?;
        let mut responses = Vec::new();

        match pdu {
            DrdynvcServerPdu::Capabilities(caps_request) => {
                debug!("Got DVC Capabilities Request PDU: {caps_request:?}");
                let server_version = Self::get_caps_version(&caps_request);
                // Capabilities always use TCP per MS-RDPEDYC - initial handshake
                responses.push(self.create_capabilities_response(server_version));
            }
            DrdynvcServerPdu::Create(create_request) => {
                debug!("Got DVC Create Request PDU: {create_request:?}");
                let channel_name = create_request.channel_name;
                let channel_id = create_request.channel_id;

                if !self.cap_handshake_done {
                    debug!(
                        "Got DVC Create Request PDU before a Capabilities Request PDU. \
                        Sending Capabilities Response PDU before the Create Response PDU."
                    );
                    // Server didn't send Caps Request, assume V1 for compatibility
                    responses.push(self.create_capabilities_response(CapsVersion::V1));
                }

                let channel_exists = self
                    .dynamic_channels
                    .get_by_channel_name(&channel_name)
                    .is_some();
                let (creation_status, start_messages) = if channel_exists {
                    // If we have a handler for this channel, attach the channel ID
                    // and get any start messages.
                    self.dynamic_channels
                        .attach_channel_id(channel_name.clone(), channel_id);
                    let dynamic_channel = self
                        .dynamic_channels
                        .get_by_channel_name_mut(&channel_name)
                        .expect("channel exists");
                    (CreationStatus::OK, dynamic_channel.start()?)
                } else {
                    (CreationStatus::NO_LISTENER, Vec::new())
                };

                let create_response =
                    DrdynvcClientPdu::Create(CreateResponsePdu::new(channel_id, creation_status));
                debug!("Send DVC Create Response PDU: {create_response:?}");
                // Per MS-RDPEDYC: "The server DVC manager sends the Create Request PDU over 
                // the selected transport, and the client responds by sending the Create Response 
                // PDU back to the server over the same transport."
                responses.push(SvcMessage::from(create_response).with_transport(transport));

                // If this DVC has start messages, send them over the same transport as the request
                if !start_messages.is_empty() {
                    responses.extend(
                        encode_dvc_messages(channel_id, start_messages, ChannelFlags::empty())
                            .map_err(|e| encode_err!(e))?
                            .into_iter()
                            .map(|msg| msg.with_transport(transport))
                    );
                }
            }
            DrdynvcServerPdu::Close(close_request) => {
                debug!("Got DVC Close Request PDU: {close_request:?}");
                self.dynamic_channels
                    .remove_by_channel_id(close_request.channel_id);

                let close_response =
                    DrdynvcClientPdu::Close(ClosePdu::new(close_request.channel_id));

                debug!("Send DVC Close Response PDU: {close_response:?}");
                // Use same transport as request (per general RDP principle)
                responses.push(SvcMessage::from(close_response).with_transport(transport));
            }
            DrdynvcServerPdu::Data(data) => {
                let channel_id = data.channel_id();

                let messages = self
                    .dynamic_channels
                    .get_by_channel_id_mut(channel_id)
                    .ok_or_else(|| pdu_other_err!("access to non existing DVC channel"))?
                    .process(data)?;

                // Determine transport based on whether this channel was switched to UDP via Soft-Sync
                // If the channel is in udp_channels map, use UDP tunnel; otherwise use TCP
                let response_transport = if let Some(&tunnel_type) = self.udp_channels.get(&channel_id) {
                    // This channel uses UDP - map tunnel_type to request_id
                    // Note: In current implementation, tunnel_type == request_id for the UDP tunnel
                    TransportContext::UdpTunnel(tunnel_type)
                } else {
                    // This channel stays on TCP (e.g., input, sound)
                    TransportContext::Tcp
                };

                // Data messages use the transport assigned to this specific channel
                responses.extend(
                    encode_dvc_messages(channel_id, messages, ChannelFlags::empty())
                        .map_err(|e| encode_err!(e))?
                        .into_iter()
                        .map(|msg| msg.with_transport(response_transport))
                );
            }
            DrdynvcServerPdu::SoftSyncRequest(request) => {
                debug!("Got DVC SoftSync Request PDU: {request:?}");
                self.handle_soft_sync_request_with_transport(&request, &mut responses, transport)?;
            }
            DrdynvcServerPdu::SoftSyncResponse(response) => {
                debug!("Got DVC SoftSync Response PDU: {response:?}");
                // Server confirmed that channels have been switched to UDP
                // Store the first tunnel type for notification
                if let Some(&tunnel_type) = response.tunnels_to_switch.first() {
                    debug!(
                        "Server confirmed soft-sync for tunnel_type=0x{:08X}",
                        tunnel_type
                    );
                    self.soft_sync_completed_tunnel = Some(tunnel_type);
                }
            }
        }

        Ok(responses)
    }

    /// Clears the soft-sync completion status (used after handling the event)
    pub fn clear_soft_sync_completed(&mut self) {
        self.soft_sync_completed_tunnel = None;
    }

    pub fn get_dvc_by_type_id<T>(&self) -> Option<&DynamicVirtualChannel>
    where
        T: DvcProcessor,
    {
        self.dynamic_channels.get_by_type_id(TypeId::of::<T>())
    }

    pub fn get_dvc_by_type_id_mut<T>(&mut self) -> Option<&mut DynamicVirtualChannel>
    where
        T: DvcProcessor,
    {
        self.dynamic_channels.get_by_type_id_mut(TypeId::of::<T>())
    }

    pub fn get_dvc_by_channel_id(&self, channel_id: u32) -> Option<&DynamicVirtualChannel> {
        self.dynamic_channels.get_by_channel_id(channel_id)
    }

    fn create_capabilities_response(&mut self, server_version: CapsVersion) -> SvcMessage {
        // Per MS-RDPEDYC §2.2.1.2, client MUST respond with the version level it supports
        // We support up to V3, but must negotiate with server's requested version
        const CLIENT_MAX_VERSION: CapsVersion = CapsVersion::V3;
        
        // Negotiate: use minimum of client max and server requested
        let negotiated_version = match (CLIENT_MAX_VERSION, server_version) {
            (CapsVersion::V3, CapsVersion::V3) => CapsVersion::V3,
            (CapsVersion::V3, CapsVersion::V2) => CapsVersion::V2,
            (CapsVersion::V3, CapsVersion::V1) => CapsVersion::V1,
            (CapsVersion::V2, CapsVersion::V3) => CapsVersion::V2,
            (CapsVersion::V2, CapsVersion::V2) => CapsVersion::V2,
            (CapsVersion::V2, CapsVersion::V1) => CapsVersion::V1,
            (CapsVersion::V1, _) => CapsVersion::V1,
        };
        
        let caps_response =
            DrdynvcClientPdu::Capabilities(CapabilitiesResponsePdu::new(negotiated_version));
        debug!(
            "Send DVC Capabilities Response PDU: {caps_response:?} (server requested {:?}, client max {:?}, negotiated {:?})",
            server_version, CLIENT_MAX_VERSION, negotiated_version
        );
        self.cap_handshake_done = true;
        SvcMessage::from(caps_response)
    }
    
    /// Extract the version from a CapabilitiesRequestPdu
    fn get_caps_version(caps_request: &crate::pdu::CapabilitiesRequestPdu) -> CapsVersion {
        match caps_request {
            crate::pdu::CapabilitiesRequestPdu::V1 { .. } => CapsVersion::V1,
            crate::pdu::CapabilitiesRequestPdu::V2 { .. } => CapsVersion::V2,
            crate::pdu::CapabilitiesRequestPdu::V3 { .. } => CapsVersion::V3,
        }
    }

    fn handle_soft_sync_request(
        &mut self,
        request: &SoftSyncRequestPdu,
        responses: &mut Vec<SvcMessage>,
    ) -> PduResult<()> {
        self.handle_soft_sync_request_internal(request, responses, None)
    }

    fn handle_soft_sync_request_with_transport(
        &mut self,
        request: &SoftSyncRequestPdu,
        responses: &mut Vec<SvcMessage>,
        transport: ironrdp_svc::TransportContext,
    ) -> PduResult<()> {
        self.handle_soft_sync_request_internal(request, responses, Some(transport))
    }

    fn handle_soft_sync_request_internal(
        &mut self,
        request: &SoftSyncRequestPdu,
        responses: &mut Vec<SvcMessage>,
        transport: Option<ironrdp_svc::TransportContext>,
    ) -> PduResult<()> {
        debug!(
            "📥 Received Soft-Sync Request from server: {} tunnel(s)",
            request.tunnels.len()
        );

        // Per MS-RDPEDYC §2.2.5.1, validate SOFT_SYNC_TCP_FLUSHED flag
        // "This flag MUST be set to indicate no more data will be sent over TCP for the specified DVCs"
        if (request.flags & crate::pdu::SOFT_SYNC_TCP_FLUSHED) == 0 {
            warn!(
                "⚠️  Server Soft-Sync Request missing SOFT_SYNC_TCP_FLUSHED flag (flags=0x{:04X})",
                request.flags
            );
            warn!("   Per MS-RDPEDYC §2.2.5.1, this flag MUST be set - continuing anyway");
        }

        for tunnel in &request.tunnels {
            debug!(
                "   Tunnel type=0x{:08X}, {} channel(s)",
                tunnel.tunnel_type,
                tunnel.channel_ids.len()
            );
            for &channel_id in &tunnel.channel_ids {
                // Mark this channel as using UDP with this tunnel type
                debug!("   Marking channel_id={} for UDP tunnel 0x{:08X}", channel_id, tunnel.tunnel_type);
                self.udp_channels.insert(channel_id, tunnel.tunnel_type);
                
                if let Some(channel) = self.dynamic_channels.get_by_channel_id_mut(channel_id) {
                    channel.on_soft_sync(tunnel.tunnel_type);
                } else {
                    debug!(
                        "SoftSync tunnel references unknown channel_id={channel_id} (tunnel_type=0x{:08X})",
                        tunnel.tunnel_type
                    );
                }
            }

            // Store the first tunnel type for notification
            if self.soft_sync_completed_tunnel.is_none() {
                self.soft_sync_completed_tunnel = Some(tunnel.tunnel_type);
            }
        }

        let response =
            DrdynvcClientPdu::SoftSyncResponse(SoftSyncResponsePdu::from_request(request));
        debug!("📤 Sending DVC SoftSync Response PDU: {response:?}");
        
        // Per MS-RDPEDYC: Soft-Sync responses MUST go over TCP
        // This is the transition message that signals switching to UDP is complete
        // The response itself uses TCP, but signals that subsequent Data PDUs can use UDP
        let mut msg = SvcMessage::from(response).with_transport(ironrdp_svc::TransportContext::Tcp);
        
        responses.push(msg);
        Ok(())
    }

    /// Create and return a Soft-Sync Request PDU to switch dynamic channels to UDP transport.
    ///
    /// This should be called after the multitransport (UDP) tunnel is established.
    /// The tunnel_type should be:
    /// - 0x00000001 for TUNNELTYPE_UDPFECR (Reliable UDP)
    /// - 0x00000003 for TUNNELTYPE_UDPFECL (Lossy UDP)
    pub fn create_soft_sync_request(&self, tunnel_type: u32) -> PduResult<SvcMessage> {
        use crate::pdu::{
            SoftSyncChannelListEntry, SOFT_SYNC_CHANNEL_LIST_PRESENT, SOFT_SYNC_TCP_FLUSHED,
        };

        // Collect all active dynamic channel IDs
        let channel_ids: Vec<u32> = self
            .dynamic_channels
            .values()
            .filter_map(|channel| channel.channel_id())
            .collect();

        if channel_ids.is_empty() {
            return Err(pdu_other_err!("No active dynamic channels to switch"));
        }

        let mut tunnels = Vec::new();
        tunnels.push(SoftSyncChannelListEntry {
            tunnel_type,
            channel_ids,
        });

        let flags = SOFT_SYNC_TCP_FLUSHED | SOFT_SYNC_CHANNEL_LIST_PRESENT;
        let request = SoftSyncRequestPdu::new(flags, tunnels);

        debug!(
            "Created Soft-Sync Request PDU: tunnel_type=0x{:08X}, {} channels",
            tunnel_type,
            request.tunnels[0].channel_ids.len()
        );

        Ok(SvcMessage::from(DrdynvcClientPdu::SoftSyncRequest(request)))
    }
}

impl_as_any!(DrdynvcClient);

impl Default for DrdynvcClient {
    fn default() -> Self {
        Self::new()
    }
}

impl SvcProcessor for DrdynvcClient {
    fn channel_name(&self) -> ChannelName {
        DrdynvcClient::NAME
    }

    fn compression_condition(&self) -> CompressionCondition {
        CompressionCondition::WhenRdpDataIsCompressed
    }

    fn process(&mut self, payload: &[u8]) -> PduResult<Vec<SvcMessage>> {
        let pdu = decode_dvc_message(payload).map_err(|e| decode_err!(e))?;
        let mut responses = Vec::new();

        match pdu {
            DrdynvcServerPdu::Capabilities(caps_request) => {
                // Per MS-RDPEDYC §3.1.3: Some servers send Capabilities Request over UDP after switch
                // This handler works for both TCP (initial handshake) and UDP (post-switch) paths
                debug!("Got DVC Capabilities Request PDU: {caps_request:?}");
                let server_version = Self::get_caps_version(&caps_request);
                responses.push(self.create_capabilities_response(server_version));
            }
            DrdynvcServerPdu::Create(create_request) => {
                debug!("Got DVC Create Request PDU: {create_request:?}");
                let channel_name = create_request.channel_name;
                let channel_id = create_request.channel_id;

                if !self.cap_handshake_done {
                    debug!(
                        "Got DVC Create Request PDU before a Capabilities Request PDU. \
                        Sending Capabilities Response PDU before the Create Response PDU."
                    );
                    // Server didn't send Caps Request, assume V1 for compatibility
                    responses.push(self.create_capabilities_response(CapsVersion::V1));
                }

                let channel_exists = self
                    .dynamic_channels
                    .get_by_channel_name(&channel_name)
                    .is_some();
                let (creation_status, start_messages) = if channel_exists {
                    // If we have a handler for this channel, attach the channel ID
                    // and get any start messages.
                    self.dynamic_channels
                        .attach_channel_id(channel_name.clone(), channel_id);
                    let dynamic_channel = self
                        .dynamic_channels
                        .get_by_channel_name_mut(&channel_name)
                        .expect("channel exists");
                    (CreationStatus::OK, dynamic_channel.start()?)
                } else {
                    (CreationStatus::NO_LISTENER, Vec::new())
                };

                let create_response =
                    DrdynvcClientPdu::Create(CreateResponsePdu::new(channel_id, creation_status));
                debug!("Send DVC Create Response PDU: {create_response:?}");
                responses.push(SvcMessage::from(create_response));

                // If this DVC has start messages, send them.
                if !start_messages.is_empty() {
                    responses.extend(
                        encode_dvc_messages(channel_id, start_messages, ChannelFlags::empty())
                            .map_err(|e| encode_err!(e))?,
                    );
                }
            }
            DrdynvcServerPdu::Close(close_request) => {
                debug!("Got DVC Close Request PDU: {close_request:?}");
                self.dynamic_channels
                    .remove_by_channel_id(close_request.channel_id);

                let close_response =
                    DrdynvcClientPdu::Close(ClosePdu::new(close_request.channel_id));

                debug!("Send DVC Close Response PDU: {close_response:?}");
                responses.push(SvcMessage::from(close_response));
            }
            DrdynvcServerPdu::Data(data) => {
                let channel_id = data.channel_id();

                let messages = self
                    .dynamic_channels
                    .get_by_channel_id_mut(channel_id)
                    .ok_or_else(|| pdu_other_err!("access to non existing DVC channel"))?
                    .process(data)?;

                // Determine transport based on whether this channel was switched to UDP via Soft-Sync
                let encoded_messages = encode_dvc_messages(channel_id, messages, ChannelFlags::empty())
                    .map_err(|e| encode_err!(e))?;
                
                if let Some(&tunnel_type) = self.udp_channels.get(&channel_id) {
                    // This channel uses UDP - tag messages with UDP transport
                    responses.extend(
                        encoded_messages
                            .into_iter()
                            .map(|msg| msg.with_transport(TransportContext::UdpTunnel(tunnel_type)))
                    );
                } else {
                    // This channel stays on TCP (or Soft-Sync hasn't happened yet)
                    // Don't tag with transport, let it default to TCP
                    responses.extend(encoded_messages);
                }
            }
            DrdynvcServerPdu::SoftSyncRequest(request) => {
                debug!("Got DVC SoftSync Request PDU: {request:?}");
                self.handle_soft_sync_request(&request, &mut responses)?;
            }
            DrdynvcServerPdu::SoftSyncResponse(response) => {
                debug!("Got DVC SoftSync Response PDU: {response:?}");
                // Server confirmed that channels have been switched to UDP
                // Store the first tunnel type for notification
                if let Some(&tunnel_type) = response.tunnels_to_switch.first() {
                    debug!(
                        "Server confirmed soft-sync for tunnel_type=0x{:08X}",
                        tunnel_type
                    );
                    self.soft_sync_completed_tunnel = Some(tunnel_type);
                }
            }
        }

        Ok(responses)
    }
}

impl SvcClientProcessor for DrdynvcClient {}

fn decode_dvc_message(user_data: &[u8]) -> DecodeResult<DrdynvcServerPdu> {
    DrdynvcServerPdu::decode(&mut ReadCursor::new(user_data))
}
