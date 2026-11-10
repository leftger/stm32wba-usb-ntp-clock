//! NTP client, time calculations, and timezone definitions.

use embassy_net::dns::DnsQueryType;
use embassy_net::udp::UdpSocket;
use embassy_net::wire::{IpAddr, Ipv4Addr};
use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};

pub const NTP_PORT: u16 = 123;
/// Difference between NTP epoch (1900-01-01) and Unix epoch (1970-01-01) in seconds.
pub const NTP_TO_UNIX_OFFSET: i64 = 2_208_988_800;

#[derive(Clone, Copy, PartialEq)]
pub struct TimeZone {
    pub name: &'static str,
    pub region: &'static str,
    pub offset_hours: i8,
    pub offset_mins: i8,
    pub lat: f32,
    pub lon: f32,
}

pub const TIME_ZONES: &[TimeZone] = &[
    TimeZone {
        name: "UTC",
        region: "Coordinated Universal Time",
        offset_hours: 0,
        offset_mins: 0,
        lat: 51.48,
        lon: 0.00,
    },
    TimeZone {
        name: "London",
        region: "UK (GMT / BST)",
        offset_hours: 0,
        offset_mins: 0,
        lat: 51.51,
        lon: -0.13,
    },
    TimeZone {
        name: "Paris",
        region: "Central Europe (CET)",
        offset_hours: 1,
        offset_mins: 0,
        lat: 48.86,
        lon: 2.35,
    },
    TimeZone {
        name: "Cairo",
        region: "Eastern Europe (EET)",
        offset_hours: 2,
        offset_mins: 0,
        lat: 30.04,
        lon: 31.24,
    },
    TimeZone {
        name: "Dubai",
        region: "Gulf Standard (GST)",
        offset_hours: 4,
        offset_mins: 0,
        lat: 25.20,
        lon: 55.27,
    },
    TimeZone {
        name: "Mumbai",
        region: "India Standard (IST)",
        offset_hours: 5,
        offset_mins: 30,
        lat: 19.08,
        lon: 72.88,
    },
    TimeZone {
        name: "Bangkok",
        region: "Indochina Time (ICT)",
        offset_hours: 7,
        offset_mins: 0,
        lat: 13.76,
        lon: 100.50,
    },
    TimeZone {
        name: "Tokyo",
        region: "Japan Standard (JST)",
        offset_hours: 9,
        offset_mins: 0,
        lat: 35.68,
        lon: 139.69,
    },
    TimeZone {
        name: "Sydney",
        region: "Eastern Australia (AEST)",
        offset_hours: 10,
        offset_mins: 0,
        lat: -33.87,
        lon: 151.21,
    },
    TimeZone {
        name: "Auckland",
        region: "New Zealand (NZST)",
        offset_hours: 12,
        offset_mins: 0,
        lat: -36.85,
        lon: 174.76,
    },
    TimeZone {
        name: "Honolulu",
        region: "Hawaii Standard (HST)",
        offset_hours: -10,
        offset_mins: 0,
        lat: 21.31,
        lon: -157.86,
    },
    TimeZone {
        name: "Anchorage",
        region: "Alaska Standard (AKST)",
        offset_hours: -9,
        offset_mins: 0,
        lat: 61.22,
        lon: -149.90,
    },
    TimeZone {
        name: "Los Angeles",
        region: "US Pacific (PST/PDT)",
        offset_hours: -8,
        offset_mins: 0,
        lat: 34.05,
        lon: -118.24,
    },
    TimeZone {
        name: "Denver",
        region: "US Mountain (MST/MDT)",
        offset_hours: -7,
        offset_mins: 0,
        lat: 39.74,
        lon: -104.99,
    },
    TimeZone {
        name: "Chicago",
        region: "US Central (CST/CDT)",
        offset_hours: -6,
        offset_mins: 0,
        lat: 41.88,
        lon: -87.63,
    },
    TimeZone {
        name: "New York",
        region: "US Eastern (EST/EDT)",
        offset_hours: -5,
        offset_mins: 0,
        lat: 40.71,
        lon: -74.01,
    },
    TimeZone {
        name: "Sao Paulo",
        region: "Brazil Time (BRT)",
        offset_hours: -3,
        offset_mins: 0,
        lat: -23.55,
        lon: -46.63,
    },
];

#[derive(Clone, Copy, Debug)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub min: u8,
    pub sec: u8,
    pub day_of_week: u8, // 0 = Sunday, 1 = Monday, ..., 6 = Saturday
}

impl DateTime {
    pub const fn day_name(&self) -> &'static str {
        match self.day_of_week {
            0 => "Sunday",
            1 => "Monday",
            2 => "Tuesday",
            3 => "Wednesday",
            4 => "Thursday",
            5 => "Friday",
            6 => "Saturday",
            _ => "Unknown",
        }
    }

    pub const fn month_name(&self) -> &'static str {
        match self.month {
            1 => "January",
            2 => "February",
            3 => "March",
            4 => "April",
            5 => "May",
            6 => "June",
            7 => "July",
            8 => "August",
            9 => "September",
            10 => "October",
            11 => "November",
            12 => "December",
            _ => "Unknown",
        }
    }
}

/// Convert Unix timestamp (seconds since 1970-01-01 00:00:00 UTC) to calendar date & time.
/// Uses Howard Hinnant's standard algorithm.
pub fn unix_to_datetime(unix_secs: i64) -> DateTime {
    let secs_per_day = 86_400i64;
    let mut days = unix_secs / secs_per_day;
    let mut rem_secs = unix_secs % secs_per_day;
    if rem_secs < 0 {
        rem_secs += secs_per_day;
        days -= 1;
    }

    let hour = (rem_secs / 3600) as u8;
    let min = ((rem_secs % 3600) / 60) as u8;
    let sec = (rem_secs % 60) as u8;

    // 1970-01-01 was Thursday (4)
    let day_of_week = (((days + 4) % 7 + 7) % 7) as u8;

    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1024 + doe / 1461 - doe / 146096) / 365;
    let y = (yoe as i32) + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    let y = (if m <= 2 { y + 1 } else { y }) as u16;

    DateTime {
        year: y,
        month: m,
        day: d,
        hour,
        min,
        sec,
        day_of_week,
    }
}

/// Shared clock status readable by the UI task.
pub struct ClockState {
    pub base_unix_secs: i64,
    pub base_instant: Option<Instant>,
    pub is_synced: bool,
    pub syncing: bool,
    pub last_server: &'static str,
    pub stratum: u8,
    pub latency_ms: u32,
    pub local_ip: Option<Ipv4Addr>,
}

impl ClockState {
    pub const fn new() -> Self {
        Self {
            base_unix_secs: 0,
            base_instant: None,
            is_synced: false,
            syncing: false,
            last_server: "None",
            stratum: 0,
            latency_ms: 0,
            local_ip: None,
        }
    }

    /// Calculate the current live Unix timestamp in seconds.
    pub fn current_unix_secs(&self) -> Option<i64> {
        let base = self.base_instant?;
        let elapsed = (Instant::now() - base).as_secs() as i64;
        Some(self.base_unix_secs + elapsed)
    }

    /// Calculate the DateTime for a specific hour and minute offset from UTC.
    pub fn current_datetime_for_offset(&self, offset_hours: i8, offset_mins: i8) -> Option<DateTime> {
        let unix = self.current_unix_secs()?;
        let min_sec = if offset_hours < 0 {
            -(offset_mins as i64 * 60)
        } else {
            offset_mins as i64 * 60
        };
        let tz_offset = (offset_hours as i64 * 3600) + min_sec;
        Some(unix_to_datetime(unix + tz_offset))
    }

    /// Calculate the DateTime for a specific timezone.
    pub fn current_datetime_for_tz(&self, tz: &TimeZone) -> Option<DateTime> {
        self.current_datetime_for_offset(tz.offset_hours, tz.offset_mins)
    }
}

pub static CLOCK: Mutex<CriticalSectionRawMutex, ClockState> = Mutex::new(ClockState::new());
pub static RESYNC_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Background task that performs periodic NTP sync and responds to resync requests.
#[embassy_executor::task]
pub async fn ntp_task(stack: Stack<'static>, iface: embassy_net::iface::Iface<'static>) -> ! {
    defmt::info!("ntp_task: waiting for DHCP link and IP configuration...");

    // Wait until DHCP gives us an IP
    iface.wait_config_up().await;
    defmt::info!("ntp_task: network configuration is UP!");

    // Store IP address in shared state
    if let Some(addr) = iface.ip_addrs().first() {
        let IpAddr::V4(ip) = addr.cidr.address();
        let mut state = CLOCK.lock().await;
        state.local_ip = Some(ip);
        defmt::info!("ntp_task: local IP is {}", ip);
    }

    // List of fallback NTP server IP addresses (Cloudflare, Google, NIST)
    let fallback_ips = [
        Ipv4Addr::new(216, 239, 35, 0),  // time.google.com
        Ipv4Addr::new(162, 159, 200, 1), // time.cloudflare.com
        Ipv4Addr::new(129, 6, 15, 28),   // time.nist.gov
    ];

    loop {
        {
            let mut state = CLOCK.lock().await;
            state.syncing = true;
        }

        defmt::info!("ntp_task: resolving NTP server address...");
        let (server_ip, server_name) = match stack.dns_query("pool.ntp.org", DnsQueryType::A).await
        {
            Ok(addrs) => {
                if let Some(IpAddr::V4(ip)) = addrs.first() {
                    defmt::info!("ntp_task: resolved pool.ntp.org -> {}", ip);
                    (*ip, "pool.ntp.org")
                } else {
                    (fallback_ips[0], "time.google.com")
                }
            }
            Err(e) => {
                defmt::warn!(
                    "ntp_task: DNS query failed ({:?}), using fallback IP",
                    defmt::Debug2Format(&e)
                );
                (fallback_ips[0], "time.google.com")
            }
        };

        defmt::info!(
            "ntp_task: sending SNTP query to {} ({}:123)",
            server_name,
            server_ip
        );

        let mut socket = match UdpSocket::new(stack) {
            Ok(s) => s,
            Err(e) => {
                defmt::error!("ntp_task: failed to allocate UDP socket: {:?}", e);
                Timer::after(Duration::from_secs(5)).await;
                continue;
            }
        };

        let remote = (server_ip, NTP_PORT);
        if let Err(e) = socket.bind(0, remote) {
            defmt::error!("ntp_task: failed to bind socket: {:?}", e);
            Timer::after(Duration::from_secs(5)).await;
            continue;
        }

        // Prepare SNTP client request packet (48 bytes, LI=0, VN=4, Mode=3)
        let mut ntp_packet = [0u8; 48];
        ntp_packet[0] = 0x23;

        let start_time = Instant::now();
        if let Err(e) = socket.send_to(&ntp_packet, remote).await {
            defmt::warn!("ntp_task: UDP send error: {:?}", e);
        } else {
            // Receive response with 4 second timeout
            let timeout_fut = embassy_time::with_timeout(
                Duration::from_secs(4),
                socket.recv_from(&mut ntp_packet),
            );

            match timeout_fut.await {
                Ok(Ok((len, _meta))) if len >= 48 => {
                    let latency = (Instant::now() - start_time).as_millis() as u32;
                    let stratum = ntp_packet[1];
                    let ntp_secs = u32::from_be_bytes([
                        ntp_packet[40],
                        ntp_packet[41],
                        ntp_packet[42],
                        ntp_packet[43],
                    ]);

                    if (ntp_secs as i64) > NTP_TO_UNIX_OFFSET {
                        let unix_secs = (ntp_secs as i64) - NTP_TO_UNIX_OFFSET;
                        let sync_instant = Instant::now();

                        defmt::info!(
                            "ntp_task: SUCCESS! Unix time: {}, stratum: {}, latency: {} ms",
                            unix_secs,
                            stratum,
                            latency
                        );

                        let mut state = CLOCK.lock().await;
                        state.base_unix_secs = unix_secs;
                        state.base_instant = Some(sync_instant);
                        state.is_synced = true;
                        state.syncing = false;
                        state.last_server = server_name;
                        state.stratum = stratum;
                        state.latency_ms = latency;
                    } else {
                        defmt::warn!("ntp_task: invalid NTP timestamp: {}", ntp_secs);
                    }
                }
                Ok(Ok((len, _))) => {
                    defmt::warn!("ntp_task: received short packet ({} bytes)", len);
                }
                Ok(Err(e)) => {
                    defmt::warn!("ntp_task: UDP recv error: {:?}", e);
                }
                Err(_) => {
                    defmt::warn!("ntp_task: NTP request timed out after 4 seconds");
                }
            }
        }

        {
            let mut state = CLOCK.lock().await;
            state.syncing = false;
        }

        // Wait for next periodic sync (e.g. 10 minutes) OR manual trigger via RESYNC_SIGNAL
        embassy_futures::select::select(
            Timer::after(Duration::from_secs(600)),
            RESYNC_SIGNAL.wait(),
        )
        .await;

        defmt::info!("ntp_task: waking up for next NTP sync");
    }
}
