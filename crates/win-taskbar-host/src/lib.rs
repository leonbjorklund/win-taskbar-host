//! Host your own native content inside the Windows 11 taskbar.
//!
//! The host attaches a container window to Explorer's primary taskbar and asks
//! your factory for a child view inside it. It handles the right-click menu with
//! your items and Move, saved placement, DPI and layout changes, auto-hide and
//! recovery after Explorer restarts. You own the content and your app's actions.
//!
//! ```no_run
//! use win_taskbar_host::{Hwnd, Surface, TaskbarHost};
//! # fn create_label(_: &Surface) -> Result<Hwnd, String> { unimplemented!() }
//! # fn run_message_loop() {}
//! # fn quit() {}
//!
//! // On a UI thread that runs a Win32 message loop:
//! let host = TaskbarHost::builder(160.0)
//!     .save_placement_as("example.label")
//!     .menu_item("Exit", quit)
//!     .create(create_label)?; // create_label makes a child window of surface.parent
//! run_message_loop();
//! drop(host); // on the same thread, while the state your callbacks use is alive
//! # Ok::<(), String>(())
//! ```
//!
//! # Threading
//! [`TaskbarHost`] is `!Send`. Create, use and drop it on one UI thread. All
//! callbacks run there, from its message loop. Keep them short. Explorer shares
//! input with the content, so a blocked UI thread stalls taskbar input. Every
//! method is safe to call from callbacks, and dropping the host inside one
//! closes it once the callback returns.
//!
//! # DPI awareness
//! The factory, [`Content::resized`] and your view's window procedure run with
//! per-monitor-v2 DPI awareness, like the taskbar, so your app needs no DPI
//! manifest. Code outside the view that measures the content should use
//! [`Surface::dpi`]. Menu actions keep the awareness of the thread that created
//! the host.
//!
//! # Diagnostics
//! Failure details and placement file errors go to `OutputDebugString`.
#![cfg(windows)]
#![warn(missing_docs)]

mod host;
mod placement;
mod shell;

use std::{ffi::c_void, rc::Rc};

/// A raw window handle (`HWND`), compatible with `windows-sys`.
pub type Hwnd = *mut c_void;

/// Container and size passed to the factory and [`Content::resized`]. It is
/// also the C ABI's `wth_surface`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Surface {
    /// Container window. Create your view as its child.
    pub parent: Hwnd,
    /// Content width in physical pixels.
    pub width: i32,
    /// Content height in physical pixels.
    pub height: i32,
    /// Taskbar DPI. 96 is 100% scaling.
    pub dpi: u32,
}

/// Your view inside the taskbar, returned by the content factory. A plain
/// [`Hwnd`] works when you need neither [`Content::resized`] nor cleanup.
///
/// The host destroys the window with its container before dropping the content,
/// for example on close or after Explorer destroyed the container. Keep resources
/// used by your window procedure alive until `Drop`, and use `Drop` only to
/// release your own resources.
pub trait Content {
    /// Your per-monitor-v2 child window of [`Surface::parent`]. The host sizes
    /// and shows it. Any other window fails the host.
    fn hwnd(&self) -> Hwnd;

    /// Called after the host resized the view or the DPI changed.
    fn resized(&mut self, _surface: &Surface) {}
}

impl Content for Hwnd {
    fn hwnd(&self) -> Hwnd {
        *self
    }
}

/// Lifecycle state of a live host. The values are the C ABI's `WTH_STATE_*`.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// No usable taskbar yet, for example while Explorer restarts. The host retries on its own.
    WaitingForTaskbar = 1,
    /// The content is inside the taskbar.
    Attached,
    /// The content does not fit the taskbar. Resumes when the width or the taskbar changes.
    NoSpace,
    /// The factory failed or returned an unusable window, or the taskbar cannot
    /// host content. Final. Create a new host to try again.
    Failed,
}

/// Options for a [`TaskbarHost`].
pub struct Builder {
    width_dip: f64,
    height_dip: Option<f64>,
    key: Option<String>,
    menu: Vec<(String, host::MenuAction)>,
    context_menu: Option<host::ContextMenu>,
}

impl Builder {
    /// Content height in DIPs. By default the content fills the taskbar height
    /// less 4 DIPs at the top and bottom.
    pub fn height_dip(mut self, height: f64) -> Self {
        self.height_dip = Some(height);
        self
    }

    /// Saves placement changes to `%LOCALAPPDATA%\win-taskbar-host\<key>.placement`
    /// and restores them on the next launch. Without a key, nothing is written to disk.
    pub fn save_placement_as(mut self, key: &str) -> Self {
        self.key = Some(key.to_owned());
        self
    }

    /// Adds an item to the menu that right-clicking the content shows. Items
    /// appear in call order, above Move. `action` runs after the menu closes.
    /// `&` marks the access key and `&&` shows an ampersand.
    pub fn menu_item(mut self, label: &str, action: impl FnMut() + 'static) -> Self {
        self.menu.push((label.to_owned(), Box::new(action)));
        self
    }

    /// Shows your menu instead of the built-in one. Runs on a right-click at
    /// screen `(x, y)` in physical pixels with `owner`, a hidden window in the
    /// foreground that may own a popup menu. Return once the menu has closed,
    /// after calling [`TaskbarHost::begin_move`] if the user chose to move.
    pub fn context_menu(mut self, show: impl FnMut(Hwnd, i32, i32) + 'static) -> Self {
        self.context_menu = Some(Box::new(show));
        self
    }

    /// Creates the host on the current UI thread. Attachment and the first
    /// factory call happen later, from this thread's message loop.
    ///
    /// The factory runs each time the host creates a container, including after
    /// Explorer restarts. Keep application data outside the view so a
    /// replacement shows current state.
    pub fn create<F, C>(self, mut factory: F) -> Result<TaskbarHost, String>
    where
        F: FnMut(&Surface) -> Result<C, String> + 'static,
        C: Content + 'static,
    {
        let factory: host::Factory = Box::new(move |surface| {
            factory(surface).map(|content| Box::new(content) as Box<dyn Content>)
        });
        host::Inner::create(self, factory).map(|inner| TaskbarHost { inner })
    }
}

/// A live taskbar host. Dropping it closes the host and drops the content.
pub struct TaskbarHost {
    inner: Rc<host::Inner>,
}

impl TaskbarHost {
    /// Starts configuring a host whose content is `width_dip` wide. A DIP is
    /// one physical pixel at 100% scaling. Positions are measured against this
    /// width, so pass the widest content you will show and shrink it with
    /// [`TaskbarHost::set_width_dip`].
    pub fn builder(width_dip: f64) -> Builder {
        Builder { width_dip, height_dip: None, key: None, menu: Vec::new(), context_menu: None }
    }

    /// Current lifecycle state.
    pub fn state(&self) -> State {
        self.inner.state.get()
    }

    /// Position along the taskbar, from `0.0` at the leading edge to `1.0` at
    /// the trailing edge. It is `0.25` until the user moves the content or a
    /// saved placement restores it.
    pub fn position(&self) -> f64 {
        self.inner.position.get()
    }

    /// Moves the content, for example from an accessible settings control, and
    /// saves it under the placement key.
    pub fn set_position(&self, position: f64) {
        self.inner.set_position(position)
    }

    /// Starts Move as if chosen from the built-in menu. Call it from
    /// [`Builder::context_menu`] or a click handler, so the host may take the
    /// foreground for the drag.
    pub fn begin_move(&self) {
        self.inner.begin_move()
    }

    /// Changes the content width in DIPs. The leading edge stays where it is,
    /// unless a wider content would leave the taskbar. Move cannot take narrower
    /// content closer to the trailing edge than the builder's width allows.
    pub fn set_width_dip(&self, width: f64) -> Result<(), String> {
        self.inner.set_width(width)
    }
}

impl Drop for TaskbarHost {
    fn drop(&mut self) {
        self.inner.close();
    }
}

pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
