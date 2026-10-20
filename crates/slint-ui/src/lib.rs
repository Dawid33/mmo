//! The game UI in Slint, built on the Factorio-style `widgets` library from
//! `~/Projects/ui`, drawn by Slint's software renderer into a pixel buffer.
//!
//! Engine-free (no Bevy, no windowing): the client feeds window events in,
//! sets properties, and uploads the dirty part of [`Overlay::pixels`] as a
//! texture (`crates/client/src/renderer/slint_overlay.rs`). The software
//! renderer is the one `~/Projects/ui`'s reftest measures against the game
//! it copies, so the pixels here are the measured ones. There is no Slint
//! event loop: the client calls [`Overlay::render`] once per frame, which
//! also advances Slint's timers and animations.
//!
//! Single-threaded like all of Slint: an `Overlay` stays on the thread that
//! made it. Design: docs/superpowers/specs/2026-10-20-slint-ui-library-in-bevy-investigation.md.

use std::cell::RefCell;
use std::rc::Rc;

pub use slint;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter, WindowEvent, WindowEventDispatchResult};
use slint::{ComponentHandle, PhysicalSize};

slint::include_modules!();

thread_local! {
    /// The window the next component is created in. Slint's platform can be
    /// set only once, so [`Overlay::new`] parks its window here and the
    /// platform hands it out.
    static NEXT_WINDOW: RefCell<Option<Rc<MinimalSoftwareWindow>>> = const { RefCell::new(None) };
}

struct OverlayPlatform;

impl Platform for OverlayPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        NEXT_WINDOW
            .with(|next| next.borrow_mut().take())
            .map(|window| window as Rc<dyn WindowAdapter>)
            .ok_or_else(|| slint::PlatformError::Other("no overlay window parked".into()))
    }
}

/// The part of the buffer a render changed, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dirty {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One [`GameUi`] in its own headless window, with the pixels it drew.
pub struct Overlay {
    ui: GameUi,
    window: Rc<MinimalSoftwareWindow>,
    /// Row-major, `size.0` pixels per row. Transparent where nothing is drawn.
    pixels: Vec<PremultipliedRgbaColor>,
    /// Physical pixels.
    size: (u32, u32),
    /// Physical pixels per Slint logical pixel.
    scale: f32,
    /// Repaint everything next frame (after a resize).
    full_repaint: bool,
}

impl Overlay {
    pub fn new() -> Result<Self, slint::PlatformError> {
        // Fails harmlessly when an earlier Overlay already installed it.
        let _ = slint::platform::set_platform(Box::new(OverlayPlatform));
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        NEXT_WINDOW.with(|next| *next.borrow_mut() = Some(window.clone()));
        let ui = GameUi::new()?;
        ui.show()?;
        Ok(Self { ui, window, pixels: Vec::new(), size: (0, 0), scale: 1.0, full_repaint: true })
    }

    /// The component: set its properties and connect its callbacks.
    pub fn ui(&self) -> &GameUi {
        &self.ui
    }

    /// Physical size of the buffer.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Match the target: `width` × `height` physical pixels, `scale` physical
    /// pixels per logical (UI) pixel. The widgets are pixel-exact at 1 and 2.
    pub fn resize(&mut self, width: u32, height: u32, scale: f32) {
        if (width, height) == self.size && scale == self.scale {
            return;
        }
        if scale != self.scale {
            self.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
            self.scale = scale;
        }
        self.size = (width, height);
        self.window.set_size(PhysicalSize::new(width, height));
        self.pixels = vec![PremultipliedRgbaColor::default(); width as usize * height as usize];
        self.full_repaint = true;
        self.window.request_redraw();
    }

    /// Feed one event; positions are logical (UI) pixels. Returns whether the
    /// UI took it (a pointer event on an element that handles it, a key the
    /// focused element used).
    pub fn dispatch(&self, event: WindowEvent) -> bool {
        matches!(
            self.window.window().dispatch_event_with_result(event),
            Ok(WindowEventDispatchResult::Accepted)
        )
    }

    /// Advance timers and animations, then redraw whatever changed. Returns
    /// the changed region, `None` when nothing was drawn.
    pub fn render(&mut self) -> Option<Dirty> {
        slint::platform::update_timers_and_animations();
        let (width, height) = self.size;
        if width == 0 || height == 0 {
            return None;
        }
        let full = std::mem::take(&mut self.full_repaint);
        let pixels = &mut self.pixels;
        let mut dirty = None;
        self.window.draw_if_needed(|renderer| {
            if full {
                renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
            }
            let region = renderer.render(pixels, width as usize);
            if full {
                renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
            }
            let origin = region.bounding_box_origin();
            let size = region.bounding_box_size();
            let x = (origin.x.max(0) as u32).min(width);
            let y = (origin.y.max(0) as u32).min(height);
            let w = size.width.min(width - x);
            let h = size.height.min(height - y);
            if w > 0 && h > 0 {
                dirty = Some(Dirty { x, y, width: w, height: h });
            }
        });
        if self.window.has_active_animations() {
            self.window.request_redraw();
        }
        dirty
    }

    /// The buffer, premultiplied, `size().0` pixels per row.
    pub fn pixels(&self) -> &[PremultipliedRgbaColor] {
        &self.pixels
    }

    /// Whether the UI drew anything at this physical pixel: the hit test that
    /// decides if the pointer belongs to the UI or to the game underneath.
    pub fn opaque_at(&self, x: u32, y: u32) -> bool {
        let (width, height) = self.size;
        x < width && y < height && self.pixels[(y * width + x) as usize].alpha > 0
    }
}
