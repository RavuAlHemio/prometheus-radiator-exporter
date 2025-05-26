use std::fmt;


pub(crate) struct HexDumper<'a> {
    pub slice: &'a [u8],
}
impl<'a> fmt::Display for HexDumper<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[")?;
        let mut first_byte = true;
        for b in self.slice {
            if first_byte {
                first_byte = false;
            } else {
                write!(f, " ")?;
            }
            write!("{:02X}", b)?;
        }
        write!(f, "]")
    }
}
