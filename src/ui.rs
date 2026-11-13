//! Declarative UI built with `embedded-gui` for the ILI9341 320x240 display.

use core::cell::UnsafeCell;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;

use embedded_gui::context::GuiContext;
use embedded_gui::font::FontId;
pub use embedded_gui::geometry::Rect as GuiRect;
use embedded_gui::style::Style;
use embedded_gui::widget::{PropertyKey, PropertyValue, WidgetId};

pub const VIEW_WIDTH: usize = 320;
pub const VIEW_HEIGHT: usize = 240;
pub const VIEW_PIXELS: usize = VIEW_WIDTH * VIEW_HEIGHT;

pub struct FrameBuffer(pub [Rgb565; VIEW_PIXELS]);
pub struct SafeFrameBuf(pub UnsafeCell<FrameBuffer>);
unsafe impl Sync for SafeFrameBuf {}

pub static RAW_FRAMEBUF: SafeFrameBuf =
    SafeFrameBuf(UnsafeCell::new(FrameBuffer([Rgb565::BLACK; VIEW_PIXELS])));

/// Borrow the single static framebuffer. Only call from the single UI task.
pub fn framebuf() -> &'static mut [Rgb565; VIEW_PIXELS] {
    unsafe { &mut (*RAW_FRAMEBUF.0.get()).0 }
}

pub struct FbTarget<'a>(pub &'a mut [Rgb565; VIEW_PIXELS]);

impl<'a> Dimensions for FbTarget<'a> {
    fn bounding_box(&self) -> Rectangle {
        Rectangle::new(
            Point::new(0, 0),
            Size::new(VIEW_WIDTH as u32, VIEW_HEIGHT as u32),
        )
    }
}

impl<'a> DrawTarget for FbTarget<'a> {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(coord, color) in pixels.into_iter() {
            if coord.x >= 0
                && coord.x < VIEW_WIDTH as i32
                && coord.y >= 0
                && coord.y < VIEW_HEIGHT as i32
            {
                self.0[coord.y as usize * VIEW_WIDTH + coord.x as usize] = color;
            }
        }
        Ok(())
    }
}

/// Small `no_std` string formatting buffer.
pub struct StringBuf<const N: usize> {
    pub buf: [u8; N],
    pub len: usize,
}

impl<const N: usize> StringBuf<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl<const N: usize> core::fmt::Write for StringBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let remaining = N - self.len;
        let to_copy = bytes.len().min(remaining);
        self.buf[self.len..self.len + to_copy].copy_from_slice(&bytes[..to_copy]);
        self.len += to_copy;
        Ok(())
    }
}

/// Static text cells to provide `'static str` views to the retained `embedded-gui`.
pub struct TextCell<const N: usize>(UnsafeCell<StringBuf<N>>);
unsafe impl<const N: usize> Sync for TextCell<N> {}

impl<const N: usize> TextCell<N> {
    pub const fn new() -> Self {
        Self(UnsafeCell::new(StringBuf::new()))
    }

    pub fn format(&'static self, args: core::fmt::Arguments<'_>) -> &'static str {
        let buf = unsafe { &mut *self.0.get() };
        buf.clear();
        let _ = core::fmt::write(buf, args);
        unsafe {
            core::str::from_utf8_unchecked(core::slice::from_raw_parts(buf.buf.as_ptr(), buf.len))
        }
    }
}

pub static TX_IP: TextCell<32> = TextCell::new();
pub static TX_TZ: TextCell<64> = TextCell::new();
pub static TX_TIME: TextCell<16> = TextCell::new();
pub static TX_DATE: TextCell<48> = TextCell::new();
pub static TX_WEATHER_MAIN: TextCell<64> = TextCell::new();
pub static TX_WEATHER_SUB: TextCell<64> = TextCell::new();
pub static TX_SUB: TextCell<64> = TextCell::new();
pub static TX_STATUS: TextCell<64> = TextCell::new();

// Palette
pub const BG: Rgb565 = Rgb565::new(2, 4, 8); // Deep Navy
pub const ACCENT: Rgb565 = Rgb565::CYAN;
pub const WHITE: Rgb565 = Rgb565::WHITE;
pub const AMBER: Rgb565 = Rgb565::new(31, 48, 0);
pub const GREEN: Rgb565 = Rgb565::GREEN;
pub const YELLOW: Rgb565 = Rgb565::YELLOW;
pub const DIM: Rgb565 = Rgb565::new(10, 22, 28);

// Embedded-gui configurations
pub const GUI_NODES: usize = 36;
pub const GUI_EVENTS: usize = 8;
pub const GUI_DIRTY: usize = 20;

pub type Gui = GuiContext<'static, GUI_NODES, GUI_EVENTS, GUI_DIRTY>;

pub struct ClockUi {
    pub ip_label: WidgetId,
    pub tz_label: WidgetId,
    pub time_label: WidgetId,
    pub date_label: WidgetId,
    pub weather_main_label: WidgetId,
    pub weather_sub_label: WidgetId,
    pub sub_label: WidgetId,
    pub status_label: WidgetId,
}

pub fn build_clock_ui(gui: &mut Gui) -> ClockUi {
    let r = GuiRect::new;

    // Background panels
    gui.add_themed_panel(r(0, 0, 320, 26))
        .expect("header panel");
    gui.add_themed_panel(r(6, 28, 308, 28))
        .expect("tz selector panel");
    gui.add_themed_panel(r(6, 58, 308, 126))
        .expect("clock stage panel");
    gui.add_themed_panel(r(0, 186, 320, 54))
        .expect("footer panel");

    // Header labels
    let st_title = Style::label()
        .with_font_id(FontId::Scaled6x10)
        .with_foreground(ACCENT);
    gui.add_label(r(10, 6, 170, 14), "STM32WBA65 IoT Hub", st_title)
        .expect("title");

    let st_ip = Style::label()
        .with_font_id(FontId::Scaled6x10)
        .with_foreground(WHITE);
    let ip_label = gui
        .add_label(r(180, 6, 130, 14), "DHCP: Waiting...", st_ip)
        .expect("ip label");

    // Timezone / City selector line
    let st_arrow = Style::label()
        .with_font_id(FontId::Scaled6x10)
        .with_foreground(AMBER);
    gui.add_label(r(10, 35, 48, 14), "[< B1]", st_arrow)
        .expect("prev arrow");
    gui.add_label(r(262, 35, 48, 14), "[B3 >]", st_arrow)
        .expect("next arrow");

    let st_tz = Style::label()
        .with_font_id(FontId::Scaled6x10)
        .with_foreground(WHITE);
    let tz_label = gui
        .add_label(r(60, 35, 200, 14), "Local (Auto-detecting...)", st_tz)
        .expect("tz label");

    // Center Big Digital Clock
    let st_clock = Style::label()
        .with_font_id(FontId::Vector(3))
        .with_foreground(ACCENT);
    let time_label = gui
        .add_label(r(64, 62, 192, 32), "--:--:--", st_clock)
        .expect("time label");

    // Date
    let st_date = Style::label()
        .with_font_id(FontId::Scaled6x10)
        .with_foreground(WHITE);
    let date_label = gui
        .add_label(r(12, 98, 296, 14), "Synchronizing with NTP...", st_date)
        .expect("date label");

    // Weather Line 1: Temp & Condition & High/Low
    let st_weather1 = Style::label()
        .with_font_id(FontId::Scaled6x10)
        .with_foreground(GREEN);
    let weather_main_label = gui
        .add_label(r(12, 116, 296, 14), "Weather: Contacting Open-Meteo...", st_weather1)
        .expect("weather main label");

    // Weather Line 2: Humidity & Wind
    let st_weather2 = Style::label()
        .with_font_id(FontId::Scaled6x10)
        .with_foreground(YELLOW);
    let weather_sub_label = gui
        .add_label(r(12, 134, 296, 14), "Forecast: Waiting for network...", st_weather2)
        .expect("weather sub label");

    // NTP details line (UTC time, stratum, ping)
    let st_sub = Style::label()
        .with_font_id(FontId::Scaled6x10)
        .with_foreground(DIM);
    let sub_label = gui
        .add_label(r(12, 162, 296, 14), "NTP: Idle", st_sub)
        .expect("sub label");

    // Footer lines
    let st_status = Style::label()
        .with_font_id(FontId::Scaled6x10)
        .with_foreground(WHITE);
    let status_label = gui
        .add_label(
            r(10, 192, 300, 14),
            "Status: Connecting to USB network...",
            st_status,
        )
        .expect("status label");

    let st_hints = Style::label()
        .with_font_id(FontId::Scaled6x10)
        .with_foreground(AMBER);
    gui.add_label(
        r(10, 212, 300, 14),
        "[B1] Prev City   [B2] Sync Now   [B3] Next City",
        st_hints,
    )
    .expect("hints label");

    ClockUi {
        ip_label,
        tz_label,
        time_label,
        date_label,
        weather_main_label,
        weather_sub_label,
        sub_label,
        status_label,
    }
}

/// Longest text remembered per widget; longer text is never cached (always applied).
const CACHE_TEXT_MAX: usize = 64;
const CACHE_UNSET: u8 = u8::MAX;

/// Copy of the last text applied to each widget, indexed by `WidgetId`.
///
/// `embedded-gui` marks a widget dirty on every `set_text`, even when the text is
/// unchanged, which makes the UI loop redraw and re-flush every label on every tick.
/// The `TX_*` cells are also rewritten in place, so the text the GUI holds can't be
/// compared against; we keep our own copy instead.
struct TextCache(UnsafeCell<[([u8; CACHE_TEXT_MAX], u8); GUI_NODES]>);
// Only touched from the single UI task.
unsafe impl Sync for TextCache {}

static TEXT_CACHE: TextCache =
    TextCache(UnsafeCell::new([([0; CACHE_TEXT_MAX], CACHE_UNSET); GUI_NODES]));

/// Set a label's text, skipping the update (and the redraw it triggers) if the text
/// is identical to what was last applied to this widget.
pub fn set_text(gui: &mut Gui, id: WidgetId, text: &'static str) {
    let bytes = text.as_bytes();
    let cacheable = bytes.len() <= CACHE_TEXT_MAX;
    // Only call from the single UI task.
    let cache = unsafe { &mut *TEXT_CACHE.0.get() };

    if cacheable {
        if let Some((buf, len)) = cache.get_mut(id.raw() as usize) {
            if *len != CACHE_UNSET && &buf[..*len as usize] == bytes {
                return;
            }
            buf[..bytes.len()].copy_from_slice(bytes);
            *len = bytes.len() as u8;
        }
    }

    let _ = gui.set_widget_property(id, PropertyKey::Text, PropertyValue::Str(text));
}

/// Clear a dirty framebuffer rectangle before re-rendering.
pub fn clear_rect(framebuf: &mut [Rgb565; VIEW_PIXELS], r: GuiRect) {
    let x0 = r.x.max(0) as usize;
    let y0 = r.y.max(0) as usize;
    let x1 = ((r.x + r.w as i32).max(0) as usize).min(VIEW_WIDTH);
    let y1 = ((r.y + r.h as i32).max(0) as usize).min(VIEW_HEIGHT);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    for y in y0..y1 {
        framebuf[y * VIEW_WIDTH + x0..y * VIEW_WIDTH + x1].fill(BG);
    }
}
