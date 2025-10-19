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
    pub extra_data: Vec<u8>,
}

impl CapabilitySet {
    pub fn new(version: u32, flags: u32) -> Self {
        Self {
            version,
            flags,
            extra_data: Vec::new(),
        }
    }

    pub fn with_extra(version: u32, flags: u32, extra_data: Vec<u8>) -> Self {
        Self {
            version,
            flags,
            extra_data,
        }
    }

    /// Get default capability sets to advertise. Mirrors FreeRDP ordering/flags so
    /// Windows enables mixed-mode or full H.264 rendering when supported.
    pub fn default_sets(
        small_cache: bool,
        avc420_enabled: bool,
        avc444_enabled: bool,
    ) -> Vec<Self> {
        use tracing::info;
        info!(
            "🔧 Building capability sets: small_cache={}, avc420={}, avc444={}",
            small_cache, avc420_enabled, avc444_enabled
        );

        let thin_client = false;
        let scaling_supported = false;

        let mut caps = Vec::new();

        // RDP 8.0 (no H.264 support)
        let mut flags_v8 = 0u32;
        if thin_client {
            flags_v8 |= cap_flags::THINCLIENT;
        }
        if small_cache && !thin_client {
            flags_v8 |= cap_flags::SMALL_CACHE;
        }
        caps.push(Self::new(cap_version::V8, flags_v8));

        // RDP 8.1 with AVC420 support (software encoding)
        let mut flags_81 = 0u32;
        if thin_client {
            flags_81 |= cap_flags::THINCLIENT;
        }
        if small_cache {
            flags_81 |= cap_flags::SMALL_CACHE;
        }
        if avc420_enabled {
            // AVC_DISABLED is only valid for 10.x capability sets; Windows drops the channel
            // if we advertise it in the 8.1 block, so only set the positive capability here.
            flags_81 |= cap_flags::AVC420_ENABLED;
        }
        caps.push(Self::new(cap_version::V81, flags_81));

        // RDP 10.x capability sets mirror the behavior of Windows and FreeRDP clients.
        let mut caps10_flags = 0u32;
        if small_cache {
            caps10_flags |= cap_flags::SMALL_CACHE;
        }
        if !avc444_enabled {
            caps10_flags |= cap_flags::AVC_DISABLED;
        }
        if thin_client && (caps10_flags & cap_flags::AVC_DISABLED == 0) {
            caps10_flags |= cap_flags::AVC_THINCLIENT;
        }

        caps.push(Self::new(cap_version::V10, caps10_flags));

        // Version 10.1 requires a 16-byte payload (length 0x10) with 12 bytes of zeros.
        caps.push(Self::with_extra(cap_version::V101, 0, vec![0; 12]));

        caps.push(Self::new(cap_version::V102, caps10_flags));

        let flags_103 = caps10_flags & !cap_flags::SMALL_CACHE;
        caps.push(Self::new(cap_version::V103, flags_103));

        caps.push(Self::new(cap_version::V104, caps10_flags));

        if scaling_supported {
            caps.push(Self::new(cap_version::V105, caps10_flags));
            caps.push(Self::new(cap_version::V106, caps10_flags));
            caps.push(Self::new(cap_version::V106_ERR, caps10_flags));
        }

        let mut flags_107 = caps10_flags;
        if !scaling_supported {
            flags_107 |= cap_flags::SCALEDMAP_DISABLE;
        }
        caps.push(Self::new(cap_version::V107, flags_107));

        info!("✅ Advertising {} capability sets", caps.len());

        caps
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let length = 4 + self.extra_data.len();
        let mut buf = Vec::with_capacity(self.serialized_len());
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&(length as u32).to_le_bytes());
        buf.extend_from_slice(&self.flags.to_le_bytes());
        buf.extend_from_slice(&self.extra_data);
        buf
    }

    /// Total number of bytes this capability set occupies when serialized.
    pub fn serialized_len(&self) -> usize {
        // version (4) + length (4) + flags (4) + extra payload
        12 + self.extra_data.len()
    }

    /// Parse from bytes
    pub fn from_bytes(data: &[u8]) -> anyhow::Result<Self> {
        if data.len() < 12 {
            anyhow::bail!("CapabilitySet too short");
        }

        let version = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let length = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;

        if length < 4 {
            anyhow::bail!("Invalid capability length: {}", length);
        }

        if data.len() < 8 + length {
            anyhow::bail!("CapabilitySet payload truncated");
        }

        let flags = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let extra = data[12..8 + length].to_vec();

        Ok(Self {
            version,
            flags,
            extra_data: extra,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rdp81_caps_do_not_disable_avc() {
        let caps = CapabilitySet::default_sets(false, false, false);
        let v81 = caps
            .iter()
            .find(|cap| cap.version == cap_version::V81)
            .expect("V8.1 capability set missing");
        assert_eq!(v81.flags & cap_flags::AVC_DISABLED, 0);
    }
}
