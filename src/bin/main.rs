#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::dma_buffers;
use esp_hal::timer::systimer::SystemTimer;
use esp_hal::i2s::master::{DataFormat, I2s, Standard};
use esp_hal::time::Rate;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

static WAVE: &[u8] = include_bytes!("../../assets/test.raw");

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal_embassy::main]
async fn main(_spawner: Spawner) {
    esp_println::logger::init_logger_from_env();

    log::info!("Main function started");
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let timer0 = SystemTimer::new(peripherals.SYSTIMER);
    esp_hal_embassy::init(timer0.alarm0);

    let dma_channel = peripherals.DMA_CH0;
    let (_, _, tx_buffer, tx_descriptors) = dma_buffers!(4 * 4096, 4 * 4096);

    log::info!("Creating I2S instance");

    let i2s = I2s::new(
        peripherals.I2S0,
        Standard::Philips,
        DataFormat::Data16Channel16,
        Rate::from_hz(44100),
        dma_channel
    );

    log::info!("Configuring I2S instance");

    let i2s = i2s.with_mclk(peripherals.GPIO9);
    let mut i2s_tx = i2s
        .i2s_tx
        .with_bclk(peripherals.GPIO6)
        .with_ws(peripherals.GPIO8)
        .with_dout(peripherals.GPIO7)
        .build(tx_descriptors);

    log::info!("I2S instance configured");

    let mut transfer = i2s_tx.write_dma_circular(tx_buffer).unwrap();
    let mut idx= 0;
    log::info!("Into main loop!");
    loop {
        let mut avail = transfer.available().unwrap();
        if avail > 0 {
            //log::info!("Avail: {}", avail);
            if idx + avail >= WAVE.len() {
                //log::info!("Wrote {} bytes", WAVE.len() - idx);
                //log::info!("Writing {} bytes", WAVE[idx..].len());
                //transfer.push(&WAVE[idx..]).unwrap();
                //avail = avail - (WAVE.len() - idx);
                idx = 0;
            }
            //log::info!("idx {}", idx);
            //log::info!("Writing {} bytes", avail);
            transfer.push(&WAVE[idx..idx + avail]).unwrap();
            //log::info!("Wrote {} bytes", avail);
            idx = idx + avail;
            if idx >= WAVE.len() {
                idx = 0;
            }
        }
    }
}
