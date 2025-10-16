/// This test generates a binary packet file that can be analyzed with Wireshark/tshark
/// to verify the ClientInfo PDU is correctly formed according to the RDP specification.
///
/// Run with: cargo test --test generate_test_packet -- --nocapture
/// Then analyze with: tshark -r /tmp/clientinfo_test.bin -V

use ironrdp_pdu::rdp::client_info::{
    AddressFamily, ClientInfo, ClientInfoFlags, CompressionType, Credentials, ExtendedClientInfo,
    ExtendedClientOptionalInfo, PerformanceFlags, TimezoneInfo,
};
use ironrdp_pdu::rdp::headers::{BasicSecurityHeader, BasicSecurityHeaderFlags};
use ironrdp_pdu::rdp::ClientInfoPdu;
use ironrdp_pdu::{mcs, x224::X224};
use ironrdp_core::encode_vec;
use std::fs::File;
use std::io::Write;

#[test]
fn generate_clientinfo_packet_for_wireshark() {
    // Create the exact same ClientInfo as in the real connection
    let client_info_pdu = ClientInfoPdu {
        security_header: BasicSecurityHeader {
            flags: BasicSecurityHeaderFlags::INFO_PKT,
        },
        client_info: ClientInfo {
            credentials: Credentials {
                domain: None,
                username: "espen".to_string(),
                password: "knad489".to_string(),
            },
            code_page: 0,
            flags: ClientInfoFlags::MOUSE
                | ClientInfoFlags::MOUSE_HAS_WHEEL
                | ClientInfoFlags::UNICODE
                | ClientInfoFlags::DISABLE_CTRL_ALT_DEL
                | ClientInfoFlags::LOGON_NOTIFY
                | ClientInfoFlags::LOGON_ERRORS
                | ClientInfoFlags::VIDEO_DISABLE
                | ClientInfoFlags::ENABLE_WINDOWS_KEY
                | ClientInfoFlags::MAXIMIZE_SHELL,
            compression_type: CompressionType::K8,
            alternate_shell: String::new(),
            work_dir: String::new(),
            extra_info: ExtendedClientInfo {
                address_family: AddressFamily::INET,
                address: "192.168.0.105".to_string(),
                dir: String::new(),
                optional_data: ExtendedClientOptionalInfo::builder()
                    .timezone(TimezoneInfo::default())
                    .session_id(0)
                    .performance_flags(PerformanceFlags::default())
                    .build(),
            },
        },
    };

    // Encode just the ClientInfoPdu (without MCS/X224 wrapping for now)
    let client_info_bytes = encode_vec(&client_info_pdu).expect("Failed to encode ClientInfoPdu");

    println!("\n=== CLIENT INFO PDU ===");
    println!("Size: {} bytes", client_info_bytes.len());
    println!("\nHex dump:");
    for (i, chunk) in client_info_bytes.chunks(16).enumerate() {
        print!("{:04x}  ", i * 16);
        for byte in chunk {
            print!("{:02x} ", byte);
        }
        print!(" ");
        for byte in chunk {
            if *byte >= 0x20 && *byte <= 0x7e {
                print!("{}", *byte as char);
            } else {
                print!(".");
            }
        }
        println!();
    }

    // Write to file
    let output_path = "/tmp/clientinfo_raw.bin";
    let mut file = File::create(output_path).expect("Failed to create file");
    file.write_all(&client_info_bytes).expect("Failed to write file");
    println!("\n✅ Wrote {} bytes to {}", client_info_bytes.len(), output_path);

    // Now wrap it in MCS Send Data Request
    let user_data = client_info_bytes;
    let mcs_pdu = mcs::SendDataRequest {
        initiator_id: 1007, // Example user channel ID
        channel_id: 1003,   // Example IO channel ID
        user_data: std::borrow::Cow::Owned(user_data),
    };

    let mcs_bytes = encode_vec(&X224(mcs_pdu)).expect("Failed to encode MCS PDU");

    println!("\n=== MCS SEND DATA REQUEST (with X.224) ===");
    println!("Size: {} bytes", mcs_bytes.len());

    let mcs_output_path = "/tmp/clientinfo_mcs.bin";
    let mut file = File::create(mcs_output_path).expect("Failed to create file");
    file.write_all(&mcs_bytes).expect("Failed to write file");
    println!("✅ Wrote {} bytes to {}", mcs_bytes.len(), mcs_output_path);

    println!("\n=== ANALYSIS COMMANDS ===");
    println!("To analyze with Wireshark dissector:");
    println!("  tshark -r {} -V | less", mcs_output_path);
    println!("\nTo compare hex dumps:");
    println!("  hexdump -C {}", output_path);
    println!("  hexdump -C {}", mcs_output_path);
    
    println!("\n=== VERIFICATION ===");
    println!("Expected length field values:");
    println!("  cbDomain:        0x0000 (0 bytes)");
    println!("  cbUserName:      0x000a (10 bytes for 'espen')");
    println!("  cbPassword:      0x000e (14 bytes for 'knad489')");
    println!("  cbAlternateShell: 0x0000 (0 bytes)");
    println!("  cbWorkingDir:    0x0000 (0 bytes)");
}

#[test]
fn compare_with_malformed_packet() {
    println!("\n=== ORIGINAL MALFORMED PACKET (from hex dump) ===");
    
    // The malformed packet from your trace (just the ClientInfo portion)
    let malformed_hex = "
40 00 00 00 00 00 00 00 00 00 00 7b 01 4b 00 00 00 0a 00 0e 00
00 00 00 00 00 00 65 00 73 00 70 00 65 00 6e 00 00 00 6b 00 6e 00 61 00 64 00 34 00 38 00 39
00 00 00 00 00 00 00 02 00 1c 00 31 00 39 00 32 00 2e 00 31 00 36 00 38 00 2e 00 30 00 2e 00 31
00 30 00 35 00 00 00 02 00 00 00
";

    let malformed_bytes: Vec<u8> = malformed_hex
        .split_whitespace()
        .filter_map(|s| u8::from_str_radix(s, 16).ok())
        .collect();

    println!("Malformed packet size: {} bytes", malformed_bytes.len());
    
    // Parse the bad length fields
    let bad_cb_domain = u16::from_le_bytes([malformed_bytes[12], malformed_bytes[13]]);
    let bad_cb_username = u16::from_le_bytes([malformed_bytes[14], malformed_bytes[15]]);
    let bad_cb_password = u16::from_le_bytes([malformed_bytes[16], malformed_bytes[17]]);

    println!("\nMALFORMED packet length fields:");
    println!("  cbDomain:     0x{:04x} ({} bytes) ❌ WRONG! Should be 0", bad_cb_domain, bad_cb_domain);
    println!("  cbUserName:   0x{:04x} ({} bytes) ❌ WRONG! Should be 10", bad_cb_username, bad_cb_username);
    println!("  cbPassword:   0x{:04x} ({} bytes) ❌ WRONG! Should be 14", bad_cb_password, bad_cb_password);

    // Now generate the correct packet
    let correct_pdu = ClientInfoPdu {
        security_header: BasicSecurityHeader {
            flags: BasicSecurityHeaderFlags::INFO_PKT,
        },
        client_info: ClientInfo {
            credentials: Credentials {
                domain: None,
                username: "espen".to_string(),
                password: "knad489".to_string(),
            },
            code_page: 0,
            flags: ClientInfoFlags::UNICODE | ClientInfoFlags::MOUSE,
            compression_type: CompressionType::K8,
            alternate_shell: String::new(),
            work_dir: String::new(),
            extra_info: ExtendedClientInfo {
                address_family: AddressFamily::INET,
                address: "192.168.0.105".to_string(),
                dir: String::new(),
                optional_data: ExtendedClientOptionalInfo::builder()
                    .timezone(TimezoneInfo::default())
                    .session_id(0)
                    .performance_flags(PerformanceFlags::default())
                    .build(),
            },
        },
    };

    let correct_bytes = encode_vec(&correct_pdu).expect("Failed to encode");

    // ClientInfoPdu structure:
    // 0-3: Security header
    // 4-7: codePage (4 bytes)
    // 8-11: flags (4 bytes)  
    // 12-13: cbDomain
    // 14-15: cbUserName
    // 16-17: cbPassword
    let correct_cb_domain = u16::from_le_bytes([correct_bytes[12], correct_bytes[13]]);
    let correct_cb_username = u16::from_le_bytes([correct_bytes[14], correct_bytes[15]]);
    let correct_cb_password = u16::from_le_bytes([correct_bytes[16], correct_bytes[17]]);

    println!("\nCORRECTED packet length fields:");
    println!("  cbDomain:     0x{:04x} ({} bytes) ✅ CORRECT!", correct_cb_domain, correct_cb_domain);
    println!("  cbUserName:   0x{:04x} ({} bytes) ✅ CORRECT!", correct_cb_username, correct_cb_username);
    println!("  cbPassword:   0x{:04x} ({} bytes) ✅ CORRECT!", correct_cb_password, correct_cb_password);

    assert_eq!(correct_cb_domain, 0, "Domain length should be 0");
    assert_eq!(correct_cb_username, 10, "Username length should be 10");
    assert_eq!(correct_cb_password, 14, "Password length should be 14");

    println!("\n✅ Fix verified! The corrected packet has proper length fields.");
}
