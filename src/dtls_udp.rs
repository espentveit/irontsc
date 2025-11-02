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
            protocol_name,
            config.server_name
        );

        // Create TLS or DTLS context based on protocol
        let ssl_method = match config.protocol {
            EncryptionProtocol::Tls => SslMethod::tls(),
            EncryptionProtocol::Dtls => SslMethod::dtls(),
        };
        
        let mut ctx_builder =
            SslContext::builder(ssl_method)
                .with_context(|| format!("Failed to create {} context", protocol_name))?;

        // Set version to 1.2 (required by most RDP servers)
        ctx_builder
            .set_min_proto_version(Some(SslVersion::TLS1_2))
            .with_context(|| format!("Failed to set min {} version", protocol_name))?;
        ctx_builder
            .set_max_proto_version(Some(SslVersion::TLS1_2))
            .with_context(|| format!("Failed to set max {} version", protocol_name))?;

        // Configure certificate verification
        if config.verify_certificate {
            ctx_builder.set_verify(SslVerifyMode::PEER);
            ctx_builder
                .set_default_verify_paths()
                .context("Failed to load system CA certificates")?;
        } else {
            warn!("⚠️  {} certificate verification disabled (insecure, for testing only)", protocol_name);
            ctx_builder.set_verify(SslVerifyMode::NONE);
        }

        // Set recommended cipher suites for RDP
        ctx_builder
            .set_cipher_list(
                "ECDHE-RSA-AES256-GCM-SHA384:\
                 ECDHE-RSA-AES128-GCM-SHA256:\
                 AES256-GCM-SHA384:\
                 AES128-GCM-SHA256",
            )
            .context("Failed to set cipher list")?;

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
        
        info!("🤝 Starting {} 1.2 handshake with {}", protocol_name, self.server_addr);

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

        // Feed data into read BIO
        Self::write_to_rbio(ssl, data)?;

        // Continue handshake
        let ret = unsafe { ffi::SSL_do_handshake(Self::ssl_ptr(ssl)) };

        if ret == 1 {
            // Handshake complete
            info!("✅ DTLS handshake complete");
            self.handshake_complete = true;
            return Ok(None);
        }

        // Check error code
        let error_code =
            unsafe { ErrorCode::from_raw(ffi::SSL_get_error(Self::ssl_ptr(ssl), ret)) };

        match error_code {
            ErrorCode::WANT_READ => {
                // Need more data from server, extract any outgoing packets first
                let packets = Self::drain_wbio(ssl)?;
                if packets.is_empty() {
                    trace!("DTLS waiting for more server data");
                    Ok(None)
                } else {
                    trace!("DTLS handshake produced {} response packets", packets.len());
                    Ok(Some(packets))
                }
            }
            ErrorCode::WANT_WRITE => {
                // Data ready to send
                let packets = Self::drain_wbio(ssl)?;
                trace!("DTLS handshake produced {} packets", packets.len());
                Ok(Some(packets))
            }
            other => Err(anyhow!("DTLS handshake failed (error {:?})", other)),
        }
    }

    /// Check if DTLS handshake is complete
    pub fn is_handshake_complete(&self) -> bool {
        self.handshake_complete
    }

    /// Encrypt plaintext payload(s) into DTLS records.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<Vec<u8>>> {
        if !self.handshake_complete {
            return Err(anyhow!("Cannot encrypt: DTLS handshake not complete"));
        }

        if plaintext.len() > c_int::MAX as usize {
            return Err(anyhow!("Plaintext payload too large for DTLS write"));
        }

        let ssl = self
            .ssl_conn
            .as_mut()
            .context("DTLS session not initialized")?;

        Self::ssl_write_datagram(ssl, plaintext)
            .with_context(|| format!("DTLS encrypt failed for {} bytes", plaintext.len()))?;
        trace!("📤 DTLS encrypt {} bytes", plaintext.len());

        Self::drain_wbio(ssl)
    }

    /// Decrypt incoming DTLS record into plaintext payload(s).
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<Vec<u8>>> {
        if !self.handshake_complete {
            return Err(anyhow!("Cannot decrypt: DTLS handshake not complete"));
        }

        if ciphertext.len() > c_int::MAX as usize {
            return Err(anyhow!("Ciphertext payload too large for DTLS read"));
        }

        let ssl = self
            .ssl_conn
            .as_mut()
            .context("DTLS session not initialized")?;

        Self::write_to_rbio(ssl, ciphertext)?;
        Self::drain_plaintext(ssl)
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

    fn ssl_write_datagram(ssl: &mut Ssl, buf: &[u8]) -> Result<usize> {
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
                Err(anyhow!("DTLS SSL_write failed ({:?})", code))
            }
        }
    }

    fn ssl_read_datagram(ssl: &mut Ssl, buf: &mut [u8]) -> Result<Option<usize>> {
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
                    _ => Err(anyhow!("DTLS SSL_read failed ({:?})", code)),
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
                packets.push(buf);
            }

            Ok(packets)
        }
    }

    fn drain_plaintext(ssl: &mut Ssl) -> Result<Vec<Vec<u8>>> {
        let mut results = Vec::new();
        let mut buffer = vec![0u8; MAX_DTLS_RECORD_SIZE];

        while let Some(len) = Self::ssl_read_datagram(ssl, &mut buffer)? {
            trace!("📦 Decrypted DTLS payload ({} bytes)", len);
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
        };

        let dtls = DtlsUdpSocket::new(server_addr, config);
        assert!(dtls.is_ok());
    }
}
