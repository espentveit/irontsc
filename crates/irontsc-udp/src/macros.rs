//! Helper macros for building RDPUDP packets in a declarative style.
//!
//! These macros allow call-sites to construct packet structures without
//! repeatedly writing field-by-field boilerplate initialisation code.

#[macro_export]
macro_rules! rdpudp_v1_packet {
    (@set_flag $packet:ident, $flag:expr, $enabled:expr) => {{
        if $enabled {
            $packet.header.flags |= $flag;
        } else {
            $packet.header.flags.remove($flag);
        }
    }};
    (@assign $packet:ident, header, $value:expr) => {{
        $packet.header = $value;
    }};
    (@assign $packet:ident, ack, $value:expr) => {{
        let value = $value;
        rdpudp_v1_packet!(@set_flag $packet, $crate::v1::HeaderFlags::ACK, value.is_some());
        $packet.ack = value;
    }};
    (@assign $packet:ident, ack_of_acks, $value:expr) => {{
        let value = $value;
        rdpudp_v1_packet!(
            @set_flag $packet,
            $crate::v1::HeaderFlags::ACK_OF_ACKS,
            value.is_some()
        );
        $packet.ack_of_acks = value;
    }};
    (@assign $packet:ident, correlation_id, $value:expr) => {{
        let value = $value;
        rdpudp_v1_packet!(
            @set_flag $packet,
            $crate::v1::HeaderFlags::CORRELATION_ID,
            value.is_some()
        );
        $packet.correlation_id = value;
    }};
    (@assign $packet:ident, syn_data, $value:expr) => {{
        $packet.syn_data = $value;
    }};
    (@assign $packet:ident, syn_data_ex, $value:expr) => {{
        let value = $value;
        rdpudp_v1_packet!(
            @set_flag $packet,
            $crate::v1::HeaderFlags::SYN_EX,
            value.is_some()
        );
        $packet.syn_data_ex = value;
    }};
    (@assign $packet:ident, source_payload, $value:expr) => {{
        $packet.source_payload = $value;
    }};
    (@assign $packet:ident, fec_payload, $value:expr) => {{
        let value = $value;
        rdpudp_v1_packet!(
            @set_flag $packet,
            $crate::v1::HeaderFlags::FEC,
            value.is_some()
        );
        $packet.fec_payload = value;
    }};
    ($($field:ident : $value:expr),+ $(,)?) => {{
        let mut packet = $crate::v1::Packet::default();
        $(
            rdpudp_v1_packet!(@assign packet, $field, $value);
        )+
        packet
    }};
}

#[macro_export]
macro_rules! rdpudp_v1_syn_packet {
    ($window:expr, $lossy:expr, $syn_data:expr $(, correlation = $cid:expr)? $(, syn_ex = $syn_ex:expr)? ) => {{
        $crate::rdpudp_v1_packet!(
            header: $crate::v1::RdpUdpFecHeader::syn($window, $lossy),
            syn_data: Some($syn_data)
            $(, correlation_id: $cid)?
            $(, syn_data_ex: $syn_ex)?
        )
    }};
}

#[macro_export]
macro_rules! rdpudp_v1_syn_ack_packet {
    ($ack_seq:expr, $window:expr, $syn_data:expr $(, lossy = $lossy:expr)? $(, correlation = $cid:expr)? $(, syn_ex = $syn_ex:expr)? ) => {{
        let mut header = $crate::v1::RdpUdpFecHeader {
            sn_source_ack: $ack_seq,
            receive_window_size: $window,
            flags: $crate::v1::HeaderFlags::SYN | $crate::v1::HeaderFlags::ACK,
        };
        $(
            if $lossy {
                header.flags |= $crate::v1::HeaderFlags::SYN_LOSSY;
            }
        )?
        $crate::rdpudp_v1_packet!(
            header: header,
            syn_data: Some($syn_data)
            $(, correlation_id: $cid)?
            $(, syn_data_ex: $syn_ex)?
        )
    }};
}

#[macro_export]
macro_rules! rdpudp_v1_packet_bytes {
    ($($field:ident : $value:expr),+ $(,)?) => {{
        $crate::rdpudp_v1_packet!($($field : $value),+).encode()
    }};
}

#[macro_export]
macro_rules! rdpudp_v1_syn_packet_bytes {
    ($($args:tt)*) => {{
        $crate::rdpudp_v1_syn_packet!($($args)*).encode()
    }};
}

#[macro_export]
macro_rules! rdpudp_v1_syn_ack_packet_bytes {
    ($($args:tt)*) => {{
        $crate::rdpudp_v1_syn_ack_packet!($($args)*).encode()
    }};
}

#[macro_export]
macro_rules! rdpudp_v2_packet {
    (@set_flag $packet:ident, $flag:expr, $enabled:expr) => {{
        if $enabled {
            $packet.header.flags |= $flag;
        } else {
            $packet.header.flags.remove($flag);
        }
    }};
    (@assign $packet:ident, header, $value:expr) => {{
        $packet.header = $value;
    }};
    (@assign $packet:ident, ack, $value:expr) => {{
        let value = $value;
        rdpudp_v2_packet!(
            @set_flag
            $packet,
            $crate::v2::HeaderFlags::ACK,
            value.is_some()
        );
        $packet.ack = value;
    }};
    (@assign $packet:ident, overhead_size, $value:expr) => {{
        let value = $value;
        rdpudp_v2_packet!(
            @set_flag
            $packet,
            $crate::v2::HeaderFlags::OVERHEADSIZE,
            value.is_some()
        );
        $packet.overhead_size = value;
    }};
    (@assign $packet:ident, delay_ack_info, $value:expr) => {{
        let value = $value;
        rdpudp_v2_packet!(
            @set_flag
            $packet,
            $crate::v2::HeaderFlags::DELAYACKINFO,
            value.is_some()
        );
        $packet.delay_ack_info = value;
    }};
    (@assign $packet:ident, ack_of_acks, $value:expr) => {{
        let value = $value;
        rdpudp_v2_packet!(
            @set_flag
            $packet,
            $crate::v2::HeaderFlags::AOA,
            value.is_some()
        );
        $packet.ack_of_acks = value;
    }};
    (@assign $packet:ident, ack_vector, $value:expr) => {{
        let value = $value;
        rdpudp_v2_packet!(
            @set_flag
            $packet,
            $crate::v2::HeaderFlags::ACKVEC,
            value.is_some()
        );
        $packet.ack_vector = value;
    }};
    (@assign $packet:ident, data_header, $value:expr) => {{
        let value = $value;
        rdpudp_v2_packet!(
            @set_flag
            $packet,
            $crate::v2::HeaderFlags::DATA,
            value.is_some()
        );
        $packet.data_header = value;
    }};
    (@assign $packet:ident, data_body, $value:expr) => {{
        $packet.data_body = $value;
    }};
    ($($field:ident = $value:expr),+ $(,)?) => {{
        let mut packet = $crate::v2::Packet::default();
        $(
            rdpudp_v2_packet!(@assign packet, $field, $value);
        )+
        packet
    }};
}

#[macro_export]
macro_rules! rdpudp_v2_packet_bytes {
    ($($field:ident = $value:expr),+ $(,)?) => {{
        $crate::rdpudp_v2_packet!($($field = $value),+)
            .encode_on_wire($crate::v2::PacketPrefixByte::TYPE_STANDARD)
    }};
    (prefix = $prefix:expr; $($field:ident = $value:expr),+ $(,)?) => {{
        $crate::rdpudp_v2_packet!($($field = $value),+).encode_on_wire($prefix)
    }};
}

#[macro_export]
macro_rules! rdpudp_v1_flags {
    () => {{
        $crate::v1::HeaderFlags::empty()
    }};
    ($flag:ident $(| $rest:ident)*) => {{
        let mut flags = $crate::v1::HeaderFlags::empty();
        flags |= $crate::v1::HeaderFlags::$flag;
        $(
            flags |= $crate::v1::HeaderFlags::$rest;
        )*
        flags
    }};
}

#[macro_export]
macro_rules! rdpudp_v2_flags {
    () => {{
        $crate::v2::HeaderFlags::empty()
    }};
    ($flag:ident $(| $rest:ident)*) => {{
        let mut flags = $crate::v2::HeaderFlags::empty();
        flags |= $crate::v2::HeaderFlags::$flag;
        $(
            flags |= $crate::v2::HeaderFlags::$rest;
        )*
        flags
    }};
}

#[macro_export]
macro_rules! rdpudp_v1_syn_ex {
    () => {{
        $crate::v1::SynDataExPayload {
            flags: $crate::v1::SynExFlags::empty(),
            udp_version: None,
            cookie_hash: None,
        }
    }};
    ($($field:ident = $value:expr),+ $(,)?) => {{
        let mut payload = rdpudp_v1_syn_ex!();
        $(
            rdpudp_v1_syn_ex!(@assign payload, $field, $value);
        )+
        payload
    }};
    (@assign $payload:ident, flags, $value:expr) => {{
        $payload.flags = $value;
    }};
    (@assign $payload:ident, udp_version, $value:expr) => {{
        $payload.udp_version = Some($value);
        $payload.flags |= $crate::v1::SynExFlags::VERSION_INFO_VALID;
    }};
    (@assign $payload:ident, cookie_hash, $value:expr) => {{
        $payload.cookie_hash = Some($value);
    }};
}

#[macro_export]
macro_rules! rdpudp_v2_prefix {
    (standard => $len:expr) => {{
        $crate::v2::PacketPrefixByte::with_length($crate::v2::PacketPrefixByte::TYPE_STANDARD, $len)
    }};
    (dummy => $len:expr) => {{
        $crate::v2::PacketPrefixByte::with_length($crate::v2::PacketPrefixByte::TYPE_DUMMY, $len)
    }};
}

#[macro_export]
macro_rules! rdpudp_v2_ack_of_acks {
    ($sequence:expr) => {{
        $crate::v2::AckOfAcksPayload {
            sequence_number: $sequence,
        }
    }};
}

#[macro_export]
macro_rules! rdpudp_v2_overhead {
    ($size:expr) => {{
        $crate::v2::OverheadSizePayload {
            overhead_size: $size,
        }
    }};
}

#[macro_export]
macro_rules! rdpudp_v2_delay_ack_info {
    ($max:expr, $timeout:expr) => {{
        $crate::v2::DelayAckInfoPayload {
            max_delayed_acks: $max,
            delayed_ack_timeout_ms: $timeout,
        }
    }};
}
