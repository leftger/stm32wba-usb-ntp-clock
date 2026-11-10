#![no_std]
#![no_main]

mod ntp;
mod ui;
mod weather;

use defmt::*;
use defmt_rtt as _;
use panic_probe as _;

use embassy_executor::Spawner;
use embassy_net::{StackStorage, StaticPool};
use embassy_stm32::dma::InterruptHandler as DmaInterruptHandler;
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_stm32::mode::Async;
use embassy_stm32::rcc::*;
use embassy_stm32::rng::{self, Rng};
use embassy_stm32::spi::mode::Master;
use embassy_stm32::spi::{Config as SpiConfig, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::usb::{self, Driver};
use embassy_stm32::{bind_interrupts, peripherals, Config};
use embassy_time::{Duration, Timer};

use embassy_usb::class::cdc_ncm::device::embassy_net::{
    Device, Runner as NcmRunner, State as NetState,
};
use embassy_usb::class::cdc_ncm::device::{CdcNcmClass, State as NcmState};
use embassy_usb::{Builder as UsbBuilder, UsbDevice, UsbDeviceSpeed};

use embedded_graphics::pixelcolor::IntoStorage;
use embedded_graphics::pixelcolor::Rgb565;

use static_cell::StaticCell;

use crate::ntp::{ntp_task, CLOCK, RESYNC_SIGNAL, TIME_ZONES};
use crate::ui::*;
use crate::weather::{weather_desc, weather_task, LOCAL_GEO, WEATHER, WEATHER_REQUEST};

bind_interrupts!(struct Irqs {
    USB_OTG_HS => usb::InterruptHandler<peripherals::USB_OTG_HS>;
    GPDMA1_CHANNEL0 => DmaInterruptHandler<peripherals::GPDMA1_CH0>;
    RNG => rng::InterruptHandler<peripherals::RNG>;
});

type UsbDriver = Driver<'static, peripherals::USB_OTG_HS>;
type DisplaySpi = Spi<'static, Async, Master>;

#[embassy_executor::task]
async fn usb_task(mut device: UsbDevice<'static, UsbDriver>) -> ! {
    device.run().await
}

#[embassy_executor::task]
async fn usb_ncm_task(runner: NcmRunner<'static, UsbDriver>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static>) -> ! {
    runner.run().await
}

async fn write_command(
    spi: &mut DisplaySpi,
    cs: &mut Output<'static>,
    dc: &mut Output<'static>,
    cmd: u8,
    data: &[u8],
) {
    cs.set_low();
    dc.set_low();
    let _ = spi.write(&[cmd]).await;
    if !data.is_empty() {
        dc.set_high();
        let _ = spi.write(data).await;
    }
    cs.set_high();
}

async fn set_window(
    spi: &mut DisplaySpi,
    cs: &mut Output<'static>,
    dc: &mut Output<'static>,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
) {
    let x_end = x + w - 1;
    let y_end = y + h - 1;
    write_command(
        spi,
        cs,
        dc,
        0x2A,
        &[(x >> 8) as u8, x as u8, (x_end >> 8) as u8, x_end as u8],
    )
    .await;
    write_command(
        spi,
        cs,
        dc,
        0x2B,
        &[(y >> 8) as u8, y as u8, (y_end >> 8) as u8, y_end as u8],
    )
    .await;
}

async fn init_display(
    spi: &mut DisplaySpi,
    cs: &mut Output<'static>,
    dc: &mut Output<'static>,
    rst: &mut Output<'static>,
) {
    rst.set_low();
    Timer::after(Duration::from_micros(10)).await;
    rst.set_high();
    Timer::after(Duration::from_millis(5)).await;

    // MADCTL: BGR (0x08) + rotate 90 deg + flip (0xE8)
    write_command(spi, cs, dc, 0x36, &[0xE8]).await;
    write_command(spi, cs, dc, 0xB4, &[0x00]).await; // inversion control off
    write_command(spi, cs, dc, 0x20, &[]).await;     // normal color mode
    write_command(spi, cs, dc, 0x3A, &[0x55]).await; // 16-bit RGB565
    write_command(spi, cs, dc, 0x13, &[]).await;     // normal mode
    Timer::after(Duration::from_millis(120)).await;
    write_command(spi, cs, dc, 0x11, &[]).await;     // sleep out
    Timer::after(Duration::from_millis(140)).await;
    write_command(spi, cs, dc, 0x29, &[]).await;     // display on
}

async fn flush_rect(
    spi: &mut DisplaySpi,
    cs: &mut Output<'static>,
    dc: &mut Output<'static>,
    framebuf: &[Rgb565; VIEW_PIXELS],
    r: GuiRect,
) {
    let x0 = (r.x.max(0) as usize).min(VIEW_WIDTH);
    let y0 = (r.y.max(0) as usize).min(VIEW_HEIGHT);
    let x1 = ((r.x + r.w as i32).max(0) as usize).min(VIEW_WIDTH);
    let y1 = ((r.y + r.h as i32).max(0) as usize).min(VIEW_HEIGHT);
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let w = (x1 - x0) as u16;
    let h = (y1 - y0) as u16;

    set_window(spi, cs, dc, x0 as u16, y0 as u16, w, h).await;
    write_command(spi, cs, dc, 0x2C, &[]).await;

    cs.set_low();
    dc.set_high();

    let mut line_buf = [0u8; VIEW_WIDTH * 2];
    for y in y0..y1 {
        let row_start = y * VIEW_WIDTH;
        for (i, p) in framebuf[row_start + x0..row_start + x1].iter().enumerate() {
            let val = p.into_storage();
            line_buf[i * 2] = (val >> 8) as u8;
            line_buf[i * 2 + 1] = val as u8;
        }
        let _ = spi.write(&line_buf[..w as usize * 2]).await;
    }

    cs.set_high();
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("========================================================");
    info!(" STM32WBA65 USB Ethernet NTP Clock (embassy-net + xarxa)");
    info!("========================================================");

    // 1. Clock configuration: 96 MHz sysclk, 16 MHz reference to OTG_HS
    let mut config = Config::default();
    config.rcc.pll1 = Some(Pll {
        source: PllSource::Hsi,
        prediv: PllPreDiv::Div1,
        mul: PllMul::Mul30,
        divr: Some(PllDiv::Div5),
        divq: Some(PllDiv::Div10),
        divp: Some(PllDiv::Div30),
        frac: Some(0),
    });
    config.rcc.sys = Sysclk::Pll1R;
    config.rcc.ahb_pre = AHBPrescaler::Div1;
    config.rcc.apb1_pre = APBPrescaler::Div1;
    config.rcc.apb2_pre = APBPrescaler::Div1;
    config.rcc.voltage_scale = VoltageScale::Range1;
    config.rcc.mux.otghssel = mux::Otghssel::Pll1P;
    config.rcc.mux.rngsel = mux::Rngsel::Hsi;
    let p = embassy_stm32::init(config);

    // 2. Hardware RNG for stack seed
    let mut rng = Rng::new(p.RNG, Irqs);
    let mut seed = [0u8; 8];
    unwrap!(rng.fill_bytes(&mut seed).await);
    let seed = u64::from_le_bytes(seed);

    // 3. USB CDC-NCM Ethernet Device (High-Speed Bulk 512B)
    static EP_OUT: StaticCell<[u8; 1024]> = StaticCell::new();
    let mut driver_config = embassy_stm32::usb::Config::default();
    driver_config.vbus_detection = false;
    let driver = Driver::new_hs(
        p.USB_OTG_HS,
        p.PD6,
        p.PD7,
        Irqs,
        EP_OUT.init([0u8; 1024]),
        driver_config,
    );

    let mut usb_config = embassy_usb::Config::new(0xc0de, 0xcafe);
    usb_config.manufacturer = Some("Embassy");
    usb_config.product = Some("STM32WBA65 USB Ethernet");
    usb_config.serial_number = Some("wba65-ntp-clock");
    usb_config.max_power = 500;
    usb_config.max_packet_size_0 = 64;
    usb_config.max_speed = UsbDeviceSpeed::High;

    static CONFIG_DESC: StaticCell<[u8; 512]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 512]> = StaticCell::new();

    let mut builder = UsbBuilder::new(
        driver,
        usb_config,
        &mut CONFIG_DESC.init([0; 512])[..],
        &mut BOS_DESC.init([0; 256])[..],
        &mut [],
        &mut CONTROL_BUF.init([0; 512])[..],
    );

    let our_mac = [0xCC, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC];
    let host_mac = [0x88, 0x88, 0x88, 0x88, 0x88, 0x88];

    static NCM_STATE: StaticCell<NcmState> = StaticCell::new();
    let class = CdcNcmClass::new(&mut builder, NCM_STATE.init(NcmState::new()), host_mac, 512);

    let usb = builder.build();
    spawner.spawn(usb_task(usb).unwrap());

    static NET_STATE: StaticCell<NetState<4, 4>> = StaticCell::new();
    let (ncm_runner, device) = class.into_embassy_net_device::<4, 4>(
        NET_STATE.init(NetState::new()),
        our_mac,
        1514,
    );
    spawner.spawn(usb_ncm_task(ncm_runner).unwrap());

    // 4. Network stack with DHCPv4 and DNS (xarxa-backed embassy-net)
    static STACK: StaticCell<StackStorage> = StaticCell::new();
    static POOL: StaticPool = StaticPool::new();
    let (stack, net_runner) = embassy_net::Stack::new(STACK.init(StackStorage::new()), &POOL, seed);

    static DEVICE: StaticCell<Device<'static>> = StaticCell::new();
    let iface = unwrap!(stack.add_iface_borrowed(DEVICE.init(device)));
    unwrap!(iface.set_dhcpv4(Some(Default::default())));

    spawner.spawn(net_task(net_runner).unwrap());
    spawner.spawn(ntp_task(stack, iface).unwrap());
    spawner.spawn(weather_task(stack, iface).unwrap());

    info!("USB Ethernet and network stack tasks active!");
    embassy_futures::yield_now().await;

    // 5. Buttons (active-low, internal pull-up)
    let b1 = Input::new(p.PC13, Pull::Up); // Prev Timezone
    let b2 = Input::new(p.PC5, Pull::Up);  // Resync NTP Now
    let b3 = Input::new(p.PB4, Pull::Up);  // Next Timezone

    // 6. ILI9341 Display on SPI2 with GPDMA1_CH0
    let mut spi_config = SpiConfig::default();
    spi_config.frequency = Hertz(25_000_000);
    let mut spi = Spi::new_txonly(p.SPI2, p.PB10, p.PC3, p.GPDMA1_CH0, Irqs, spi_config);
    let mut cs = Output::new(p.PB9, Level::High, Speed::VeryHigh);
    let mut dc = Output::new(p.PB11, Level::Low, Speed::VeryHigh);
    let mut rst = Output::new(p.PA10, Level::High, Speed::VeryHigh);

    init_display(&mut spi, &mut cs, &mut dc, &mut rst).await;
    info!("ILI9341 display initialized (320x240) with async DMA");

    // 7. Embedded-GUI setup
    let full_rect = GuiRect::new(0, 0, VIEW_WIDTH as u32, VIEW_HEIGHT as u32);
    let mut gui = Gui::new(full_rect);
    let widgets = build_clock_ui(&mut gui);
    let framebuf = framebuf();

    // Initial full-screen clear and render
    framebuf.fill(BG);
    let _ = gui.render(&mut FbTarget(&mut *framebuf));
    flush_rect(&mut spi, &mut cs, &mut dc, framebuf, full_rect).await;

    // 8. Main UI event and render loop
    const TOTAL_SLOTS: usize = 1 + TIME_ZONES.len();
    let mut tz_idx = 0usize;
    let mut b1_last = true;
    let mut b2_last = true;
    let mut b3_last = true;

    let mut ticker = embassy_time::Ticker::every(embassy_time::Duration::from_millis(50));

    loop {
        ticker.next().await;

        // Button handling with falling edge detection
        let b1_curr = b1.is_high();
        let b2_curr = b2.is_high();
        let b3_curr = b3.is_high();

        if !b1_curr && b1_last {
            // B1 pressed: Previous Timezone / City
            if tz_idx == 0 {
                tz_idx = TOTAL_SLOTS - 1;
            } else {
                tz_idx -= 1;
            }
            if tz_idx == 0 {
                let geo = LOCAL_GEO.lock().await;
                if geo.detected {
                    info!("Selected: Local ({}, {})", geo.city.as_str(), geo.region.as_str());
                    WEATHER_REQUEST.signal((geo.lat, geo.lon));
                } else {
                    info!("Selected: Local (Detecting...)");
                }
            } else {
                let tz = &TIME_ZONES[tz_idx - 1];
                info!("Selected City: {}", tz.name);
                WEATHER_REQUEST.signal((tz.lat, tz.lon));
            }
        }

        if !b2_curr && b2_last {
            // B2 pressed: Force NTP & Weather sync
            info!("B2 pressed: requesting immediate NTP & Weather resync");
            RESYNC_SIGNAL.signal(());
            if tz_idx == 0 {
                let geo = LOCAL_GEO.lock().await;
                if geo.detected {
                    WEATHER_REQUEST.signal((geo.lat, geo.lon));
                }
            } else {
                let tz = &TIME_ZONES[tz_idx - 1];
                WEATHER_REQUEST.signal((tz.lat, tz.lon));
            }
        }

        if !b3_curr && b3_last {
            // B3 pressed: Next Timezone / City
            tz_idx = (tz_idx + 1) % TOTAL_SLOTS;
            if tz_idx == 0 {
                let geo = LOCAL_GEO.lock().await;
                if geo.detected {
                    info!("Selected: Local ({}, {})", geo.city.as_str(), geo.region.as_str());
                    WEATHER_REQUEST.signal((geo.lat, geo.lon));
                } else {
                    info!("Selected: Local (Detecting...)");
                }
            } else {
                let tz = &TIME_ZONES[tz_idx - 1];
                info!("Selected City: {}", tz.name);
                WEATHER_REQUEST.signal((tz.lat, tz.lon));
            }
        }

        b1_last = b1_curr;
        b2_last = b2_curr;
        b3_last = b3_curr;

        // Retrieve current time & state
        let clock = CLOCK.lock().await;

        // Update IP label
        if let Some(ip) = clock.local_ip {
            set_text(
                &mut gui,
                widgets.ip_label,
                TX_IP.format(format_args!("IP: {}", ip)),
            );
        } else {
            set_text(&mut gui, widgets.ip_label, "DHCP: Waiting...");
        }

        // Determine current timezone details & datetime
        let dt = if tz_idx == 0 {
            let geo = LOCAL_GEO.lock().await;
            if geo.detected {
                let offset_sign = if geo.offset_hours >= 0 { '+' } else { '-' };
                let abs_hours = geo.offset_hours.unsigned_abs();
                if geo.offset_mins != 0 {
                    set_text(
                        &mut gui,
                        widgets.tz_label,
                        TX_TZ.format(format_args!(
                            "{}, {} (UTC{}{}:{:02})",
                            geo.city.as_str(),
                            geo.region.as_str(),
                            offset_sign,
                            abs_hours,
                            geo.offset_mins
                        )),
                    );
                } else {
                    set_text(
                        &mut gui,
                        widgets.tz_label,
                        TX_TZ.format(format_args!(
                            "{}, {} (UTC{}{})",
                            geo.city.as_str(),
                            geo.region.as_str(),
                            offset_sign,
                            abs_hours
                        )),
                    );
                }
                clock.current_datetime_for_offset(geo.offset_hours, geo.offset_mins)
            } else {
                set_text(&mut gui, widgets.tz_label, "Local (Auto-detecting...)");
                clock.current_datetime_for_offset(0, 0)
            }
        } else {
            let current_tz = &TIME_ZONES[tz_idx - 1];
            let offset_sign = if current_tz.offset_hours >= 0 { '+' } else { '-' };
            let abs_hours = current_tz.offset_hours.unsigned_abs();
            if current_tz.offset_mins != 0 {
                set_text(
                    &mut gui,
                    widgets.tz_label,
                    TX_TZ.format(format_args!(
                        "{} (UTC{}{}:{:02})",
                        current_tz.name, offset_sign, abs_hours, current_tz.offset_mins
                    )),
                );
            } else {
                set_text(
                    &mut gui,
                    widgets.tz_label,
                    TX_TZ.format(format_args!(
                        "{} (UTC{}{})",
                        current_tz.name, offset_sign, abs_hours
                    )),
                );
            }
            clock.current_datetime_for_tz(current_tz)
        };

        // Update Clock & Date
        if let Some(dt) = dt {
            set_text(
                &mut gui,
                widgets.time_label,
                TX_TIME.format(format_args!(
                    "{:02}:{:02}:{:02}",
                    dt.hour, dt.min, dt.sec
                )),
            );
            set_text(
                &mut gui,
                widgets.date_label,
                TX_DATE.format(format_args!(
                    "{}, {} {}, {}",
                    dt.day_name(),
                    dt.month_name(),
                    dt.day,
                    dt.year
                )),
            );

            // Sub info line
            if let Some(utc_dt) = clock.current_datetime_for_offset(0, 0) {
                set_text(
                    &mut gui,
                    widgets.sub_label,
                    TX_SUB.format(format_args!(
                        "UTC {:02}:{:02}:{:02} | Stratum {} | RTT {}ms",
                        utc_dt.hour, utc_dt.min, utc_dt.sec, clock.stratum, clock.latency_ms
                    )),
                );
            }
        } else {
            set_text(&mut gui, widgets.time_label, "--:--:--");
            set_text(&mut gui, widgets.date_label, "Waiting for initial NTP sync...");
            set_text(&mut gui, widgets.sub_label, "NTP: Connecting...");
        }

        // Update Footer Status
        if clock.syncing {
            set_text(
                &mut gui,
                widgets.status_label,
                "Status: Synchronizing with NTP server...",
            );
        } else if clock.is_synced {
            set_text(
                &mut gui,
                widgets.status_label,
                TX_STATUS.format(format_args!("Status: Synced via {}", clock.last_server)),
            );
        } else {
            set_text(
                &mut gui,
                widgets.status_label,
                "Status: Waiting for USB link/DHCP...",
            );
        }

        // Update Weather labels
        let weather = WEATHER.lock().await;
        if weather.valid {
            let desc = weather_desc(weather.weather_code);
            let temp_int = weather.temp as i32;
            let temp_frac = ((weather.temp - temp_int as f32).abs() * 10.0) as u32;
            let max_int = weather.temp_max as i32;
            let min_int = weather.temp_min as i32;

            set_text(
                &mut gui,
                widgets.weather_main_label,
                TX_WEATHER_MAIN.format(format_args!(
                    "Weather: {}.{} C - {} (H:{} C / L:{} C)",
                    temp_int, temp_frac, desc, max_int, min_int
                )),
            );

            let wind_int = weather.wind_speed as u32;
            let wind_frac = ((weather.wind_speed - wind_int as f32) * 10.0) as u32;
            set_text(
                &mut gui,
                widgets.weather_sub_label,
                TX_WEATHER_SUB.format(format_args!(
                    "Humidity: {}%  |  Wind: {}.{} km/h",
                    weather.humidity, wind_int, wind_frac
                )),
            );
        } else if weather.updating {
            set_text(
                &mut gui,
                widgets.weather_main_label,
                "Weather: Fetching live forecast...",
            );
            set_text(
                &mut gui,
                widgets.weather_sub_label,
                "Contacting api.open-meteo.com...",
            );
        } else {
            set_text(
                &mut gui,
                widgets.weather_main_label,
                "Weather: Waiting for network link...",
            );
            set_text(
                &mut gui,
                widgets.weather_sub_label,
                "Forecast will load automatically",
            );
        }
        drop(weather);

        drop(clock);

        // Blit dirty rectangles only!
        let rects = gui.dirty_regions();
        if !rects.is_empty() {
            for r in rects {
                clear_rect(framebuf, *r);
            }
            let _ = gui.render_dirty(&mut FbTarget(&mut *framebuf));
            for r in rects {
                flush_rect(&mut spi, &mut cs, &mut dc, framebuf, *r).await;
            }
            gui.clear_dirty();
        }
    }
}
