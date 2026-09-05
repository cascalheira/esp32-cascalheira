//! Pin map of the Waveshare/SpotPear ESP32-S3-Relay-6CH (confirmed from the schematic).

/// Relay driver GPIOs for CH1..CH6. Active-high; hardware pull-downs keep them off at boot.
pub const RELAY_GPIOS: [u8; super::CHANNELS] = [1, 2, 41, 42, 45, 46];
/// WS2812 RGB LED data pin.
pub const RGB_LED_GPIO: u8 = 38;
/// Passive buzzer (needs PWM).
pub const BUZZER_GPIO: u8 = 21;
/// BOOT button, usable as a user button after boot (active low).
pub const BUTTON_GPIO: u8 = 0;
/// RS485 UART1 pins; direction is handled in hardware.
pub const RS485_TX_GPIO: u8 = 17;
pub const RS485_RX_GPIO: u8 = 18;
