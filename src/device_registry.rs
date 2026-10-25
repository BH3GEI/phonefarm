//! Declared device identities only. Registration never changes execution scope.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const SCHEMA: &str = "phonefarm/device-registry@1";
const CURRENT: &str = "device-registry.json";
const HISTORY: &str = "device-registry-history";
const LIMIT: u64 = 64 * 1024;
const MAX_REVISION: u64 = (1_u64 << 53) - 1;
const BINDING_SCHEMA: &str = "phonefarm/device-binding@1";

#[derive(Debug)]
struct Error {
    code: &'static str,
    message: String,
}
type Result<T> = std::result::Result<T, Error>;
fn error(code: &'static str, message: impl Into<String>) -> Error {
    Error {
        code,
        message: message.into(),
    }
}
fn unavailable(message: impl ToString) -> Error {
    error("unavailable", message.to_string())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Device {
    pub device_id: String,
    pub label: String,
    pub aliases: Vec<String>,
    pub enabled: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeviceBinding {
    pub schema: String,
    pub domain_id: String,
    pub registry_revision: u64,
    pub device_id: String,
    pub serial: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    expected_revision: u64,
    device: Device,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    schema: String,
    revision: u64,
    devices: Vec<Device>,
    updated_at: String,
    identity_basis: String,
    physical_identity_verified: bool,
    execution_scope: String,
}

pub(crate) fn valid_device_id(value: &str) -> std::result::Result<(), String> {
    let id = value.as_bytes();
    if id.is_empty()
        || id.len() > 64
        || !id[0].is_ascii_alphanumeric()
        || !id
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        return Err("device_id must be 1..64 ASCII letters/digits/underscore/hyphen, starting with a letter or digit".into());
    }
    Ok(())
}
fn valid_serial(serial: &str) -> std::result::Result<(), String> {
    if serial.is_empty()
        || serial.len() > 256
        || serial.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(
            "serial must contain 1..256 bytes without whitespace or control characters".into(),
        );
    }
    Ok(())
}
fn valid_device(device: &Device) -> std::result::Result<(), String> {
    valid_device_id(&device.device_id)?;
    if !(1..=128).contains(&device.label.chars().count())
        || device.label.chars().any(char::is_control)
    {
        return Err("label must contain 1..128 characters without control characters".into());
    }
    if !(1..=32).contains(&device.aliases.len()) {
        return Err("aliases must contain 1..32 serial strings".into());
    }
    let mut seen = HashSet::new();
    for alias in &device.aliases {
        valid_serial(alias)?;
        if !seen.insert(alias) {
            return Err("aliases must not repeat within one device".into());
        }
    }
    Ok(())
}
fn unique_aliases(devices: &[Device]) -> std::result::Result<(), String> {
    let mut seen = HashSet::new();
    for device in devices {
        for alias in &device.aliases {
            if !seen.insert(alias) {
                return Err("an alias is already assigned to another registered device (including disabled devices)".into());
            }
        }
    }
    Ok(())
}
fn parse_request(bytes: &[u8]) -> Result<Request> {
    if bytes.len() as u64 > LIMIT {
        return Err(error("invalid_request", "request exceeds 64 KiB"));
    }
    let request: Request = serde_json::from_slice(bytes)
        .map_err(|e| error("invalid_request", format!("invalid registry request: {e}")))?;
    if request.expected_revision > MAX_REVISION {
        return Err(error("invalid_request", "expected_revision exceeds 2^53-1"));
    }
    valid_device(&request.device).map_err(|e| error("invalid_request", e))?;
    Ok(request)
}
fn parse_registry(bytes: &[u8]) -> Result<Registry> {
    let registry: Registry = serde_json::from_slice(bytes)
        .map_err(|e| unavailable(format!("invalid registry record: {e}")))?;
    if registry.schema != SCHEMA
        || !(1..=MAX_REVISION).contains(&registry.revision)
        || registry.identity_basis != "operator_declared"
        || registry.physical_identity_verified
        || registry.execution_scope != "global"
        || registry.devices.is_empty()
        || registry.devices.len() > 128
        || chrono::DateTime::parse_from_rfc3339(&registry.updated_at).is_err()
    {
        return Err(unavailable("registry envelope is invalid or unsupported"));
    }
    for device in &registry.devices {
        valid_device(device).map_err(unavailable)?;
    }
    if registry
        .devices
        .windows(2)
        .any(|p| p[0].device_id >= p[1].device_id)
    {
        return Err(unavailable(
            "registry device IDs are duplicated or not sorted",
        ));
    }
    unique_aliases(&registry.devices).map_err(unavailable)?;
    Ok(registry)
}
fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(unavailable(e)),
    }
}
fn check_directory(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path).map_err(unavailable)?.is_dir() {
        return Err(unavailable(format!(
            "not a regular directory: {}",
            path.display()
        )));
    }
    crate::shared_access::check_directory(path).map_err(unavailable)
}
fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>> {
    if !exists(path)? {
        return Ok(None);
    }
    crate::shared_access::check_existing(path, false).map_err(unavailable)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(unavailable)?;
    let metadata = file.metadata().map_err(unavailable)?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(unavailable(format!(
            "not a bounded regular record: {}",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(unavailable)?;
    if bytes.len() as u64 > LIMIT {
        return Err(unavailable("record exceeds 64 KiB"));
    }
    Ok(Some(bytes))
}
fn read_current(root: &Path) -> Result<Option<(Registry, Vec<u8>)>> {
    if exists(root)? {
        check_directory(root)?;
    }
    let bytes = read_bounded(&root.join(CURRENT))?;
    let history = root.join(HISTORY);
    if exists(&history)? {
        check_directory(&history)?;
    }
    let Some(bytes) = bytes else {
        if exists(&history)?
            && fs::read_dir(&history)
                .map_err(unavailable)?
                .next()
                .is_some()
        {
            return Err(unavailable(
                "current registry is missing but historical records exist; preserved",
            ));
        }
        return Ok(None);
    };
    let registry = parse_registry(&bytes)?;
    // A crash after archiving and before publication may leave this exact copy.
    if let Some(archived) = read_bounded(&history.join(format!("{}.json", registry.revision)))? {
        if archived != bytes {
            return Err(unavailable(
                "current registry differs from its immutable archived original",
            ));
        }
    }
    if registry.revision > 1 {
        let previous = read_bounded(&history.join(format!("{}.json", registry.revision - 1)))?
            .ok_or_else(|| unavailable("previous registry original is missing"))?;
        if parse_registry(&previous)?.revision != registry.revision - 1 {
            return Err(unavailable(
                "previous registry original has an inconsistent revision",
            ));
        }
    }
    Ok(Some((registry, bytes)))
}
pub(crate) fn snapshot(root: &Path) -> std::result::Result<(u64, Vec<Device>), String> {
    Ok(read_current(root).map_err(|e| e.message)?.map_or_else(
        || (0, Vec::new()),
        |(registry, _)| (registry.revision, registry.devices),
    ))
}
fn valid_binding(binding: &DeviceBinding) -> std::result::Result<(), String> {
    if binding.schema != BINDING_SCHEMA
        || !crate::queue_owner::valid_instance(&binding.domain_id)
        || !(1..=MAX_REVISION).contains(&binding.registry_revision)
    {
        return Err("unsupported or invalid device binding identity".into());
    }
    valid_device_id(&binding.device_id)?;
    valid_serial(&binding.serial)
}
pub(crate) fn parse_binding(raw: &str) -> std::result::Result<DeviceBinding, String> {
    if raw.len() > 4096 {
        return Err("device binding exceeds 4 KiB".into());
    }
    let binding =
        serde_json::from_str(raw).map_err(|e| format!("invalid device binding JSON: {e}"))?;
    valid_binding(&binding)?;
    Ok(binding)
}
pub(crate) fn resolve_binding(
    storage: &crate::lease_storage::Storage,
    serial: &str,
) -> std::result::Result<DeviceBinding, String> {
    valid_serial(serial)?;
    let (device_root, root) = storage.require_ready()?;
    let paths = crate::lease_domain::inspect(&root)?;
    paths.require_device_root(&device_root)?;
    let domain_id = paths
        .domain_id
        .ok_or("device binding requires layout 2; use explicit domain-init first")?;
    let (revision, devices) = snapshot(&root)?;
    let device = devices
        .iter()
        .find(|d| d.aliases.iter().any(|alias| alias == serial))
        .ok_or("serial is not a registered device alias")?;
    if !device.enabled {
        return Err("registered device is disabled".into());
    }
    Ok(DeviceBinding {
        schema: BINDING_SCHEMA.into(),
        domain_id,
        registry_revision: revision,
        device_id: device.device_id.clone(),
        serial: serial.into(),
    })
}
/// New-start admission only. Recovery uses the original recorded binding.
pub(crate) fn validate_binding(
    root: &Path,
    binding: &DeviceBinding,
) -> std::result::Result<(), String> {
    valid_binding(binding)?;
    let paths = crate::lease_domain::inspect(root)?;
    if paths.domain_id.as_ref() != Some(&binding.domain_id) {
        return Err("device binding domain does not match the configured domain".into());
    }
    let (revision, devices) = snapshot(root)?;
    if revision != binding.registry_revision {
        return Err("device binding registry revision changed; no device work was started".into());
    }
    if !devices.iter().any(|d| {
        d.device_id == binding.device_id
            && d.enabled
            && d.aliases.iter().any(|alias| alias == &binding.serial)
    }) {
        return Err(
            "device binding is disabled or does not match the registered device/alias".into(),
        );
    }
    Ok(())
}
/// Existing ownership is attributed using its original published registry,
/// never today's alias assignment or enabled flag. This grants no new start.
pub(crate) fn validate_recorded_binding(
    root: &Path,
    binding: &DeviceBinding,
) -> std::result::Result<(), String> {
    valid_binding(binding)?;
    if crate::lease_domain::inspect(root)?.domain_id.as_ref() != Some(&binding.domain_id) {
        return Err("recorded device binding domain does not match the configured domain".into());
    }
    let (current, _) = read_current(root)
        .map_err(|e| e.message)?
        .ok_or("recorded device binding has no registry original")?;
    if binding.registry_revision > current.revision {
        return Err("recorded device binding revision is newer than the registry".into());
    }
    let original = if binding.registry_revision == current.revision {
        current
    } else {
        let bytes = read_bounded(
            &root
                .join(HISTORY)
                .join(format!("{}.json", binding.registry_revision)),
        )
        .map_err(|e| e.message)?
        .ok_or("recorded device binding registry original is missing")?;
        parse_registry(&bytes).map_err(|e| e.message)?
    };
    if original.revision != binding.registry_revision
        || !original.devices.iter().any(|d| {
            d.device_id == binding.device_id
                && d.enabled
                && d.aliases.iter().any(|alias| alias == &binding.serial)
        })
    {
        return Err("recorded device binding differs from its registry original".into());
    }
    Ok(())
}
fn empty() -> Value {
    json!({"schema":SCHEMA,"revision":0,"devices":[],"updated_at":null,
        "identity_basis":"operator_declared","physical_identity_verified":false,"execution_scope":"global"})
}
fn get(root: &Path) -> Result<Value> {
    match read_current(root)? {
        Some((registry, _)) => serde_json::to_value(registry).map_err(unavailable),
        None => Ok(empty()),
    }
}

fn require_clear(root: &Path) -> Result<()> {
    for name in ["devlock.d", "phonefarm-lease.d"] {
        if exists(&root.join(name))? {
            return Err(error(
                "busy",
                "lease/queue owner or recovery record exists; preserved",
            ));
        }
    }
    if let Some(queue) = read_bounded(&root.join("devlock.queue"))? {
        let queue = std::str::from_utf8(&queue).map_err(unavailable)?;
        if !queue.trim().is_empty() {
            return Err(error("busy", "device work is queued"));
        }
    }
    Ok(())
}
fn archive(root: &Path, revision: u64, bytes: &[u8]) -> Result<()> {
    let dir = root.join(HISTORY);
    if exists(&dir)? {
        check_directory(&dir)?;
    } else {
        crate::shared_access::create_dir(&dir).map_err(unavailable)?;
    }
    let path = dir.join(format!("{revision}.json"));
    if let Some(old) = read_bounded(&path)? {
        if old != bytes {
            return Err(unavailable(
                "immutable registry original differs; preserved",
            ));
        }
        // A previous attempt may have stopped after the hard link but before
        // its directory sync. Re-establish durability before replacing current.
        File::open(&dir)
            .and_then(|f| f.sync_all())
            .map_err(unavailable)?;
        return File::open(root)
            .and_then(|f| f.sync_all())
            .map_err(unavailable);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(unavailable)?
        .as_nanos();
    let temp = dir.join(format!(".registry-{}-{stamp}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temp)
        .map_err(unavailable)?;
    let result = (|| {
        crate::shared_access::created_file(&temp, &file, false).map_err(unavailable)?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(unavailable)?;
        // Publish a complete original create-once; never expose a partial archive.
        fs::hard_link(&temp, &path).map_err(unavailable)?;
        File::open(&dir)
            .and_then(|f| f.sync_all())
            .map_err(unavailable)
    })();
    let removed = fs::remove_file(&temp).map_err(unavailable);
    result?;
    removed?;
    File::open(root)
        .and_then(|f| f.sync_all())
        .map_err(unavailable)
}
fn update_locked(root: &Path, queue_dir: &Path, request: Request) -> Result<Value> {
    require_clear(queue_dir)?;
    let current = read_current(root)?;
    let revision = current.as_ref().map_or(0, |(r, _)| r.revision);
    if request.expected_revision != revision {
        return Err(error("conflict", format!("expected_revision does not match current revision {revision}; read the registry before any new submission")));
    }
    let mut devices = current
        .as_ref()
        .map_or_else(Vec::new, |(r, _)| r.devices.clone());
    match devices.binary_search_by(|d| d.device_id.cmp(&request.device.device_id)) {
        Ok(index) if devices[index] == request.device => {
            return serde_json::to_value(&current.as_ref().unwrap().0).map_err(unavailable)
        }
        Ok(index) => devices[index] = request.device,
        Err(index) if devices.len() < 128 => devices.insert(index, request.device),
        Err(_) => {
            return Err(error(
                "invalid_request",
                "registry is limited to 128 devices",
            ))
        }
    }
    unique_aliases(&devices).map_err(|e| error("conflict", e))?;
    if revision == MAX_REVISION {
        return Err(error("conflict", "registry revision is exhausted"));
    }
    let next = Registry {
        schema: SCHEMA.into(),
        revision: revision + 1,
        devices,
        updated_at: chrono::Utc::now().to_rfc3339(),
        identity_basis: "operator_declared".into(),
        physical_identity_verified: false,
        execution_scope: "global".into(),
    };
    let mut bytes = serde_json::to_vec(&next).map_err(unavailable)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > LIMIT {
        return Err(error(
            "invalid_request",
            "resulting registry exceeds 64 KiB",
        ));
    }
    if let Some((old, original)) = current {
        archive(root, old.revision, &original)?;
    }
    crate::devlease::durable_replace(&root.join(CURRENT), &bytes).map_err(unavailable)?;
    serde_json::to_value(next).map_err(unavailable)
}
fn set(root: &Path, device_root: &Path, request: Request) -> Result<Value> {
    crate::shared_access::create_dir_all(root).map_err(unavailable)?;
    check_directory(root)?;
    let bridge = crate::shared_access::lock_file(&root.join("phonefarm-bridge.lock"), 0o600)
        .map_err(unavailable)?;
    bridge.try_lock().map_err(|e| match e {
        std::fs::TryLockError::WouldBlock => error("busy", "device lease bridge is held"),
        other => unavailable(other),
    })?;
    let _owner = crate::queue_owner::lock(root).map_err(|e| {
        if e.starts_with("queue metadata mutex unavailable:") {
            error("busy", e)
        } else {
            unavailable(e)
        }
    })?;
    let paths = crate::lease_domain::inspect(root).map_err(unavailable)?;
    paths
        .require_device_root(device_root)
        .map_err(unavailable)?;
    if crate::devlease::lane_occupancy(root).map_err(unavailable)? {
        return Err(error(
            "busy",
            "device lane owner or recovery record exists; preserved",
        ));
    }
    let mut queue = crate::queue_owner::QueueMutex::acquire(&paths.queue_dir).map_err(|e| {
        if e == crate::queue_owner::QUEUE_MUTEX_BUSY {
            error("busy", e)
        } else {
            unavailable(e)
        }
    })?;
    let result = update_locked(root, &paths.queue_dir, request);
    queue.release().map_err(unavailable)?;
    result
}
fn request_bytes(path: &str) -> Result<Vec<u8>> {
    if path != "-" {
        return read_bounded(Path::new(path))
            .map_err(|e| error("invalid_request", e.message))?
            .ok_or_else(|| error("invalid_request", "request file is missing"));
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| error("invalid_request", e.to_string()))?;
    Ok(bytes)
}
fn run(args: &[String]) -> Result<Value> {
    if args.first().map(String::as_str) == Some("resolve") {
        let mut serial = None;
        let mut json = false;
        let mut it = args[1..].iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--serial" if serial.is_none() => {
                    serial =
                        Some(it.next().ok_or_else(|| {
                            error("invalid_request", "--serial requires a value")
                        })?);
                }
                "--json" if !json => json = true,
                _ => {
                    return Err(error(
                        "invalid_request",
                        "expected lease resolve --serial S --json",
                    ))
                }
            }
        }
        let serial = serial.filter(|_| json).ok_or_else(|| {
            error(
                "invalid_request",
                "expected lease resolve --serial S --json",
            )
        })?;
        valid_serial(serial).map_err(|e| error("invalid_request", e))?;
        return serde_json::to_value(
            resolve_binding(&crate::lease_storage::resolve(), serial).map_err(unavailable)?,
        )
        .map_err(unavailable);
    }
    let request = match args.first().map(String::as_str) {
        Some("devices") if args.len() == 2 && args[1] == "--json" => None,
        Some("devices-set") => {
            let mut request = None;
            let mut json = false;
            let mut it = args[1..].iter();
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "--request" if request.is_none() => {
                        request = Some(
                            it.next()
                                .ok_or_else(|| {
                                    error("invalid_request", "--request requires FILE or -")
                                })?
                                .as_str(),
                        )
                    }
                    "--json" if !json => json = true,
                    _ => {
                        return Err(error(
                            "invalid_request",
                            "expected lease devices-set --request FILE|- --json",
                        ))
                    }
                }
            }
            if !json {
                return Err(error("invalid_request", "devices-set requires --json"));
            }
            Some(parse_request(&request_bytes(request.ok_or_else(
                || error("invalid_request", "devices-set requires --request FILE|-"),
            )?)?)?)
        }
        _ => {
            return Err(error(
                "invalid_request",
                "expected lease devices --json or lease devices-set --request FILE|- --json",
            ))
        }
    };
    let (devices, root) = crate::lease_storage::resolve()
        .require_ready()
        .map_err(unavailable)?;
    match request {
        Some(request) => set(&root, &devices, request),
        None => get(&root),
    }
}
pub(crate) fn run_cli(args: &[String]) -> i32 {
    match run(args) {
        Ok(value) => {
            println!("{value}");
            0
        }
        Err(e) => {
            println!(
                "{}",
                json!({"schema":"phonefarm/device-registry-error@1","code":e.code,"error":e.message})
            );
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    fn set(root: &Path, request: Request) -> Result<Value> {
        super::set(root, &root.join("devices"), request)
    }
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(
                std::env::temp_dir()
                    .join(format!("pf-device-registry-{}-{stamp}", std::process::id())),
            )
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn request(revision: u64, id: &str, aliases: &[&str], enabled: bool) -> Request {
        parse_request(
            &serde_json::to_vec(&json!({"expected_revision":revision,
            "device":{"device_id":id,"label":id,"aliases":aliases,"enabled":enabled}}))
            .unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn binding_resolves_aliases_and_rejects_changed_or_unavailable_identity() {
        let s = Scratch::new();
        set(&s.0, request(0, "A", &["usb-A", "192.0.2.1:5555"], true)).unwrap();
        assert!(resolve_binding(&crate::lease_storage::Storage::custom(&s.0), "usb-A").is_err());
        crate::lease_domain::init(&s.0, &s.0.join("devices")).unwrap();
        let storage = crate::lease_storage::Storage::custom(&s.0);
        let usb = resolve_binding(&storage, "usb-A").unwrap();
        let wifi = resolve_binding(&storage, "192.0.2.1:5555").unwrap();
        assert_eq!(usb.device_id, wifi.device_id);
        assert_eq!(usb.domain_id, wifi.domain_id);
        assert_eq!(snapshot(&s.0).unwrap().0, usb.registry_revision);
        validate_binding(&s.0, &usb).unwrap();
        let original = serde_json::to_string(&usb).unwrap();
        assert_eq!(parse_binding(&original).unwrap(), usb);
        assert!(parse_binding(&(original.clone() + &" ".repeat(4096))).is_err());
        let mut invalid = serde_json::to_value(&usb).unwrap();
        invalid["extra"] = json!(true);
        assert!(parse_binding(&invalid.to_string()).is_err());
        let mut wrong = usb.clone();
        wrong.domain_id = "ffffffffffffffffffffffffffffffff".into();
        assert!(validate_binding(&s.0, &wrong).is_err());
        wrong = usb.clone();
        wrong.serial = "not-an-alias".into();
        assert!(validate_binding(&s.0, &wrong).is_err());
        set(&s.0, request(1, "A", &["usb-A", "192.0.2.1:5555"], false)).unwrap();
        assert!(validate_binding(&s.0, &usb).is_err());
        validate_recorded_binding(&s.0, &usb).unwrap();
        validate_recorded_binding(&s.0, &wifi).unwrap();
        wrong = usb;
        wrong.registry_revision = 2;
        assert!(validate_binding(&s.0, &wrong).is_err());
        assert!(validate_recorded_binding(&s.0, &wrong).is_err());
        assert!(resolve_binding(&storage, "usb-A").is_err());
        assert_eq!(get(&s.0).unwrap()["execution_scope"], "global");
        fs::write(s.0.join(HISTORY).join("1.json"), b"{}\n").unwrap();
        assert!(validate_recorded_binding(&s.0, &wifi).is_err());
    }
    #[test]
    fn registry_cas_preserves_originals_and_disabled_aliases() {
        let scratch = Scratch::new();
        assert_eq!(get(&scratch.0).unwrap()["revision"], 0);
        assert!(!scratch.0.exists());
        let first = set(
            &scratch.0,
            request(0, "A", &["usb-A", "192.0.2.1:5555"], true),
        )
        .unwrap();
        let bytes = fs::read(scratch.0.join(CURRENT)).unwrap();
        assert_eq!(
            set(
                &scratch.0,
                request(1, "A", &["usb-A", "192.0.2.1:5555"], true)
            )
            .unwrap(),
            first
        );
        assert_eq!(
            set(&scratch.0, request(0, "B", &["usb-B"], true))
                .unwrap_err()
                .code,
            "conflict"
        );
        assert_eq!(
            set(&scratch.0, request(1, "B", &["usb-A"], true))
                .unwrap_err()
                .code,
            "conflict"
        );
        set(
            &scratch.0,
            request(1, "A", &["usb-A", "192.0.2.1:5555"], false),
        )
        .unwrap();
        assert_eq!(
            fs::read(scratch.0.join(HISTORY).join("1.json")).unwrap(),
            bytes
        );
        assert_eq!(
            set(&scratch.0, request(2, "B", &["usb-A"], true))
                .unwrap_err()
                .code,
            "conflict"
        );
        set(&scratch.0, request(2, "A", &["192.0.2.1:5555"], false)).unwrap();
        let final_value = set(&scratch.0, request(3, "B", &["usb-A"], true)).unwrap();
        assert_eq!(final_value["revision"], 4);
        assert_eq!(final_value["execution_scope"], "global");
        assert_eq!(final_value["physical_identity_verified"], false);
        assert_eq!(
            fs::read(scratch.0.join(HISTORY).join("1.json")).unwrap(),
            bytes
        );
        crate::lease_domain::init(&scratch.0, &scratch.0.join("devices")).unwrap();
        assert_eq!(
            set(&scratch.0, request(4, "B", &["usb-A"], false)).unwrap()["revision"],
            5
        );
        assert!(scratch.0.join("devlock.d").is_file());
    }
    #[test]
    fn registry_writes_refuse_bridge_queue_and_unknown_owner_without_cleanup() {
        let scratch = Scratch::new();
        set(&scratch.0, request(0, "A", &["usb-A"], true)).unwrap();
        let original = fs::read(scratch.0.join(CURRENT)).unwrap();
        let bridge = File::open(scratch.0.join("phonefarm-bridge.lock")).unwrap();
        bridge.try_lock().unwrap();
        assert_eq!(
            set(&scratch.0, request(1, "B", &["usb-B"], true))
                .unwrap_err()
                .code,
            "busy"
        );
        drop(bridge);
        for name in ["devlock.d", "phonefarm-lease.d", "devlock.qmutex"] {
            fs::create_dir(scratch.0.join(name)).unwrap();
            assert_eq!(
                set(&scratch.0, request(1, "B", &["usb-B"], true))
                    .unwrap_err()
                    .code,
                "busy"
            );
            assert!(scratch.0.join(name).is_dir());
            fs::remove_dir(scratch.0.join(name)).unwrap();
        }
        fs::write(scratch.0.join("devlock.queue"), b"queued\n").unwrap();
        assert_eq!(
            set(&scratch.0, request(1, "B", &["usb-B"], true))
                .unwrap_err()
                .code,
            "busy"
        );
        assert_eq!(fs::read(scratch.0.join(CURRENT)).unwrap(), original);
        assert!(!scratch.0.join("devlock.qmutex").exists());
        assert_eq!(get(&scratch.0).unwrap()["revision"], 1);
    }
    #[test]
    fn registry_rejects_invalid_input_and_preserves_interrupted_archive() {
        for value in [
            json!({"expected_revision":0,"device":{"device_id":"../bad","label":"bad","aliases":["a"],"enabled":true}}),
            json!({"expected_revision":0,"device":{"device_id":"A","label":"a","aliases":["a","a"],"enabled":true}}),
            json!({"expected_revision":0,"device":{"device_id":"A","label":"a","aliases":["a b"],"enabled":true}}),
            json!({"expected_revision":0,"device":{"device_id":"A","label":"a","aliases":["a"],"enabled":true,"scope":"device"}}),
        ] {
            assert_eq!(
                parse_request(&serde_json::to_vec(&value).unwrap())
                    .err()
                    .unwrap()
                    .code,
                "invalid_request"
            );
        }
        let scratch = Scratch::new();
        set(&scratch.0, request(0, "A", &["usb-A"], true)).unwrap();
        let original = fs::read(scratch.0.join(CURRENT)).unwrap();
        archive(&scratch.0, 1, &original).unwrap(); // publication never happened
        assert_eq!(get(&scratch.0).unwrap()["revision"], 1);
        set(&scratch.0, request(1, "B", &["usb-B"], true)).unwrap();
        assert_eq!(
            fs::read(scratch.0.join(HISTORY).join("1.json")).unwrap(),
            original
        );
        let current = fs::read(scratch.0.join(CURRENT)).unwrap();
        fs::write(scratch.0.join(HISTORY).join("2.json"), b"{}\n").unwrap();
        assert_eq!(
            set(&scratch.0, request(2, "A", &["usb-A"], false))
                .unwrap_err()
                .code,
            "unavailable"
        );
        assert_eq!(fs::read(scratch.0.join(CURRENT)).unwrap(), current);
        fs::write(scratch.0.join(CURRENT), b"{}\n").unwrap();
        assert_eq!(get(&scratch.0).unwrap_err().code, "unavailable");
    }
}
