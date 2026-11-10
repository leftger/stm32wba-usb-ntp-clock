# STM32WBA65 USB Ethernet NTP Clock

An asynchronous Rust application for the **STM32WBA65RI** (Arm Cortex-M33 with FPU, 2 MB Flash, 512 KB RAM) running **Embassy** and **`embedded-gui`** on an ILI9341 2.8" TFT display (DM-TFT28-116).

The board implements **USB CDC-NCM Ethernet** on its integrated **USB High-Speed PHY (480 Mbps)**, connects to the Internet through the host PC via NAT/DHCP, automatically discovers its local geographic location via **IP Geolocation** (`ipwho.is`), fetches live weather forecasts from **Open-Meteo**, and acts as a high-precision **NTP World Clock** with selectable time zones and real-time dirty-rectangle GUI updates.

---

## ✨ Features

- **USB High-Speed CDC-NCM**: 480 Mbps Ethernet-over-USB networking driven by Embassy's USB driver on the WBA65 High-Speed PHY.
- **Embedded Network Stack**: Backed by `embassy-net` and `xarxa` with asynchronous DHCPv4 and DNS resolution.
- **NTP Synchronization**: Queries `pool.ntp.org` via SNTP over UDP to establish high-precision Unix epoch time.
- **Dynamic IP Geolocation**: Detects the host's external city, country, coordinates, and UTC offset seconds via `ipwho.is` using zero-allocation JSON streaming.
- **Live Weather Forecast**: Pulls live temperature, conditions, daily high/low, humidity, and wind speed from `api.open-meteo.com` for the active location.
- **Interactive Timezone Carousel**:
  - **Slot 0**: Auto-detected local location and weather.
  - **Slots 1–17**: Global cities (UTC, London, Paris, Tokyo, Mumbai, New York, etc.) navigable with onboard buttons B1 and B3.
  - **Button B2**: Instant NTP and weather resync.
- **Declarative GUI**: Built with [`embedded-gui`](https://github.com/leftger/embedded-gui) using partial dirty-region SPI DMA blitting on the 320x240 ILI9341 display.

---

## 🛠️ Hardware Setup

### Microcontroller Target
- **MCU**: STM32WBA65RI (NUCLEO-WBA65RI board)
- **Target Triple**: `thumbv8m.main-none-eabihf`
- **Framework**: [Embassy](https://embassy.dev) (latest git main: `embassy-net` backed by `xarxa`)
- **GUI Engine**: [`embedded-gui`](https://github.com/leftger/embedded-gui)

### USB High-Speed Connection
- **User USB-HS Port**: Connected to `PD6` (DP) and `PD7` (DM) with integrated High-Speed PHY.
- **Port to PC**: Connect a USB-C cable from the **user USB port** (CN15 / user USB-C) to your PC.
- **ST-Link Port**: Connect the standard ST-Link USB connector to your PC for flashing and defmt RTT logging.

### Display Pinout (ILI9341 SPI2 Shield)
| Function | STM32WBA Pin | Embassy Signal | Notes |
| :--- | :--- | :--- | :--- |
| **SPI SCK** | `PB10` | `SPI2` SCK | 25 MHz SPI |
| **SPI MOSI** | `PC3` | `SPI2` MOSI | Transmit data |
| **TFT CS** | `PB9` | GPIO Output | Active-low Chip Select |
| **TFT DC** | `PB11` | GPIO Output | Data / Command |
| **TFT RST** | `PA10` | GPIO Output | Hardware Reset |

### Nucleo User Buttons
| Button | Pin | Function |
| :--- | :--- | :--- |
| **B1 (Left)** | `PC13` | Previous Timezone (◄) |
| **B2 (Center)** | `PC5` | Force NTP Resync Now (⟳) |
| **B3 (Right)** | `PB4` | Next Timezone (►) |

---

## 🌐 Host Network Setup (Internet Sharing)

When connected to your PC, the board enumerates as a standard USB CDC-NCM Ethernet network interface:

### Linux
1. Check the newly created interface with `ip link` (usually `usb0` or `enx...`).
2. Share your internet connection via NetworkManager:
   ```bash
   nmcli connection modify "Wired connection 1" ipv4.method shared
   ```
   *Or with iptables:*
   ```bash
   sudo sysctl -w net.ipv4.ip_forward=1
   sudo iptables -t nat -A POSTROUTING -o <wifi_interface> -j MASQUERADE
   ```

### macOS
1. Open **System Settings → General → Sharing → Internet Sharing**.
2. Share your Wi-Fi connection to the **USB NCM / Ethernet Adapter**.

### Windows 11
1. CDC-NCM is supported out-of-the-box in Windows 11.
2. In **Network Connections**, right-click your main Wi-Fi/Ethernet adapter → **Properties → Sharing tab**.
3. Check **"Allow other network users to connect through this computer's Internet connection"** and select the WBA65 USB Ethernet adapter.

---

## 🚀 Building & Running

Flash and run with RTT logs over SWD:

```bash
cargo run --release
```

Or build debug:
```bash
cargo build
```
