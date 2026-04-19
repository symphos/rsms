use num_enum::TryFromPrimitive;

#[derive(TryFromPrimitive)]
#[repr(u8)]
#[derive(Clone, Debug, PartialEq, Eq, Copy)]
pub enum DataCoding {
    SMSCDefault = 0x00,
    IA5 = 0x01,
    Binary = 0x02,
    ISO88591 = 0x03,
    ISO88595 = 0x04,
    ISO885910 = 0x05,
    ISO885915 = 0x06,
    ISO10646 = 0x07,
    JIS = 0x08,
    Cyrillic = 0x09,
    Hebrew = 0x0A,
    UCS2 = 0x10,
    Pictogram = 0x11,
    ISO2022JP = 0x12,
    Reserved = 0x13,
    Extended = 0x14,
}

impl DataCoding {
    pub fn is_7bit(&self) -> bool {
        matches!(self, DataCoding::SMSCDefault | DataCoding::IA5)
    }

    pub fn is_8bit(&self) -> bool {
        matches!(self, DataCoding::Binary | DataCoding::ISO88591)
    }

    pub fn is_ucs2(&self) -> bool {
        matches!(self, DataCoding::UCS2 | DataCoding::ISO10646)
    }
}
