pub fn parse(s: &str) -> Result<(u16, bool), String> {
    let s = s.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    let v = u32::from_str_radix(s, 16).map_err(|e| format!("bad scancode: {}", e))?;
    if v > 0xFFFF {
        return Err("scancode out of range".to_string());
    }
    if (v & 0xFF00) == 0xE000 {
        Ok(((v & 0x00FF) as u16, true))
    } else if v <= 0xFF {
        Ok((v as u16, false))
    } else {
        Err("scancode must be 0xNN or 0xE0NN".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain() {
        assert_eq!(parse("0x11").unwrap(), (0x11, false));
        assert_eq!(parse("0x11").unwrap(), (0x11, false));
    }

    #[test]
    fn parse_extended() {
        assert_eq!(parse("0xE04B").unwrap(), (0x4B, true));
    }

    #[test]
    fn parse_no_prefix() {
        assert_eq!(parse("11").unwrap(), (0x11, false));
    }
}