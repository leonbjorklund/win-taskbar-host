# win-taskbar-host

Embed your UI in the Windows 11 x64 primary taskbar. Handles placement, the Move menu, DPI changes, auto-hide and Explorer restarts.

## Agent Prompt

```text
Use https://github.com/leonbjorklund/win-taskbar-host
for taskbar handling with the binding that fits this app.
```

## Rust

```toml
win-taskbar-host = { git = "https://github.com/leonbjorklund/win-taskbar-host", tag = "v0.1.0" }
```

```rust
use win_taskbar_host::TaskbarHost;

let host = TaskbarHost::builder(140.0)
    .save_placement_as("my-app")
    .create(create_content)?;

host.set_position(0.25);
```

## C# / WPF

[NuGet package](https://github.com/leonbjorklund/win-taskbar-host/releases/download/v0.1.0/WinTaskbarHost.0.1.0.nupkg)

```csharp
using WinTaskbarHost;

using var host = TaskbarHost.Create(myView, 140, savePlacementAs: "my-app");
host.Position = 0.25;
```

## C / C++

[Native package](https://github.com/leonbjorklund/win-taskbar-host/releases/download/v0.1.0/win-taskbar-host-0.1.0-x64.zip)

```c
wth_options options = {0};
options.width_dip = 140;
options.placement_key = "my-app";
options.create_content = create_content;

wth_host *host = NULL;
if (wth_create(&options, &host) == WTH_OK) {
    wth_set_position(host, 0.25);
}
```

In Rust and C, `create_content` is your callback that creates a child window inside the supplied surface.
