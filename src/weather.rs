//! Live Weather and GeoIP client using Open-Meteo & ip-api.com HTTP JSON APIs.

use core::fmt::Write as _;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};

#[derive(Clone)]
pub struct LocalGeoLocation {
    pub city: heapless::String<32>,
    pub region: heapless::String<32>,
    pub offset_hours: i8,
    pub offset_mins: i8,
    pub lat: f32,
    pub lon: f32,
    pub detected: bool,
}

impl LocalGeoLocation {
    pub const fn new() -> Self {
        Self {
            city: heapless::String::new(),
            region: heapless::String::new(),
            offset_hours: 0,
            offset_mins: 0,
            lat: 0.0,
            lon: 0.0,
            detected: false,
        }
    }
}

pub static LOCAL_GEO: Mutex<CriticalSectionRawMutex, LocalGeoLocation> =
    Mutex::new(LocalGeoLocation::new());

#[derive(Clone, Copy)]
pub struct WeatherData {
    pub temp: f32,
    pub temp_max: f32,
    pub temp_min: f32,
    pub humidity: u8,
    pub wind_speed: f32,
    pub weather_code: u8,
    pub valid: bool,
    pub updating: bool,
}

impl Default for WeatherData {
    fn default() -> Self {
        Self {
            temp: 0.0,
            temp_max: 0.0,
            temp_min: 0.0,
            humidity: 0,
            wind_speed: 0.0,
            weather_code: 0,
            valid: false,
            updating: false,
        }
    }
}

pub static WEATHER: Mutex<CriticalSectionRawMutex, WeatherData> = Mutex::new(WeatherData {
    temp: 0.0,
    temp_max: 0.0,
    temp_min: 0.0,
    humidity: 0,
    wind_speed: 0.0,
    weather_code: 0,
    valid: false,
    updating: false,
});

/// Signal to request weather update for (lat, lon) coordinates
pub static WEATHER_REQUEST: Signal<CriticalSectionRawMutex, (f32, f32)> = Signal::new();

pub fn weather_desc(code: u8) -> &'static str {
    match code {
        0 => "Clear Sky",
        1 => "Mainly Clear",
        2 => "Partly Cloudy",
        3 => "Overcast",
        45 | 48 => "Foggy",
        51 | 53 | 55 => "Drizzle",
        56 | 57 => "Freezing Drizzle",
        61 | 63 | 65 => "Rain",
        66 | 67 => "Freezing Rain",
        71 | 73 | 75 => "Snow",
        77 => "Snow Grains",
        80 | 81 | 82 => "Showers",
        85 | 86 => "Snow Showers",
        95 => "Thunderstorm",
        96 | 99 => "Hail Storm",
        _ => "Fair",
    }
}

/// Helper buffer for formatting HTTP GET queries without allocations.
struct RequestBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> RequestBuf<N> {
    pub fn new() -> Self {
        Self { buf: [0; N], len: 0 }
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> core::fmt::Write for RequestBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let rem = N - self.len;
        let to_copy = bytes.len().min(rem);
        self.buf[self.len..self.len + to_copy].copy_from_slice(&bytes[..to_copy]);
        self.len += to_copy;
        Ok(())
    }
}

fn parse_i32(s: &str) -> Option<i32> {
    let mut val = 0i32;
    for b in s.bytes() {
        if b.is_ascii_digit() {
            val = val.checked_mul(10)?.checked_add((b - b'0') as i32)?;
        } else {
            return None;
        }
    }
    Some(val)
}

fn parse_float(s: &str) -> Option<f32> {
    let (neg, s) = if let Some(stripped) = s.strip_prefix('-') {
        (true, stripped)
    } else {
        (false, s)
    };
    let mut parts = s.split('.');
    let int_part = parse_i32(parts.next()?)? as f32;
    let frac_part = if let Some(frac_str) = parts.next() {
        let frac_val = parse_i32(frac_str)? as f32;
        let mut div = 1.0f32;
        for _ in 0..frac_str.len() {
            div *= 10.0;
        }
        frac_val / div
    } else {
        0.0
    };
    let res = int_part + frac_part;
    Some(if neg { -res } else { res })
}

pub fn parse_json_float(json: &str, key: &str) -> Option<f32> {
    let idx = json.find(key)?;
    let mut rest = &json[idx + key.len()..];
    rest = rest.trim_start_matches(|c: char| c == ':' || c == '[' || c.is_whitespace());
    let end = rest.find(|c: char| c == ',' || c == ']' || c == '}' || c.is_whitespace())?;
    parse_float(&rest[..end])
}

pub fn parse_json_str<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let idx = json.find(key)?;
    let mut rest = &json[idx + key.len()..];
    rest = rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    if let Some(stripped) = rest.strip_prefix('"') {
        rest = stripped;
    } else {
        return None;
    }
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Fetch local geographic location and timezone offset based on the host's external IP address.
pub async fn fetch_geoip(stack: Stack<'static>) -> Result<LocalGeoLocation, ()> {
    defmt::info!("geoip: resolving ipwho.is...");
    let server_ip = match stack.dns_query("ipwho.is", DnsQueryType::A).await {
        Ok(addrs) => {
            if let Some(embassy_net::wire::IpAddr::V4(ip)) = addrs.first() {
                defmt::info!("geoip: resolved ipwho.is -> {}", ip);
                *ip
            } else {
                return Err(());
            }
        }
        Err(e) => {
            defmt::warn!("geoip: DNS query failed: {:?}", defmt::Debug2Format(&e));
            return Err(());
        }
    };

    let mut rx_buf = [0u8; 1536];
    let mut tx_buf = [0u8; 512];
    let mut socket = match TcpSocket::new(stack, &mut rx_buf, &mut tx_buf) {
        Ok(s) => s,
        Err(_) => return Err(()),
    };
    socket.set_timeout(Some(Duration::from_secs(8)));

    defmt::info!("geoip: connecting to {}:80...", server_ip);
    if let Err(e) = socket.connect((server_ip, 80)).await {
        defmt::warn!("geoip: TCP connect failed: {:?}", defmt::Debug2Format(&e));
        return Err(());
    }
    defmt::info!("geoip: connected! Sending request...");

    let req = b"GET / HTTP/1.1\r\nHost: ipwho.is\r\nConnection: close\r\nUser-Agent: WBA65\r\n\r\n";
    let mut written = 0;
    while written < req.len() {
        match socket.write(&req[written..]).await {
            Ok(0) => return Err(()),
            Ok(n) => written += n,
            Err(_) => return Err(()),
        }
    }
    defmt::info!("geoip: request queued, reading response...");

    let mut resp_buf = [0u8; 1536];
    let mut total = 0;
    while total < resp_buf.len() {
        match socket.read(&mut resp_buf[total..]).await {
            Ok(0) => {
                defmt::info!("geoip: EOF reached, total bytes: {}", total);
                break;
            }
            Ok(n) => {
                total += n;
                defmt::info!("geoip: read chunk of {} bytes, total: {}", n, total);
                if let Ok(text) = core::str::from_utf8(&resp_buf[..total]) {
                    if let Some(body_idx) = text.find("\r\n\r\n") {
                        let body = &text[body_idx + 4..];
                        if body.ends_with('}') || body.ends_with("}\n") || body.ends_with("}\r\n") {
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                defmt::warn!("geoip: read error: {:?}", defmt::Debug2Format(&e));
                break;
            }
        }
    }

    let text = core::str::from_utf8(&resp_buf[..total]).unwrap_or("");
    defmt::info!("geoip: raw HTTP response: {}", text);
    let json = if let Some(idx) = text.find("\r\n\r\n") {
        &text[idx + 4..]
    } else {
        text
    };

    if !json.contains("\"success\":true") && !json.contains("\"success\": true") {
        defmt::warn!("geoip: response did not contain success:true");
        return Err(());
    }

    let city_str = parse_json_str(json, "\"city\"").ok_or(())?;
    let country_str = parse_json_str(json, "\"country\"").unwrap_or("");
    let lat = parse_json_float(json, "\"latitude\"")
        .or_else(|| parse_json_float(json, "\"lat\""))
        .ok_or(())?;
    let lon = parse_json_float(json, "\"longitude\"")
        .or_else(|| parse_json_float(json, "\"lon\""))
        .ok_or(())?;
    let offset_sec = parse_json_float(json, "\"offset\"").ok_or(())? as i32;
    let offset_hours = (offset_sec / 3600) as i8;
    let offset_mins = ((offset_sec.abs() % 3600) / 60) as i8;

    let mut city = heapless::String::<32>::new();
    let _ = city.push_str(city_str);

    let mut region = heapless::String::<32>::new();
    let _ = region.push_str(country_str);

    Ok(LocalGeoLocation {
        city,
        region,
        offset_hours,
        offset_mins,
        lat,
        lon,
        detected: true,
    })
}

async fn fetch_weather(stack: Stack<'static>, lat: f32, lon: f32) -> Result<WeatherData, ()> {
    // 1. Resolve Open-Meteo API IP
    let server_ip = match stack.dns_query("api.open-meteo.com", DnsQueryType::A).await {
        Ok(addrs) => {
            if let Some(embassy_net::wire::IpAddr::V4(ip)) = addrs.first() {
                *ip
            } else {
                defmt::warn!("weather_task: no IPv4 address resolved");
                return Err(());
            }
        }
        Err(e) => {
            defmt::warn!("weather_task: DNS lookup failed: {:?}", defmt::Debug2Format(&e));
            return Err(());
        }
    };

    // 2. Open TCP connection
    let mut rx_buf = [0u8; 1536];
    let mut tx_buf = [0u8; 512];
    let mut socket = match TcpSocket::new(stack, &mut rx_buf, &mut tx_buf) {
        Ok(s) => s,
        Err(_) => return Err(()),
    };
    socket.set_timeout(Some(Duration::from_secs(8)));

    if let Err(e) = socket.connect((server_ip, 80)).await {
        defmt::warn!("weather_task: TCP connect failed: {:?}", defmt::Debug2Format(&e));
        return Err(());
    }

    // 3. Format coordinates without relying on float formatting in core::fmt
    let lat_sign = if lat < 0.0 { "-" } else { "" };
    let lat_abs = if lat < 0.0 { -lat } else { lat };
    let lat_scaled = (lat_abs * 100.0 + 0.5) as u32;
    let lat_int = lat_scaled / 100;
    let lat_frac = lat_scaled % 100;

    let lon_sign = if lon < 0.0 { "-" } else { "" };
    let lon_abs = if lon < 0.0 { -lon } else { lon };
    let lon_scaled = (lon_abs * 100.0 + 0.5) as u32;
    let lon_int = lon_scaled / 100;
    let lon_frac = lon_scaled % 100;

    let mut req_buf = RequestBuf::<384>::new();
    let _ = write!(
        req_buf,
        "GET /v1/forecast?latitude={}{}.{:02}&longitude={}{}.{:02}&current=temperature_2m,relative_humidity_2m,weather_code,wind_speed_10m&daily=temperature_2m_max,temperature_2m_min&forecast_days=1 HTTP/1.1\r\nHost: api.open-meteo.com\r\nConnection: close\r\nUser-Agent: WBA65\r\n\r\n",
        lat_sign, lat_int, lat_frac, lon_sign, lon_int, lon_frac
    );

    let req_bytes = req_buf.as_str().as_bytes();
    let mut written = 0;
    while written < req_bytes.len() {
        match socket.write(&req_bytes[written..]).await {
            Ok(0) => return Err(()),
            Ok(n) => written += n,
            Err(e) => {
                defmt::warn!("weather_task: HTTP request write failed: {:?}", defmt::Debug2Format(&e));
                return Err(());
            }
        }
    }

    // 4. Read HTTP response into response buffer
    let mut resp_buf = [0u8; 1536];
    let mut total = 0;
    while total < resp_buf.len() {
        match socket.read(&mut resp_buf[total..]).await {
            Ok(0) => break, // EOF
            Ok(n) => total += n,
            Err(e) => {
                defmt::warn!("weather_task: read error: {:?}", defmt::Debug2Format(&e));
                break;
            }
        }
    }

    let text = core::str::from_utf8(&resp_buf[..total]).unwrap_or("");
    let json = if let Some(idx) = text.find("\r\n\r\n") {
        &text[idx + 4..]
    } else {
        text
    };

    // 5. Parse JSON values using section delimiters to avoid collision with current_units
    let current_section = if let Some(idx) = json.find("\"current\":") {
        &json[idx..]
    } else {
        json
    };

    let daily_section = if let Some(idx) = json.find("\"daily\":") {
        &json[idx..]
    } else {
        json
    };

    let temp = parse_json_float(current_section, "\"temperature_2m\"").unwrap_or(0.0);
    let humidity = parse_json_float(current_section, "\"relative_humidity_2m\"").unwrap_or(0.0) as u8;
    let weather_code = parse_json_float(current_section, "\"weather_code\"").unwrap_or(0.0) as u8;
    let wind_speed = parse_json_float(current_section, "\"wind_speed_10m\"").unwrap_or(0.0);
    let temp_max = parse_json_float(daily_section, "\"temperature_2m_max\"").unwrap_or(temp);
    let temp_min = parse_json_float(daily_section, "\"temperature_2m_min\"").unwrap_or(temp);

    Ok(WeatherData {
        temp,
        temp_max,
        temp_min,
        humidity,
        wind_speed,
        weather_code,
        valid: true,
        updating: false,
    })
}

#[embassy_executor::task]
pub async fn weather_task(stack: Stack<'static>, iface: embassy_net::iface::Iface<'static>) -> ! {
    defmt::info!("weather_task: waiting for network link...");
    while !iface.is_config_up() {
        Timer::after(Duration::from_millis(500)).await;
    }
    defmt::info!("weather_task: network ready, starting weather & geoip service");

    let mut current_lat = 51.51f32; // Fallback: London
    let mut current_lon = -0.13f32;

    // Wait 2 seconds on boot so NTP gets initial socket priority
    Timer::after(Duration::from_secs(2)).await;

    // Detect location via host external IP
    defmt::info!("weather_task: detecting local location via IP geolocation...");
    for attempt in 1..=3 {
        defmt::info!("weather_task: GeoIP attempt {}/3...", attempt);
        match fetch_geoip(stack).await {
            Ok(geo) => {
                defmt::info!(
                    "weather_task: location detected! City: {}, Region: {}, Offset: {}:{:02}, Lat: {}, Lon: {}",
                    geo.city.as_str(),
                    geo.region.as_str(),
                    geo.offset_hours,
                    geo.offset_mins,
                    geo.lat,
                    geo.lon
                );
                current_lat = geo.lat;
                current_lon = geo.lon;
                let mut local_geo = LOCAL_GEO.lock().await;
                *local_geo = geo;
                break;
            }
            Err(_) => {
                defmt::warn!("weather_task: GeoIP attempt {} failed, retrying...", attempt);
                Timer::after(Duration::from_millis(1500)).await;
            }
        }
    }

    loop {
        {
            let mut w = WEATHER.lock().await;
            w.updating = true;
        }

        defmt::info!("weather_task: fetching weather for ({}, {})...", current_lat, current_lon);
        match fetch_weather(stack, current_lat, current_lon).await {
            Ok(data) => {
                defmt::info!("weather_task: updated! {} C, code {}", data.temp, data.weather_code);
                let mut w = WEATHER.lock().await;
                *w = data;
            }
            Err(_) => {
                defmt::warn!("weather_task: weather fetch failed, will retry");
                let mut w = WEATHER.lock().await;
                w.updating = false;
            }
        }

        // Wait for next manual city change OR periodic 10-minute refresh
        let timer = Timer::after(Duration::from_secs(600));
        let signal = WEATHER_REQUEST.wait();
        match embassy_futures::select::select(timer, signal).await {
            embassy_futures::select::Either::First(_) => {
                // Periodic refresh
            }
            embassy_futures::select::Either::Second((new_lat, new_lon)) => {
                current_lat = new_lat;
                current_lon = new_lon;
                // Immediate refresh for new city
            }
        }
    }
}
