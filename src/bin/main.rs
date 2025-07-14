#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use embassy_executor::Spawner;
use esp_hal::clock::CpuClock;
use esp_hal::{dma_buffers, ram};
use esp_hal::timer::systimer::SystemTimer;
use esp_hal::i2s::master::{DataFormat, I2s, Standard};
use esp_hal::spi;
use esp_hal::time::Rate;
use threepm::easy_mode::EasyMode;
use bytemuck::cast_slice;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Output, OutputConfig, Level};


use embedded_sdmmc::{SdCard, VolumeManager, ShortFileName, VolumeIdx, Directory, Mode};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

//static MP3: &[u8] = include_bytes!("../../assets/test.mp3");
const CHUNK_SZ: usize = 512;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

pub struct TimeSource;

impl embedded_sdmmc::TimeSource for TimeSource {
    fn get_timestamp(&self) -> embedded_sdmmc::Timestamp {
        embedded_sdmmc::Timestamp {
            year_since_1970: 0,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

struct Error;
type MyDirectory<'a, 'b> =  Directory<'a, SdCard<embedded_hal_bus::spi::ExclusiveDevice<spi::master::Spi<'b, esp_hal::Blocking>, Output<'b>, embedded_hal_bus::spi::NoDelay>, Delay>, TimeSource, 4, 4, 1>;

fn list_dir(directory: MyDirectory<'_, '_>, path: &str) -> Result<(), Error> {
    log::info!("Listing {path}");
    directory.iterate_dir(|entry| {
        log::info!(
            "{:12} {:9} {} {}",
            entry.name,
            entry.size,
            entry.mtime,
            if entry.attributes.is_directory() {
                "<DIR>"
            } else {
                ""
            }
        );
        if entry.attributes.is_directory()
            && entry.name != ShortFileName::parent_dir()
            && entry.name != ShortFileName::this_dir()
        {
            //children.push(entry.name.clone());
        }
    }).unwrap();
    Ok(())
}

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
    #[allow(clippy::manual_div_ceil)]
    let (_, _, tx_buffer, tx_descriptors) = dma_buffers!(4 * 4096, 4 * 4096);

    let mosi = peripherals.GPIO4;
    let miso = peripherals.GPIO2;
    let sck = peripherals.GPIO3;
    let cs = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());

    let sdmmc_spi = spi::master::Spi::new(
        peripherals.SPI2,
        spi::master::Config::default()
            .with_frequency(Rate::from_khz(14000))
            .with_mode(spi::Mode::_0),
    )
    .unwrap()
    .with_mosi(mosi)
    .with_miso(miso)
    .with_sck(sck);

    let delay = Delay::new();
    let exclusive_spi = embedded_hal_bus::spi::ExclusiveDevice::new_no_delay(sdmmc_spi, cs).unwrap();
    let sdcard = SdCard::new(exclusive_spi, delay);
    // Get the card size (this also triggers card initialisation because it's not been done yet)
    log::info!("Card size is {} bytes", sdcard.num_bytes().unwrap());
    // Now let's look for volumes (also known as partitions) on our block device.
    // To do this we need a Volume Manager. It will take ownership of the block device.
    let volume_mgr = VolumeManager::new(sdcard, TimeSource);
    // Try and access Volume 0 (i.e. the first partition).
    // The volume object holds information about the filesystem on that volume.
    let volume0 = volume_mgr.open_volume(VolumeIdx(0)).unwrap();
    log::info!("Volume 0: {volume0:?}");
    // Open the root directory (mutably borrows from the volume).
    let root_dir = volume0.open_root_dir().unwrap();
    // List files in the root directory.
    let _ = list_dir(root_dir, "/");

    let root_dir = volume0.open_root_dir().unwrap();

    let filename = "PAULMC~1.MP3";
    let f = root_dir.open_file_in_dir(filename, Mode::ReadOnly).unwrap();

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

    let mut readbuf = [0u8; CHUNK_SZ];
    let mut decode_buf = [0i16; 4096];
    let mut data_len = 0;

    // skip past the id3 tags and anything else up to the first mp3 sync tag
    while !easy.mp3_decode_ready() && easy.buffer_free() >= CHUNK_SZ {
        if !f.is_eof() {
            let len = f.read(&mut readbuf).unwrap();
            log::info!("Read {len} bytes from file");
            easy.add_data(&readbuf);
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
        log::info!("First MP3 frame info: {frame:?}");
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
                if !f.is_eof() {
                    let len = f.read(&mut readbuf).unwrap();
                    log::info!("Read {len} bytes from file");
                    easy.add_data(&readbuf);
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
                    log::error!("Failed to decode MP3 frame: {e:?}");
                }
            }
        }
    }
    transfer.stop().unwrap();
    panic!("Done! Exiting main loop.");
}
