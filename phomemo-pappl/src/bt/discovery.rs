//! Paired Phomemo printers, from `BlueZ` over the system D-Bus.
//!
//! [`paired_devices`] asks `BlueZ` for its paired devices on a helper
//! thread, at most one at a time ([`SingleFlight`]), so a caller waits at
//! most its own timeout however slow D-Bus is, and a hung bus costs one
//! stuck thread rather than one per caller. The bus connection is kept
//! while it works and made again after a failure, so discovery recovers
//! once D-Bus is back - after a restart, or when the application started
//! first.
//!
//! A printer is recognised by its Bluetooth name: some use their model
//! name (`M110`, `D30_1234`), others their serial number
//! (`Q198G5949230062`), whose first four characters give the model. Both
//! the name a user may have given it and its own name are looked at.

use std::collections::HashMap;
use std::error::Error as StdError;
use std::ffi::CStr;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::blocking::fdo::ObjectManagerProxy;
use zbus::zvariant::OwnedValue;

use super::address::{BdAddr, BtUri};
use super::flight::{FlightError, SingleFlight};
use super::lock;
use crate::models::Model;

/// How long a caller waits for `BlueZ`.
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_millis(2500);

/// How long one D-Bus call may take before the query gives up, which
/// bounds how long a query can stay in flight.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// The IEEE 1284 device ID of a printer whose model is unknown: no `MDL`,
/// so PAPPL's auto-add falls back to the description
/// (`crate::autoadd::match_model`).
const ANONYMOUS_DEVICE_ID: &CStr = c"MFG:Phomemo;CMD:PHOMEMO;";

/// Serial-number prefixes and the model each stands for, for the models
/// this application drives (`re/protocol/models.md`, "SN Prefix → Model
/// Name Mapping", from Print Master's `PrinterInfo.getDeviceModelName`).
const SERIAL_PREFIXES: &[(&str, &str)] = &[
    ("Q006", "M200"),
    ("Q009", "M120"),
    ("Q017", "M206"),
    ("Q018", "D30"),
    ("Q038", "M126"),
    ("Q040", "D30"),
    ("Q046", "D30"),
    ("Q049", "D30"),
    ("Q050", "D30"),
    ("Q053", "M208"),
    ("Q054", "M220"),
    ("Q057", "M219"),
    ("Q058", "M220"),
    ("Q069", "D30"),
    ("Q082", "Q30"),
    ("Q083", "D50"),
    ("Q086", "M200"),
    ("Q092", "D30"),
    ("Q093", "D30"),
    ("Q104", "M200"),
    ("Q107", "D30"),
    ("Q109", "D30"),
    ("Q110", "D30"),
    ("Q121", "M200"),
    ("Q130", "Q30"),
    ("Q138", "D30"),
    ("Q155", "M220"),
    ("Q156", "M200"),
    ("Q157", "M219"),
    ("Q158", "M120"),
    ("Q159", "D30"),
    ("Q162", "D30"),
    ("Q169", "Q30"),
    ("Q172", "D30"),
    ("Q189", "D30"),
    ("Q193", "M120"),
    ("Q197", "M200"),
    ("Q198", "M220"),
    ("Q223", "D30"),
    ("Q244", "M120"),
    ("Q294", "A30"),
    ("Q305", "M209"),
    ("Q306", "M102"),
    ("Q317", "M105"),
    ("Q377", "M150"),
    ("Q378", "M100"),
    // D30S, a D30 variant (`MODEL_ALIASES`).
    ("Q036", "D30S"),
    ("Q048", "D30S"),
    ("Q097", "D30S"),
    ("Q111", "D30S"),
    ("Q125", "D30S"),
    ("Q149", "D30S"),
    ("Q150", "D30S"),
    ("Q183", "D30S"),
];

/// Names of models this application has no row for, which Print Master
/// groups into the series of one it does (`re/protocol/models.md`, "Series
/// Groupings", from `SeriesChecker.java`), and the model that drives them.
/// The variants the model table leaves out on purpose because they print
/// differently - M110C, M120C, M221 (`phomemo_protocol::model`) - are not
/// here either.
const MODEL_ALIASES: &[(&str, &str)] = &[
    // D30_SERIES; `D30S_NEW` and `D30S_PRO` start with the word `D30S`.
    ("D30S", "D30"),
    ("D30N", "D30"),
    ("D30PRO", "D30"),
    // Q30_SERIES.
    ("Q30S", "Q30"),
    ("Q31", "Q30"),
    ("Q32", "Q30"),
    // M110_SERIES.
    ("M108TA", "M108"),
    ("M108Z", "M108"),
    ("M110R", "M110"),
    ("M110SA", "M110S"),
];

/// A paired Bluetooth device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtDevice {
    /// Its address.
    pub address: BdAddr,
    /// The name `BlueZ` shows: the device's own, unless the user renamed
    /// it.
    pub alias: String,
    /// The device's own name, if it told.
    pub name: Option<String>,
}

impl BtDevice {
    /// Whether either name looks like a Phomemo printer's.
    #[must_use]
    pub fn is_phomemo(&self) -> bool {
        self.names().any(|name| {
            serial_prefix(name).is_some()
                || model_for_name(name).is_some()
                || words(name).any(|word| word.eq_ignore_ascii_case("phomemo"))
        })
    }

    /// The model, if either name tells.
    #[must_use]
    pub fn model(&self) -> Option<&'static Model> {
        self.names().find_map(model_for_name)
    }

    /// The alias, then the device's own name.
    fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.alias.as_str()).chain(self.name.as_deref())
    }

    /// The device URI.
    #[must_use]
    pub const fn uri(&self) -> BtUri {
        BtUri::new(self.address)
    }

    /// A description for people: `Phomemo M220 (Q198G5949230062)`.
    #[must_use]
    pub fn info(&self) -> String {
        // Names come from the device; keep control characters off screens
        // and logs.
        let name: String = self.alias.chars().filter(|c| !c.is_control()).collect();
        match self.model() {
            Some(model) if name.eq_ignore_ascii_case(model.name()) => {
                format!("Phomemo {}", model.name())
            }
            Some(model) => format!("Phomemo {} ({name})", model.name()),
            None => format!("Phomemo printer ({name})"),
        }
    }
}

/// The IEEE 1284 device ID for `model`, or one without a model when it is
/// unknown. A device's name never goes into the ID, so whatever it
/// contains cannot corrupt it.
#[must_use]
pub fn device_id(model: Option<&'static Model>) -> &'static CStr {
    model.map_or(ANONYMOUS_DEVICE_ID, Model::device_id)
}

/// The model a Bluetooth name stands for.
///
/// A serial number (`Q198G5949230062`) is resolved by its prefix alone:
/// its other characters are no model name, even when they happen to
/// spell one. Any other name must start with a model's name as a whole
/// word, or with `Phomemo` and then one: `D30_1234`, `M220-A4B3`,
/// `Phomemo M220S` - but not `M2200`, nor `Galaxy A30`.
#[must_use]
pub fn model_for_name(name: &str) -> Option<&'static Model> {
    if let Some(prefix) = serial_prefix(name) {
        return SERIAL_PREFIXES
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(prefix))
            .and_then(|&(_, model)| model_named(model));
    }
    let mut words = words(name);
    let first = words.next()?;
    if first.eq_ignore_ascii_case("phomemo") {
        model_named(words.next()?)
    } else {
        model_named(first)
    }
}

/// The model called `name`, directly or as one of its variants
/// ([`MODEL_ALIASES`]).
fn model_named(name: &str) -> Option<&'static Model> {
    Model::by_name(name).or_else(|| {
        MODEL_ALIASES
            .iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(name))
            .and_then(|&(_, model)| Model::by_name(model))
    })
}

/// The serial-number prefix of `name` if it is a serial number: `Q`, three
/// digits, then letters and digits only.
fn serial_prefix(name: &str) -> Option<&str> {
    let bytes = name.as_bytes();
    let is_serial = bytes.len() > 4
        && bytes[0].eq_ignore_ascii_case(&b'Q')
        && bytes[1..4].iter().all(u8::is_ascii_digit)
        && bytes.iter().all(u8::is_ascii_alphanumeric);
    is_serial.then(|| &name[..4])
}

/// The alphanumeric words of `name`.
fn words(name: &str) -> impl Iterator<Item = &str> {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
}

/// Why `BlueZ` could not be asked.
#[derive(Debug)]
pub enum Error {
    /// D-Bus failed.
    Bus(Arc<zbus::Error>),
    /// The query did not answer.
    Query(FlightError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bus(err) => write!(f, "D-Bus: {err}"),
            Self::Query(err) => write!(f, "BlueZ: {err}"),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Bus(err) => Some(&**err),
            Self::Query(err) => Some(err),
        }
    }
}

/// The Phomemo printers among the paired devices.
///
/// # Errors
///
/// As [`paired_devices`].
pub fn printers(timeout: Duration) -> Result<Vec<BtDevice>, Error> {
    let mut devices = paired_devices(timeout)?;
    devices.retain(BtDevice::is_phomemo);
    Ok(devices)
}

/// The model of the paired device at `address`, if its name tells. When
/// `BlueZ` cannot be asked, its last answer is used.
#[must_use]
pub fn model_at(address: BdAddr, timeout: Duration) -> Option<&'static Model> {
    paired_devices(timeout)
        .ok()
        .or_else(|| lock(&LAST_ANSWER).clone())?
        .into_iter()
        .find(|device| device.address == address)?
        .model()
}

/// Every paired device `BlueZ` knows, waiting at most `timeout` for it.
///
/// # Errors
///
/// Fails if `BlueZ` does not answer in time or D-Bus fails.
pub fn paired_devices(timeout: Duration) -> Result<Vec<BtDevice>, Error> {
    QUERY
        .ask(query, timeout)
        .map_err(Error::Query)?
        .map_err(Error::Bus)
}

/// The query of `BlueZ`, run on a thread of its own.
static QUERY: SingleFlight<Result<Vec<BtDevice>, Arc<zbus::Error>>> = SingleFlight::new();

/// The latest successful answer.
static LAST_ANSWER: Mutex<Option<Vec<BtDevice>>> = Mutex::new(None);

/// The system bus connection, while it works.
static BUS: Mutex<Option<Connection>> = Mutex::new(None);

/// Ask `BlueZ`; remember a successful answer, and forget the bus after a
/// failure, so that the next query connects afresh.
fn query() -> Result<Vec<BtDevice>, Arc<zbus::Error>> {
    let answer = query_bluez().map_err(Arc::new);
    match &answer {
        Ok(devices) => *lock(&LAST_ANSWER) = Some(devices.clone()),
        Err(_) => drop(lock(&BUS).take()),
    }
    answer
}

/// The paired devices, from `BlueZ`'s object manager.
fn query_bluez() -> zbus::Result<Vec<BtDevice>> {
    let bus = system_bus()?;
    let objects = ObjectManagerProxy::builder(&bus)
        .destination("org.bluez")?
        .path("/")?
        .build()?
        .get_managed_objects()?;
    Ok(objects
        .values()
        .filter_map(|interfaces| interfaces.get("org.bluez.Device1"))
        .filter_map(paired_device)
        .collect())
}

/// The system bus, connecting if there is no working connection.
fn system_bus() -> zbus::Result<Connection> {
    let mut bus = lock(&BUS);
    if let Some(connection) = &*bus {
        return Ok(connection.clone());
    }
    let connection = zbus::blocking::connection::Builder::system()?
        .method_timeout(CALL_TIMEOUT)
        .build()?;
    *bus = Some(connection.clone());
    drop(bus);
    Ok(connection)
}

/// The device an `org.bluez.Device1` describes, if it is paired.
fn paired_device(properties: &HashMap<String, OwnedValue>) -> Option<BtDevice> {
    let text = |name| {
        properties
            .get(name)
            .and_then(|value| <&str>::try_from(value).ok())
    };
    if !bool::try_from(properties.get("Paired")?).ok()? {
        return None;
    }
    let address = text("Address")?.parse().ok()?;
    let name = text("Name").map(str::to_owned);
    let alias = text("Alias").map(str::to_owned).or_else(|| name.clone())?;
    Some(BtDevice {
        address,
        alias,
        name,
    })
}

#[cfg(test)]
mod tests {
    use zbus::zvariant::{Str, Value};

    use super::*;

    const ADDRESS: BdAddr = BdAddr::new([0x27, 0xa6, 0x4f, 0x5d, 0x03, 0x99]);

    fn device(name: &str) -> BtDevice {
        BtDevice {
            address: ADDRESS,
            alias: name.to_owned(),
            name: Some(name.to_owned()),
        }
    }

    fn renamed(alias: &str, name: &str) -> BtDevice {
        BtDevice {
            address: ADDRESS,
            alias: alias.to_owned(),
            name: Some(name.to_owned()),
        }
    }

    fn model(name: &str) -> Option<&'static str> {
        model_for_name(name).map(Model::name)
    }

    #[test]
    fn serial_numbers_resolve_by_prefix() {
        assert_eq!(model("Q198G5949230062"), Some("M220"));
        assert_eq!(model("q155A1234567890"), Some("M220"));
        assert_eq!(model("Q053A1234567890"), Some("M208"));
        assert_eq!(model("Q294A1234567890"), Some("A30"));
        assert_eq!(model("Q183A1234567890"), Some("D30"));
    }

    #[test]
    fn a_serial_number_is_never_read_as_a_model_name() {
        // Unknown prefix: not resolved through the "M220" it contains, nor
        // through a whole-word match.
        assert_eq!(model("Q999M220123456"), None);
        // B246D, which this application does not drive.
        assert_eq!(model("Q100A1234567890"), None);
    }

    #[test]
    fn names_start_with_a_whole_model_name() {
        assert_eq!(model("M220"), Some("M220"));
        assert_eq!(model("m110"), Some("M110"));
        assert_eq!(model("D30_1234"), Some("D30"));
        assert_eq!(model("M220-A4B3"), Some("M220"));
        assert_eq!(model("Phomemo M220S"), Some("M220S"));
        assert_eq!(model("Phomemo-M200C"), Some("M200C"));
        assert_eq!(model("M2200"), None);
        assert_eq!(model("D1000 Headphones"), None);
        assert_eq!(model("Galaxy A30"), None);
        assert_eq!(model("My M220"), None);
        assert_eq!(model("Phomemo"), None);
        assert_eq!(model(""), None);
    }

    #[test]
    fn series_variants_resolve_to_their_model() {
        assert_eq!(model("D30S"), Some("D30"));
        assert_eq!(model("D30S_PRO"), Some("D30"));
        assert_eq!(model("D30PRO-1234"), Some("D30"));
        assert_eq!(model("Q31"), Some("Q30"));
        assert_eq!(model("Phomemo Q32"), Some("Q30"));
        assert_eq!(model("M108Z"), Some("M108"));
        assert_eq!(model("M110SA"), Some("M110S"));
    }

    #[test]
    fn variants_left_out_of_the_model_table_stay_out() {
        assert_eq!(model("M110C"), None);
        assert_eq!(model("M120C"), None);
        assert_eq!(model("M221"), None);
    }

    #[test]
    fn every_serial_prefix_and_alias_names_a_driven_model() {
        for &(prefix, name) in SERIAL_PREFIXES {
            assert!(serial_prefix(&format!("{prefix}X")).is_some(), "{prefix}");
            assert!(model_named(name).is_some(), "{prefix} -> {name}");
        }
        for &(alias, name) in MODEL_ALIASES {
            assert!(Model::by_name(alias).is_none(), "{alias} has a row");
            assert!(Model::by_name(name).is_some(), "{alias} -> {name}");
        }
    }

    #[test]
    fn phomemo_names_are_recognised() {
        assert!(device("Q198G5949230062").is_phomemo());
        assert!(device("Q999A1234567890").is_phomemo());
        assert!(device("M220-A4B3").is_phomemo());
        assert!(device("Phomemo X9").is_phomemo());
        assert!(!device("Q12").is_phomemo());
        assert!(!device("Q123-456").is_phomemo());
        assert!(!device("HP LaserJet").is_phomemo());
        assert!(!device("M2200").is_phomemo());
        assert!(!device("Galaxy A30").is_phomemo());
        assert!(!device("").is_phomemo());
    }

    #[test]
    fn a_renamed_printer_is_known_by_its_own_name() {
        let printer = renamed("Kitchen labels", "Q198G5949230062");
        assert!(printer.is_phomemo());
        assert_eq!(printer.model().map(Model::name), Some("M220"));
        assert_eq!(printer.info(), "Phomemo M220 (Kitchen labels)");
        let unnamed = BtDevice {
            name: None,
            ..renamed("M110", "")
        };
        assert_eq!(unnamed.model().map(Model::name), Some("M110"));
    }

    #[test]
    fn the_device_id_names_the_model_or_none() {
        assert_eq!(
            device_id(device("Q198G5949230062").model()),
            c"MFG:Phomemo;MDL:M220;CMD:PHOMEMO;"
        );
        assert_eq!(
            device_id(device("M220;CMD:X").model()),
            c"MFG:Phomemo;MDL:M220;CMD:PHOMEMO;"
        );
        assert_eq!(
            device_id(device("Phomemo;MDL:X:Y").model()),
            ANONYMOUS_DEVICE_ID
        );
    }

    #[test]
    fn descriptions() {
        assert_eq!(device("M220").info(), "Phomemo M220");
        assert_eq!(
            device("Q198G5949230062").info(),
            "Phomemo M220 (Q198G5949230062)"
        );
        assert_eq!(
            device("Phomemo X9\n\0").info(),
            "Phomemo printer (Phomemo X9)"
        );
    }

    #[test]
    fn uris_use_the_hyphen_form() {
        assert_eq!(
            device("M220").uri().to_string(),
            "btspp://27-A6-4F-5D-03-99"
        );
    }

    fn properties(entries: &[(&str, Value<'_>)]) -> HashMap<String, OwnedValue> {
        entries
            .iter()
            .map(|(name, value)| {
                let value =
                    OwnedValue::try_from(value.try_clone().expect("clone")).expect("owned value");
                ((*name).to_owned(), value)
            })
            .collect()
    }

    #[test]
    fn devices_are_read_from_typed_properties() {
        let paired = properties(&[
            ("Paired", Value::Bool(true)),
            ("Address", Value::Str(Str::from("27:A6:4F:5D:03:99"))),
            ("Name", Value::Str(Str::from("Q198G5949230062"))),
            ("Alias", Value::Str(Str::from("Label printer"))),
        ]);
        assert_eq!(
            paired_device(&paired),
            Some(renamed("Label printer", "Q198G5949230062"))
        );

        let unnamed = properties(&[
            ("Paired", Value::Bool(true)),
            ("Address", Value::Str(Str::from("27:A6:4F:5D:03:99"))),
            ("Name", Value::Str(Str::from("M220"))),
        ]);
        assert_eq!(paired_device(&unnamed), Some(device("M220")));

        let nameless = properties(&[
            ("Paired", Value::Bool(true)),
            ("Address", Value::Str(Str::from("27:A6:4F:5D:03:99"))),
            ("Alias", Value::Str(Str::from("27-A6-4F-5D-03-99"))),
        ]);
        assert_eq!(
            paired_device(&nameless),
            Some(BtDevice {
                address: ADDRESS,
                alias: "27-A6-4F-5D-03-99".to_owned(),
                name: None,
            })
        );
    }

    #[test]
    fn unpaired_or_malformed_devices_are_skipped() {
        let unpaired = properties(&[
            ("Paired", Value::Bool(false)),
            ("Address", Value::Str(Str::from("27:A6:4F:5D:03:99"))),
            ("Alias", Value::Str(Str::from("M220"))),
        ]);
        assert_eq!(paired_device(&unpaired), None);

        let mistyped = properties(&[
            ("Paired", Value::Str(Str::from("yes"))),
            ("Address", Value::Str(Str::from("27:A6:4F:5D:03:99"))),
            ("Alias", Value::Str(Str::from("M220"))),
        ]);
        assert_eq!(paired_device(&mistyped), None);

        let bad_address = properties(&[
            ("Paired", Value::Bool(true)),
            ("Address", Value::Str(Str::from("27:A6"))),
            ("Alias", Value::Str(Str::from("M220"))),
        ]);
        assert_eq!(paired_device(&bad_address), None);
    }
}
