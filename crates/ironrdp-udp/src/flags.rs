use bitflags::bitflags;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct DatagramFlags: u16 {
        // MS-RDPEUDP 2.2.1 RDPUDP_FEC_HEADER uFlags field (little-endian)
        const SYN = 0x0001;              // Bit 0: SYN (handshake packets)
        const FIN = 0x0002;              // Bit 1: FIN (deprecated)
        const ACK = 0x0004;              // Bit 2: ACK vector present / piggybacked ACK
        const DATA = 0x0008;             // Bit 3: Packet contains source data
        const FEC = 0x0010;              // Bit 4: Packet contains FEC payload
        const CONGESTION_NOTIFICATION = 0x0020; // Bit 5: Congestion notification
        const CONGESTION_WINDOW_REDUCED = 0x0040; // Bit 6: Congestion window reduced
        const SACK_OPTION = 0x0080;      // Bit 7: SACK option (unused by RDP)
        const ACK_OF_ACKS = 0x0100;      // Bit 8: ACK-of-ACK vector present
        const SYNLOSSY = 0x0200;         // Bit 9: Lossy SYN (unreliable mode)
        const ACKDELAYED = 0x0400;       // Bit 10: ACK was delayed
        const CORRELATION_ID = 0x0800;   // Bit 11: Correlation ID present
        const SYNEX = 0x1000;            // Bit 12: Extended SYN payload present
    }
}
