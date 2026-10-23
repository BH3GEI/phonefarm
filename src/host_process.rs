//! Host boot and process-instance identity. It does not prove device recovery.
#[cfg(target_os = "linux")]
use std::fs;

pub(crate) fn clean_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Process {
    pub(crate) parent: u32,
    pub(crate) start: String,
    pub(crate) zombie: bool,
}

#[cfg(target_os = "macos")]
pub(crate) fn process(pid: u32) -> Result<Option<Process>, String> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let got = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            1, // Include unreaped zombies; a short-lived child still has an identity.
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if got == size {
        return Ok(Some(Process {
            parent: info.pbi_ppid,
            start: format!("{}.{:06}", info.pbi_start_tvsec, info.pbi_start_tvusec),
            zombie: info.pbi_status == 5,
        }));
    }
    let error = std::io::Error::last_os_error();
    if got <= 0 && error.raw_os_error() == Some(libc::ESRCH) {
        Ok(None)
    } else {
        Err(format!("cannot verify host process {pid}: {error}"))
    }
}
#[cfg(target_os = "linux")]
pub(crate) fn process(pid: u32) -> Result<Option<Process>, String> {
    let text = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot verify host process {pid}: {e}")),
    };
    let (_, fields) = text.rsplit_once(") ").ok_or("invalid host process stat")?;
    let parts: Vec<_> = fields.split_whitespace().collect();
    if parts.len() < 20 {
        return Err("short host process stat".into());
    }
    let parent = parts[1]
        .parse()
        .map_err(|_| "invalid host process parent")?;
    let start: u64 = parts[19]
        .parse()
        .map_err(|_| "invalid host process start")?;
    Ok(Some(Process {
        parent,
        start: start.to_string(),
        zombie: matches!(parts[0], "Z" | "X"),
    }))
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn process(_: u32) -> Result<Option<Process>, String> {
    Err("queue process identity is unsupported on this host".into())
}

#[cfg(target_os = "macos")]
pub(crate) fn boot_id() -> Result<String, String> {
    let mut boot: libc::timeval = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of_val(&boot);
    let result = unsafe {
        libc::sysctlbyname(
            b"kern.boottime\0".as_ptr().cast(),
            &mut boot as *mut _ as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size != std::mem::size_of_val(&boot) || boot.tv_sec <= 0 {
        return Err(format!(
            "cannot read host boot identity: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(format!("{}.{:06}", boot.tv_sec, boot.tv_usec))
}
#[cfg(target_os = "linux")]
pub(crate) fn boot_id() -> Result<String, String> {
    let id = fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(|e| e.to_string())?;
    let id = id.trim();
    if !clean_identity(id) {
        return Err("invalid host boot identity".into());
    }
    Ok(id.into())
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn boot_id() -> Result<String, String> {
    Err("queue boot identity is unsupported on this host".into())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) pid: u32,
    pub(crate) start: String,
    pub(crate) host_boot_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstanceState {
    Alive,
    PreviousBoot,
    Exited,
    Zombie,
    PidReused,
}

fn valid_pid(pid: u32) -> bool {
    pid > 0 && pid <= i32::MAX as u32
}

/// Capture an existing process, including an exited child that has not been reaped.
pub(crate) fn capture(pid: u32) -> Result<Identity, String> {
    if !valid_pid(pid) {
        return Err("invalid host process PID".into());
    }
    let host_boot_id = boot_id()?;
    let observed = process(pid)?.ok_or_else(|| format!("host process {pid} is absent"))?;
    if !clean_identity(&host_boot_id) || !clean_identity(&observed.start) {
        return Err("invalid host process identity".into());
    }
    Ok(Identity {
        pid,
        start: observed.start,
        host_boot_id,
    })
}

/// Classify this exact process instance; errors remain unknown, never exited.
/// A missing or replaced leader on the same boot does not prove its group is gone.
pub(crate) fn inspect(identity: &Identity) -> Result<InstanceState, String> {
    if !valid_pid(identity.pid)
        || !clean_identity(&identity.start)
        || !clean_identity(&identity.host_boot_id)
    {
        return Err("invalid host process identity".into());
    }
    if boot_id()? != identity.host_boot_id {
        return Ok(InstanceState::PreviousBoot);
    }
    let Some(observed) = process(identity.pid)? else {
        return Ok(InstanceState::Exited);
    };
    if observed.start != identity.start {
        return Ok(InstanceState::PidReused);
    }
    if observed.zombie {
        return Ok(InstanceState::Zombie);
    }
    Ok(InstanceState::Alive)
}
