set shell := ["pwsh", "-NoProfile", "-Command"]

version := `(Select-String -LiteralPath crates/win-taskbar-host/Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value`

# Format check, lint and unit tests, which touch no windows.
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
    dotnet run --project tests/wpf-smoke -c Release -- --native-load-failure

# Lifecycle tests against the real taskbar, the C header check and the WPF smoke test. Needs an interactive desktop nobody is using.
test-desktop: build-dll
    cargo test -p win-taskbar-host -- --include-ignored --test-threads=1
    pwsh -NoProfile -File tests/c-abi/run.ps1
    dotnet run --project tests/wpf-smoke -c Release

# Use a fixed architecture/output directory and omit local source paths from release binaries.
build-dll:
    if ($env:RUSTFLAGS -or $env:CARGO_ENCODED_RUSTFLAGS -or $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS) { throw 'Unset RUSTFLAGS, CARGO_ENCODED_RUSTFLAGS and CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS.' }
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }; $flags = @("--remap-path-prefix=$((Get-Location).Path)=/win-taskbar-host", "--remap-path-prefix=$cargoHome=/cargo"); cargo build --locked --release --target x86_64-pc-windows-msvc --target-dir target -p win-taskbar-host-ffi --config ('target.x86_64-pc-windows-msvc.rustflags=' + (ConvertTo-Json -InputObject $flags -Compress))

# Native DLL and C# assemblies.
build: build-dll
    dotnet build bindings/csharp/WinTaskbarHost.csproj -c Release --nologo

# C/C++ zip and NuGet package under dist/.
dist: build-dll
    $notices = Join-Path (rustc --print sysroot) 'share/doc/rust/COPYRIGHT-library.html'; if (-not (Test-Path -LiteralPath $notices)) { throw 'Run rustup component add rust-docs to package the Rust standard library notices.' }; Copy-Item -LiteralPath $notices -Destination target/RUST-STDLIB-NOTICES.html
    dotnet pack bindings/csharp/WinTaskbarHost.csproj -c Release --nologo -p:Version={{version}}
    Compress-Archive -Force -DestinationPath dist/win-taskbar-host-{{version}}-x64.zip -LiteralPath target/x86_64-pc-windows-msvc/release/win_taskbar_host.dll, target/x86_64-pc-windows-msvc/release/win_taskbar_host.dll.lib, crates/win-taskbar-host-ffi/include/win_taskbar_host.h, README.md, LICENSE, THIRD-PARTY-NOTICES.txt, target/RUST-STDLIB-NOTICES.html
