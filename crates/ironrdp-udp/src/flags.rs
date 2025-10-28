use bitflags::bitflags;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct DatagramFlags: u16 {
        // MS-RDPEUDP 2.2.1 RDPUDP_FEC_HEADER uFlags field
        // Note: Bit 0 has dual meaning:
        //   - For SYN packets (v1/v2/v3): SYN flag
        //   - For DATA packets (v2/v3): ACK flag
        // These are mutually exclusive (a packet cannot be both SYN and DATA)
        const SYN = 0x0001;              // Bit 0: SYN (handshake packets)
        const ACK = 0x0001;              // Bit 0: ACK (data packets) - same bit as SYN!
        const FIN = 0x0002;              // Bit 1: FIN (v1 only, deprecated)
        const DATA = 0x0004;             // Bit 2: Packet contains source data  
        const ACK_VEC = 0x0008;          // Bit 3: Packet contains ACK vector
        const ACK_OF_ACKS = 0x0010;      // Bit 4: Packet contains ACK of ACKs vector
        const UNUSED2 = 0x0020;          // Bit 5: Reserved, must be zero
        const UNUSED3 = 0x0040;          // Bit 6: Reserved, must be zero
        const UNUSED4 = 0x0080;          // Bit 7: Reserved, must be zero
        const DELAYED_ACK_INFO = 0x0100; // Bit 8: Packet contains delayed ACK information
        const SYNLOSSY = 0x0200;         // Bit 9: SYN for lossy UDP (unreliable mode)
        const ACKDELAYED = 0x0400;       // Bit 10: Delayed ACK
        const CORRELATION_ID = 0x0800;   // Bit 11: Packet contains correlation ID
        const SYNEX = 0x1000;            // Bit 12: Extended SYN
    }
}
