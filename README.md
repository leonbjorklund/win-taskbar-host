# win-taskbar-host

Host your own window inside the Windows 11 taskbar. The library handles attachment, placement, a right-click Move menu, DPI and layout changes, auto-hide and Explorer restart recovery. Your app owns rendering and application data.

Supported scope: Windows 11 x64, Explorer's primary horizontal taskbar. Secondary taskbars, other architectures and replacement shells are unsupported. Explorer integration uses behavior Windows does not promise to preserve across updates.

Create, use and dispose a host on one UI thread with a running message loop. Keep callbacks short because Explorer shares input with that thread. Attachment happens asynchronously; check `State` (`wth_get_state` in C) for failure. Native failure details and placement errors go to `OutputDebugString`.

## Rust

Requires Rust 1.88 or later with the MSVC toolchain and Windows SDK. Create an app with `cargo new taskbar-label`, then add these dependencies to its `Cargo.toml`:

```toml
[dependencies]
win-taskbar-host = { git = "https://github.com/leonbjorklund/win-taskbar-host", tag = "v0.1.0" }
windows-sys = { version = "0.61.2", features = ["Win32_Foundation", "Win32_UI_WindowsAndMessaging"] }
```

Replace `src/main.rs` with:

```rust
use std::ptr::null_mut;
use win_taskbar_host::TaskbarHost;
use windows_sys::{w, Win32::UI::WindowsAndMessaging::*};

fn main() -> Result<(), String> {
    let host = TaskbarHost::builder(140.0)
        .menu_item("Exit", || unsafe { PostQuitMessage(0) })
        .create(|surface| unsafe {
            let view = CreateWindowExW(
                0, w!("STATIC"), w!("Taskbar label"), WS_CHILD,
                0, 0, surface.width, surface.height,
                surface.parent, null_mut(), null_mut(), null_mut(),
            );
            if view.is_null() {
                Err(std::io::Error::last_os_error().to_string())
            } else {
                Ok(view)
            }
        })?;
    unsafe {
        let mut message = std::mem::zeroed();
        loop {
            match GetMessageW(&mut message, null_mut(), 0, 0) {
                -1 => return Err(std::io::Error::last_os_error().to_string()),
                0 => break,
                _ => { TranslateMessage(&message); DispatchMessageW(&message); }
            }
        }
    }
    drop(host);
    Ok(())
}
```

Run `cargo run --target x86_64-pc-windows-msvc`. Right-click the label for Move or Exit. `cargo doc --open` describes the API. Return a `Content` implementation when your view needs resize notifications or resource cleanup. Its window may already be destroyed when `Drop` runs. The factory runs again after Explorer restarts, so keep application data outside it.

## C# / WPF

Requires the .NET 8 SDK or later and an x64 app. The package includes the native DLL. Download `WinTaskbarHost.0.1.0.nupkg` from the [release](https://github.com/leonbjorklund/win-taskbar-host/releases/tag/v0.1.0) into a local `packages` directory. Create `TaskbarLabel.csproj` next to that directory:

```xml
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>WinExe</OutputType>
    <TargetFramework>net8.0-windows</TargetFramework>
    <UseWPF>true</UseWPF>
    <RuntimeIdentifier>win-x64</RuntimeIdentifier>
    <ApplicationManifest>app.manifest</ApplicationManifest>
  </PropertyGroup>
</Project>
```

Install the package with `dotnet add package WinTaskbarHost --version 0.1.0 --source ./packages`. Create `app.manifest`:

```xml
<assembly manifestVersion="1.0" xmlns="urn:schemas-microsoft-com:asm.v1">
  <assemblyIdentity version="1.0.0.0" name="TaskbarLabel" />
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>
```

Create `Program.cs`:

```csharp
using System;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using WinTaskbarHost;

static class Program
{
    [STAThread]
    static void Main()
    {
        var app = new Application { ShutdownMode = ShutdownMode.OnExplicitShutdown };
        var view = new Border {
            Background = Brushes.Black,
            Child = new TextBlock { Text = "Taskbar label", Foreground = Brushes.White }
        };
        using var host = TaskbarHost.Create(view, 140,
            menuItems: new[] { ("Exit", (Action)(() => app.Shutdown())) });
        app.Exit += (_, _) => host.Dispose();
        app.Run();
    }
}
```

Run `dotnet run`. Right-click for Move or Exit. Dispose the host on its dispatcher thread before shutting down that thread. Use an opaque background; set `Focusable = false` on controls that should not take keyboard focus. A WPF `ContextMenu` replaces the built-in menu; call `host.BeginMove()` from its Move item.

## C / C++

Requires an MSVC x64 compiler and Windows SDK. Extract `win-taskbar-host-0.1.0-x64.zip` from the [release](https://github.com/leonbjorklund/win-taskbar-host/releases/tag/v0.1.0) into `native`. The DLL statically links the C runtime, so users do not need the Visual C++ redistributable. Create `main.c` alongside `native`:

```c
#include <windows.h>
#include <stdio.h>
#include "win_taskbar_host.h"

static void *create_content(void *context, const wth_surface *surface) {
    (void)context;
    return CreateWindowExW(0, L"STATIC", L"Taskbar label", WS_CHILD,
        0, 0, surface->width, surface->height, (HWND)surface->parent,
        NULL, NULL, NULL);
}

static void on_menu(void *context, uint32_t index) {
    (void)context; (void)index;
    PostQuitMessage(0);
}

int main(void) {
    const char *labels[] = { "Exit" };
    wth_options options = {0};
    options.width_dip = 140;
    options.create_content = create_content;
    options.menu_labels = labels;
    options.menu_count = 1;
    options.on_menu = on_menu;
    wth_host *host = NULL;
    if (wth_create(&options, &host) != WTH_OK) {
        fprintf(stderr, "%s\n", wth_last_error());
        return 1;
    }
    MSG message;
    int status;
    while ((status = GetMessageW(&message, NULL, 0, 0)) > 0) {
        TranslateMessage(&message);
        DispatchMessageW(&message);
    }
    int closed = wth_destroy(host);
    return status < 0 || closed != WTH_OK;
}
```

In an **x64 Native Tools Command Prompt**, run:

```bat
cl /nologo /W4 /WX main.c /Inative native\win_taskbar_host.dll.lib user32.lib /link /OUT:native\TaskbarLabel.exe
native\TaskbarLabel.exe
```

Right-click for Move or Exit. Ship the DLL beside your executable and preserve the license notices. `win_taskbar_host.h` documents callbacks, ownership and errors. The host owns the child window; `destroy_content` releases your other resources.

## Placement and development

Placement is in memory unless you supply a key with Rust's `save_placement_as`, C#'s `savePlacementAs` or C's `placement_key`. A key saves to `%LOCALAPPDATA%\win-taskbar-host\<key>.placement`. `set_position`, `Position` or `wth_set_position` also supports an accessible alternative to dragging. Use an app-specific key.

Repository builds need PowerShell 7, [just](https://github.com/casey/just), the Rust MSVC toolchain, MSVC x64 tools, a Windows SDK and the .NET 10 SDK. Packaging also needs the toolchain's `rust-docs` component for attribution.

```powershell
just check
cargo doc --workspace --no-deps
just dist
```

`dist/` contains the native zip and NuGet package. `just test-desktop` drives the real taskbar and input; run it only on an idle interactive desktop. `tools/acceptance.ps1 -Process <name>` tests a running consumer. `tools/restart-explorer.ps1 -Process <name>` restarts Explorer and verifies recovery.

## Releases

GitHub Actions runs `just check`, builds API documentation and packages Windows x64 on pull requests and pushes to `main`. These checks do not run the interactive desktop tests.

To release:

1. Set the version in `crates/win-taskbar-host/Cargo.toml`, update `Cargo.lock` with `cargo check`, and update the README download links and examples.
2. Add a matching `## vX.Y.Z` section to `CHANGELOG.md`. Commit and push to `main`, then wait for the **Check and release** workflow to pass.
3. Tag that commit with `git tag -a vX.Y.Z -m "vX.Y.Z"` and push with `git push origin vX.Y.Z`.

The tag must match the crate version. After the checks pass, the workflow publishes the native ZIP, the NuGet package and `SHA256SUMS` as a GitHub release, using the changelog section as its notes. Packages are distributed through GitHub Releases. No registry credentials or personal access token are needed.

If uploading fails, re-run the failed job in GitHub Actions; it can resume an unpublished draft. Published releases are not overwritten. Use a new version for changes to a published package.
