/// DTLS wrapper for UDP datagrams (MS-RDPEMT requirement)
///
/// Provides DTLS 1.2 encryption/decryption for the MS-RDPEMT UDP transport.
use anyhow::{Context, Result, anyhow};
use foreign_types::ForeignType;
use libc::{c_int, c_void};
use openssl::ssl::{
    ErrorCode, Ssl, SslContext, SslMethod, SslMode, SslOptions, SslVerifyMode, SslVersion,
};
use openssl_sys as ffi;
use std::net::SocketAddr;
use tracing::{debug, info, trace, warn};

// Additional FFI declarations not available in openssl_sys
unsafe extern "C" {
    fn BIO_ctrl_pending(b: *mut ffi::BIO) -> libc::size_t;
    fn ERR_error_string_n(e: libc::c_ulong, buf: *mut libc::c_char, len: libc::size_t);
}

const MAX_DTLS_RECORD_SIZE: usize = 64 * 1024;
const CLIENT_MTU: u32 = 1232;

/// Protocol type for MS-RDPEMT encryption
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionProtocol {
    /// TLS 1.2 for Reliable mode
    Tls,
    /// DTLS 1.2 for Lossy mode
    Dtls,
}

/// TLS/DTLS configuration for MS-RDPEMT
pub struct DtlsConfig {
    /// Server hostname for certificate validation
    pub server_name: String,
    /// Whether to verify server certificate (should be true in production)
    pub verify_certificate: bool,
    /// Protocol to use (TLS for Reliable, DTLS for Lossy)
    pub protocol: EncryptionProtocol,
}

/// TLS/DTLS wrapper for encrypting/decrypting UDP datagrams
/// Note: This does NOT handle socket I/O - packets must be wrapped in RDP UDP DATA frames
pub struct DtlsUdpSocket {
    /// SSL context
    ssl_context: SslContext,
    /// Server address (for logging/debugging)
    server_addr: SocketAddr,
    /// Configuration
    config: DtlsConfig,
    /// Whether handshake is complete
    handshake_complete: bool,
    /// SSL connection instance (created after handshake)
    ssl_conn: Option<Ssl>,
    /// Protocol being used
    protocol: EncryptionProtocol,
}

impl DtlsUdpSocket {
    /// Create a new TLS/DTLS encryption layer (does not handle socket I/O)
    pub fn new(server_addr: SocketAddr, config: DtlsConfig) -> Result<Self> {
        let protocol_name = match config.protocol {
            EncryptionProtocol::Tls => "TLS",
            EncryptionProtocol::Dtls => "DTLS",
        };

        info!(
            "🔐 Initializing {} for MS-RDPEMT (server: {})",
            protocol_name, config.server_name
        );

        // Create TLS or DTLS context based on protocol
        let ssl_method = match config.protocol {
            EncryptionProtocol::Tls => SslMethod::tls(),
            EncryptionProtocol::Dtls => SslMethod::dtls(),
        };

        let mut ctx_builder = SslContext::builder(ssl_method)
            .with_context(|| format!("Failed to create {} context", protocol_name))?;

        // Set version range to support TLS 1.0 through 1.3 (match FreeRDP)
        // The working capture shows: TLS 1.3, 1.2, 1.1, 1.0 support
        ctx_builder
            .set_min_proto_version(Some(SslVersion::TLS1))
            .with_context(|| format!("Failed to set min {} version", protocol_name))?;
        ctx_builder
            .set_max_proto_version(Some(SslVersion::TLS1_3))
            .with_context(|| format!("Failed to set max {} version", protocol_name))?;

        // Configure certificate verification
        if config.verify_certificate {
            ctx_builder.set_verify(SslVerifyMode::PEER);
            ctx_builder
                .set_default_verify_paths()
                .context("Failed to load system CA certificates")?;
        } else {
            warn!(
                "⚠️  {} certificate verification disabled (insecure, for testing only)",
                protocol_name
            );
            ctx_builder.set_verify(SslVerifyMode::NONE);
        }

        //Set TLS 1.3 cipher suites exactly matching FreeRDP
        // FreeRDP uses only AES-GCM variants (no ChaCha20)
        // OpenSSL 3.x tends to add ChaCha20 by default, so we need to be explicit
        // Using a strict list without ChaCha20
        ctx_builder
            .set_ciphersuites(
                "TLS_AES_256_GCM_SHA384:\
                 TLS_AES_128_GCM_SHA256",
            )
            .context("Failed to set TLS 1.3 ciphersuites")?;

        // TLS 1.0-1.2 cipher list matching FreeRDP exactly
        // Note: OpenSSL naming differs from RFC naming (e.g., SHA384 vs CBC-SHA384)
        ctx_builder
            .set_cipher_list(
                "ECDHE-ECDSA-AES256-GCM-SHA384:\
                 ECDHE-ECDSA-AES128-GCM-SHA256:\
                 ECDHE-RSA-AES256-GCM-SHA384:\
                 ECDHE-RSA-AES128-GCM-SHA256:\
                 ECDHE-ECDSA-AES256-SHA384:\
                 ECDHE-ECDSA-AES128-SHA256:\
                 ECDHE-RSA-AES256-SHA384:\
                 ECDHE-RSA-AES128-SHA256:\
                 ECDHE-ECDSA-AES256-SHA:\
                 ECDHE-ECDSA-AES128-SHA:\
                 ECDHE-RSA-AES256-SHA:\
                 ECDHE-RSA-AES128-SHA:\
                 AES256-GCM-SHA384:\
                 AES128-GCM-SHA256:\
                 AES256-SHA256:\
                 AES128-SHA256:\
                 AES256-SHA:\
                 AES128-SHA",
            )
            .context("Failed to set cipher list")?;

        // Disable ChaCha20 globally for both TLS 1.3 and TLS 1.2
        // This is a workaround for OpenSSL adding ChaCha20 by default
        let mut options = SslOptions::NO_TICKET | SslOptions::CIPHER_SERVER_PREFERENCE;
        
        // Enable middlebox compatibility mode to send ChangeCipherSpec
        // This makes TLS 1.3 handshakes look like TLS 1.2 for compatibility
        // The Windows RDP server expects to receive ChangeCipherSpec
        options |= SslOptions::ENABLE_MIDDLEBOX_COMPAT;
        
        ctx_builder.set_options(options);

        // DTLS-specific options
        if config.protocol == EncryptionProtocol::Dtls {
            // Ensure OpenSSL does not attempt to probe MTU on its own
            ctx_builder.set_options(SslOptions::NO_QUERY_MTU);
        }

        ctx_builder.set_mode(SslMode::AUTO_RETRY);

        let ssl_context = ctx_builder.build();
        let protocol = config.protocol;

        Ok(Self {
            ssl_context,
            server_addr,
            config,
            handshake_complete: false,
            ssl_conn: None,
            protocol,
        })
    }

    /// Start TLS/DTLS handshake and return ClientHello packet to send
    /// Call process_handshake_data() with server responses until handshake completes
    pub fn start_handshake(&mut self) -> Result<Vec<u8>> {
        let protocol_name = match self.protocol {
            EncryptionProtocol::Tls => "TLS",
            EncryptionProtocol::Dtls => "DTLS",
        };

        info!(
            "🤝 Starting {} 1.2 handshake with {}",
            protocol_name, self.server_addr
        );

        // Create SSL connection instance
        let mut ssl = Ssl::new(&self.ssl_context).context("Failed to create SSL connection")?;

        // Set SNI (Server Name Indication)
        ssl.set_hostname(&self.config.server_name)
            .context("Failed to set SNI hostname")?;

        // Configure protocol-specific options
        ssl.set_connect_state();

        // Only set MTU for DTLS (not needed for TLS)
        if self.protocol == EncryptionProtocol::Dtls {
            unsafe {
                let ssl_ptr = Self::ssl_ptr(&ssl);
                if ffi::SSL_set_mtu(ssl_ptr, CLIENT_MTU as libc::c_long) <= 0 {
                    debug!("Unable to set DTLS MTU to {}", CLIENT_MTU);
                }
            }
        }

        // Attach in-memory BIOs for manual packet handling
        let (rbio, wbio) = Self::create_memory_bios()?;
        unsafe { ffi::SSL_set_bio(Self::ssl_ptr(&ssl), rbio, wbio) };

        // Initiate handshake to generate ClientHello
        let ret = unsafe { ffi::SSL_do_handshake(Self::ssl_ptr(&ssl)) };

        if ret == 1 {
            // Unlikely to complete on first call, but handle it
            self.ssl_conn = Some(ssl);
            self.handshake_complete = true;
            return Ok(Vec::new());
        }

        // Extract ClientHello from write BIO
        let packets = Self::drain_wbio(&mut ssl)?;

        // Store SSL connection for continued handshake
        self.ssl_conn = Some(ssl);

        // Return first packet (ClientHello)
        packets
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("DTLS handshake did not produce ClientHello"))
    }

    /// Process incoming DTLS handshake data and return any outgoing packets
    /// Returns Ok(Some(packets)) if there are packets to send
    /// Returns Ok(None) if handshake is complete
    pub fn process_handshake_data(&mut self, data: &[u8]) -> Result<Option<Vec<Vec<u8>>>> {
        if self.handshake_complete {
            return Ok(None);
        }

        let ssl = self
            .ssl_conn
            .as_mut()
            .context("DTLS handshake not started - call start_handshake() first")?;

        debug!(
            "🔍 Processing {} bytes of TLS/DTLS handshake data. First 32 bytes: {:02x?}",
            data.len(),
            &data[..data.len().min(32)]
        );

        // Parse TLS record header to see what we're receiving
        if data.len() >= 5 {
            let record_type = data[0];
            let version = u16::from_be_bytes([data[1], data[2]]);
            let record_length = u16::from_be_bytes([data[3], data[4]]);
            debug!(
                "🔍 TLS record: type={} version=0x{:04x} length={} (total_data={})",
                record_type,
                version,
                record_length,
                data.len()
            );

            // Check if we have multiple records
            let expected_total = 5 + record_length as usize;
            if data.len() > expected_total {
                debug!(
                    "🔍 Multiple TLS records detected: first={} bytes, total={} bytes",
                    expected_total,
                    data.len()
                );
                // Check what the next record is
                if expected_total + 5 <= data.len() {
                    let next_type = data[expected_total];
                    let next_version =
                        u16::from_be_bytes([data[expected_total + 1], data[expected_total + 2]]);
                    let next_length =
                        u16::from_be_bytes([data[expected_total + 3], data[expected_total + 4]]);
                    debug!(
                        "🔍   Next record: type={} version=0x{:04x} length={}",
                        next_type, next_version, next_length
                    );
                }
            } else if data.len() < expected_total {
                warn!(
                    "🔍 Incomplete TLS record: have {} bytes, need {} bytes",
                    data.len(),
                    expected_total
                );
            }
        }

        // Feed data into read BIO
        Self::write_to_rbio(ssl, data)?;

        debug!(
            "🔍 Calling SSL_do_handshake after feeding {} bytes...",
            data.len()
        );

        // Continue handshake
        let ret = unsafe { ffi::SSL_do_handshake(Self::ssl_ptr(ssl)) };

        debug!("🔍 SSL_do_handshake returned: {}", ret);

        // Check what protocol version was negotiated
        let ssl_version = unsafe { ffi::SSL_version(Self::ssl_ptr(ssl)) };
        let version_str = match ssl_version {
            0x0300 => "SSL 3.0",
            0x0301 => "TLS 1.0",
            0x0302 => "TLS 1.1",
            0x0303 => "TLS 1.2",
            0x0304 => "TLS 1.3",
            _ => "Unknown",
        };
        debug!(
            "🔍 Negotiated protocol version: 0x{:04x} ({})",
            ssl_version, version_str
        );

        // IMPORTANT: Always check for outgoing data first, even if there was an error
        // OpenSSL might have prepared a response before encountering an error
        let outgoing_packets = Self::drain_wbio(ssl)?;
        if !outgoing_packets.is_empty() {
            debug!(
                "🔍 OpenSSL produced {} response packet(s) during handshake",
                outgoing_packets.len()
            );
        }

        if ret == 1 {
            // Handshake complete
            let protocol_name = match self.protocol {
                EncryptionProtocol::Tls => "TLS",
                EncryptionProtocol::Dtls => "DTLS",
            };
            info!("✅ {} handshake complete", protocol_name);
            // Note: Don't set handshake_complete = true yet!
            // The caller needs to send the final handshake messages (ChangeCipherSpec, Finished)
            // BEFORE encryption kicks in. The caller will mark it complete after sending.
            if outgoing_packets.is_empty() {
                // No final messages to send, mark complete now
                self.handshake_complete = true;
                return Ok(None);
            } else {
                // Return final messages but DON'T mark complete yet
                // Caller must call mark_handshake_complete() after sending these
                return Ok(Some(outgoing_packets));
            }
        }

        // Check error code
        let error_code =
            unsafe { ErrorCode::from_raw(ffi::SSL_get_error(Self::ssl_ptr(ssl), ret)) };

        match error_code {
            ErrorCode::WANT_READ => {
                // Need more data from server
                if outgoing_packets.is_empty() {
                    trace!("DTLS waiting for more server data");
                    Ok(None)
                } else {
                    trace!(
                        "DTLS handshake produced {} response packets",
                        outgoing_packets.len()
                    );
                    Ok(Some(outgoing_packets))
                }
            }
            ErrorCode::WANT_WRITE => {
                // Data ready to send (already extracted above)
                trace!("DTLS handshake produced {} packets", outgoing_packets.len());
                Ok(Some(outgoing_packets))
            }
            other => {
                // Get detailed error information
                let error_str = unsafe {
                    let err = ffi::ERR_get_error();
                    if err == 0 {
                        // No error in queue - might be normal (e.g., need more data)
                        // Treat like WANT_READ if we have packets to send
                        if !outgoing_packets.is_empty() {
                            debug!(
                                "🔍 SSL_do_handshake needs more data, but produced {} packets - sending them",
                                outgoing_packets.len()
                            );
                            return Ok(Some(outgoing_packets));
                        }
                        format!("No OpenSSL error details (error code {:?})", other)
                    } else {
                        let mut buf = vec![0u8; 256];
                        ERR_error_string_n(err, buf.as_mut_ptr() as *mut i8, buf.len());
                        let err_str = std::ffi::CStr::from_ptr(buf.as_ptr() as *const i8)
                            .to_string_lossy()
                            .to_string();
                        format!(
                            "OpenSSL error: {} (code {:?}, raw: 0x{:x})",
                            err_str, other, err
                        )
                    }
                };
                warn!("🔍 TLS/DTLS handshake error details: {}", error_str);

                // Even though there's an error, we might have produced packets that need to be sent
                // (e.g., a Finished message before encountering a decryption error on the next record)
                if !outgoing_packets.is_empty() {
                    warn!(
                        "⚠️  Returning {} packets despite error - they may need to be sent",
                        outgoing_packets.len()
                    );
                    Ok(Some(outgoing_packets))
                } else {
                    Err(anyhow!("DTLS handshake failed: {}", error_str))
                }
            }
        }
    }

    /// Check if DTLS handshake is complete
    pub fn is_handshake_complete(&self) -> bool {
        self.handshake_complete
    }

    /// Manually mark the handshake as complete (e.g., when tunnel establishment confirms encryption is ready)
    pub fn mark_handshake_complete(&mut self) {
        self.handshake_complete = true;
    }

    /// Encrypt plaintext payload(s) into DTLS records.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<Vec<u8>>> {
        if !self.handshake_complete {
            return Err(anyhow!("Cannot encrypt: handshake not complete"));
        }

        if plaintext.len() > c_int::MAX as usize {
            return Err(anyhow!("Plaintext payload too large for write"));
        }

        let ssl = self
            .ssl_conn
            .as_mut()
            .context("SSL session not initialized")?;

        Self::ssl_write_datagram(ssl, plaintext, self.protocol)
            .with_context(|| format!("Encrypt failed for {} bytes", plaintext.len()))?;
        let protocol_name = match self.protocol {
            EncryptionProtocol::Tls => "TLS",
            EncryptionProtocol::Dtls => "DTLS",
        };
        trace!("📤 {} encrypt {} bytes", protocol_name, plaintext.len());

        Self::drain_wbio(ssl)
    }

    /// Decrypt incoming DTLS record into plaintext payload(s).
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<Vec<u8>>> {
        if !self.handshake_complete {
            return Err(anyhow!("Cannot decrypt: handshake not complete"));
        }

        if ciphertext.len() > c_int::MAX as usize {
            return Err(anyhow!("Ciphertext payload too large for read"));
        }

        let ssl = self
            .ssl_conn
            .as_mut()
            .context("SSL session not initialized")?;

        Self::write_to_rbio(ssl, ciphertext)?;
        Self::drain_plaintext(ssl, self.protocol)
    }

    #[inline]
    fn ssl_ptr(ssl: &Ssl) -> *mut ffi::SSL {
        ForeignType::as_ptr(ssl)
    }

    fn create_memory_bios() -> Result<(*mut ffi::BIO, *mut ffi::BIO)> {
        unsafe {
            let rbio = ffi::BIO_new(ffi::BIO_s_mem());
            if rbio.is_null() {
                return Err(anyhow!("Failed to create DTLS read BIO"));
            }

            let wbio = ffi::BIO_new(ffi::BIO_s_mem());
            if wbio.is_null() {
                ffi::BIO_free_all(rbio);
                return Err(anyhow!("Failed to create DTLS write BIO"));
            }

            Ok((rbio, wbio))
        }
    }

    fn ssl_write_datagram(ssl: &mut Ssl, buf: &[u8], protocol: EncryptionProtocol) -> Result<usize> {
        unsafe {
            let ssl_ptr = Self::ssl_ptr(ssl);
            if buf.is_empty() {
                return Ok(0);
            }

            let len = usize::min(buf.len(), c_int::MAX as usize) as c_int;
            let ret = ffi::SSL_write(ssl_ptr, buf.as_ptr() as *const c_void, len);

            if ret > 0 {
                Ok(ret as usize)
            } else {
                let code = ErrorCode::from_raw(ffi::SSL_get_error(ssl_ptr, ret));
                let protocol_name = match protocol {
                    EncryptionProtocol::Tls => "TLS",
                    EncryptionProtocol::Dtls => "DTLS",
                };
                Err(anyhow!("{} SSL_write failed ({:?})", protocol_name, code))
            }
        }
    }

    fn ssl_read_datagram(ssl: &mut Ssl, buf: &mut [u8], protocol: EncryptionProtocol) -> Result<Option<usize>> {
        unsafe {
            let ssl_ptr = Self::ssl_ptr(ssl);
            if buf.is_empty() {
                return Ok(None);
            }

            let len = usize::min(buf.len(), c_int::MAX as usize) as c_int;
            let ret = ffi::SSL_read(ssl_ptr, buf.as_mut_ptr() as *mut c_void, len);

            if ret > 0 {
                Ok(Some(ret as usize))
            } else {
                let code = ErrorCode::from_raw(ffi::SSL_get_error(ssl_ptr, ret));
                match code {
                    ErrorCode::WANT_READ | ErrorCode::ZERO_RETURN => Ok(None),
                    _ => {
                        let protocol_name = match protocol {
                            EncryptionProtocol::Tls => "TLS",
                            EncryptionProtocol::Dtls => "DTLS",
                        };
                        Err(anyhow!("{} SSL_read failed ({:?})", protocol_name, code))
                    }
                }
            }
        }
    }

    fn drain_wbio(ssl: &mut Ssl) -> Result<Vec<Vec<u8>>> {
        unsafe {
            let wbio = ffi::SSL_get_wbio(Self::ssl_ptr(ssl));
            if wbio.is_null() {
                return Err(anyhow!("DTLS write BIO not present"));
            }

            let mut packets = Vec::new();
            loop {
                let pending = BIO_ctrl_pending(wbio);
                if pending <= 0 {
                    break;
                }

                let to_read = std::cmp::min(pending as usize, MAX_DTLS_RECORD_SIZE).max(1);
                let mut buf = vec![0u8; to_read];
                let read = ffi::BIO_read(wbio, buf.as_mut_ptr() as *mut c_void, to_read as c_int);

                if read <= 0 {
                    break;
                }

                buf.truncate(read as usize);
                
                // Log TLS record type for debugging
                if buf.len() >= 1 {
                    let record_type = buf[0];
                    let type_name = match record_type {
                        20 => "ChangeCipherSpec",
                        21 => "Alert",
                        22 => "Handshake",
                        23 => "Application Data",
                        _ => "Unknown",
                    };
                    debug!("🔍 OpenSSL produced {} byte TLS record, type={} ({})", 
                           buf.len(), record_type, type_name);
                }
                
                packets.push(buf);
            }

            Ok(packets)
        }
    }

    fn drain_plaintext(ssl: &mut Ssl, protocol: EncryptionProtocol) -> Result<Vec<Vec<u8>>> {
        let mut results = Vec::new();
        let mut buffer = vec![0u8; MAX_DTLS_RECORD_SIZE];

        while let Some(len) = Self::ssl_read_datagram(ssl, &mut buffer, protocol)? {
            let protocol_name = match protocol {
                EncryptionProtocol::Tls => "TLS",
                EncryptionProtocol::Dtls => "DTLS",
            };
            trace!("📦 Decrypted {} payload ({} bytes)", protocol_name, len);
            results.push(buffer[..len].to_vec());
        }

        Ok(results)
    }

    fn write_to_rbio(ssl: &mut Ssl, data: &[u8]) -> Result<()> {
        unsafe {
            let rbio = ffi::SSL_get_rbio(Self::ssl_ptr(ssl));
            if rbio.is_null() {
                return Err(anyhow!("DTLS read BIO not present"));
            }

            let written = ffi::BIO_write(rbio, data.as_ptr() as *const c_void, data.len() as c_int);

            if written <= 0 {
                return Err(anyhow!("Failed to feed ciphertext into DTLS BIO"));
            }
        }

        Ok(())
    }

    // OpenSSL 3.x removed DTLSv1_handle_timeout and DTLSv1_get_timeout
    // DTLS retransmissions are handled internally by OpenSSL with memory BIOs
    // We just need to keep calling SSL_do_handshake() and feeding packets
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[tokio::test]
    async fn test_dtls_creation() {
        let server_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 3389);

        let config = DtlsConfig {
            server_name: "test.example.com".to_string(),
            verify_certificate: false,
            protocol: EncryptionProtocol::Dtls,
        };

        let dtls = DtlsUdpSocket::new(server_addr, config);
        assert!(dtls.is_ok());
    }
}
