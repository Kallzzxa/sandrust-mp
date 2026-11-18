//! MC-RP Launcher: a Win32 server-browser GUI front-end for `samp-launcher`.
//!
//! The GUI shells out to `samp-launcher.exe` (which does the real work:
//! start gta_sa.exe suspended, inject samp_client.dll, resume). This lib
//! holds the platform-independent pieces so they can be unit-tested on any
//! host: the SA-MP server query protocol (opcode `i`, info) and the
//! favorites file format. The Win32 window itself lives in `gui` and only
//! builds on Windows.

pub mod favorites;
pub mod query;
