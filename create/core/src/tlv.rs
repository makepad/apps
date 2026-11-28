//! Optional wire fields, each framed by a one-byte tag and a little-endian u16 length.

pub fn write_tlv(out: &mut Vec<u8>, tag: u8, val: &[u8]) {
    let len = u16::try_from(val.len()).expect("field must fit its u16 wire length");
    out.push(tag);
    out.extend(len.to_le_bytes());
    out.extend_from_slice(val);
}

pub fn read_tlv<'a>(rest: &mut &'a [u8]) -> Result<Option<(u8, &'a [u8])>, &'static str> {
    if rest.is_empty() {
        return Ok(None);
    }
    let (&[tag, lo, hi], tail) = rest.split_first_chunk().ok_or("incomplete tlv header")?;
    let len = usize::from(u16::from_le_bytes([lo, hi]));
    let (val, tail) = tail.split_at_checked(len).ok_or("truncated tlv value")?;
    *rest = tail;
    Ok(Some((tag, val)))
}

pub fn skip_tlv_tail(rest: &mut &[u8]) -> Result<(), &'static str> {
    while read_tlv(rest)?.is_some() {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tlv_fields_advance_only_over_complete_values() {
        let mut wire = Vec::new();
        write_tlv(&mut wire, 3, &[]);
        write_tlv(&mut wire, 8, b"future");
        let mut rest = wire.as_slice();
        assert_eq!(read_tlv(&mut rest), Ok(Some((3, &[][..]))));
        assert_eq!(read_tlv(&mut rest), Ok(Some((8, &b"future"[..]))));
        assert_eq!(read_tlv(&mut rest), Ok(None));
        assert_eq!(skip_tlv_tail(&mut wire.as_slice()), Ok(()));
        for bytes in [&[3][..], &[3, 4]] {
            assert_eq!(read_tlv(&mut &bytes[..]), Err("incomplete tlv header"));
        }
        assert_eq!(skip_tlv_tail(&mut &[8, 2, 0, 7][..]), Err("truncated tlv value"));
    }
}
