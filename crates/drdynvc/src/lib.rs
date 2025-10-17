//! Minimal DRDYNVC (Dynamic Virtual Channel) client implementation
//!
//! This crate provides a lightweight client for the MS-RDPEDYC protocol,
//! sufficient to open dynamic channels like RDPGFX and pass data bidirectionally.
//!
//! # Example
//! ```no_run
//! use drdynvc::Drdynvc;
//!
//! let mut dvc = Drdynvc::new();
//!
//! // Send capabilities
//! let caps_pdu = dvc.build_caps_advertise(3)?;
//! // ... send caps_pdu over static "drdynvc" channel ...
//!
//! // Open a dynamic channel
//! let gfx_id = dvc.open_channel("Microsoft::Windows::RDS::Graphics")?;
//!
//! // Send data
//! let payload = vec![0x01, 0x02, 0x03];
//! let pdu = dvc.build_send_data(gfx_id, &payload)?;
//! // ... send pdu over static "drdynvc" channel ...
//!
//! // Receive and process incoming data
//! let incoming_pdu = vec![...]; // from static channel
//! if let Some(incoming) = dvc.process_incoming(&incoming_pdu)? {
//!     println!("Received {} bytes on channel {}", incoming.data.len(), incoming.channel_id);
//! }
//! ```

use anyhow::{anyhow, bail, Context, Result};
use bytes::{Buf, BufMut, BytesMut};
use std::collections::HashMap;
use tracing::{debug, trace, warn};

/// PDU command types (MS-RDPEDYC 2.2.1)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PduCmd {
    CreateRequest = 0x01,
    DataFirst = 0x02,
    Data = 0x03,
    CloseRequest = 0x04,
    Capability = 0x05,
    DataFirstCompressed = 0x06,
    DataCompressed = 0x07,
    SoftSyncRequest = 0x08,
    SoftSyncResponse = 0x09,
}

impl TryFrom<u8> for PduCmd {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(PduCmd::CreateRequest),
            0x02 => Ok(PduCmd::DataFirst),
            0x03 => Ok(PduCmd::Data),
            0x04 => Ok(PduCmd::CloseRequest),
            0x05 => Ok(PduCmd::Capability),
            0x06 => Ok(PduCmd::DataFirstCompressed),
            0x07 => Ok(PduCmd::DataCompressed),
            0x08 => Ok(PduCmd::SoftSyncRequest),
            0x09 => Ok(PduCmd::SoftSyncResponse),
            _ => bail!("Unknown DRDYNVC command: 0x{:02X}", value),
        }
    }
}

/// Channel state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelState {
    Opening,
    Running,
    Closed,
}

/// Information about a dynamic channel
#[derive(Debug)]
struct DvcChannel {
    id: u32,
    name: String,
    state: ChannelState,
    /// Reassembly buffer for fragmented messages
    reassembly: Option<ReassemblyBuffer>,
}

/// Reassembly buffer for fragmented DATA_FIRST + DATA messages
#[derive(Debug)]
struct ReassemblyBuffer {
    total_length: u32,
    data: BytesMut,
}

/// Incoming message from a dynamic channel
#[derive(Debug)]
pub struct Incoming {
    pub channel_id: u32,
    pub data: Vec<u8>,
}

/// DRDYNVC client state machine
#[derive(Debug)]
pub struct Drdynvc {
    /// Protocol version (2 or 3)
    version: u8,
    /// Next channel ID to allocate
    next_channel_id: u32,
    /// Active channels by ID
    channels: HashMap<u32, DvcChannel>,
    /// Channels by name (for reverse lookup)
    channels_by_name: HashMap<String, u32>,
    /// Maximum fragment size (typically 1600)
    chunk_size: usize,
}

impl Default for Drdynvc {
    fn default() -> Self {
        Self::new()
    }
}

impl Drdynvc {
    /// Create a new DRDYNVC client
    pub fn new() -> Self {
        Self {
            version: 3, // RDP8+
            next_channel_id: 1,
            channels: HashMap::new(),
            channels_by_name: HashMap::new(),
            chunk_size: 1600,
        }
    }

    /// Set protocol version (2 or 3)
    pub fn set_version(&mut self, version: u8) -> Result<()> {
        if version < 2 || version > 3 {
            bail!("Unsupported DRDYNVC version: {}", version);
        }
        self.version = version;
        Ok(())
    }

    /// Build a capabilities advertise PDU
    pub fn build_caps_advertise(&self, version: u8) -> Result<Vec<u8>> {
        let mut buf = BytesMut::new();

        // Header: Cmd (4 bits) | Sp (2 bits) | CbId (2 bits)
        // For CAPABILITY, Sp and CbId are typically 0
        buf.put_u8((PduCmd::Capability as u8) << 4);

        // Pad (1 byte)
        buf.put_u8(0);

        // Version (2 bytes, little-endian)
        buf.put_u16_le(version as u16);

        if version >= 2 {
            // PriorityCharge0-3 (4 x 2 bytes)
            buf.put_u16_le(0); // PriorityCharge0
            buf.put_u16_le(0); // PriorityCharge1
            buf.put_u16_le(0); // PriorityCharge2
            buf.put_u16_le(0); // PriorityCharge3
        }

        Ok(buf.to_vec())
    }

    /// Open a new dynamic channel
    /// Returns the channel ID for future use
    pub fn open_channel(&mut self, name: &str) -> Result<u32> {
        // Check if already opened
        if let Some(&existing_id) = self.channels_by_name.get(name) {
            return Ok(existing_id);
        }

        let channel_id = self.next_channel_id;
        self.next_channel_id += 1;

        let channel = DvcChannel {
            id: channel_id,
            name: name.to_string(),
            state: ChannelState::Opening,
            reassembly: None,
        };

        self.channels.insert(channel_id, channel);
        self.channels_by_name.insert(name.to_string(), channel_id);

        debug!("Opened dynamic channel '{}' with ID {}", name, channel_id);
        Ok(channel_id)
    }

    /// Build a CREATE_REQUEST PDU for a channel
    pub fn build_create_request(&self, channel_id: u32) -> Result<Vec<u8>> {
        let channel = self
            .channels
            .get(&channel_id)
            .ok_or_else(|| anyhow!("Channel {} not found", channel_id))?;

        let mut buf = BytesMut::new();

        // Determine channel ID field size
        let cb_chid = Self::get_var_int_size(channel_id);

        // Header: Cmd (4 bits) | Sp (2 bits, 0 for CREATE) | CbId (2 bits)
        buf.put_u8((PduCmd::CreateRequest as u8) << 4 | cb_chid);

        // Channel ID (variable length)
        Self::write_var_int(&mut buf, channel_id, cb_chid);

        // Channel name (null-terminated string)
        buf.put_slice(channel.name.as_bytes());
        buf.put_u8(0); // null terminator

        trace!(
            "Built CREATE_REQUEST for channel '{}' (ID {})",
            channel.name,
            channel_id
        );
        Ok(buf.to_vec())
    }

    /// Build DATA PDU(s) to send on a channel
    /// Returns one or more PDUs (fragmented if data is large)
    pub fn build_send_data(&self, channel_id: u32, data: &[u8]) -> Result<Vec<Vec<u8>>> {
        let channel = self
            .channels
            .get(&channel_id)
            .ok_or_else(|| anyhow!("Channel {} not found", channel_id))?;

        if channel.state != ChannelState::Running {
            bail!(
                "Channel {} ('{}') is not in running state",
                channel_id,
                channel.name
            );
        }

        let cb_chid = Self::get_var_int_size(channel_id);
        let header_size = 1 + Self::var_int_size(cb_chid); // Cmd byte + channel ID

        let mut pdus = Vec::new();

        if data.len() <= self.chunk_size - header_size {
            // Single DATA PDU
            let mut buf = BytesMut::new();
            buf.put_u8((PduCmd::Data as u8) << 4 | cb_chid);
            Self::write_var_int(&mut buf, channel_id, cb_chid);
            buf.put_slice(data);
            pdus.push(buf.to_vec());
        } else {
            // Fragment: DATA_FIRST + DATA chunks
            let total_length = data.len() as u32;
            let cb_len = Self::get_var_int_size(total_length);
            let first_header_size = 1 + Self::var_int_size(cb_chid) + Self::var_int_size(cb_len);

            // First chunk
            let first_chunk_size = self.chunk_size - first_header_size;
            let mut buf = BytesMut::new();
            buf.put_u8((PduCmd::DataFirst as u8) << 4 | (cb_len << 2) | cb_chid);
            Self::write_var_int(&mut buf, channel_id, cb_chid);
            Self::write_var_int(&mut buf, total_length, cb_len);
            buf.put_slice(&data[..first_chunk_size]);
            pdus.push(buf.to_vec());

            // Remaining chunks
            let mut offset = first_chunk_size;
            while offset < data.len() {
                let chunk_size = (data.len() - offset).min(self.chunk_size - header_size);
                let mut buf = BytesMut::new();
                buf.put_u8((PduCmd::Data as u8) << 4 | cb_chid);
                Self::write_var_int(&mut buf, channel_id, cb_chid);
                buf.put_slice(&data[offset..offset + chunk_size]);
                pdus.push(buf.to_vec());
                offset += chunk_size;
            }
        }

        trace!(
            "Built {} PDU(s) for {} bytes on channel {}",
            pdus.len(),
            data.len(),
            channel_id
        );
        Ok(pdus)
    }

    /// Build a CLOSE_REQUEST PDU
    pub fn build_close_request(&mut self, channel_id: u32) -> Result<Vec<u8>> {
        let channel = self
            .channels
            .get_mut(&channel_id)
            .ok_or_else(|| anyhow!("Channel {} not found", channel_id))?;

        let cb_chid = Self::get_var_int_size(channel_id);

        let mut buf = BytesMut::new();
        buf.put_u8((PduCmd::CloseRequest as u8) << 4 | cb_chid);
        Self::write_var_int(&mut buf, channel_id, cb_chid);

        channel.state = ChannelState::Closed;
        debug!(
            "Built CLOSE_REQUEST for channel {} ('{}')",
            channel_id, channel.name
        );
        Ok(buf.to_vec())
    }

    /// Process incoming PDU from the static drdynvc channel
    /// Returns Some(Incoming) if a complete message is received
    pub fn process_incoming(&mut self, pdu: &[u8]) -> Result<Option<Incoming>> {
        if pdu.is_empty() {
            bail!("Empty DRDYNVC PDU");
        }

        let mut buf = &pdu[..];
        let header = buf.get_u8();

        let cmd = (header >> 4) & 0x0F;
        let sp = (header >> 2) & 0x03;
        let cb_chid = header & 0x03;

        let cmd = PduCmd::try_from(cmd)?;

        match cmd {
            PduCmd::Capability => self.process_capability(buf, sp),
            PduCmd::CreateRequest => self.process_create_response(buf, cb_chid),
            PduCmd::Data => self.process_data(buf, cb_chid),
            PduCmd::DataFirst => self.process_data_first(buf, sp, cb_chid),
            PduCmd::CloseRequest => self.process_close(buf, cb_chid),
            _ => {
                warn!("Unhandled DRDYNVC command: {:?}", cmd);
                Ok(None)
            }
        }
    }

    fn process_capability(&mut self, mut buf: &[u8], _sp: u8) -> Result<Option<Incoming>> {
        if buf.remaining() < 3 {
            bail!("CAPABILITY PDU too short");
        }

        buf.get_u8(); // pad
        let version = buf.get_u16_le();

        debug!("Server CAPABILITY: version={}", version);

        if version >= 2 && buf.remaining() >= 8 {
            // Priority charges (versions 2-3)
            let _pc0 = buf.get_u16_le();
            let _pc1 = buf.get_u16_le();
            let _pc2 = buf.get_u16_le();
            let _pc3 = buf.get_u16_le();
        }

        self.version = version.min(3) as u8;
        Ok(None)
    }

    fn process_create_response(&mut self, mut buf: &[u8], cb_chid: u8) -> Result<Option<Incoming>> {
        let channel_id = Self::read_var_int(&mut buf, cb_chid)?;

        if buf.is_empty() {
            bail!("CREATE_RESPONSE missing status code");
        }

        let status = buf.get_u32_le();

        let channel = self
            .channels
            .get_mut(&channel_id)
            .ok_or_else(|| anyhow!("CREATE_RESPONSE for unknown channel {}", channel_id))?;

        if status == 0 {
            channel.state = ChannelState::Running;
            debug!(
                "Channel {} ('{}') opened successfully",
                channel_id, channel.name
            );
        } else {
            channel.state = ChannelState::Closed;
            warn!(
                "Channel {} ('{}') creation failed: status=0x{:08X}",
                channel_id, channel.name, status
            );
        }

        Ok(None)
    }

    fn process_data(&mut self, mut buf: &[u8], cb_chid: u8) -> Result<Option<Incoming>> {
        let channel_id = Self::read_var_int(&mut buf, cb_chid)?;

        let channel = self
            .channels
            .get_mut(&channel_id)
            .ok_or_else(|| anyhow!("DATA for unknown channel {}", channel_id))?;

        if let Some(reassembly) = &mut channel.reassembly {
            // Continuation of fragmented message
            reassembly.data.put_slice(buf);

            if reassembly.data.len() >= reassembly.total_length as usize {
                // Complete message
                let data = reassembly.data.to_vec();
                channel.reassembly = None;
                trace!("Reassembled {} bytes on channel {}", data.len(), channel_id);
                return Ok(Some(Incoming { channel_id, data }));
            }

            Ok(None)
        } else {
            // Single unfragmented message
            let data = buf.to_vec();
            trace!("Received {} bytes on channel {}", data.len(), channel_id);
            Ok(Some(Incoming { channel_id, data }))
        }
    }

    fn process_data_first(
        &mut self,
        mut buf: &[u8],
        sp: u8,
        cb_chid: u8,
    ) -> Result<Option<Incoming>> {
        let channel_id = Self::read_var_int(&mut buf, cb_chid)?;
        let total_length = Self::read_var_int(&mut buf, sp)?;

        let channel = self
            .channels
            .get_mut(&channel_id)
            .ok_or_else(|| anyhow!("DATA_FIRST for unknown channel {}", channel_id))?;

        let mut data = BytesMut::with_capacity(total_length as usize);
        data.put_slice(buf);

        channel.reassembly = Some(ReassemblyBuffer { total_length, data });

        trace!(
            "Started reassembly of {} bytes on channel {}",
            total_length,
            channel_id
        );
        Ok(None)
    }

    fn process_close(&mut self, mut buf: &[u8], cb_chid: u8) -> Result<Option<Incoming>> {
        let channel_id = Self::read_var_int(&mut buf, cb_chid)?;

        if let Some(channel) = self.channels.get_mut(&channel_id) {
            channel.state = ChannelState::Closed;
            debug!(
                "Channel {} ('{}') closed by server",
                channel_id, channel.name
            );
        }

        Ok(None)
    }

    // Variable-length integer helpers

    fn get_var_int_size(val: u32) -> u8 {
        if val <= 0xFF {
            0
        } else if val <= 0xFFFF {
            1
        } else {
            2
        }
    }

    fn var_int_size(cb: u8) -> usize {
        match cb {
            0 => 1,
            1 => 2,
            _ => 4,
        }
    }

    fn write_var_int(buf: &mut BytesMut, val: u32, cb: u8) {
        match cb {
            0 => buf.put_u8(val as u8),
            1 => buf.put_u16_le(val as u16),
            _ => buf.put_u32_le(val),
        }
    }

    fn read_var_int(buf: &mut &[u8], cb: u8) -> Result<u32> {
        let val = match cb {
            0 => {
                if buf.remaining() < 1 {
                    bail!("Not enough data for u8");
                }
                buf.get_u8() as u32
            }
            1 => {
                if buf.remaining() < 2 {
                    bail!("Not enough data for u16");
                }
                buf.get_u16_le() as u32
            }
            _ => {
                if buf.remaining() < 4 {
                    bail!("Not enough data for u32");
                }
                buf.get_u32_le()
            }
        };
        Ok(val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_var_int_roundtrip() {
        let mut buf = BytesMut::new();

        // 1-byte
        Drdynvc::write_var_int(&mut buf, 42, 0);
        assert_eq!(buf.len(), 1);
        let mut slice = &buf[..];
        assert_eq!(Drdynvc::read_var_int(&mut slice, 0).unwrap(), 42);

        // 2-byte
        buf.clear();
        Drdynvc::write_var_int(&mut buf, 1234, 1);
        assert_eq!(buf.len(), 2);
        let mut slice = &buf[..];
        assert_eq!(Drdynvc::read_var_int(&mut slice, 1).unwrap(), 1234);

        // 4-byte
        buf.clear();
        Drdynvc::write_var_int(&mut buf, 0x12345678, 2);
        assert_eq!(buf.len(), 4);
        let mut slice = &buf[..];
        assert_eq!(Drdynvc::read_var_int(&mut slice, 2).unwrap(), 0x12345678);
    }

    #[test]
    fn test_caps_advertise() {
        let dvc = Drdynvc::new();
        let pdu = dvc.build_caps_advertise(3).unwrap();

        assert_eq!(pdu[0], 0x50); // CAPABILITY cmd
        assert_eq!(pdu[1], 0x00); // pad
        assert_eq!(u16::from_le_bytes([pdu[2], pdu[3]]), 3); // version
    }

    #[test]
    fn test_open_channel() {
        let mut dvc = Drdynvc::new();
        let id = dvc.open_channel("test::channel").unwrap();
        assert_eq!(id, 1);

        let pdu = dvc.build_create_request(id).unwrap();
        assert_eq!(pdu[0] & 0xF0, 0x10); // CREATE_REQUEST cmd
    }
}
