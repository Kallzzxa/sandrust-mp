# SA-MP 0.3.DL-R1 support plan (Rust)

Goal: make this client speak **0.3.DL-R1** in addition to 0.3.7-R4, so it can
join 0.3.DL servers (custom models / artwork downloads) — without ever
needing Visual Studio .NET 2003.

## Ground facts

- **Game version: GTA: San Andreas 1.0 US (`gta_sa.exe` v1.00)** — the same
  game the 0.3.7 client uses. 0.3.DL does not need another GTA version.
- **"Build the C++ with Rust" is not a thing**: `rustc` does not compile
  C++. The 1therealcloud/samp C++ code needs MSVC 7.1 (VS 2003) because the
  DLL must match the original's ABI/CRT (`msvcr71`). The Rust path is a
  port/extension of *this* codebase, not a different compiler for that one.
- **Reference implementation**: 1therealcloud/samp (C++ reimplementation of
  0.3.DL-R1, `source/`, 178 files, 63,784 lines). All DL facts below were
  read from that tree (commit `a9551b1`).

## What 0.3.DL adds on top of 0.3.7

Measured from the C++ reference (paths are `source/` in 1therealcloud/samp):

| # | Piece | Reference | Size | Status here |
|---|-------|-----------|------|-------------|
| 1 | Join version | `net/netgame.cpp:284` `version = 0xfde` (4062); `netgame.cpp:158` version string `"0.3.DL-R1"` (9 chars); mod byte `1` | — | **Missing** — add `DL_R1` profile (`session.rs`) |
| 2 | RPC 179 `ModelRequest` (S→C) | `net/netrpc.cpp` `ScrCustomModelRecord` | 94-byte record | **Codec exists, dormant** (`rpcs/world.rs`, `DORMANT_IN_R4`) |
| 3 | RPC 183 `ModelUrl` (S→C) | `net/netrpc.cpp` `ScrCustomModelDownload` | sub-types 0/1/2/5/6 | **Codec exists, dormant** |
| 4 | RPC 185 `DownloadCompleted` (S→C) | `net/netrpc.cpp` `ScrCustomModelsComplete` | empty | **Codec exists, dormant** |
| 5 | RPC 181 `RequestDff` (C→S), 182 `RequestTxd` (C→S), 184 `FinishDownload` (C→S) | `net/netrpc.cpp` registrations | u32 checksum / 1 bit | **Codecs exist, dormant** |
| 6 | RPC 180 `ModelsReset` (S→C) | `net/netrpc.cpp` `ScrCustomModelsReset` | empty | Folded into RPC 183 sub-type 0 in this codebase |
| 7 | Download manager | `game/custommodels.cpp` (1,897 lines) + `downloadwindow.cpp` (404) | libcurl HTTP + in-band RakNet chunks + LZMA + CRC32 + `%s\0x%X.dff/.txd` cache | **Missing** — Phase B |
| 8 | Model file loading | `game/modelfile.cpp` (2,502 lines) + `game/modelfile.h` (494) | RenderWare DFF/TXD parsing, `CustomModelLzmaDecode` | **Missing** — Phase B |
| 9 | Game hooks for custom models | `game/hooks.h` `g_gameCustomModelHookJump`, `game/custommodels.*` | CModelInfo extension, model slots | **Missing** — Phase B |
| 10 | Spawn gating | `net/netgame.cpp:912` `ProcessCustomModelWait()` | client waits for downloads before spawn | **Missing** — Phase B |

### The download protocol (from `ScrCustomModelDownload`, RPC 183)

Status byte first, then:

- `0` — abort download (error 6001).
- `2` — `FinishDownload`.
- `6` — URL redirect: `u8 type, u32 hash, u8 url_len, url bytes` →
  `BeginUrlDownload(type, hash, url)` — HTTP fetch via libcurl, worker
  threads, user-agent, optional proxy; file validated by length + CRC32
  (`"[error] file 0x%X bad length / bad checksum"`), cached as
  `<dir>\0x<hash>.dff` / `.txd`.
- `5` — direct download: `u8 type, u32 hash` → `BeginDirectDownload` —
  client then requests the file with RPC 181 (DFF) / 182 (TXD) by checksum.
- `1` — compressed chunk: `u8 stage, u32 unpacked_len, i32 len (0…4096),
  bytes` → `ProcessCompressedChunk` — LZMA stream, reassembled until
  `unpacked_len` is reached.

### The model record (RPC 179)

`u32 pool id, u32 total count, u8 type, i32 virtual world, i32 base id,
i32 new id, u32 DFF checksum, u32 TXD checksum, u32 DFF size, u32 TXD
size, u8 time on, u8 time off` — matches open.mp. Custom model ids live
at 20000+ (DL range).

### Join packet (RPC 25) — DL-R1 vs R4

Byte-identical layout, only two values differ:

```
u32 version        4062 (0xFDE)     vs 4057 (0xFD9) in R4
u8  mod flag       1                vs 1
u8  nickname len + nickname
u32 challenge ^ version
u8  serial len + serial
u8  version string len (9) + "0.3.DL-R1"   vs 8 + "0.3.7-R4"
u32 challenge ^ version   (trailing copy)
```

## Phases

### Phase A — protocol (this branch, in progress)

- `DL_R1: VersionProfile { netgame_version: 4062, mod_flag: 1,
  version_string: "0.3.DL-R1" }` in `crates/samp-proto/src/session.rs`.
- Dispatch the dormant RPCs (179, 183, 185 in; 181, 182, 184 out) when the
  profile is DL.
- `samp-bot --dl`: connect as DL-R1, log the model RPCs. Proves the
  handshake bytes against the C++ layout (unit test on the join packet).
- Verify: `cargo test -p samp-proto`, `cargo build --release --target
  i686-pc-windows-gnu -p samp-client -p samp-launcher`.

### Phase B — download manager (next)

- New module (probably `crates/samp-dl`): model record table, HTTP
  downloads (pure-Rust HTTP client), in-band chunk reassembly, LZMA
  decompression (`lzma-rs`, pure Rust — no system liblzma for the Windows
  GNU target), CRC32 checks, hash-named cache, download progress UI,
  spawn gating until `DownloadCompleted`.
- Wire into `samp-client` (custom-model manager the R4 code already
  references as never-created) and `samp-bot` (request DFF/TXD, verify
  bytes).

### Phase C — game-side model loading (largest)

- Port `custommodels.cpp` + `modelfile.cpp`: DFF/TXD parsing and
  CModelInfo hooks so downloaded models actually appear in-game.
- Live validation against a real 0.3.DL server (needs a DL server; the
  VM has no UDP and no GTA, so this runs on the user's PC).

## What is NOT needed

- Visual Studio .NET 2003, DX 9.0 SDK April 2005, or any product key.
  The whole build stays `cargo build --release --target
  i686-pc-windows-gnu`.
- Another GTA version: 1.0 US throughout.

## MC-RP Launcher GUI (2026-11-18, commit e6c6ec1)

- New crate `samp-launcher-gui`, binary `mc-rp-launcher.exe` (Win32 GUI
  subsystem): server-browser GUI in the shape of samp.exe, dark theme +
  crimson accent. Favorites (`favorites.txt`, `name|host|port`), live
  server info via the SA-MP query protocol (opcode `i`, 1.5 s timeout),
  nickname/host/port/password fields, DL checkbox (on by default),
  owner-drawn Connect button, log box streaming `samp-launcher.exe`
  output. The GUI sets `SAMP_RS_AUTH_TABLE` from `data/auth_table.txt`
  next to the exe. Query + favorites are platform-independent and
  host-tested; the Win32 window is Windows-only and BUILD-ONLY (never
  run on a real Windows desktop here).
- `STARTED_DL` is now `"{FFFFFF}MC-RP {B9C9BF}Costum Launcher
  {FFFFFF}Started"` (user's spelling, "Costum").

## NYX Launcher (2026-11-18)

- New crate `nyx-launcher`, binary `nyx-launcher.exe` (eframe 0.29 /
  egui, Windows GUI subsystem): the NYX design from the user's mockup —
  dark left panel (rgb 38,39,41), NYX logo top-left, "NYX LAUNCHER"
  title (blue + white), "San Andreas Multiplayer Costum Launcher"
  subtitle ("Louncher" typo in the mockup fixed to "Launcher",
  "Costum" spelling kept), gradient username field, blue
  "Connect To Server" button, city-sunset banner on the right,
  frameless window with rounded corners (860x520), custom
  minimize/close buttons, drag area. Assets embedded at compile time
  (`assets/banner.jpg` from the user's sunset photo, 730x694;
  `assets/logo.png` from the user's NYX logo, 517x483 RGBA).
- Connect joins 192.168.1.4:7777 (the user's local server, 0.3.7) —
  no server picker, by the user's request. The 0.3.DL-R1 checkbox is
  OFF by default (that server is 0.3.7); tick it only for a DL server.
  Nickname is remembered in `username.txt`; child output streams to
  the status line and `nyx-launcher.log`; the child is spawned with
  CREATE_NO_WINDOW so no black console pops up. Same launch path as
  the Win32 GUI: `samp-launcher.exe --gta gta_sa.exe --dll
  samp_client.dll --name <nick> --host 192.168.1.4 --port 7777
  [--dl]`, `SAMP_RS_AUTH_TABLE` from `data/auth_table.txt`.
- eframe 0.29 API notes (the Gemini code was written for a newer
  egui): the type is `Rounding` (not `CornerRadius`), `Rounding::same`
  takes f32, `Frame::rounding` / `Image::rounding` (not
  `corner_radius`), `Painter::rect_stroke` takes 3 args (no
  `StrokeKind`). Build: `cargo build --release --target
  i686-pc-windows-gnu -p nyx-launcher` (cross-compiles fine;
  first build ~8 min for the eframe dep tree). Binary is ~20 MB
  stripped (MinGW strip), vs 1.4 MB for the Win32 GUI.
- BUILD-ONLY, like the Win32 GUI: never run on a real Windows desktop
  here; the user is the first tester. `mc-rp-launcher.exe` is still
  shipped in the same zip for the DL server (play.uifserver.net:7776).

## Compat: DL client joining 0.3.7 servers (2026-11-18)

- The NYX launcher has no version picker: it always requests a DL join
  (`--dl`). `samp-launcher` asks the server for its version first (query
  opcode `r`, rule `version`, 1.5 s timeout) and, when the server reports
  0.3.7 (e.g. `0.3.7-R2`), joins as 0.3.7-R4 instead — the
  samp-client-compat behaviour, built into the client. A server that does
  not answer the query keeps the requested mode. The switch prints
  "The Server Cannot Support 0.3 DL-R1 Switch To 0.3.7" (launcher stderr,
  hence the NYX status line) and the client passes `-compat` on the game
  command line, so the DLL prints the same line in-game after "Started".
- Why it works: the join packet layout is byte-identical between R4 and
  DL_R1; only `netgame_version` (4057 vs 4062) and the version string
  differ (`session::R4` / `session::DL_R1`). A 0.3.7 server accepts any
  0.3.7 client, so joining as R4 is a normal 0.3.7 join — no custom
  models, skins fall back to the base ones (same consequence as the
  server-side samp-compat plugin).
- `cmdline::ConnectArgs` gained a `compat` flag (`-compat` switch);
  `client.rs` passes it to `ui::on_netgame_start`, which prints
  `msg::COMPAT_SWITCH` after the Started line. Host tests cover the
  rules parser, the negotiation decision, the `-compat` round-trip and
  the in-game notice.
- The NYX title font is now an embedded bold font (Liberation Sans Bold,
  `assets/TitleBold.ttf`): egui's default font has no bold weight, so
  `.strong()` still rendered thin.

## Stock auth table (v6, 2026-11-18) — the server is stock samp03svr, not open.mp

- The user's local RP server (192.168.1.4:7777, "RP omp-cef Server") runs
  the **stock `samp03svr`** from the rp-omp-cef serverpack — not omp-server.
  The cef.so plugin loads on it, but the RakNet auth table is the stock
  server's own, NOT open.mp's.
- Symptom (v5): compat switch worked ("SA-MP 0.3.7-R4 Started" +
  COMPAT_SWITCH), open-connection accepted after 2 requests, then stuck
  forever at "Connecting to 192.168.1.4:7777...". `samp-rs.log`:
  `net: auth challenge 7BE54C3EAF7E7E not in table` → `session closed:
  auth challenge not in table`; samp-bot: `challenge 5474B6545366252
  not in table`. The client closes silently (no chat line), so the screen
  stays on "Connecting...".
- Stock table: 256 pairs embedded in `samp03svr` as a contiguous array of
  96-byte entries — [challenge: 32 bytes][response: 64 bytes], NUL-padded,
  base 0x148960. Challenges are 12–16 hex chars, responses 40 hex chars.
  Extracted with `tools/extract_auth_table_stock.py` (finds the run of 256
  40-hex strings spaced 96 bytes apart, validates every entry). Both
  observed challenges resolve through it. **Zero overlap** with open.mp's
  256 pairs, so the two tables merge safely.
- Shipped `data/auth_table.txt` is now the **merged 512-pair** table
  (open.mp + stock). No code change: `AuthTable::parse` reads one pair per
  line. The DLL and samp-bot both read the same file, so one merged file
  fixes both. v6 reuses the v5 binaries byte-for-byte; only the table
  changed.
- Lesson: the auth table is per-server-implementation. A server that sends
  a challenge outside every known table will always fail closed with
  "not in table" — the log line is the diagnostic, not the open phase.
