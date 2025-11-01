use bitflags::bitflags;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct DatagramFlags: u16 {
        // MS-RDPEUDP 2.2.1 RDPUDP_FEC_HEADER uFlags field (little-endian)
        // Note: Bit 0 is used as SYN during handshake and ACK during data transfer.
        const SYN = 0x0001;              // Bit 0: SYN (handshake packets)
        const ACK = 0x0001;              // Bit 0: ACK (data packets) - same bit as SYN
        const FIN = 0x0002;              // Bit 1: FIN (deprecated)
        const DATA = 0x0004;             // Bit 2: Packet contains source data
        const ACK_VEC = 0x0008;          // Bit 3: Packet carries ACK vector
        const ACK_OF_ACKS = 0x0010;      // Bit 4: Packet carries ACK-of-ACK vector
        const UNUSED2 = 0x0020;          // Bit 5: Reserved / Congestion notification
        const UNUSED3 = 0x0040;          // Bit 6: Reserved / Congestion window reduced
        const UNUSED4 = 0x0080;          // Bit 7: Reserved
        const DELAYED_ACK_INFO = 0x0100; // Bit 8: Delayed ACK info present
        const SYNLOSSY = 0x0200;         // Bit 9: Lossy SYN (unreliable mode)
        const ACKDELAYED = 0x0400;       // Bit 10: ACK represents delayed ACK
        const CORRELATION_ID = 0x0800;   // Bit 11: Correlation ID present
        const SYNEX = 0x1000;            // Bit 12: SYNEX payload present
    }
}
