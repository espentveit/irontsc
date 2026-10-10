use std::borrow::Cow;

use ironrdp_core::{decode, encode_vec, Decode, Encode, WriteBuf};
use ironrdp_pdu::rdp;
use ironrdp_pdu::rdp::headers::ServerDeactivateAll;
use ironrdp_pdu::x224::X224;

use crate::{general_err, reason_err, ConnectorError, ConnectorErrorExt as _, ConnectorResult};

pub fn encode_send_data_request<T>(
    initiator_id: u16,
    channel_id: u16,
    user_msg: &T,
    buf: &mut WriteBuf,
) -> ConnectorResult<usize>
where
    T: Encode,
{
    let user_data = encode_vec(user_msg).map_err(ConnectorError::encode)?;

    let pdu = ironrdp_pdu::mcs::SendDataRequest {
        initiator_id,
        channel_id,
        user_data: Cow::Owned(user_data),
    };

    let written = ironrdp_core::encode_buf(&X224(pdu), buf).map_err(ConnectorError::encode)?;

    Ok(written)
}

#[derive(Debug, Clone, Copy)]
pub struct SendDataIndicationCtx<'a> {
    pub initiator_id: u16,
    pub channel_id: u16,
    pub user_data: &'a [u8],
}

impl<'a> SendDataIndicationCtx<'a> {
    pub fn decode_user_data<'de, T>(&self) -> ConnectorResult<T>
    where
        T: Decode<'de>,
        'a: 'de,
    {
        let msg = decode::<T>(self.user_data).map_err(ConnectorError::decode)?;
        Ok(msg)
    }
}

pub fn decode_send_data_indication(src: &[u8]) -> ConnectorResult<SendDataIndicationCtx<'_>> {
    use ironrdp_pdu::mcs::McsMessage;

    let mcs_msg = decode::<X224<McsMessage<'_>>>(src).map_err(ConnectorError::decode)?;

    match mcs_msg.0 {
        McsMessage::SendDataIndication(msg) => {
            let Cow::Borrowed(user_data) = msg.user_data else {
                unreachable!()
            };

            Ok(SendDataIndicationCtx {
                initiator_id: msg.initiator_id,
                channel_id: msg.channel_id,
                user_data,
            })
        }
        McsMessage::DisconnectProviderUltimatum(msg) => Err(reason_err!(
            "decode_send_data_indication",
            "received disconnect provider ultimatum: {:?}",
            msg.reason
        )),
        _ => Err(reason_err!(
            "decode_send_data_indication",
            "unexpected MCS message: {}",
            ironrdp_core::name(&mcs_msg)
        )),
    }
}

pub fn encode_share_control(
    initiator_id: u16,
    channel_id: u16,
    share_id: u32,
    pdu: rdp::headers::ShareControlPdu,
    buf: &mut WriteBuf,
) -> ConnectorResult<usize> {
    let pdu_source = initiator_id;

    let share_control_header = rdp::headers::ShareControlHeader {
        share_control_pdu: pdu,
        pdu_source,
        share_id,
    };

    encode_send_data_request(initiator_id, channel_id, &share_control_header, buf)
}

#[derive(Debug, Clone)]
pub struct ShareControlCtx {
    pub initiator_id: u16,
    pub channel_id: u16,
    pub share_id: u32,
    pub pdu_source: u16,
    pub pdu: rdp::headers::ShareControlPdu,
}

pub fn decode_share_control(ctx: SendDataIndicationCtx<'_>) -> ConnectorResult<ShareControlCtx> {
    tracing::trace!(len = ctx.user_data.len(), "decode_share_control");

    // A 28-byte PDU may be an Initiate Multitransport Request (4+2+2+16), which should be
    // handled at a higher level in the state machine.
    if ctx.user_data.len() == 28 {
        tracing::debug!("28-byte PDU might be an Initiate Multitransport Request; decoding as ShareControlHeader");
    }

    let user_msg = ctx.decode_user_data::<rdp::headers::ShareControlHeader>()?;

    Ok(ShareControlCtx {
        initiator_id: ctx.initiator_id,
        channel_id: ctx.channel_id,
        share_id: user_msg.share_id,
        pdu_source: user_msg.pdu_source,
        pdu: user_msg.share_control_pdu,
    })
}

/// Attempts to detect and decode an InitiateMultitransportRequest PDU.
/// Returns Some(request) if the data is a valid multitransport request, None otherwise.
///
/// The InitiateMultitransportRequest can appear in two forms:
/// 1. Raw PDU (24 bytes): sent during capabilities exchange
/// 2. With security header (28 bytes): 4-byte BasicSecurityHeader + 24-byte PDU
pub fn detect_multitransport_request(
    ctx: &SendDataIndicationCtx<'_>,
) -> Option<rdp::multitransport::InitiateMultitransportRequest> {
    // InitiateMultitransportRequest has a fixed size of 24 bytes:
    // - requestId: 4 bytes
    // - protocol: 2 bytes
    // - reserved: 2 bytes
    // - securityCookie: 16 bytes
    const MULTITRANSPORT_PDU_SIZE: usize = 24;
    const SECURITY_HEADER_SIZE: usize = 4;

    let pdu_data = match ctx.user_data.len() {
        MULTITRANSPORT_PDU_SIZE => {
            // Case 1: Raw PDU without security header (during capabilities exchange)
            tracing::trace!("decoding 24-byte PDU as raw InitiateMultitransportRequest");
            ctx.user_data
        }
        size if size == MULTITRANSPORT_PDU_SIZE + SECURITY_HEADER_SIZE => {
            // Case 2: PDU with 4-byte security header prefix
            tracing::trace!("decoding 28-byte PDU as InitiateMultitransportRequest with security header");
            &ctx.user_data[SECURITY_HEADER_SIZE..]
        }
        _ => {
            // Not the right size for a multitransport request
            return None;
        }
    };

    // Try to decode as InitiateMultitransportRequest
    match decode::<rdp::multitransport::InitiateMultitransportRequest>(pdu_data) {
        Ok(request) => {
            tracing::debug!(
                request_id = request.request_id,
                protocol = ?request.requested_protocol,
                "decoded InitiateMultitransportRequest"
            );
            Some(request)
        }
        Err(e) => {
            tracing::debug!(error = ?e, "failed to decode as InitiateMultitransportRequest");
            None
        }
    }
}

/// Encodes an InitiateMultitransportResponse PDU to be sent to the server.
/// This should be called in response to an InitiateMultitransportRequest.
///
/// Per MS-RDPBCGR spec, the response must be sent on the MCS Message Channel
/// (from ServerMessageChannelData in GCC Conference Create Response).
pub fn encode_multitransport_response(
    user_channel_id: u16,
    message_channel_id: u16,
    request_id: u32,
    buf: &mut WriteBuf,
) -> ConnectorResult<usize> {
    let response = rdp::multitransport::InitiateMultitransportResponse::success(request_id);

    tracing::debug!(
        request_id,
        user_channel_id,
        message_channel_id,
        "encoding InitiateMultitransportResponse"
    );

    encode_send_data_request(user_channel_id, message_channel_id, &response, buf)
}

pub fn encode_share_data(
    initiator_id: u16,
    channel_id: u16,
    share_id: u32,
    pdu: rdp::headers::ShareDataPdu,
    buf: &mut WriteBuf,
) -> ConnectorResult<usize> {
    let share_data_header = rdp::headers::ShareDataHeader {
        share_data_pdu: pdu,
        stream_priority: rdp::headers::StreamPriority::Medium,
        compression_flags: rdp::headers::CompressionFlags::empty(),
        compression_type: rdp::client_info::CompressionType::K8, // ignored if CompressionFlags::empty()
    };

    let share_control_pdu = rdp::headers::ShareControlPdu::Data(share_data_header);

    encode_share_control(initiator_id, channel_id, share_id, share_control_pdu, buf)
}

#[derive(Debug, Clone)]
pub struct ShareDataCtx {
    pub initiator_id: u16,
    pub channel_id: u16,
    pub share_id: u32,
    pub pdu_source: u16,
    pub pdu: rdp::headers::ShareDataPdu,
}

pub fn decode_share_data(ctx: SendDataIndicationCtx<'_>) -> ConnectorResult<ShareDataCtx> {
    let ctx = decode_share_control(ctx)?;

    let rdp::headers::ShareControlPdu::Data(share_data_header) = ctx.pdu else {
        return Err(general_err!(
            "received unexpected Share Control Pdu (expected Share Data Header)"
        ));
    };

    Ok(ShareDataCtx {
        initiator_id: ctx.initiator_id,
        channel_id: ctx.channel_id,
        share_id: ctx.share_id,
        pdu_source: ctx.pdu_source,
        pdu: share_data_header.share_data_pdu,
    })
}

pub enum IoChannelPdu {
    Data(ShareDataCtx),
    DeactivateAll(ServerDeactivateAll),
}

pub fn decode_io_channel(ctx: SendDataIndicationCtx<'_>) -> ConnectorResult<IoChannelPdu> {
    let ctx = decode_share_control(ctx)?;

    match ctx.pdu {
        rdp::headers::ShareControlPdu::ServerDeactivateAll(deactivate_all) => {
            Ok(IoChannelPdu::DeactivateAll(deactivate_all))
        }
        rdp::headers::ShareControlPdu::Data(share_data_header) => {
            let share_data_ctx = ShareDataCtx {
                initiator_id: ctx.initiator_id,
                channel_id: ctx.channel_id,
                share_id: ctx.share_id,
                pdu_source: ctx.pdu_source,
                pdu: share_data_header.share_data_pdu,
            };

            Ok(IoChannelPdu::Data(share_data_ctx))
        }
        _ => Err(general_err!(
            "received unexpected Share Control Pdu (expected Share Data Header or Server Deactivate All)"
        )),
    }
}
