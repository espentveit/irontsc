use bitflags::bitflags;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct DatagramFlags: u16 {
        const SYN = 0x0001;
        const FIN = 0x0002;
        const ACK = 0x0004;
        const DATA = 0x0008;
        const FEC = 0x0010;
        const CN = 0x0020;
        const CWR = 0x0040;
        const ACK_VECTOR = 0x0080; // Same as SACK_OPTION
        const ACK_OF_ACKS = 0x0100;
        const SYNLOSSY = 0x0200;
        const ACKDELAYED = 0x0400;
        const CORRELATION_ID = 0x0800;
        const SYNEX = 0x1000;
    }
}
