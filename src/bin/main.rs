#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use embassy_executor::Spawner;
//use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::{dma_buffers, ram};
use esp_hal::timer::systimer::SystemTimer;
use esp_hal::i2s::master::{DataFormat, I2s, Standard};
use esp_hal::time::Rate;
use threepm::easy_mode::{EasyMode, EasyModeErr};
use bytemuck::cast_slice;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

static MP3: &[u8] = include_bytes!("../../assets/test.mp3");
const CHUNK_SZ: usize = 512;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[ram]
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

    let mut easy = EasyMode::new();
    let mp3_loader = &mut MP3.chunks(CHUNK_SZ);

    let mut decode_buf = [0i16; 4096];
    let mut data_len = 0;

    // skip past the id3 tags and anything else up to the first mp3 sync tag
    while !easy.mp3_decode_ready() && easy.buffer_free() >= CHUNK_SZ {
        if let Some(mp3data) = mp3_loader.next() {
            easy.add_data(mp3data);
        } else {
            log::error!("Out of data!");
            break;
        }
    }

    // Move our decode window up to the next sync word in the stream
    let syncd = easy.skip_to_next_sync_word();
    log::info!("Synced: {syncd}");

    // We're past the header now, so we should be able to correctly decode an MP3 frame
    // Metadata is stored in every frame, so check that now:
    if let Ok(frame) = easy.mp3_info() {
        log::info!("First MP3 frame info: {:?}", frame);
    }

    let mut transfer = i2s_tx.write_dma_circular(tx_buffer).unwrap();
    log::info!("Into main loop!");
    loop {
        let avail = transfer.available().unwrap();
        if data_len > 0 && avail > data_len * 2 {
            if transfer.push(cast_slice(&decode_buf[..data_len])).is_err() {
                log::error!("Failed to push data to transfer buffer");
            }
            data_len = 0;
        }

        if data_len == 0 {
            // Fill decoder buffer with data
            if easy.buffer_free() >= CHUNK_SZ {
                if let Some(mp3data) = mp3_loader.next() {
                    easy.add_data(mp3data);
                } else {
                    log::warn!("Out of data while filling decoder buffer!");
                    break;
                }
            }

            // Decode the frame
            match easy.decode(&mut decode_buf) {
                Ok(samples) => {
                    data_len = samples;
                },
                Err(e) => {
                    log::error!("Failed to decode MP3 frame: {:?}", e);
                }
            }
        }
    }
    transfer.stop().unwrap();
    panic!("Done! Exiting main loop.");
}
