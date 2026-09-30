//! Parsing what the user types into the "Host address" field.

/// Errors in a host address.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddressError {
    #[error("the address is empty")]
    Empty,
    #[error("\"{0}\" is not a valid port number")]
    BadPort(String),
    #[error("missing closing ']' in IPv6 address")]
    UnclosedBracket,
}

/// Splits `host`, `host:port`, `[ipv6]:port`, `[ipv6]` or a bare IPv6
/// address into host and port, using `default_port` when none is given.
pub fn parse_address(input: &str, default_port: u16) -> Result<(String, u16), AddressError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(AddressError::Empty);
    }
    if let Some(rest) = input.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or(AddressError::UnclosedBracket)?;
        let port = match after.strip_prefix(':') {
            Some(p) => parse_port(p)?,
            None if after.is_empty() => default_port,
            None => return Err(AddressError::BadPort(after.to_owned())),
        };
        return Ok((host.to_owned(), port));
    }
    match input.matches(':').count() {
        0 => Ok((input.to_owned(), default_port)),
        1 => {
            let (host, port) = input.split_once(':').unwrap();
            if host.is_empty() {
                return Err(AddressError::Empty);
            }
            Ok((host.to_owned(), parse_port(port)?))
        }
        // More than one colon without brackets: a bare IPv6 address.
        _ => Ok((input.to_owned(), default_port)),
    }
}

fn parse_port(text: &str) -> Result<u16, AddressError> {
    match text.parse::<u16>() {
        Ok(p) if p > 0 => Ok(p),
        _ => Err(AddressError::BadPort(text.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_forms() {
        assert_eq!(
            parse_address("203.0.113.7", 49700),
            Ok(("203.0.113.7".into(), 49700))
        );
        assert_eq!(
            parse_address(" 203.0.113.7:50000 ", 49700),
            Ok(("203.0.113.7".into(), 50000))
        );
        assert_eq!(
            parse_address("pilot.example.com:1234", 49700),
            Ok(("pilot.example.com".into(), 1234))
        );
    }

    #[test]
    fn ipv6_forms() {
        assert_eq!(
            parse_address("[2001:db8::1]:50000", 49700),
            Ok(("2001:db8::1".into(), 50000))
        );
        assert_eq!(
            parse_address("[2001:db8::1]", 49700),
            Ok(("2001:db8::1".into(), 49700))
        );
        assert_eq!(
            parse_address("2001:db8::1", 49700),
            Ok(("2001:db8::1".into(), 49700))
        );
    }

    #[test]
    fn errors() {
        assert_eq!(parse_address("  ", 49700), Err(AddressError::Empty));
        assert_eq!(parse_address(":49700", 49700), Err(AddressError::Empty));
        assert_eq!(
            parse_address("host:0", 49700),
            Err(AddressError::BadPort("0".into()))
        );
        assert_eq!(
            parse_address("host:99999", 49700),
            Err(AddressError::BadPort("99999".into()))
        );
        assert_eq!(
            parse_address("[::1", 49700),
            Err(AddressError::UnclosedBracket)
        );
        assert_eq!(
            parse_address("[::1]x", 49700),
            Err(AddressError::BadPort("x".into()))
        );
    }
}
