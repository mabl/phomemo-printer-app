//! Bluetooth device addresses, RFCOMM channels and `btspp://` device URIs.

use std::error::Error as StdError;
use std::fmt;
use std::str::FromStr;

/// The device URI scheme of the Bluetooth backend.
pub const SCHEME: &str = "btspp";

/// A Bluetooth device address (`BD_ADDR`), most significant byte first -
/// the order it is written in, `27:A6:4F:5D:03:99`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BdAddr([u8; 6]);

impl BdAddr {
    /// The address whose bytes, most significant first, are `bytes`.
    #[must_use]
    pub const fn new(bytes: [u8; 6]) -> Self {
        Self(bytes)
    }

    /// The bytes in the kernel's `bdaddr_t` order: least significant first.
    #[must_use]
    pub const fn to_bdaddr_t(self) -> [u8; 6] {
        let [b0, b1, b2, b3, b4, b5] = self.0;
        [b5, b4, b3, b2, b1, b0]
    }

    /// The address with `separator` between its bytes.
    const fn separated(self, separator: char) -> Separated {
        Separated {
            address: self,
            separator,
        }
    }
}

/// [`BdAddr`] written with a chosen separator.
struct Separated {
    address: BdAddr,
    separator: char,
}

impl fmt::Display for Separated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, byte) in self.address.0.iter().enumerate() {
            if i > 0 {
                write!(f, "{}", self.separator)?;
            }
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

/// `27:A6:4F:5D:03:99`, as `BlueZ` writes it.
impl fmt::Display for BdAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.separated(':').fmt(f)
    }
}

/// A string that is not a Bluetooth device address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseBdAddrError;

impl fmt::Display for ParseBdAddrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected six two-digit hexadecimal bytes separated by ':' or '-'")
    }
}

impl StdError for ParseBdAddrError {}

/// Six two-digit hexadecimal bytes, all separated by `:` or all by `-`.
impl FromStr for BdAddr {
    type Err = ParseBdAddrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let separator = if s.contains('-') { '-' } else { ':' };
        let mut parts = s.split(separator);
        let mut bytes = [0; 6];
        for byte in &mut bytes {
            let part = parts.next().ok_or(ParseBdAddrError)?;
            // `from_str_radix` alone would also take `+f`.
            if part.len() != 2 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(ParseBdAddrError);
            }
            *byte = u8::from_str_radix(part, 16).map_err(|_| ParseBdAddrError)?;
        }
        match parts.next() {
            Some(_) => Err(ParseBdAddrError),
            None => Ok(Self(bytes)),
        }
    }
}

/// An RFCOMM server channel, 1 to 30.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Channel(u8);

impl Channel {
    /// The channel Phomemo printers serve SPP on
    /// (`src/phomemo/transport/transport.py`, `_DEFAULT_CHANNEL`).
    pub const DEFAULT: Self = Self(1);

    /// Channel `number`, or `None` outside 1-30.
    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        match number {
            1..=30 => Some(Self(number)),
            _ => None,
        }
    }

    /// The channel number.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A string that is not an RFCOMM channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseChannelError;

impl fmt::Display for ParseChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected an RFCOMM channel from 1 to 30")
    }
}

impl StdError for ParseChannelError {}

impl FromStr for Channel {
    type Err = ParseChannelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // `u8::from_str` alone would also take `+3`.
        if !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ParseChannelError);
        }
        s.parse().ok().and_then(Self::new).ok_or(ParseChannelError)
    }
}

/// A `btspp://` device URI: the printer's address and, optionally, the
/// RFCOMM channel to use.
///
/// The address is written with hyphens, `btspp://27-A6-4F-5D-03-99`:
/// PAPPL's URI parser would take a colon for a port separator. Colons are
/// accepted too. A `channel` query parameter, `?channel=3`, picks the
/// channel to try first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BtUri {
    /// The printer.
    pub address: BdAddr,
    /// The channel to try first, if the URI names one.
    pub channel: Option<Channel>,
}

impl BtUri {
    /// The URI of `address`, without a channel.
    #[must_use]
    pub const fn new(address: BdAddr) -> Self {
        Self {
            address,
            channel: None,
        }
    }
}

impl fmt::Display for BtUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{SCHEME}://{}", self.address.separated('-'))?;
        if let Some(channel) = self.channel {
            write!(f, "?channel={channel}")?;
        }
        Ok(())
    }
}

/// Why a string is not a `btspp://` URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseUriError {
    /// The scheme is not `btspp`.
    Scheme,
    /// The address is malformed.
    Address(ParseBdAddrError),
    /// The `channel` parameter is malformed or repeated.
    Channel,
    /// A query parameter other than `channel`.
    Parameter(String),
}

impl fmt::Display for ParseUriError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scheme => write!(f, "not a {SCHEME}:// URI"),
            Self::Address(err) => write!(f, "bad Bluetooth address: {err}"),
            Self::Channel => write!(f, "bad channel: {ParseChannelError}, given once"),
            Self::Parameter(name) => write!(f, "unknown parameter '{name}'"),
        }
    }
}

impl StdError for ParseUriError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Address(err) => Some(err),
            _ => None,
        }
    }
}

impl FromStr for BtUri {
    type Err = ParseUriError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s
            .split_once("://")
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case(SCHEME))
            .ok_or(ParseUriError::Scheme)?
            .1;
        let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
        let host = path.strip_suffix('/').unwrap_or(path);
        let address = host.parse().map_err(ParseUriError::Address)?;

        let mut channel = None;
        for parameter in query.split('&').filter(|parameter| !parameter.is_empty()) {
            let (name, value) = parameter.split_once('=').unwrap_or((parameter, ""));
            if name != "channel" {
                return Err(ParseUriError::Parameter(name.to_owned()));
            }
            if channel.is_some() {
                return Err(ParseUriError::Channel);
            }
            channel = Some(value.parse().map_err(|_| ParseUriError::Channel)?);
        }

        Ok(Self { address, channel })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRINTER: BdAddr = BdAddr::new([0x27, 0xa6, 0x4f, 0x5d, 0x03, 0x99]);

    #[test]
    fn addresses_parse_with_either_separator() {
        assert_eq!("27:A6:4F:5D:03:99".parse(), Ok(PRINTER));
        assert_eq!("27-a6-4f-5d-03-99".parse(), Ok(PRINTER));
    }

    #[test]
    fn malformed_addresses_are_rejected() {
        for junk in [
            "",
            "27:A6:4F:5D:03",
            "27:A6:4F:5D:03:99:00",
            "27:A6:4F:5D:03:9",
            "27:A6:4F:5D:03:999",
            "27:A6:4F:5D:03:+f",
            "27:A6:4F:5D:03:9G",
            "27-A6:4F:5D:03:99",
            "27:A6:4F:5D:03:99:",
            " 27:A6:4F:5D:03:99",
        ] {
            assert_eq!(junk.parse::<BdAddr>(), Err(ParseBdAddrError), "{junk:?}");
        }
    }

    #[test]
    fn addresses_display_most_significant_first() {
        assert_eq!(PRINTER.to_string(), "27:A6:4F:5D:03:99");
    }

    #[test]
    fn the_kernel_order_is_reversed() {
        assert_eq!(PRINTER.to_bdaddr_t(), [0x99, 0x03, 0x5d, 0x4f, 0xa6, 0x27]);
    }

    #[test]
    fn channels_are_one_to_thirty() {
        assert_eq!(Channel::new(0), None);
        assert_eq!(Channel::new(1), Some(Channel::DEFAULT));
        assert_eq!(Channel::new(30).map(Channel::get), Some(30));
        assert_eq!(Channel::new(31), None);
        assert_eq!("3".parse::<Channel>().map(Channel::get), Ok(3));
        for junk in ["", "0", "31", "+3", "-1", "3 ", "x"] {
            assert_eq!(junk.parse::<Channel>(), Err(ParseChannelError), "{junk:?}");
        }
    }

    #[test]
    fn uris_parse_in_the_hyphen_form() {
        assert_eq!("btspp://27-A6-4F-5D-03-99".parse(), Ok(BtUri::new(PRINTER)));
    }

    #[test]
    fn uris_parse_in_the_colon_form_and_with_a_trailing_slash() {
        assert_eq!(
            "btspp://27:A6:4F:5D:03:99/".parse(),
            Ok(BtUri::new(PRINTER))
        );
        assert_eq!("BTSPP://27-A6-4F-5D-03-99".parse(), Ok(BtUri::new(PRINTER)));
    }

    #[test]
    fn uris_may_name_a_channel() {
        assert_eq!(
            "btspp://27-A6-4F-5D-03-99?channel=3".parse(),
            Ok(BtUri {
                address: PRINTER,
                channel: Channel::new(3),
            })
        );
    }

    #[test]
    fn malformed_uris_are_rejected() {
        let parse = str::parse::<BtUri>;
        assert_eq!(parse("27-A6-4F-5D-03-99"), Err(ParseUriError::Scheme));
        assert_eq!(parse("usb://27-A6-4F-5D-03-99"), Err(ParseUriError::Scheme));
        assert_eq!(
            parse("btspp://27-A6-4F-5D-03"),
            Err(ParseUriError::Address(ParseBdAddrError))
        );
        assert_eq!(
            parse("btspp://27-A6-4F-5D-03-99/x"),
            Err(ParseUriError::Address(ParseBdAddrError))
        );
        assert_eq!(
            parse("btspp://27-A6-4F-5D-03-99?channel=31"),
            Err(ParseUriError::Channel)
        );
        assert_eq!(
            parse("btspp://27-A6-4F-5D-03-99?channel"),
            Err(ParseUriError::Channel)
        );
        assert_eq!(
            parse("btspp://27-A6-4F-5D-03-99?channel=1&channel=2"),
            Err(ParseUriError::Channel)
        );
        assert_eq!(
            parse("btspp://27-A6-4F-5D-03-99?speed=fast"),
            Err(ParseUriError::Parameter("speed".to_owned()))
        );
    }

    #[test]
    fn uris_round_trip() {
        for uri in [
            "btspp://27-A6-4F-5D-03-99",
            "btspp://27-A6-4F-5D-03-99?channel=3",
        ] {
            assert_eq!(
                uri.parse::<BtUri>().map(|uri| uri.to_string()),
                Ok(uri.to_owned())
            );
        }
    }
}
