//! samp-launcher: starts gta_sa.exe suspended with the original launcher's
//! command-line convention (`-c -n NAME -h HOST -p PORT [-z PASSWORD]`), loads our client
//! DLL into it with a remote `LoadLibraryW` thread, then resumes the game.
//!
//! Usage: samp-launcher [--gta PATH] [--dll PATH]... [--name NAME] [--host HOST] [--port PORT] [--password PASS]
//!        [--no-wait] [--dl]
//!
//! `--dl` joins as 0.3.DL-R1 (custom models) instead of 0.3.7-R4 (docs/DL-PLAN.md).
//! Before launching, the server is asked for its version (query opcode `r`):
//! a 0.3.7 server cannot take a DL client, so the launcher joins as 0.3.7-R4
//! instead (samp-client-compat behaviour, built into the client) and prints
//! "The Server Cannot Support 0.3 DL-R1 Switch To 0.3.7". If the server does
//! not answer the query, the requested mode is used as-is.
//!
//! `--dll` may be given several times; the DLLs are loaded in that order
//! (e.g. `--dll samp-tracer.dll --dll samp.dll`) before the game resumes.
//! Without `--dll`, `samp_client.dll` next to the launcher is loaded.

use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    gta: String,
    dlls: Vec<String>,
    name: String,
    host: String,
    port: u16,
    /// Server password, passed as `-z` only when given.
    password: Option<String>,
    wait: bool,
    /// Join as 0.3.DL-R1 (custom models) instead of 0.3.7-R4.
    dl: bool,
    /// Set by version negotiation, not a CLI flag: the server is 0.3.7, so the
    /// client joins as 0.3.7-R4 and the DLL prints the switch notice in-game.
    compat: bool,
}

fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Options, String> {
    let mut o = Options {
        gta: "gta_sa.exe".into(),
        dlls: Vec::new(),
        name: "Player".into(),
        host: "127.0.0.1".into(),
        port: 7777,
        password: None,
        wait: true,
        dl: false,
        compat: false,
    };
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |flag: &str| it.next().ok_or_else(|| format!("{flag} needs a value"));
        match a.as_str() {
            "--gta" => o.gta = val("--gta")?,
            "--dll" => o.dlls.push(val("--dll")?),
            "--name" => o.name = val("--name")?,
            "--host" => o.host = val("--host")?,
            "--port" => o.port = val("--port")?.parse().map_err(|_| "--port must be 1..65535".to_string())?,
            "--password" => o.password = Some(val("--password")?),
            "--no-wait" => o.wait = false,
            "--dl" => o.dl = true,
            "--help" | "-?" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
    }
    if o.name.is_empty() || o.name.contains(char::is_whitespace) {
        return Err("--name must be non-empty without spaces".into());
    }
    if let Some(p) = &o.password {
        check_password(p)?;
    }
    Ok(o)
}

const USAGE: &str = "usage: samp-launcher [--gta PATH] [--dll PATH]... [--name NAME] [--host HOST] [--port PORT] \
     [--password PASS] [--no-wait] [--dl]";

/// The client reads `-z` as one word of the raw command line (docs/re/handshake.md, "Command line"): it ends at
/// a space, `-` or `/`, quotes are not removed, and past 254 characters the bytes sent no longer match the
/// server's copy (which keeps the terminator). Such a password could never be sent as given.
fn check_password(p: &str) -> Result<(), String> {
    if p.is_empty() {
        return Err("--password must not be empty (leave it out for no password)".into());
    }
    if let Some(c) = p.chars().find(|c| !c.is_ascii_graphic() || matches!(c, '-' | '/' | '"')) {
        return Err(format!(
            "--password cannot contain {c:?}: SA:MP's -z switch takes printable ASCII without '-', '/', '\"' or spaces"
        ));
    }
    if p.len() > 254 {
        return Err("--password is longer than 254 characters".into());
    }
    Ok(())
}

/// Shown when a 0.3.DL-R1 join is downgraded to 0.3.7 because the server
/// cannot take DL clients (also printed in-game by the client DLL).
pub const COMPAT_MESSAGE: &str = "The Server Cannot Support 0.3 DL-R1 Switch To 0.3.7";

/// How long the version query gets to answer before the requested mode is used as-is.
const VERSION_QUERY_TIMEOUT: Duration = Duration::from_millis(1500);

/// Build the `r` (rules) request for `addr` (the server's UDP address).
fn build_rules_request(addr: SocketAddr) -> Vec<u8> {
    let mut out = Vec::with_capacity(11);
    out.extend_from_slice(b"SAMP");
    match addr.ip() {
        std::net::IpAddr::V4(v4) => out.extend_from_slice(&v4.octets()),
        std::net::IpAddr::V6(_) => out.extend_from_slice(&[0, 0, 0, 0]),
    }
    out.extend_from_slice(&addr.port().to_le_bytes());
    out.push(b'r');
    out
}

/// Parse an `r` response into (name, value) rules.
/// Response: `SAMP` + IPv4 (4) + port u16 LE + `r` + rule count u16 LE
///           + per rule: name_len u8 + name + value_len u8 + value.
fn parse_rules_response(buf: &[u8]) -> Option<Vec<(String, String)>> {
    let mut pos = 0usize;
    let take = |pos: &mut usize, n: usize| -> Option<&[u8]> {
        let end = pos.checked_add(n)?;
        let s = buf.get(*pos..end)?;
        *pos = end;
        Some(s)
    };
    if take(&mut pos, 4)? != b"SAMP" {
        return None;
    }
    let _ip = take(&mut pos, 4)?;
    let _port = take(&mut pos, 2)?;
    if take(&mut pos, 1)? != b"r" {
        return None;
    }
    let count = u16::from_le_bytes(take(&mut pos, 2)?.try_into().ok()?);
    let mut rules = Vec::new();
    for _ in 0..count {
        let name_len = *take(&mut pos, 1)?.first()? as usize;
        let name = String::from_utf8_lossy(take(&mut pos, name_len)?).into_owned();
        let value_len = *take(&mut pos, 1)?.first()? as usize;
        let value = String::from_utf8_lossy(take(&mut pos, value_len)?).into_owned();
        rules.push((name, value));
    }
    Some(rules)
}

/// The server's `version` rule (e.g. "0.3.7-R2", "0.3.DL-R1"), if present.
fn server_version(rules: &[(String, String)]) -> Option<&str> {
    rules.iter().find(|(k, _)| k == "version").map(|(_, v)| v.as_str())
}

/// Whether a server version string is a 0.3.DL server.
fn is_dl_server(version: &str) -> bool {
    version.starts_with("0.3.DL")
}

/// Ask the server for its version (query opcode `r`). `Ok(None)` means the
/// server did not answer in time.
fn query_server_version(host: &str, port: u16) -> io::Result<Option<String>> {
    let addr = format!("{host}:{port}")
        .to_socket_addrs()?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "could not resolve host"))?;
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_read_timeout(Some(VERSION_QUERY_TIMEOUT))?;
    sock.connect(addr)?;
    sock.send(&build_rules_request(addr))?;
    let mut buf = [0u8; 2048];
    match sock.recv(&mut buf) {
        Ok(n) => Ok(parse_rules_response(&buf[..n]).and_then(|r| server_version(&r).map(str::to_string))),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => Ok(None),
        Err(e) => Err(e),
    }
}

/// Decide the join mode: (join as DL, tell the client it was a compat switch).
/// A DL request against a 0.3.7 server downgrades to 0.3.7-R4; anything else
/// keeps the requested mode.
fn negotiate_dl(dl: bool, server_version: Option<&str>) -> (bool, bool) {
    match (dl, server_version) {
        (true, Some(v)) if !is_dl_server(v) => (false, true),
        _ => (dl, false),
    }
}

/// Version negotiation for a `--dl` join: ask the server what it is and
/// downgrade to 0.3.7 when it cannot take a DL client. A server that does
/// not answer the query keeps the requested mode.
fn negotiate(mut o: Options) -> Options {
    if !o.dl {
        return o;
    }
    match query_server_version(&o.host, o.port) {
        Ok(version) => {
            let (dl, compat) = negotiate_dl(true, version.as_deref());
            if compat {
                eprintln!("{COMPAT_MESSAGE}");
            }
            o.dl = dl;
            o.compat = compat;
            o
        }
        Err(e) => {
            eprintln!("version query failed ({e}); joining as 0.3.DL-R1 as requested");
            o
        }
    }
}

/// Quotes one argument for a Windows command line (CommandLineToArgvW rules).
fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in a.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

fn command_line(o: &Options) -> String {
    let mut parts = vec![quote_arg(&o.gta)];
    parts.extend(samp_client_args(o).iter().map(|a| quote_arg(a)));
    parts.join(" ")
}

/// Same convention as the original SA:MP launcher (see samp-client `cmdline`).
fn samp_client_args(o: &Options) -> Vec<String> {
    let mut args: Vec<String> =
        ["-c", "-n", &o.name, "-h", &o.host, "-p", &o.port.to_string()].iter().map(|s| s.to_string()).collect();
    if o.dl {
        args.push("-dl".to_string());
    }
    if o.compat {
        args.push("-compat".to_string());
    }
    if let Some(p) = &o.password {
        args.extend(["-z".to_string(), p.clone()]);
    }
    args
}

fn main() {
    let opts = match parse_args(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let opts = negotiate(opts);
    std::process::exit(run(&opts));
}

#[cfg(not(windows))]
fn run(o: &Options) -> i32 {
    eprintln!("samp-launcher only runs on Windows (or Wine); would run: {}", command_line(o));
    1
}

#[cfg(windows)]
fn run(o: &Options) -> i32 {
    match win::launch(o) {
        Ok(code) => code,
        Err(e) => {
            win::log(&format!("error: {e}"));
            1
        }
    }
}

#[cfg(windows)]
mod win {
    use super::{Options, command_line};
    use std::ffi::c_void;
    use std::io::Write;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CreateProcessW, CreateRemoteThread, GetExitCodeProcess, GetExitCodeThread, INFINITE,
        PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
    };

    static LOG_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

    /// Prints to stdout and appends to `samp-rs-launcher.log` next to gta_sa.exe.
    pub fn log(msg: &str) {
        println!("[samp-launcher] {msg}");
        if let Some(p) = LOG_PATH.get()
            && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p)
        {
            let _ = writeln!(f, "{msg}");
        }
    }

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: we own the handle.
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    fn last_error(what: &str) -> String {
        format!("{what} failed: {}", std::io::Error::last_os_error())
    }

    pub fn launch(o: &Options) -> Result<i32, String> {
        let gta = std::path::absolute(&o.gta).map_err(|e| format!("bad --gta path: {e}"))?;
        if !gta.is_file() {
            return Err(format!("gta_sa.exe not found at {}", gta.display()));
        }
        let gta_dir = gta.parent().unwrap_or(Path::new(".")).to_path_buf();
        let _ = std::fs::remove_file(gta_dir.join("samp-rs-launcher.log"));
        let _ = LOG_PATH.set(gta_dir.join("samp-rs-launcher.log"));
        let mut dlls = Vec::new();
        for d in &o.dlls {
            dlls.push(std::path::absolute(d).map_err(|e| format!("bad --dll path: {e}"))?);
        }
        if dlls.is_empty() {
            dlls.push(
                std::env::current_exe()
                    .map_err(|e| e.to_string())?
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join("samp_client.dll"),
            );
        }
        for dll in &dlls {
            if !dll.is_file() {
                return Err(format!("client DLL not found at {}", dll.display()));
            }
        }
        let mut opts = o.clone();
        opts.gta = gta.to_string_lossy().into_owned();
        let cmd = command_line(&opts);
        log(&format!("starting (suspended): {cmd}"));
        for dll in &dlls {
            log(&format!("injecting: {}", dll.display()));
        }

        let app = wide(gta.as_os_str());
        let mut cmd_w = wide(std::ffi::OsStr::new(&cmd));
        let dir_w = wide(gta_dir.as_os_str());
        // SAFETY: Win32 process creation with valid, NUL-terminated buffers.
        let pi = unsafe {
            let mut si: STARTUPINFOW = std::mem::zeroed();
            si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
            if CreateProcessW(
                app.as_ptr(),
                cmd_w.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_SUSPENDED,
                std::ptr::null(),
                dir_w.as_ptr(),
                &si,
                &mut pi,
            ) == 0
            {
                return Err(last_error("CreateProcessW"));
            }
            pi
        };
        let process = Handle(pi.hProcess);
        let thread = Handle(pi.hThread);
        log(&format!("gta_sa.exe pid {}", pi.dwProcessId));

        for dll in &dlls {
            if let Err(e) = inject(process.0, dll) {
                // SAFETY: killing the child we just created.
                unsafe { TerminateProcess(process.0, 1) };
                return Err(e);
            }
        }
        // SAFETY: resuming the suspended main thread of our child.
        if unsafe { ResumeThread(thread.0) } == u32::MAX {
            unsafe { TerminateProcess(process.0, 1) };
            return Err(last_error("ResumeThread"));
        }
        log("game resumed");
        if !o.wait {
            return Ok(0);
        }
        // SAFETY: waiting on our own child handle.
        unsafe {
            WaitForSingleObject(process.0, INFINITE);
            let mut code = 0u32;
            GetExitCodeProcess(process.0, &mut code);
            log(&format!("gta_sa.exe exited with code {code:#x}"));
            Ok(code as i32)
        }
    }

    /// Classic remote-thread injection: write the DLL path into the target and
    /// run `LoadLibraryW(path)` there; kernel32 is mapped at the same address
    /// in every process of a session.
    fn inject(process: HANDLE, dll: &Path) -> Result<(), String> {
        let path_w = wide(dll.as_os_str());
        let bytes = path_w.len() * 2;
        // SAFETY: Win32 calls on a process handle we own; buffers are valid.
        unsafe {
            let k32 = GetModuleHandleW(wide(std::ffi::OsStr::new("kernel32.dll")).as_ptr());
            let load = GetProcAddress(k32, c"LoadLibraryW".as_ptr().cast()).ok_or("LoadLibraryW not found")?;
            let remote = VirtualAllocEx(process, std::ptr::null(), bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
            if remote.is_null() {
                return Err(last_error("VirtualAllocEx"));
            }
            let mut written = 0usize;
            if WriteProcessMemory(process, remote, path_w.as_ptr().cast(), bytes, &mut written) == 0 || written != bytes
            {
                return Err(last_error("WriteProcessMemory"));
            }
            let start: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(load);
            let th = CreateRemoteThread(process, std::ptr::null(), 0, Some(start), remote, 0, std::ptr::null_mut());
            if th.is_null() {
                return Err(last_error("CreateRemoteThread"));
            }
            let th = Handle(th);
            if WaitForSingleObject(th.0, 60_000) != WAIT_OBJECT_0 {
                return Err("LoadLibraryW in the game did not finish within 60 s".into());
            }
            let mut module = 0u32;
            GetExitCodeThread(th.0, &mut module);
            VirtualFreeEx(process, remote, 0, MEM_RELEASE);
            if module == 0 {
                return Err(format!("LoadLibraryW({}) failed inside the game", dll.display()));
            }
            log(&format!("client DLL loaded at {module:#x} ({})", dll.display()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn defaults_and_flags() {
        let o = parse_args(s(&[])).unwrap();
        assert_eq!((o.gta.as_str(), o.port, o.wait), ("gta_sa.exe", 7777, true));
        let o = parse_args(s(&[
            "--gta",
            "C:\\Games\\GTA San Andreas\\gta_sa.exe",
            "--dll",
            "x.dll",
            "--name",
            "Rusty",
            "--host",
            "10.0.0.2",
            "--port",
            "7778",
            "--no-wait",
        ]))
        .unwrap();
        assert_eq!(o.name, "Rusty");
        assert_eq!(o.port, 7778);
        assert!(!o.wait);
        assert_eq!(o.dlls, vec!["x.dll".to_string()]);
        assert_eq!(command_line(&o), "\"C:\\Games\\GTA San Andreas\\gta_sa.exe\" -c -n Rusty -h 10.0.0.2 -p 7778");
    }

    #[test]
    fn password_goes_last_as_z_switch() {
        let o = parse_args(s(&["--gta", "gta_sa.exe", "--name", "Rusty", "--password", "s3cret.Pw_1"])).unwrap();
        assert_eq!(command_line(&o), "gta_sa.exe -c -n Rusty -h 127.0.0.1 -p 7777 -z s3cret.Pw_1");
        assert_eq!(parse_args(s(&[])).unwrap().password, None);
    }

    #[test]
    fn dl_flag_appends_dl_switch() {
        let o = parse_args(s(&["--name", "Rusty", "--dl"])).unwrap();
        assert!(o.dl);
        assert_eq!(command_line(&o), "gta_sa.exe -c -n Rusty -h 127.0.0.1 -p 7777 -dl");
        assert!(!parse_args(s(&[])).unwrap().dl);
    }

    #[test]
    fn rejects_passwords_the_z_switch_cannot_carry() {
        for bad in ["", "a b", "a-b", "a/b", "a\"b", "caf\u{e9}"] {
            assert!(parse_args(s(&["--password", bad])).is_err(), "{bad:?}");
        }
        assert!(parse_args(s(&["--password", &"a".repeat(254)])).is_ok());
        assert!(parse_args(s(&["--password", &"a".repeat(255)])).is_err());
        assert!(parse_args(s(&["--password"])).is_err());
    }

    #[test]
    fn several_dlls_in_order() {
        let o = parse_args(s(&["--dll", "samp-tracer.dll", "--name", "Orig", "--dll", "samp.dll"])).unwrap();
        assert_eq!(o.dlls, vec!["samp-tracer.dll".to_string(), "samp.dll".to_string()]);
        assert!(parse_args(s(&[])).unwrap().dlls.is_empty());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_args(s(&["--port", "99999"])).is_err());
        assert!(parse_args(s(&["--name", "a b"])).is_err());
        assert!(parse_args(s(&["--bogus"])).is_err());
        assert!(parse_args(s(&["--gta"])).is_err());
    }

    #[test]
    fn quoting() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote_arg("C:\\dir with space\\"), "\"C:\\dir with space\\\\\"");
    }

    fn sample_rules_response(version: &str) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"SAMP");
        v.extend_from_slice(&[192, 168, 1, 4]);
        v.extend_from_slice(&7777u16.to_le_bytes());
        v.push(b'r');
        let rules = [("version", version), ("mapname", "San Andreas"), ("lagcomp", "On")];
        v.extend_from_slice(&(rules.len() as u16).to_le_bytes());
        for (k, val) in rules {
            v.push(k.len() as u8);
            v.extend_from_slice(k.as_bytes());
            v.push(val.len() as u8);
            v.extend_from_slice(val.as_bytes());
        }
        v
    }

    #[test]
    fn rules_request_layout() {
        let addr: SocketAddr = "192.168.1.4:7777".parse().unwrap();
        // "SAMP" + 192.168.1.4 + port 7777 little-endian (0x61, 0x1E) + 'r'.
        assert_eq!(build_rules_request(addr), b"SAMP\xc0\xa8\x01\x04\x61\x1er".to_vec());
    }

    #[test]
    fn rules_response_parses() {
        let rules = parse_rules_response(&sample_rules_response("0.3.7-R2")).unwrap();
        assert_eq!(server_version(&rules), Some("0.3.7-R2"));
        assert_eq!(rules.len(), 3);
    }

    #[test]
    fn rules_response_parses_dl_version() {
        let rules = parse_rules_response(&sample_rules_response("0.3.DL-R1")).unwrap();
        assert_eq!(server_version(&rules), Some("0.3.DL-R1"));
    }

    #[test]
    fn rules_response_rejects_garbage() {
        assert!(parse_rules_response(b"").is_none());
        assert!(parse_rules_response(b"SAMP").is_none());
        let mut bad = sample_rules_response("0.3.7-R2");
        bad[10] = b'i'; // wrong opcode
        assert!(parse_rules_response(&bad).is_none());
        let mut trunc = sample_rules_response("0.3.7-R2");
        trunc.truncate(15); // cut inside the first rule
        assert!(parse_rules_response(&trunc).is_none());
    }

    #[test]
    fn negotiate_dl_decisions() {
        assert_eq!(negotiate_dl(true, Some("0.3.7-R2")), (false, true));
        assert_eq!(negotiate_dl(true, Some("0.3.7-R4")), (false, true));
        assert_eq!(negotiate_dl(true, Some("0.3.DL-R1")), (true, false));
        assert_eq!(negotiate_dl(true, None), (true, false));
        assert_eq!(negotiate_dl(false, Some("0.3.7-R2")), (false, false));
        assert_eq!(negotiate_dl(false, Some("0.3.DL-R1")), (false, false));
    }

    #[test]
    fn compat_switch_in_command_line() {
        let mut o = parse_args(s(&["--name", "Rusty", "--dl"])).unwrap();
        o.dl = false;
        o.compat = true;
        assert_eq!(command_line(&o), "gta_sa.exe -c -n Rusty -h 127.0.0.1 -p 7777 -compat");
    }
}
