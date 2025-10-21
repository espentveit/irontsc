//! Drawing Orders
//!
//! Implements primary, secondary, and alternate secondary drawing orders
//! per [MS-RDPEGDI] specification

use ironrdp_core::{decode_cursor, DecodeResult, Encode, EncodeResult, ReadCursor, WriteCursor};

use super::desktop_composition::{DesktopCompositionOrder, TS_ALTSEC_COMPDESK_FIRST};

/// Drawing Order - can be primary, secondary, or alternate secondary
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrawingOrder {
    /// Desktop Composition orders (Alternate Secondary)
    DesktopComposition(DesktopCompositionOrder),
    /// Other alternate secondary orders (not yet implemented)
    AlternateSecondary { order_type: u8, data: Vec<u8> },
    /// Secondary orders (not yet implemented)
    Secondary { order_type: u8, data: Vec<u8> },
    /// Primary orders (not yet implemented)
    Primary { order_type: u8, data: Vec<u8> },
}

impl DrawingOrder {
    const NAME: &'static str = "DrawingOrder";

    /// Decode a sequence of drawing orders from the Orders Update PDU data
    ///
    /// This is the entry point for processing UpdateCode::Orders (0x0) data
    pub fn decode_orders_update(src: &[u8]) -> DecodeResult<Vec<Self>> {
        let mut cursor = ReadCursor::new(src);
        let mut orders = Vec::new();

        while cursor.len() > 0 {
            // Peek at the first byte to determine order type
            if cursor.len() < 1 {
                break;
            }

            let header_byte = cursor.peek_u8();
            let order_type_bits = header_byte & 0x03;

            let order = match order_type_bits {
                0x00 => {
                    // TS_PRIMARY_DRAWING_ORDER
                    // For now, store as raw data - full implementation would decode specific order types
                    Self::decode_primary_order(&mut cursor)?
                }
                0x01 => {
                    // TS_SECONDARY_DRAWING_ORDER
                    Self::decode_secondary_order(&mut cursor)?
                }
                0x02 => {
                    // TS_ALTSEC_DRAWING_ORDER (Alternate Secondary)
                    Self::decode_alternate_secondary_order(&mut cursor)?
                }
                _ => {
                    // Invalid or reserved
                    break;
                }
            };

            orders.push(order);
        }

        Ok(orders)
    }

    fn decode_primary_order(src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        // Primary orders have complex structure - for now just consume the header byte
        // A full implementation would decode all primary order types
        let header = src.read_u8();
        let order_type = (header >> 2) & 0x1F;

        // For minimal implementation, just store as unprocessed data
        // In reality, we'd decode the full order based on orderType
        Ok(Self::Primary {
            order_type,
            data: Vec::new(),
        })
    }

    fn decode_secondary_order(src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        // Secondary orders - not yet implemented
        let header = src.read_u8();
        let order_type = (header >> 2) & 0x1F;

        Ok(Self::Secondary {
            order_type,
            data: Vec::new(),
        })
    }

    fn decode_alternate_secondary_order(src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        // Read the header byte to determine the alternate secondary order type
        // Don't consume it yet - let the specific decoder handle it
        let header = src.peek_u8();
        let order_type = (header >> 2) & 0x1F;

        match order_type {
            TS_ALTSEC_COMPDESK_FIRST => {
                // Desktop Composition order
                let comp_order = DesktopCompositionOrder::decode_with_header(src)?;
                Ok(Self::DesktopComposition(comp_order))
            }
            _ => {
                // Other alternate secondary order types (not yet implemented)
                // For now, consume just the header byte
                let header = src.read_u8();
                Ok(Self::AlternateSecondary {
                    order_type,
                    data: Vec::new(),
                })
            }
        }
    }

    pub fn as_short_name(&self) -> &str {
        match self {
            Self::DesktopComposition(_) => "Desktop Composition",
            Self::AlternateSecondary { .. } => "Alternate Secondary",
            Self::Secondary { .. } => "Secondary",
            Self::Primary { .. } => "Primary",
        }
    }
}

impl Encode for DrawingOrder {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        match self {
            Self::DesktopComposition(order) => order.encode(dst),
            Self::AlternateSecondary { data, .. } => {
                dst.write_slice(data);
                Ok(())
            }
            Self::Secondary { data, .. } => {
                dst.write_slice(data);
                Ok(())
            }
            Self::Primary { data, .. } => {
                dst.write_slice(data);
                Ok(())
            }
        }
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        match self {
            Self::DesktopComposition(order) => order.size(),
            Self::AlternateSecondary { data, .. } => data.len(),
            Self::Secondary { data, .. } => data.len(),
            Self::Primary { data, .. } => data.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::basic_output::desktop_composition::{CompDeskToggle, CompDeskToggleEventType};

    #[test]
    fn test_decode_desktop_composition_order() {
        // Create a desktop composition toggle order
        let toggle = DesktopCompositionOrder::Toggle(CompDeskToggle::new(
            CompDeskToggleEventType::CompositionOn,
        ));

        // Encode it
        let mut buffer = vec![0u8; toggle.size()];
        let mut cursor = WriteCursor::new(&mut buffer);
        toggle.encode(&mut cursor).unwrap();

        // Decode as part of orders update
        let orders = DrawingOrder::decode_orders_update(&buffer).unwrap();

        assert_eq!(orders.len(), 1);
        match &orders[0] {
            DrawingOrder::DesktopComposition(DesktopCompositionOrder::Toggle(t)) => {
                assert_eq!(t.event_type, CompDeskToggleEventType::CompositionOn);
            }
            _ => panic!("Expected DesktopComposition Toggle order"),
        }
    }
}
