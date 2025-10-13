//! Capability negotiation for RDPEGFX

/// Capability versions (MS-RDPEGFX 2.2.3.1)
pub mod cap_version {
    pub const V8: u32 = 0x00080004; // RDP 8.0
    pub const V81: u32 = 0x00080105; // RDP 8.1
    pub const V10: u32 = 0x000A0002; // RDP 10.0
    pub const V101: u32 = 0x000A0100; // RDP 10.1
    pub const V102: u32 = 0x000A0200; // RDP 10.2
    pub const V103: u32 = 0x000A0301; // RDP 10.3
    pub const V104: u32 = 0x000A0400; // RDP 10.4
    pub const V105: u32 = 0x000A0502; // RDP 10.5
    pub const V106: u32 = 0x000A0600; // RDP 10.6
    pub const V106_ERR: u32 = 0x000A0601; // Erroneous 10.6 value
    pub const V107: u32 = 0x000A0701; // RDP 10.7
}

/// Capability flags
pub mod cap_flags {
    pub const THINCLIENT: u32 = 0x00000001; // Thin client mode
    pub const SMALL_CACHE: u32 = 0x00000002; // Small cache (2560 vs 25600)
    pub const AVC420_ENABLED: u32 = 0x00000010; // H.264/AVC420 support
    pub const AVC_DISABLED: u32 = 0x00000020; // Disable H.264
    pub const AVC_THINCLIENT: u32 = 0x00000040; // H.264 thin client
    pub const SCALEDMAP_DISABLE: u32 = 0x00000080; // Disable scaled mapping
}

/// Capability set
#[derive(Debug, Clone)]
pub struct CapabilitySet {
    pub version: u32,
    pub flags: u32,
}

impl CapabilitySet {
    pub fn new(version: u32, flags: u32) -> Self {
        Self { version, flags }
    }

    /// Get default capability sets to advertise
    pub fn default_sets(small_cache: bool, avc420_enabled: bool) -> Vec<Self> {
        let mut base_flags = 0u32;
        if small_cache {
            base_flags |= cap_flags::SMALL_CACHE;
        }

        let mut caps = Vec::new();

        // RDP 8.0 (no H.264 support)
        caps.push(Self::new(cap_version::V8, base_flags));

        // RDP 8.1+ with optional H.264/AVC420
        let mut flags_81 = base_flags;
        if avc420_enabled {
            flags_81 |= cap_flags::AVC420_ENABLED;
        }

        caps.push(Self::new(cap_version::V81, flags_81));
        caps.push(Self::new(cap_version::V10, flags_81));
        caps.push(Self::new(cap_version::V101, flags_81));
        caps.push(Self::new(cap_version::V102, flags_81));
        caps.push(Self::new(cap_version::V103, flags_81));
        caps.push(Self::new(cap_version::V104, flags_81));
        caps.push(Self::new(cap_version::V105, flags_81));
        caps.push(Self::new(cap_version::V106, flags_81));
        caps.push(Self::new(cap_version::V107, flags_81));

        caps
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(12);
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&4u32.to_le_bytes()); // length = 4
        buf.extend_from_slice(&self.flags.to_le_bytes());
        buf
    }

    /// Parse from bytes
    pub fn from_bytes(data: &[u8]) -> anyhow::Result<Self> {
        if data.len() < 12 {
            anyhow::bail!("CapabilitySet too short");
        }

        let version = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let length = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

        if length != 4 {
            anyhow::bail!("Invalid capability length: {}", length);
        }

        let flags = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);

        Ok(Self { version, flags })
    }
}

/// Get version string for logging
pub fn version_string(version: u32) -> String {
    match version {
        cap_version::V8 => "8.0".to_string(),
        cap_version::V81 => "8.1".to_string(),
        cap_version::V10 => "10.0".to_string(),
        cap_version::V101 => "10.1".to_string(),
        cap_version::V102 => "10.2".to_string(),
        cap_version::V103 => "10.3".to_string(),
        cap_version::V104 => "10.4".to_string(),
        cap_version::V105 => "10.5".to_string(),
        cap_version::V106 | cap_version::V106_ERR => "10.6".to_string(),
        cap_version::V107 => "10.7".to_string(),
        _ => format!("Unknown(0x{:08X})", version),
    }
}
