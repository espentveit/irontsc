use ironrdp_core::Encode;
use ironrdp_pdu::rdp::client_info::{
    DayOfWeek, DayOfWeekOccurrence, ExtendedClientOptionalInfo, Month, OptionalSystemTime,
    PerformanceFlags, SystemTime, TimezoneInfo,
};

#[test]
fn test_timezone_encoding() {
    let timezone = TimezoneInfo {
        bias: 0,
        standard_name: "Coordinated Universal Time".to_string(),
        standard_date: OptionalSystemTime(Some(SystemTime {
            month: Month::October,
            day_of_week: DayOfWeek::Sunday,
            day: DayOfWeekOccurrence::Last,
            hour: 3,
            minute: 0,
            second: 0,
            milliseconds: 0,
        })),
        standard_bias: 0,
        daylight_name: "Coordinated Universal Time".to_string(),
        daylight_date: OptionalSystemTime(Some(SystemTime {
            month: Month::March,
            day_of_week: DayOfWeek::Sunday,
            day: DayOfWeekOccurrence::Last,
            hour: 2,
            minute: 0,
            second: 0,
            milliseconds: 0,
        })),
        daylight_bias: 0,
    };

    let optional_info = ExtendedClientOptionalInfo::builder()
        .timezone(timezone)
        .session_id(2)
        .performance_flags(
            PerformanceFlags::DISABLE_FULLWINDOWDRAG | PerformanceFlags::DISABLE_MENUANIMATIONS,
        )
        .build();

    let mut buffer = vec![0u8; 512];
    let mut cursor = ironrdp_core::WriteCursor::new(&mut buffer);

    optional_info.encode(&mut cursor).expect("encode failed");

    let written = cursor.pos();
    println!("Encoded {} bytes", written);
    println!("Hex dump:");
    for (i, chunk) in buffer[..written].chunks(16).enumerate() {
        print!("{:04x}  ", i * 16);
        for byte in chunk {
            print!("{:02x} ", byte);
        }
        println!();
    }

    // Check that timezone is actually encoded
    assert!(
        written > 172,
        "Timezone should be 172 bytes, got {}",
        written
    );

    // Check for the string "Coordinated Universal Time" in UTF-16
    let expected_utf16: Vec<u8> = "Coordinated Universal Time"
        .encode_utf16()
        .flat_map(|c| c.to_le_bytes())
        .collect();

    let buffer_str = String::from_utf8_lossy(&buffer[..written]);
    println!(
        "Buffer contains 'Coordinated': {}",
        buffer
            .windows(expected_utf16.len())
            .any(|w| w == expected_utf16)
    );
}
