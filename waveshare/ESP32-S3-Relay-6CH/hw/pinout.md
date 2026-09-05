# Waveshare ESP32-S3-Relay-6CH — hardware notes

Also sold by SpotPear as "ESP32-S3-WROOM-1U-N8 WiFi RS485 Bluetooth Industrial 6-Channel Relay".
Confirmed from the Waveshare schematic and demo code; our unit carries the N16 module (16 MB flash,
no PSRAM, external SMA antenna).

| Function | GPIO | Notes |
|---|---|---|
| Relay CH1..CH6 | 1, 2, 41, 42, 45, 46 | Active-high. 100K pull-down on each driver base, so OFF at boot. GPIO45/46 are strapping pins: never add pull-ups. |
| RGB LED | 38 | Single WS2812B on 3V3 |
| Buzzer | 21 | Passive, needs PWM (LEDC) |
| RS485 | TX 17, RX 18 (UART1) | Direction is hardware-automatic (no DE/RE pin); 120 R terminator via jumper H3, off by default; isolated ground |
| BOOT button | 0 | Active low, 10K pull-up. Firmware: 5 s = setup AP, 15 s = factory reset |
| RESET button | EN | |
| USB-C | 19 / 20 | Native USB-Serial-JTAG, no bridge chip. Download mode: hold BOOT, tap RESET |
| UART0 | 43 / 44 | Only on the Pico header |
| I2C (optional RTC HAT) | SDA 4, SCL 5 | Waveshare Pico-RTC-DS3231 at 0x68 |

Power: 7–36 V DC screw terminal or 5 V USB-C. Relays HLS8L-DC5V, SPDT (NO/NC/COM), 10 A 250 VAC / 10 A 30 VDC.
Six coils plus WiFi may brown out a weak USB port; use the DC terminal in deployment.

## Flash layout (16 MB), written by espflash from `firmware/partitions.csv`

| Partition | Offset | Size |
|---|---|---|
| nvs | 0x9000 | 24 KB |
| otadata | 0xf000 | 8 KB |
| phy_init | 0x11000 | 4 KB |
| ota_0 | 0x20000 | 4 MB |
| ota_1 | 0x420000 | 4 MB |
| storage (spiffs, unused) | 0x820000 | 7 MB |

Bootloader: the ESP-IDF build's `bootloader.bin` (rollback enabled), flashed with
`espflash flash --bootloader ...`; espflash's bundled bootloader lacks rollback support.

Sources: https://www.waveshare.com/wiki/ESP32-S3-Relay-6CH ,
https://files.waveshare.com/wiki/ESP32-S3-Relay-6CH/ESP32-S3-Relay-6CH.pdf (schematic).
