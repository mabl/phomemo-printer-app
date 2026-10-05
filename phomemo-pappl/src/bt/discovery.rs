//! Bluetooth device discovery via `BlueZ` D-Bus.
//!
//! Queries the system D-Bus (`org.bluez`) to enumerate paired Bluetooth
//! devices and filter by name heuristic for Phomemo printers.
//!
//! Uses `zbus::blocking` for synchronous D-Bus access (no async runtime
//! needed).  A single system D-Bus connection is shared process-wide
//! via a `OnceLock`.
//!
//! Key design decisions:
//! - Do NOT require Device1.UUIDs to contain SPP — it's optional.
//! - Filter by Paired == true + name/alias heuristic.
//! - Confirm SPP during open or lightweight probe.

use std::io;
use std::time::Duration;

/// Default timeout for D-Bus discovery calls.
const DISCOVERY_TIMEOUT: Duration = Duration::from_millis(2500);

fn model_aliases() -> &'static [(String, &'static str)] {
    use std::sync::OnceLock;
    static ALIASES: OnceLock<Vec<(String, &'static str)>> = OnceLock::new();

    ALIASES.get_or_init(|| {
        let mut aliases = phomemo_protocol::model::all()
            .iter()
            .map(|model| (model.name.to_ascii_uppercase(), model.name))
            .collect::<Vec<_>>();

        aliases.sort_by(|left, right| {
            right
                .0
                .len()
                .cmp(&left.0.len())
                .then_with(|| left.0.cmp(&right.0))
        });
        aliases.dedup_by(|left, right| left.0 == right.0);
        aliases
    })
}

fn looks_like_sn_prefix(upper_name: &str) -> bool {
    let bytes = upper_name.as_bytes();
    bytes.len() >= 4
        && bytes[0] == b'Q'
        && bytes[1].is_ascii_digit()
        && bytes[2].is_ascii_digit()
        && bytes[3].is_ascii_digit()
}

/// Known serial-number prefixes that map to Phomemo models.
///
/// Some printers use their serial number as the BT device name instead
/// of the model name.
const KNOWN_SN_PREFIXES: &[(&str, &str)] = &[
    // M220 SN prefixes
    ("Q155", "M220"),
    ("Q054", "M220"),
    ("Q058", "M220"),
    ("Q198", "M220"),
    // M200 SN prefixes
    ("Q053", "M200"),
    // M110 SN prefixes
    ("Q100", "M110"),
];

/// A discovered Bluetooth device.
#[derive(Debug, Clone)]
pub struct BtDevice {
    /// Bluetooth MAC address (colon-separated, e.g. "AA:BB:CC:DD:EE:FF").
    pub address: String,
    /// Device name / alias as reported by `BlueZ`.
    pub name: String,
}

/// Check if a device name matches a known Phomemo model prefix or
/// serial-number prefix.
pub fn matches_phomemo_name(name: &str) -> bool {
    let upper = name.to_uppercase();
    model_aliases()
        .iter()
        .any(|(prefix, _)| upper.starts_with(prefix))
        || upper.contains("PHOMEMO")
        || looks_like_sn_prefix(&upper)
        || KNOWN_SN_PREFIXES
            .iter()
            .any(|(sn_prefix, _)| upper.starts_with(&sn_prefix.to_uppercase()))
}

/// Try to resolve a model name from the BT device name.
///
/// Returns the model name if the device name matches a known model prefix
/// or serial-number prefix, otherwise returns the device name as-is.
pub fn resolve_model_name(name: &str) -> &str {
    let upper = name.to_uppercase();
    // Check model aliases first (e.g. "M220-xxxx" or "Phomemo M220").
    for (prefix, canonical) in model_aliases() {
        if upper.starts_with(prefix) || upper.contains(prefix) {
            return canonical;
        }
    }

    // Check SN prefixes (e.g., "Q198G5949230062" → "M220")
    for (sn_prefix, model) in KNOWN_SN_PREFIXES {
        if upper.starts_with(&sn_prefix.to_uppercase()) {
            return model;
        }
    }
    name
}

/// Resolve model name from a MAC address by looking up the BT device name.
///
/// Queries the paired device list and matches the MAC address.
/// Returns `Some("M220")` etc., or `None` if the MAC is unknown.
pub fn resolve_model_name_from_mac(mac: &str) -> Option<String> {
    let devices = list_paired_phomemo_devices().ok()?;
    let mac_upper = mac.to_uppercase();
    devices
        .iter()
        .find(|d| d.address.to_uppercase() == mac_upper)
        .map(|d| resolve_model_name(&d.name).to_string())
}

/// Lazily-initialized system D-Bus connection, shared across all
/// discovery calls for the lifetime of the process.
fn system_bus() -> Result<&'static zbus::blocking::Connection, io::Error> {
    use std::sync::OnceLock;
    static BUS: OnceLock<Result<zbus::blocking::Connection, String>> = OnceLock::new();
    BUS.get_or_init(|| zbus::blocking::Connection::system().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| io::Error::new(io::ErrorKind::ConnectionRefused, e.as_str()))
}

/// Enumerate paired Bluetooth devices that look like Phomemo printers.
///
/// Queries `BlueZ` via the system D-Bus using
/// `org.freedesktop.DBus.ObjectManager.GetManagedObjects()` on
/// `org.bluez`.  Filters for `org.bluez.Device1` objects where
/// `Paired == true` and the name matches a known Phomemo prefix.
///
/// The call is bounded by `DISCOVERY_TIMEOUT` — if D-Bus does not
/// respond in time, an empty list (or cached results) is returned
/// instead of blocking indefinitely.
pub fn list_paired_phomemo_devices() -> Result<Vec<BtDevice>, io::Error> {
    list_paired_phomemo_devices_timeout(DISCOVERY_TIMEOUT)
}

/// Bounded version of device enumeration with an explicit timeout.
///
/// Spawns the blocking D-Bus query on a helper thread and joins with
/// the given deadline.  On timeout the thread is detached (it will
/// finish in the background) and the most recent cached result is
/// returned.
pub fn list_paired_phomemo_devices_timeout(timeout: Duration) -> Result<Vec<BtDevice>, io::Error> {
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::Builder::new()
        .name("bt-discovery".into())
        .spawn(move || {
            let result = list_paired_phomemo_devices_inner();
            let _ = tx.send(result);
        })
        .map_err(io::Error::other)?;

    match rx.recv_timeout(timeout) {
        Ok(Ok(devices)) => {
            update_cache(&devices);
            Ok(devices)
        }
        Ok(Err(e)) => {
            // D-Bus error — return cache if available
            read_cache().ok_or(e)
        }
        Err(_timeout) => {
            // Timed out — return cache (may be empty)
            Ok(read_cache().unwrap_or_default())
        }
    }
}

/// Cache of last successful discovery result.
static DISCOVERY_CACHE: std::sync::Mutex<Option<Vec<BtDevice>>> = std::sync::Mutex::new(None);

fn update_cache(devices: &[BtDevice]) {
    if let Ok(mut cache) = DISCOVERY_CACHE.lock() {
        *cache = Some(devices.to_vec());
    }
}

fn read_cache() -> Option<Vec<BtDevice>> {
    DISCOVERY_CACHE.lock().ok()?.clone()
}

/// Inner (unbounded) discovery — runs on a dedicated thread.
fn list_paired_phomemo_devices_inner() -> Result<Vec<BtDevice>, io::Error> {
    let conn = system_bus()?;

    let proxy = zbus::blocking::fdo::ObjectManagerProxy::builder(conn)
        .destination("org.bluez")
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?
        .path("/")
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?
        .build()
        .map_err(|e| io::Error::new(io::ErrorKind::ConnectionRefused, e))?;

    let objects = proxy.get_managed_objects().map_err(io::Error::other)?;

    let mut devices = Vec::new();

    for interfaces in objects.values() {
        // Only look at objects implementing org.bluez.Device1.
        let Some(props) = interfaces.get("org.bluez.Device1") else {
            continue;
        };

        // Skip unpaired devices.
        let paired = props
            .get("Paired")
            .and_then(|v| TryInto::<bool>::try_into(v.try_clone().ok()?).ok())
            .unwrap_or(false);
        if !paired {
            continue;
        }

        let address = props
            .get("Address")
            .and_then(|v| TryInto::<String>::try_into(v.try_clone().ok()?).ok())
            .unwrap_or_default();
        let name = props
            .get("Alias")
            .or_else(|| props.get("Name"))
            .and_then(|v| TryInto::<String>::try_into(v.try_clone().ok()?).ok())
            .unwrap_or_default();

        if !address.is_empty() && matches_phomemo_name(&name) {
            devices.push(BtDevice { address, name });
        }
    }

    Ok(devices)
}

/// Build a `btspp://` URI from a MAC address.
///
/// Uses hyphens instead of colons because PAPPL's internal URI parser
/// interprets colons as port delimiters and rejects MAC addresses.
pub fn make_uri(address: &str) -> String {
    format!("btspp://{}", address.replace(':', "-"))
}

/// Build an IEEE 1284 device-ID string from a device name.
pub fn make_device_id(name: &str) -> String {
    let model = resolve_model_name(name);
    format!("MFG:Phomemo;MDL:{model};CMD:PHOMEMO;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_known_models() {
        assert!(matches_phomemo_name("M220-A4B3"));
        assert!(matches_phomemo_name("M200"));
        assert!(matches_phomemo_name("m220")); // case insensitive
        assert!(matches_phomemo_name("Phomemo M220"));
        assert!(!matches_phomemo_name("HP LaserJet"));
        assert!(!matches_phomemo_name(""));
    }

    #[test]
    fn test_matches_sn_prefixes() {
        // M220 serial numbers start with Q155, Q054, Q058, Q198
        assert!(matches_phomemo_name("Q198G5949230062"));
        assert!(matches_phomemo_name("Q054A1234567890"));
        assert!(matches_phomemo_name("Q999G1234567890"));
        assert!(!matches_phomemo_name("X999G1234567890"));
    }

    #[test]
    fn test_resolve_model_from_sn() {
        assert_eq!(resolve_model_name("Q198G5949230062"), "M220");
        assert_eq!(resolve_model_name("M220-A4B3"), "M220");
        assert_eq!(resolve_model_name("Phomemo D30"), "D30");
        assert_eq!(resolve_model_name("UnknownDevice"), "UnknownDevice");
    }

    #[test]
    fn test_make_uri() {
        assert_eq!(make_uri("AA:BB:CC:DD:EE:FF"), "btspp://AA-BB-CC-DD-EE-FF");
    }

    #[test]
    fn test_make_device_id() {
        assert_eq!(
            make_device_id("M220-A4B3"),
            "MFG:Phomemo;MDL:M220;CMD:PHOMEMO;"
        );
        // SN-based name should resolve to model
        assert_eq!(
            make_device_id("Q198G5949230062"),
            "MFG:Phomemo;MDL:M220;CMD:PHOMEMO;"
        );
    }
}
