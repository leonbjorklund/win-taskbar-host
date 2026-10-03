# Repository instructions

- Run `just check`. `just test-desktop`, `tools/acceptance.ps1` and `tools/restart-explorer.ps1` drive the real desktop, so ask first.
- The comments in `host.rs`, `shell.rs`, `placement.rs` and the FFI crate's `lib.rs` record the Win32, Explorer and ABI invariants. Keep them true.
- Keep the repo private. Rendering and app data stay in consumers.
