# Changelog

## v0.2.0

- Changing the width keeps the content's leading edge where it is. Positions are measured against the width the host was built with, so build with the widest width you will use. Content that never changes width behaves as before.

## v0.1.0

First release.

- Puts your window in the Windows 11 taskbar, centered in its height. Right-click opens a menu with your items and Move, or your own menu.
- Handles Explorer restarts, DPI, layout and auto-hide changes.
- Rust crate, C/C++ DLL with header, C# package for WPF. x64, primary horizontal taskbar only.
