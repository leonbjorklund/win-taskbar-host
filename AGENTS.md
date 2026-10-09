# Repository guide

Windows 11 x64, Explorer's primary horizontal taskbar. Rendering and app data stay in consumers.

## Project map

| Working on                              | Start here                                                                                                                        |
| --------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| Rust API                                | [lib.rs](crates/win-taskbar-host/src/lib.rs)                                                                                      |
| Lifecycle, Move menu, Explorer recovery | [host.rs](crates/win-taskbar-host/src/host.rs)                                                                                    |
| Taskbar discovery, DPI, geometry        | [shell.rs](crates/win-taskbar-host/src/shell.rs)                                                                                  |
| Saved placement                         | [placement.rs](crates/win-taskbar-host/src/placement.rs)                                                                          |
| C ABI                                   | [C header](crates/win-taskbar-host-ffi/include/win_taskbar_host.h), [Rust implementation](crates/win-taskbar-host-ffi/src/lib.rs) |
| WPF binding                             | [TaskbarHost.cs](bindings/csharp/TaskbarHost.cs)                                                                                  |
| Integration tests                       | [Rust](crates/win-taskbar-host/tests/hosting.rs), [C ABI](tests/c-abi/), [WPF](tests/wpf-smoke/)                                  |

## Checks and releases

- Run `just check` on Windows. It excludes the interactive desktop tests.
- Ask before running `just test-desktop`, [acceptance.ps1](tools/acceptance.ps1), [restart-explorer.ps1](tools/restart-explorer.ps1) or [Driver](tools/Driver/). They control the real desktop.
- For CI and releases, use the [workflow](.github/workflows/release.yml); version-tag pushes publish releases after checks pass.
- Release tags must be `v` plus the `version` in [Cargo.toml](crates/win-taskbar-host/Cargo.toml), and the matching `## vX.Y.Z` section of [CHANGELOG.md](CHANGELOG.md) becomes the release notes.

## Contracts

- When changing hosting, placement or ABI code, read and preserve the invariants documented in the affected source comments.
- For ABI changes, keep the Rust types, C header and C# declarations in sync.
