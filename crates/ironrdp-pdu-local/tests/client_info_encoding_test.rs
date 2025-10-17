use ironrdp_core::{encode_vec, Decode, ReadCursor};
use ironrdp_pdu::rdp::client_info::{
    AddressFamily, ClientInfo, ClientInfoFlags, CompressionType, Credentials, ExtendedClientInfo,
    ExtendedClientOptionalInfo, PerformanceFlags, TimezoneInfo,
};
use ironrdp_pdu::rdp::headers::{BasicSecurityHeader, BasicSecurityHeaderFlags};
use ironrdp_pdu::rdp::ClientInfoPdu;

#[test]
fn test_client_info_field_order() {
    // Create credentials matching the real-world example
    let credentials = Credentials {
        domain: None,
        username: "espen".to_string(),
        password: "knad489".to_string(),
    };

    let flags = ClientInfoFlags::MOUSE
        | ClientInfoFlags::MOUSE_HAS_WHEEL
        | ClientInfoFlags::UNICODE
        | ClientInfoFlags::DISABLE_CTRL_ALT_DEL
        | ClientInfoFlags::LOGON_NOTIFY
        | ClientInfoFlags::LOGON_ERRORS
        | ClientInfoFlags::VIDEO_DISABLE
        | ClientInfoFlags::ENABLE_WINDOWS_KEY
        | ClientInfoFlags::MAXIMIZE_SHELL;

    let client_info = ClientInfo {
        credentials,
        code_page: 0,
        flags,
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
    };

    // Encode to bytes
    let encoded = encode_vec(&client_info).expect("Failed to encode ClientInfo");

    println!("=== ENCODED CLIENT INFO ===");
    println!("Total size: {} bytes", encoded.len());
    println!("Hex dump:");
    for (i, chunk) in encoded.chunks(16).enumerate() {
        print!("{:04x}  ", i * 16);
        for byte in chunk {
            print!("{:02x} ", byte);
        }
        println!();
    }

    // Parse the length fields (after codePage and flags)
    let code_page = u32::from_le_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]);
    let flags_raw = u32::from_le_bytes([encoded[4], encoded[5], encoded[6], encoded[7]]);

    let cb_domain = u16::from_le_bytes([encoded[8], encoded[9]]);
    let cb_username = u16::from_le_bytes([encoded[10], encoded[11]]);
    let cb_password = u16::from_le_bytes([encoded[12], encoded[13]]);
    let cb_alternate_shell = u16::from_le_bytes([encoded[14], encoded[15]]);
    let cb_work_dir = u16::from_le_bytes([encoded[16], encoded[17]]);

    println!("\n=== LENGTH FIELDS ===");
    println!("codePage: 0x{:08x}", code_page);
    println!("flags: 0x{:08x}", flags_raw);
    println!("cbDomain: 0x{:04x} ({} bytes)", cb_domain, cb_domain);
    println!("cbUserName: 0x{:04x} ({} bytes)", cb_username, cb_username);
    println!("cbPassword: 0x{:04x} ({} bytes)", cb_password, cb_password);
    println!(
        "cbAlternateShell: 0x{:04x} ({} bytes)",
        cb_alternate_shell, cb_alternate_shell
    );
    println!(
        "cbWorkingDir: 0x{:04x} ({} bytes)",
        cb_work_dir, cb_work_dir
    );

    // Verify expected values
    // For Unicode encoding:
    // - domain: empty = 0 bytes
    // - username: "espen" = 5 chars × 2 = 10 bytes
    // - password: "knad489" = 7 chars × 2 = 14 bytes
    // - alternate_shell: empty = 0 bytes
    // - work_dir: empty = 0 bytes

    assert_eq!(code_page, 0, "Code page should be 0");
    assert_eq!(cb_domain, 0, "Domain length should be 0 (empty domain)");
    assert_eq!(
        cb_username, 10,
        "Username length should be 10 bytes (5 Unicode chars)"
    );
    assert_eq!(
        cb_password, 14,
        "Password length should be 14 bytes (7 Unicode chars)"
    );
    assert_eq!(cb_alternate_shell, 0, "Alternate shell length should be 0");
    assert_eq!(cb_work_dir, 0, "Work dir length should be 0");

    // Verify the actual string data starts at the right position
    let strings_start = 18; // After the 5 length fields

    // Domain (empty, just null terminator: 2 bytes for Unicode)
    let domain_start = strings_start;
    let domain_null = u16::from_le_bytes([encoded[domain_start], encoded[domain_start + 1]]);
    assert_eq!(domain_null, 0, "Domain should be just a null terminator");

    // Username "espen" in Unicode (e=0x0065, s=0x0073, p=0x0070, e=0x0065, n=0x006e, null=0x0000)
    let username_start = domain_start + 2;
    let username_bytes = &encoded[username_start..username_start + 12];
    assert_eq!(username_bytes[0], 0x65); // 'e'
    assert_eq!(username_bytes[1], 0x00);
    assert_eq!(username_bytes[2], 0x73); // 's'
    assert_eq!(username_bytes[3], 0x00);
    assert_eq!(username_bytes[4], 0x70); // 'p'
    assert_eq!(username_bytes[5], 0x00);
    assert_eq!(username_bytes[6], 0x65); // 'e'
    assert_eq!(username_bytes[7], 0x00);
    assert_eq!(username_bytes[8], 0x6e); // 'n'
    assert_eq!(username_bytes[9], 0x00);
    assert_eq!(username_bytes[10], 0x00); // null
    assert_eq!(username_bytes[11], 0x00);

    // Password "knad489" in Unicode
    let password_start = username_start + 12;
    let password_bytes = &encoded[password_start..password_start + 16];
    assert_eq!(password_bytes[0], 0x6b); // 'k'
    assert_eq!(password_bytes[1], 0x00);
    assert_eq!(password_bytes[2], 0x6e); // 'n'
    assert_eq!(password_bytes[3], 0x00);
    assert_eq!(password_bytes[4], 0x61); // 'a'
    assert_eq!(password_bytes[5], 0x00);
    assert_eq!(password_bytes[6], 0x64); // 'd'
    assert_eq!(password_bytes[7], 0x00);
    assert_eq!(password_bytes[8], 0x34); // '4'
    assert_eq!(password_bytes[9], 0x00);
    assert_eq!(password_bytes[10], 0x38); // '8'
    assert_eq!(password_bytes[11], 0x00);
    assert_eq!(password_bytes[12], 0x39); // '9'
    assert_eq!(password_bytes[13], 0x00);
    assert_eq!(password_bytes[14], 0x00); // null
    assert_eq!(password_bytes[15], 0x00);

    println!("\n✅ All assertions passed!");
}

#[test]
fn test_client_info_round_trip() {
    // Create a ClientInfo with various field values
    let original = ClientInfo {
        credentials: Credentials {
            domain: Some("TESTDOMAIN".to_string()),
            username: "testuser".to_string(),
            password: "testpass123".to_string(),
        },
        code_page: 0,
        flags: ClientInfoFlags::UNICODE | ClientInfoFlags::MOUSE,
        compression_type: CompressionType::K8,
        alternate_shell: "shell.exe".to_string(),
        work_dir: "C:\\Users".to_string(),
        extra_info: ExtendedClientInfo {
            address_family: AddressFamily::INET,
            address: "10.0.0.1".to_string(),
            dir: "C:\\Windows".to_string(),
            optional_data: ExtendedClientOptionalInfo::builder()
                .timezone(TimezoneInfo::default())
                .session_id(42)
                .performance_flags(PerformanceFlags::DISABLE_WALLPAPER)
                .build(),
        },
    };

    // Encode
    let encoded = encode_vec(&original).expect("Failed to encode");

    println!("Encoded {} bytes", encoded.len());

    // Decode
    let mut cursor = ReadCursor::new(&encoded);
    let decoded = ClientInfo::decode(&mut cursor).expect("Failed to decode");

    // Verify all fields match
    assert_eq!(decoded.credentials.domain, original.credentials.domain);
    assert_eq!(decoded.credentials.username, original.credentials.username);
    assert_eq!(decoded.credentials.password, original.credentials.password);
    assert_eq!(decoded.code_page, original.code_page);
    assert_eq!(decoded.flags, original.flags);
    assert_eq!(decoded.compression_type, original.compression_type);
    assert_eq!(decoded.alternate_shell, original.alternate_shell);
    assert_eq!(decoded.work_dir, original.work_dir);
    assert_eq!(decoded.extra_info.address, original.extra_info.address);

    println!("✅ Round-trip test passed!");
}

#[test]
fn test_client_info_pdu_with_security_header() {
    // Test the complete ClientInfoPdu structure
    let pdu = ClientInfoPdu {
        security_header: BasicSecurityHeader {
            flags: BasicSecurityHeaderFlags::INFO_PKT,
        },
        client_info: ClientInfo {
            credentials: Credentials {
                domain: None,
                username: "admin".to_string(),
                password: "password".to_string(),
            },
            code_page: 0,
            flags: ClientInfoFlags::UNICODE,
            compression_type: CompressionType::K8,
            alternate_shell: String::new(),
            work_dir: String::new(),
            extra_info: ExtendedClientInfo {
                address_family: AddressFamily::INET,
                address: "127.0.0.1".to_string(),
                dir: String::new(),
                optional_data: ExtendedClientOptionalInfo::builder()
                    .timezone(TimezoneInfo::default())
                    .session_id(0)
                    .performance_flags(PerformanceFlags::default())
                    .build(),
            },
        },
    };

    // Encode
    let encoded = encode_vec(&pdu).expect("Failed to encode ClientInfoPdu");

    println!("ClientInfoPdu encoded to {} bytes", encoded.len());

    // Check security header (first 4 bytes)
    let sec_flags = u32::from_le_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]);
    assert_eq!(
        sec_flags, 0x00000040,
        "Security header should be INFO_PKT (0x40)"
    );

    // Decode
    let mut cursor = ReadCursor::new(&encoded);
    let decoded = ClientInfoPdu::decode(&mut cursor).expect("Failed to decode ClientInfoPdu");

    assert_eq!(decoded.security_header.flags, pdu.security_header.flags);
    assert_eq!(
        decoded.client_info.credentials.username,
        pdu.client_info.credentials.username
    );

    println!("✅ ClientInfoPdu test passed!");
}

#[test]
fn test_empty_credentials() {
    // Test with all empty/default values
    let client_info = ClientInfo {
        credentials: Credentials {
            domain: None,
            username: String::new(),
            password: String::new(),
        },
        code_page: 0,
        flags: ClientInfoFlags::UNICODE,
        compression_type: CompressionType::K8,
        alternate_shell: String::new(),
        work_dir: String::new(),
        extra_info: ExtendedClientInfo {
            address_family: AddressFamily::INET,
            address: String::new(),
            dir: String::new(),
            optional_data: ExtendedClientOptionalInfo::builder()
                .timezone(TimezoneInfo::default())
                .session_id(0)
                .performance_flags(PerformanceFlags::default())
                .build(),
        },
    };

    let encoded = encode_vec(&client_info).expect("Failed to encode");

    // All length fields should be 0
    let cb_domain = u16::from_le_bytes([encoded[8], encoded[9]]);
    let cb_username = u16::from_le_bytes([encoded[10], encoded[11]]);
    let cb_password = u16::from_le_bytes([encoded[12], encoded[13]]);

    assert_eq!(cb_domain, 0);
    assert_eq!(cb_username, 0);
    assert_eq!(cb_password, 0);

    // Round trip
    let mut cursor = ReadCursor::new(&encoded);
    let decoded = ClientInfo::decode(&mut cursor).expect("Failed to decode");

    assert_eq!(decoded.credentials.username, "");
    assert_eq!(decoded.credentials.password, "");

    println!("✅ Empty credentials test passed!");
}
