use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::any::TypeId;
use core::fmt;

use ironrdp_core::{impl_as_any, Decode as _, DecodeResult, ReadCursor};
use ironrdp_pdu::{self as pdu, decode_err, encode_err, pdu_other_err};
use ironrdp_svc::{
    ChannelFlags, CompressionCondition, SvcClientProcessor, SvcMessage, SvcProcessor,
};
use pdu::gcc::ChannelName;
use pdu::PduResult;
use tracing::{debug, info, warn};

use crate::pdu::{
    CapabilitiesResponsePdu, CapsVersion, ClosePdu, CreateResponsePdu, CreationStatus,
    DataFirstPdu, DataPdu, DrdynvcClientPdu, DrdynvcDataPdu, DrdynvcServerPdu, SoftSyncRequestPdu,
    SoftSyncResponsePdu,
};
use crate::{
    encode_dvc_messages, DvcCompression, DvcDecompressor, DvcProcessor, DynamicChannelId,
    DynamicChannelSet, DynamicVirtualChannel,
};

pub trait DvcClientProcessor: DvcProcessor {}

/// Format tunnel type as a human-readable string
/// Tunnel PDUs held back waiting for a Soft-Sync Request before the connection is given up on.
const MAX_HELD_TUNNEL_PDUS: usize = 1024;

fn tunnel_type_name(tunnel_type: u32) -> &'static str {
    match tunnel_type {
        0x00000001 => "TUNNELTYPE_UDPFECR (Reliable UDP)",
        0x00000003 => "TUNNELTYPE_UDPFECL (Lossy UDP)",
        _ => "Unknown",
    }
}

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
    /// Set of UDP tunnel types that have been successfully established and are available for use.
    /// Per MS-RDPEDYC §3.2.5.3.2: "Each multitransport tunnel that the client manager will use
    /// to send dynamic virtual channel data MUST be specified in the TunnelsToSwitch field."
    available_udp_tunnels: alloc::collections::BTreeSet<u32>,
    /// A Soft-Sync Request naming a tunnel that is still being set up. It is answered once
    /// every tunnel it names is up or has failed: the response must not name a tunnel before
    /// its Initiate Multitransport Response went out ([MS-RDPEDYC] 3.2.5.3.2), and answering
    /// twice -- empty first, then again -- is not something the protocol has.
    pending_soft_sync: Option<SoftSyncRequestPdu>,
    /// Tunnels that failed to come up; a Soft-Sync Request naming them is answered without them.
    failed_udp_tunnels: alloc::collections::BTreeSet<u32>,
    /// Whether the Soft-Sync Response has gone out. Until then nothing that arrives on a tunnel
    /// is read ([MS-RDPEDYC] 3.2.5.3.1), see `held_tunnel_data`.
    soft_sync_answered: bool,
    /// Tunnel PDUs that arrived before the Soft-Sync Response went out, in arrival order.
    held_tunnel_data: Vec<(Vec<u8>, ironrdp_svc::TransportContext)>,
    /// Where a channel's decompression context comes from, when the client can decompress.
    ///
    /// Its presence is what makes this a version 3 DVC manager, and so what allows the server
    /// to send `DYNVC_DATA_COMPRESSED`.
    compression: Option<alloc::boxed::Box<dyn DvcCompression>>,
    /// One decompression context per channel, made on first use and dropped with the channel.
    decompressors: BTreeMap<DynamicChannelId, alloc::boxed::Box<dyn DvcDecompressor>>,
    /// Where each channel's traffic was last seen, so that anything this client starts saying
    /// on a channel goes out the way the server is saying it.
    channel_transports: BTreeMap<DynamicChannelId, ironrdp_svc::TransportContext>,
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
            available_udp_tunnels: alloc::collections::BTreeSet::new(),
            pending_soft_sync: None,
            failed_udp_tunnels: alloc::collections::BTreeSet::new(),
            soft_sync_answered: false,
            held_tunnel_data: Vec::new(),
            compression: None,
            decompressors: BTreeMap::new(),
            channel_transports: BTreeMap::new(),
        }
    }

    /// Lets the server compress channel data, by giving this client a way to undo it.
    #[must_use]
    pub fn with_compression<T>(mut self, compression: T) -> Self
    where
        T: DvcCompression + 'static,
    {
        self.compression = Some(alloc::boxed::Box::new(compression));
        self
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

    /// The transport a channel's traffic last arrived on.
    ///
    /// A server that has moved a channel onto a multitransport tunnel stops reading that
    /// channel on TCP, so a message this client starts on its own has to follow the server
    /// there rather than take the static channel it was first offered on.
    pub fn channel_transport(
        &self,
        channel_id: DynamicChannelId,
    ) -> Option<ironrdp_svc::TransportContext> {
        self.channel_transports.get(&channel_id).copied()
    }

    /// Returns the tunnel type if soft-sync has been completed
    pub fn soft_sync_completed(&self) -> Option<u32> {
        self.soft_sync_completed_tunnel
    }

    /// Register that a UDP tunnel has been successfully established and is available for use.
    /// Per MS-RDPEDYC §3.2.5.3.2: The client MUST NOT send the Soft-Sync Response PDU until
    /// it has sent a successful Initiate Multitransport Response PDU for each tunnel.
    ///
    /// This should be called when the UDP transport initialization completes successfully.
    /// Returns any deferred CREATE requests or Soft-Sync responses that should now be processed.
    pub fn register_available_tunnel(
        &mut self,
        tunnel_type: u32,
    ) -> Result<Vec<SvcMessage>, pdu::PduError> {
        debug!(
            "Registering available UDP tunnel type: 0x{:08X} ({})",
            tunnel_type,
            tunnel_type_name(tunnel_type)
        );
        self.available_udp_tunnels.insert(tunnel_type);
        self.failed_udp_tunnels.remove(&tunnel_type);
        self.answer_pending_soft_sync()
    }

    /// Records that a tunnel failed to come up. A Soft-Sync Request waiting for it is answered
    /// now, without it, so the channels it names stay on TCP.
    pub fn tunnel_failed(&mut self, tunnel_type: u32) -> Result<Vec<SvcMessage>, pdu::PduError> {
        debug!(
            "UDP tunnel type 0x{:08X} ({}) failed",
            tunnel_type,
            tunnel_type_name(tunnel_type)
        );
        self.available_udp_tunnels.remove(&tunnel_type);
        self.failed_udp_tunnels.insert(tunnel_type);
        self.answer_pending_soft_sync()
    }

    /// Whether Soft-Sync moved any channel onto the given tunnel.
    pub fn has_channels_on(&self, tunnel_type: u32) -> bool {
        self.udp_channels.values().any(|&t| t == tunnel_type)
    }

    fn answer_pending_soft_sync(&mut self) -> Result<Vec<SvcMessage>, pdu::PduError> {
        let mut responses = Vec::new();
        if let Some(request) = self.pending_soft_sync.take() {
            info!("📦 Answering the Soft-Sync Request that was waiting for its tunnels");
            self.handle_soft_sync_request(&request, &mut responses)?;
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
        // Per MS-RDPEDYC §2.2.1.2, client MUST respond with the version level it supports.
        // Version 3 is the one that lets the server compress channel data, so it is only
        // honest to ask for it when a decompressor has been supplied.
        let client_max_version = if self.compression.is_some() {
            CapsVersion::V3
        } else {
            CapsVersion::V2
        };

        // Negotiate: use minimum of client max and server requested
        let negotiated_version = match (client_max_version, server_version) {
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
            server_version, client_max_version, negotiated_version
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

    /// Turns a compressed data PDU into the plain one the channel is waiting for.
    ///
    /// Everything downstream -- reassembly, the channel processor itself -- then sees the same
    /// `DYNVC_DATA_FIRST` / `DYNVC_DATA` sequence it would have seen without compression.
    fn decompress(&mut self, pdu: DrdynvcDataPdu) -> PduResult<DrdynvcDataPdu> {
        match pdu {
            DrdynvcDataPdu::DataFirstCompressed(pdu) => {
                let data = self.inflate(pdu.channel_id, &pdu.data)?;
                Ok(DrdynvcDataPdu::DataFirst(DataFirstPdu::new(
                    pdu.channel_id,
                    pdu.length,
                    data,
                )))
            }
            DrdynvcDataPdu::DataCompressed(pdu) => {
                let data = self.inflate(pdu.channel_id, &pdu.data)?;
                Ok(DrdynvcDataPdu::Data(DataPdu::new(pdu.channel_id, data)))
            }
            plain => Ok(plain),
        }
    }

    /// Runs one block through the channel's own decompression context, making it on first use.
    fn inflate(&mut self, channel_id: DynamicChannelId, data: &[u8]) -> PduResult<Vec<u8>> {
        let Self {
            compression,
            decompressors,
            ..
        } = self;
        let compression = compression.as_ref().ok_or_else(|| {
            pdu_other_err!("server compressed channel data for a client that asked it not to")
        })?;
        let context = decompressors
            .entry(channel_id)
            .or_insert_with(|| compression.new_context());
        let mut plain = Vec::new();
        context.decompress(data, &mut plain)?;
        Ok(plain)
    }

    fn handle_soft_sync_request(
        &mut self,
        request: &SoftSyncRequestPdu,
        responses: &mut Vec<SvcMessage>,
    ) -> PduResult<()> {
        info!(
            "📥 Received Soft-Sync Request from server: {} tunnel(s)",
            request.tunnels.len()
        );

        // [MS-RDPEDYC] 2.2.5.1: SOFT_SYNC_TCP_FLUSHED must be set. Without it the server has not
        // said its TCP data for these channels is out, so none of them moves: an empty response
        // keeps them all on TCP.
        if (request.flags & crate::pdu::SOFT_SYNC_TCP_FLUSHED) == 0 {
            warn!(
                "⚠️  Soft-Sync Request without SOFT_SYNC_TCP_FLUSHED (flags=0x{:04X}), channels stay on TCP",
                request.flags
            );
            self.pending_soft_sync = None;
            self.send_soft_sync_response(request, &alloc::collections::BTreeSet::new(), responses);
            return Ok(());
        }

        // A tunnel still being set up gets its answer once it is up or has failed.
        if let Some(tunnel) = request.tunnels.iter().find(|tunnel| {
            !self.available_udp_tunnels.contains(&tunnel.tunnel_type)
                && !self.failed_udp_tunnels.contains(&tunnel.tunnel_type)
        }) {
            info!(
                "   Tunnel 0x{:08X} ({}) is not up yet, answering Soft-Sync once it is",
                tunnel.tunnel_type,
                tunnel_type_name(tunnel.tunnel_type)
            );
            self.pending_soft_sync = Some(request.clone());
            return Ok(());
        }

        for tunnel in &request.tunnels {
            info!(
                "   Tunnel type=0x{:08X} ({}), {} channel(s)",
                tunnel.tunnel_type,
                tunnel_type_name(tunnel.tunnel_type),
                tunnel.channel_ids.len()
            );
            if !self.available_udp_tunnels.contains(&tunnel.tunnel_type) {
                info!("   Tunnel 0x{:08X} failed, its channels stay on TCP", tunnel.tunnel_type);
                continue;
            }

            let mut udp_channel_count = 0;
            for &channel_id in &tunnel.channel_ids {
                // Check if this channel supports UDP transport (skip stubs)
                if !self.dynamic_channels.supports_udp_transport(channel_id) {
                    if let Some(channel) = self.dynamic_channels.get_by_channel_id(channel_id) {
                        info!(
                            "   Skipping channel_id={} ('{}') - does not support UDP (stub/mock implementation)",
                            channel_id,
                            channel.channel_name()
                        );
                    }
                    continue; // Don't mark this channel for UDP
                }

                // Mark this channel as using UDP with this tunnel type
                self.udp_channels.insert(channel_id, tunnel.tunnel_type);
                udp_channel_count += 1;

                if let Some(channel) = self.dynamic_channels.get_by_channel_id_mut(channel_id) {
                    info!(
                        "   Marking channel_id={} ('{}') for UDP tunnel 0x{:08X} ({})",
                        channel_id,
                        channel.channel_name(),
                        tunnel.tunnel_type,
                        tunnel_type_name(tunnel.tunnel_type)
                    );
                    // Notify channel that it's being switched to UDP
                    let _ = channel.on_soft_sync(tunnel.tunnel_type);
                } else {
                    info!(
                        "   Marking channel_id={} (unknown channel) for UDP tunnel 0x{:08X} ({})",
                        channel_id,
                        tunnel.tunnel_type,
                        tunnel_type_name(tunnel.tunnel_type)
                    );
                }
            }

            info!(
                "   ✅ Accepted {}/{} channels for UDP tunnel 0x{:08X}",
                udp_channel_count,
                tunnel.channel_ids.len(),
                tunnel.tunnel_type
            );

            // Store the first tunnel type for notification
            if self.soft_sync_completed_tunnel.is_none() {
                self.soft_sync_completed_tunnel = Some(tunnel.tunnel_type);
            }
        }

        let available = self.available_udp_tunnels.clone();
        self.send_soft_sync_response(request, &available, responses);
        Ok(())
    }

    /// Queues the Soft-Sync Response, naming those of the request's tunnels in `available`.
    /// It always goes on the drdynvc static channel over TCP; from here on, tunnel data is read.
    fn send_soft_sync_response(
        &mut self,
        request: &SoftSyncRequestPdu,
        available: &alloc::collections::BTreeSet<u32>,
        responses: &mut Vec<SvcMessage>,
    ) {
        let response = DrdynvcClientPdu::SoftSyncResponse(
            SoftSyncResponsePdu::from_request_with_available(request, available),
        );
        info!("📤 Sending DVC SoftSync Response PDU: {response:?}");
        responses.push(SvcMessage::from(response).with_transport(ironrdp_svc::TransportContext::Tcp));
        self.soft_sync_answered = true;
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

    fn process(
        &mut self,
        payload: &[u8],
        transport: ironrdp_svc::TransportContext,
    ) -> PduResult<Vec<SvcMessage>> {
        if transport == ironrdp_svc::TransportContext::Tcp {
            return self.process_pdu(payload, transport);
        }

        // [MS-RDPEDYC] 3.2.5.3.1: nothing on a tunnel is read before the Soft-Sync Request has
        // been answered; only then has the server said its TCP data for the channels is out.
        // Windows does not send on the tunnel before the response, so this only holds data that
        // overtook it, which goes up with the next tunnel PDU after the response.
        if !self.soft_sync_answered {
            if self.held_tunnel_data.len() >= MAX_HELD_TUNNEL_PDUS {
                return Err(pdu_other_err!(
                    "tunnel data keeps arriving without a Soft-Sync Request"
                ));
            }
            debug!("Holding a tunnel PDU until the Soft-Sync Request is answered");
            self.held_tunnel_data.push((payload.to_vec(), transport));
            return Ok(Vec::new());
        }

        let mut responses = Vec::new();
        for (held, held_transport) in core::mem::take(&mut self.held_tunnel_data) {
            responses.extend(self.process_pdu(&held, held_transport)?);
        }
        responses.extend(self.process_pdu(payload, transport)?);
        Ok(responses)
    }
}

impl DrdynvcClient {
    fn process_pdu(
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
                    // A reopened channel starts its compression history over.
                    self.decompressors.remove(&channel_id);
                    self.channel_transports.insert(channel_id, transport);
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
                            .map(|msg| msg.with_transport(transport)),
                    );
                }
            }
            DrdynvcServerPdu::Close(close_request) => {
                debug!("Got DVC Close Request PDU: {close_request:?}");
                self.dynamic_channels
                    .remove_by_channel_id(close_request.channel_id);
                // The compression history belongs to the channel, not to the id it was given.
                self.decompressors.remove(&close_request.channel_id);
                self.channel_transports.remove(&close_request.channel_id);

                let close_response =
                    DrdynvcClientPdu::Close(ClosePdu::new(close_request.channel_id));

                debug!("Send DVC Close Response PDU: {close_response:?}");
                // Use same transport as request (per general RDP principle)
                responses.push(SvcMessage::from(close_response).with_transport(transport));
            }
            DrdynvcServerPdu::Data(data) => {
                let data = self.decompress(data)?;
                let channel_id = data.channel_id();
                self.channel_transports.insert(channel_id, transport);

                let messages = self
                    .dynamic_channels
                    .get_by_channel_id_mut(channel_id)
                    .ok_or_else(|| pdu_other_err!("access to non existing DVC channel"))?
                    .process(data)?;

                // Preserve the incoming transport context - this ensures responses go back
                // on the same tunnel/connection that the request arrived on
                // This is critical for UDP multitransport where request_id != tunnel_type
                let response_transport = transport;

                // Data messages use the same transport as the incoming request,
                // but individual messages can override this via preferred_transport()
                responses.extend(
                    encode_dvc_messages(channel_id, messages, ChannelFlags::empty())
                        .map_err(|e| encode_err!(e))?
                        .into_iter()
                        .map(|msg| {
                            // Only apply default transport if the message doesn't have one already
                            if msg.transport().is_none() {
                                msg.with_transport(response_transport)
                            } else {
                                msg
                            }
                        }),
                );
            }
            DrdynvcServerPdu::SoftSyncRequest(request) => {
                debug!("Got DVC SoftSync Request PDU: {request:?}");
                if transport != ironrdp_svc::TransportContext::Tcp {
                    return Err(pdu_other_err!("Soft-Sync Request on a multitransport tunnel"));
                }
                self.handle_soft_sync_request(&request, &mut responses)?;
            }
            DrdynvcServerPdu::SoftSyncResponse(response) => {
                debug!("Got DVC SoftSync Response PDU: {response:?}");
                // Server confirmed that channels have been switched to UDP
                // Store the first tunnel type for notification
                if let Some(&tunnel_type) = response.tunnels_to_switch.first() {
                    debug!(
                        "Server confirmed soft-sync for tunnel_type=0x{:08X} ({})",
                        tunnel_type,
                        tunnel_type_name(tunnel_type)
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

#[cfg(test)]
mod tests {
    use alloc::vec;

    use ironrdp_svc::TransportContext;

    use super::*;
    use crate::pdu::{SoftSyncChannelListEntry, SOFT_SYNC_CHANNEL_LIST_PRESENT, SOFT_SYNC_TCP_FLUSHED};

    const RELIABLE: u32 = 0x0000_0001;
    const TUNNEL: TransportContext = TransportContext::UdpTunnel(7);

    fn soft_sync_request(flags: u16) -> Vec<u8> {
        let request = SoftSyncRequestPdu::new(
            flags | SOFT_SYNC_CHANNEL_LIST_PRESENT,
            vec![SoftSyncChannelListEntry {
                tunnel_type: RELIABLE,
                channel_ids: vec![3],
            }],
        );
        ironrdp_core::encode_vec(&DrdynvcServerPdu::SoftSyncRequest(request)).expect("encode")
    }

    fn close_request(channel_id: u32) -> Vec<u8> {
        ironrdp_core::encode_vec(&DrdynvcServerPdu::Close(ClosePdu::new(channel_id))).expect("encode")
    }

    /// The tunnels a Soft-Sync Response names, or `None` when `message` is not one.
    fn soft_sync_tunnels(message: &SvcMessage) -> Option<u32> {
        let bytes = message.to_pdu_bytes().expect("encode");
        (bytes[0] >> 4 == 0x09).then(|| u32::from_le_bytes(bytes[2..6].try_into().unwrap()))
    }

    #[test]
    fn soft_sync_waits_for_a_tunnel_that_is_not_up() {
        let mut client = DrdynvcClient::new();
        let responses = client
            .process(&soft_sync_request(SOFT_SYNC_TCP_FLUSHED), TransportContext::Tcp)
            .expect("request");
        assert!(responses.is_empty(), "no response before the tunnel is up, not even an empty one");

        let responses = client.register_available_tunnel(RELIABLE).expect("register");
        assert_eq!(responses.len(), 1);
        assert_eq!(soft_sync_tunnels(&responses[0]), Some(1));
    }

    #[test]
    fn soft_sync_is_answered_without_a_failed_tunnel() {
        let mut client = DrdynvcClient::new();
        client
            .process(&soft_sync_request(SOFT_SYNC_TCP_FLUSHED), TransportContext::Tcp)
            .expect("request");
        let responses = client.tunnel_failed(RELIABLE).expect("failed");
        assert_eq!(responses.len(), 1);
        assert_eq!(soft_sync_tunnels(&responses[0]), Some(0));
        assert!(!client.has_channels_on(RELIABLE));
    }

    #[test]
    fn soft_sync_without_tcp_flushed_moves_nothing() {
        let mut client = DrdynvcClient::new();
        client.register_available_tunnel(RELIABLE).expect("register");
        let responses = client.process(&soft_sync_request(0), TransportContext::Tcp).expect("request");
        assert_eq!(responses.len(), 1);
        assert_eq!(soft_sync_tunnels(&responses[0]), Some(0));
        assert!(!client.has_channels_on(RELIABLE));
    }

    #[test]
    fn tunnel_data_waits_for_the_soft_sync_response() {
        let mut client = DrdynvcClient::new();
        client.register_available_tunnel(RELIABLE).expect("register");
        assert!(client.process(&close_request(5), TUNNEL).expect("early").is_empty());

        client
            .process(&soft_sync_request(SOFT_SYNC_TCP_FLUSHED), TransportContext::Tcp)
            .expect("request");
        let responses = client.process(&close_request(6), TUNNEL).expect("after");
        assert_eq!(responses.len(), 2, "the held PDU goes up first, then the new one");
        assert!(responses.iter().all(|r| r.transport() == Some(TUNNEL)));
    }

    #[test]
    fn tunnel_data_without_a_soft_sync_request_ends_the_connection() {
        let mut client = DrdynvcClient::new();
        for _ in 0..MAX_HELD_TUNNEL_PDUS {
            client.process(&close_request(5), TUNNEL).expect("held");
        }
        assert!(client.process(&close_request(5), TUNNEL).is_err());
    }
}
