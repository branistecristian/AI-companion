// +---------------------------------------------------------------------------+
// |                             PM/MA lab skel                                |
// +---------------------------------------------------------------------------+

//! By default, this app prints a "Hello world" message with `defmt`.

#![no_std]
#![no_main]

use {defmt_rtt as _, panic_probe as _}; // for logging + panic handling

use embassy_executor::Spawner;
use embassy_rp::i2c::{self, I2c, InterruptHandler as I2CInterruptHandler, Config as I2cConfig};
use embassy_rp::peripherals::{PIN_10, PIN_11};
use embassy_time::{Timer, Duration};

use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyle},
    pixelcolor::BinaryColor,
    prelude::*,
    text::Text,
};
use ssd1306::{
    prelude::*,
    I2CDisplayInterface,
    Ssd1306,
    mode::BufferedGraphicsMode,
    rotation::DisplayRotation,
};

// Use the logging macros provided by defmt.
use defmt::*;

use embassy_rp::bind_interrupts;

use embedded_hal_async::i2c::{Error, I2c as _};
use embassy_rp::peripherals::I2C0;


#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Initialize the embassy runtime and peripherals.
    let peripherals = embassy_rp::init(Default::default());

    bind_interrupts!(struct Irqs {
        I2C0_IRQ => i2c::InterruptHandler<I2C0>;
    });

    let i2c = I2c::new_async(
        peripherals.I2C0,     // I2C0 
        peripherals.PIN_5,    // SCL = GP5
        peripherals.PIN_4,    // SDA = GP4
        Irqs,                 // <- this is important for async!
        I2cConfig::default(),
    );

    info!("Hello world!");

    let interface = I2CDisplayInterface::new(i2c);
    let mut display: Ssd1306<_, _, BufferedGraphicsMode<_>> = Ssd1306::new(
        interface,
        DisplaySize128x64,
        DisplayRotation::Rotate0,
    )
    .into_buffered_graphics_mode();

    display.init().unwrap();
    display.flush().unwrap();

    let text_style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    Text::new("Hello :)", Point::new(32, 32), text_style)
        .draw(&mut display)
        .unwrap();
    display.flush().unwrap();

    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}
