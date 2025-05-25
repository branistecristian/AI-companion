// +-------------------------------------------------------------------------------------+
// |                             PM PROJECT - AI COMPANION                               |
// +-------------------------------------------------------------------------------------+

//! By default, this app prints a "Hello world" message with `defmt`.

#![no_std]
#![no_main]
// Removed `#![feature(alloc_error_handler)]` as it is not supported on the stable channel.

// Adds stack and heap support
extern crate alloc;
use core::cell::UnsafeCell;

use alloc::{format, string::String};
use alloc_cortex_m::CortexMHeap;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_rp::peripherals::USB;
use cyw43::JoinOptions;
use embassy_lab_utils::{init_network_stack, init_wifi};
use embassy_rp::gpio::{Input, Pull};
use embassy_rp::pio::{Common, ShiftDirection, StateMachine};
use embassy_rp::usb::{Driver};
use embassy_usb::Builder;
use embedded_tls::{TlsConfig, TlsConnection, NoVerify, TlsContext};
use fixed::{traits::ToFixed, FixedU32};
use fixed::types::extra::U16;
use fixed_macro::types::U56F8;
use static_cell::StaticCell;


use defmt_rtt;
use panic_probe;

use embassy_executor::Spawner;

use embassy_net::{dns::DnsSocket, tcp::TcpSocket, Config, IpAddress, IpEndpoint, StackResources};
use embassy_net::Stack;
use embassy_rp::{pac::UART0, peripherals::PIO0, pio::{InterruptHandler, Pio}, uart::Uart};
use embassy_rp::i2c::{self, I2c, Config as I2cConfig};
use embassy_rp::peripherals::{PIN_10, PIN_11, PIO1};
use embassy_time::{Duration, Instant, Timer};
use embassy_rp::peripherals::UART0;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::{
    mono_font::ascii::FONT_10X20,
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
use defmt::{debug, error, info, warn};

use embassy_rp::bind_interrupts;

use embedded_hal_async::i2c::{Error, I2c as _};
use embassy_rp::peripherals::I2C0;

use embedded_graphics::{
    image::{ImageRaw, Image},
};
      // socket type lives in Embassy
use embedded_nal_async::{Dns, AddrType};  // trait + enum from nal-async
use embedded_tls::Aes128GcmSha256;
use embedded_io_async::{Read, Write};
use rand_core::RngCore;
use embassy_rp::pio::program::pio_asm;
use panic_probe as _;

struct DummyRng;
impl rand_core::CryptoRng for DummyRng {}

const SAMPLE_RATE: u32 = 16_000; // 16kHz
const BIT_DEPTH: u32 = 32;       // INMP441 outputs 24-bit, but aligned in 32 bits
const CHANNELS: u32 = 1;

impl RngCore for DummyRng {
    fn next_u32(&mut self) -> u32 {
        let t = Instant::now().as_ticks() as u32;
        0x12345678 ^ t            // XOR with current timer ticks
    }
    fn next_u64(&mut self) -> u64 {
        ((self.next_u32() as u64) << 32) | self.next_u32() as u64
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(4) {
            let r = self.next_u32().to_le_bytes();
            chunk.copy_from_slice(&r[..chunk.len()]);
        }
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}


fn build_dts33a_packet(text: &str) -> heapless::Vec<u8, 256> {
    let mut packet = heapless::Vec::<u8, 256>::new();

    // Header
    packet.push(0xFD).ok();                    // Start byte
    let len = 2 + text.len();                  // 2 = command + encoding, plus text bytes
    packet.push(((len >> 8) & 0xFF) as u8).ok(); // High byte
    packet.push((len & 0xFF) as u8).ok();        // Low byte

    // Command + encoding
    packet.push(0x01).ok(); // Speak command
    packet.push(0x04).ok(); // UTF-8

    // UTF-8 text
    for &b in text.as_bytes() {
        packet.push(b).ok();
    }

    packet
}

#[embassy_executor::task]
async fn i2s_input_task_test(mut sm: StateMachine<'static, PIO1, 2>) {
    sm.set_enable(true);
    loop {
        // try to pull — if there's nothing, warn
        match sm.rx().try_pull() {
            Some(word) => {
                defmt::info!("RAW_WORD = {=u32:032b}", word);
                let left  = (word >> 16) as u16;
                let right = (word       ) as u16;
                defmt::info!("L={}  R={}", left, right);
            }
            None => defmt::warn!("FIFO empty — no sample this cycle"),
        }
        // small delay so you don't spam too hard
        Timer::after(Duration::from_millis(5)).await;
    }
}

use embassy_sync::signal::Signal;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

static RECORD_DONE: Signal<CriticalSectionRawMutex, bool> = Signal::new();
use embassy_executor::{Executor};
use embassy_rp::multicore::{spawn_core1, Stack as CoreStack};
use embassy_rp::peripherals::CORE1;
static CORE1_STACK: StaticCell<CoreStack<16384>> = StaticCell::new();
// one executor and one stack for core-1
static EXECUTOR1:      StaticCell<Executor>  = StaticCell::new();
static mut CORE1_MEM:  CoreStack<16384>      = CoreStack::new();   // 16 kB is plenty

const SEC: usize      = 5;                 // recording length , 5 seconds
const FS:  usize      = 16_000;             // sample-rate
const N_SAMPLES: usize = FS * SEC;          // 160_000
const BYTES_PCM: usize = N_SAMPLES * 2;     // i16 mono  = 320 kB
const BYTES_WAV: usize = BYTES_PCM + 44; // WAV header + data

#[inline]                       // 24->16 bit signed, pick LEFT channel
fn word_to_i16(w: u32) -> i16 {
    // 1. throw away the 8 padding bits at the bottom
    // Strip the 8 padding bits the mic always appends
    let s24 = (w >> 8) & 0x00FF_FFFF;

    // Sign-extend 24 → 32 bit
    let s32 = ((s24 << 8) as i32) >> 8;

    // Down-shift to 16-bit PCM
    (s32 >> 8) as i16
}


use byteorder::{ByteOrder, LittleEndian};
use usb_device::device::UsbVidPid;

fn write_wav_header(buf: &mut [u8], pcm_len: u32, sample_rate: u32) {
    // RIFF
    buf[0..4].copy_from_slice(b"RIFF");
    LittleEndian::write_u32(&mut buf[4..8], pcm_len + 36);
    buf[8..12].copy_from_slice(b"WAVE");

    // fmt  sub-chunk
    buf[12..16].copy_from_slice(b"fmt ");
    LittleEndian::write_u32(&mut buf[16..20], 16);          // chunk size
    LittleEndian::write_u16(&mut buf[20..22], 1);           // PCM
    LittleEndian::write_u16(&mut buf[22..24], 1);           // mono
    LittleEndian::write_u32(&mut buf[24..28], sample_rate); // sample rate
    LittleEndian::write_u32(&mut buf[28..32], sample_rate * 2); // byte rate (16-bit mono)
    LittleEndian::write_u16(&mut buf[32..34], 2);           // block align
    LittleEndian::write_u16(&mut buf[34..36], 16);          // bits / sample

    // data  sub-chunk
    buf[36..40].copy_from_slice(b"data");
    LittleEndian::write_u32(&mut buf[40..44], pcm_len);
}

pub struct WavBuf(UnsafeCell<[u8; BYTES_WAV]>);
unsafe impl Sync for WavBuf {}

#[unsafe(link_section = ".uninit")]                // optional: place in .uninit to save flash
// static mut WAV_BUF: [u8; BYTES_WAV] = [0; BYTES_WAV];
static WAV_BUF: WavBuf = WavBuf(UnsafeCell::new([0; BYTES_WAV]));
#[embassy_executor::task]
async fn record_5s(mut sm: StateMachine<'static, PIO1, 2>) {
    defmt::info!("▶️  record_5s started!");
    
    #[allow(static_mut_refs)]
    let buf: &'static mut [u8; BYTES_WAV] = unsafe { &mut *WAV_BUF.0.get() };

    write_wav_header(buf, BYTES_PCM as u32, SAMPLE_RATE);

    let mut idx = 44;                // skip WAV header
    sm.set_enable(true);

    while idx < BYTES_WAV {
        if let Some(word) = sm.rx().try_pull() {
            // defmt::info!("raw = {=u32:032b}", word);
            // let ones  = word.count_ones();
            // let zeros = 32 - ones;
            // defmt::info!("ones {}, zeros {}", ones, zeros);
            // 24-bit sample → i16, left channel only
            let s = word_to_i16(word);

            buf[idx]     = (s & 0xFF) as u8;
            buf[idx + 1] = (s >> 8)  as u8;
            idx += 2;
        } else {
            // ①  give the executor a chance to run other tasks
            Timer::after_micros(50).await;
        }
        if idx < 2048 {   // first 1 024 samples
            if idx % 256 == 0 {
                let peak = buf[44..idx]
                .chunks_exact(2)
                .map(|b| {
                    let s = i16::from_le_bytes([b[0], b[1]]);
                    (s as i32).abs() as u32
                })
                .max()
                .unwrap_or(0);
                defmt::info!("peak = {}", peak);
            }
        }
        // ②  slow the loop very slightly so the PIO FIFO doesn’t overflow
        if idx % 1024 == 0 {
            Timer::after_micros(200).await;   // ≈12 k samples/s budget
        }
    }

    defmt::info!("🎙️  captured {} samples", N_SAMPLES);
    RECORD_DONE.signal(true);           // notify when buffer is full
}

/// --- constants ------------------------------------------------------------

const WHISPER_PORT: u16  = 443;
const WHISPER_MODEL: &str = "whisper-1";

/// --- upload task ----------------------------------------------------------
#[embassy_executor::task]
async fn upload_to_whisper(stack: &'static Stack<'static>) {
    RECORD_DONE.wait().await;           // wait until recorder finished
    info!("🔔 recorder finished – starting upload");
    // 1. resolve + TLS socket ------------------------------------------------
    let mut dns = DnsSocket::new(*stack);
    let ip = match dns.get_host_by_name("api.openai.com", AddrType::IPv4).await {
        Ok(core::net::IpAddr::V4(v4)) => v4,
        _ => { defmt::error!("DNS failed"); return; }
    };
    info!("🌍 api.openai.com -> {}", ip);
    let endpoint = IpEndpoint::new(IpAddress::Ipv4(ip.octets().into()), WHISPER_PORT);
    
    let mut rx_buf = [0u8; 8192];
    let mut tx_buf = [0u8; 8192];
    let mut tcp = TcpSocket::new(*stack, &mut rx_buf, &mut tx_buf);
    tcp.connect(endpoint).await.unwrap();
    info!("🔌 TCP connected");
    let mut tls_in  = [0u8; 8192];
    let mut tls_out = [0u8; 8192];
    let cfg: TlsConfig<'_, Aes128GcmSha256> = TlsConfig::new().with_server_name("api.openai.com");
    let mut tls = TlsConnection::new(tcp, &mut tls_in, &mut tls_out);
    let mut rng = DummyRng;
    tls.open::<_, NoVerify>(TlsContext::new(&cfg, &mut rng)).await.unwrap();
    info!("🔒 TLS ready");
    // 2. multipart form header ----------------------------------------------
    const BOUNDARY: &str = "----embassyBoundary";
    let pre  = format!(
        "--{b}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"mic.wav\"\r\n\
         Content-Type: audio/wav\r\n\r\n",
        b = BOUNDARY);
    let mid  = format!(
        "\r\n--{b}\r\n\
         Content-Disposition: form-data; name=\"model\"\r\n\r\n\
         {model}\r\n\
         --{b}--\r\n",
        b = BOUNDARY, model = WHISPER_MODEL);

    // content-length = pre + wav + mid
    let total_len = pre.len()     as u32 +
                    BYTES_WAV     as u32 +
                    mid.len()     as u32;

    // 3. HTTP request line + headers ----------------------------------------
    tls.write_all(format!(
        "POST /v1/audio/transcriptions HTTP/1.1\r\n\
         Host: {host}\r\n\
         Authorization: Bearer {api}\r\n\
         Content-Type: multipart/form-data; boundary={b}\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\r\n",
        host = "api.openai.com",
        api  = "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",
        b    = BOUNDARY,
        len  = total_len).as_bytes()).await.unwrap();

    // 4. send multipart body -------------------------------------------------
    // send the header
    tls.write_all(pre.as_bytes()).await.unwrap();

    // stream the wav  (any chunk size ≤ tls_out.len() works)
    for chunk in unsafe { &*WAV_BUF.0.get() }.chunks(1024) {
        tls.write_all(chunk).await.unwrap();     // never larger than 1 KiB
    }

    // then the footer
    tls.write_all(mid.as_bytes()).await.unwrap();
    tls.flush().await.unwrap();
    info!("📤 upload finished – waiting for reply");
    // 5. read the reply ------------------------------------------------------
    let mut out = heapless::String::<2048>::new();
    let mut buf = [0u8; 256];
    while let Ok(n) = tls.read(&mut buf).await {
        if n == 0 { break; }
        if let Ok(chunk) = core::str::from_utf8(&buf[..n]) {
            out.push_str(chunk).ok();
        }
    }
    info!("📥 got {} bytes", out.len());
    match out.find("\r\n\r\n") {
        Some(off) => {
            let body = &out[off+4..];
            for chunk in body.as_bytes().chunks(128) {
                info!("json: {}",
                      core::str::from_utf8(chunk).unwrap_or("�"));
            }
        }
        None => warn!("no CRLF-CRLF delimiter – raw:\n{}", out.as_str()),
    }

    // crude JSON parse – look for `"text":"..."`
    // if let Some(idx) = out.find(r#""text":"#) {
    //     let rest = &out[idx + 8..];
    //     if let Some(end) = rest.find('"') {
    //         defmt::info!("📝 Whisper: {}", &rest[..end]);
    //     }
    // } else {
    //     defmt::warn!("No transcript in reply:\n{}", out.as_str());
    // }
}


async fn send_chatgpt_request(stack: &'static Stack<'static>, uart: &mut Uart<'_, UART0, embassy_rp::uart::Async>){
    // defmt::info!("🌐 Connecting to OpenAI...");

    // // DNS resolve first
    // let mut dns_socket = DnsSocket::new(*stack);
    // // 0.8 signature: &str, AddrType, returns core::net::IpAddr
    // let ip = match dns_socket
    //             .get_host_by_name("api.openai.com", AddrType::IPv4)
    //             .await
    // {
    //     Ok(core::net::IpAddr::V4(v4)) => v4,
    //     Ok(_) => {
    //         defmt::error!("Resolved to non-IPv4 address, not supported.");
    //         return;
    //     }
    //     Err(e) => {
    //         defmt::error!("DNS resolution failed: {:?}", e);
    //         return;
    //     }
    // };

    // let resolved_ip = IpAddress::Ipv4(ip.octets().into());
    // defmt::info!("🌐 Resolved api.openai.com to {}", resolved_ip);

    // let mut rx_buf = [0; 4096];
    // let mut tx_buf = [0; 4096];
    // let mut socket = TcpSocket::new(*stack, &mut rx_buf, &mut tx_buf);

    // let endpoint = IpEndpoint::new(resolved_ip, 443); // OpenAI IP
    // if let Err(e) = socket.connect(endpoint).await {
    //     defmt::error!("❌ TCP connect failed: {:?}", e);
    //     return;
    // }

    // // Wrap in TLS
    // let mut read_buf = [0u8; 4096];
    // let mut write_buf = [0u8; 4096];
    // let tls_config: TlsConfig<'_, Aes128GcmSha256> = TlsConfig::new().with_server_name("api.openai.com");
    // let mut tls = TlsConnection::new(socket, &mut read_buf, &mut write_buf);
    // let mut rng = DummyRng;
    // let context = TlsContext::new(&tls_config, &mut rng);

    // if let Err(e) = tls.open::<_, NoVerify>(context).await {
    //     defmt::error!("TLS error occurred");
    //     return;
    // }

    // defmt::info!("🔒 TLS ready. Sending request...");

    // let api_key = "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";

    // let body = r#"{"model":"gpt-3.5-turbo","messages":[{"role":"user","content":"HEY BRO I MADE THE PROJECT TO SPEAK WHAT DO U THINK ABOUT IT? BE SHORT IN ANSWER BTW"}]}"#;
    // let request = format!(
    //     "POST /v1/chat/completions HTTP/1.1\r\n\
    //     Host: api.openai.com\r\n\
    //     Authorization: Bearer {}\r\n\
    //     Content-Type: application/json\r\n\
    //     Content-Length: {}\r\n\
    //     Connection: close\r\n\r\n\
    //     {}",
    //     api_key,
    //     body.as_bytes().len(),
    //     body
    // );

    // defmt::info!("🧾 Full Request:\n{}", request.as_str());

    // defmt::info!("📤 Request sent. Reading response...");

    // if let Err(e) = tls.write_all(request.as_bytes()).await 
    // { 
    //     defmt::error!("TLS write error: {}", defmt::Debug2Format(&e));
    //     return; } 
    // if let Err(e) = tls.flush().await { 
    //     defmt::error!("TLS flush error: {}", defmt::Debug2Format(&e)); 
    //     return; 
    // }
    
    // let mut full_response = String::new();
    // let mut buf = [0u8; 1024];
    // loop {
    //     match tls.read(&mut buf).await {
    //         Ok(0) | Err(embedded_tls::TlsError::ConnectionClosed) => break,
    //         Err(e) => { defmt::error!("TLS: {:?}", defmt::Debug2Format(&e)); break; }
    //         Ok(n) => {
    //             let chunk = core::str::from_utf8(&buf[..n]).unwrap_or("[Invalid UTF-8]");
    //             defmt::debug!("Received {}", chunk);
    //             full_response.push_str(chunk);
    //             if let Some(start) = full_response.find(r#""content": "#) {
    //                 let s = &full_response[start + 12..];

    //                 let mut out = String::new();
    //                 let mut chars = s.chars();
    //                 let mut escape = false;

    //                 while let Some(c) = chars.next() {
    //                     if escape {
    //                         match c {
    //                             '"' => out.push('"'),
    //                             'n' => out.push('\n'),
    //                             't' => out.push('\t'),
    //                             '\\' => out.push('\\'),
    //                             _ => out.push(c),
    //                         }
    //                         escape = false;
    //                     } else if c == '\\' {
    //                         escape = true;
    //                     } else if c == '"' {
    //                         break; // End of JSON string
    //                     } else {
    //                         out.push(c);
    //                     }
    //                 }

    //                 defmt::info!("💬 GPT says: {}", out.as_str());

    //                 let packet = build_dts33a_packet(&out);
    //                 uart.write(&packet).await.unwrap();

    //                 Timer::after(Duration::from_millis(100)).await;

    //                 break;
    //             }
    //         }
    //     }
    // }

    defmt::info!("✅ Done.");
}

bind_interrupts!(struct Irqs {
        I2C0_IRQ => i2c::InterruptHandler<I2C0>;
        ADC_IRQ_FIFO => embassy_rp::adc::InterruptHandler;
        UART0_IRQ => embassy_rp::uart::InterruptHandler<UART0>;
        PIO1_IRQ_0 => InterruptHandler<PIO1>;
        USBCTRL_IRQ => embassy_rp::usb::InterruptHandler<USB>;
        });

#[global_allocator]
static HEAP: CortexMHeap = CortexMHeap::empty();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    unsafe { HEAP.init(cortex_m_rt::heap_start() as usize, 16 * 1024); }
    // Initialize the embassy runtime and peripherals.
    let mut peripherals = embassy_rp::init(Default::default());

    let usb_driver = Driver::new(peripherals.USB, Irqs);

     // Initialize the network stack
    let (net_device, mut control) = init_wifi!(&spawner, peripherals).await;

    control.join("iPhone", JoinOptions::new("braniste22".as_bytes())).await.unwrap();

    let config = Config::dhcpv4(Default::default());
    static RESOURCES: StaticCell<StackResources<8>> = StaticCell::new();
    static STACK: StaticCell<Stack<'static>> = StaticCell::new();
    let stack = STACK.init_with(|| init_network_stack(&spawner, net_device, &RESOURCES, config));
    while !stack.is_config_up() {
        Timer::after_millis(100).await;
    }
    defmt::info!("📡 Network is up!");

    let ip = stack.config_v4().unwrap().address;
    defmt::info!("🌍 My IP is: {}", ip);

    static PIO1_CELL: StaticCell<PIO1> = StaticCell::new();

    let pio1 = peripherals.PIO1;
    let pio1_static = PIO1_CELL.init(pio1);

    let Pio { mut common, mut sm0, mut sm1, mut sm2, mut sm3,.. } = Pio::new(pio1_static, Irqs);
        // I2S mic pin config:
        // WS (word select, aka L/R clock) -> GP11
        // SCK (bit clock)                 -> GP10
        // SD (serial data)               -> GP12

        // pins: adjust to your wiring
    let mut sck = peripherals.PIN_10;   // bit clock
    let mut ws  = peripherals.PIN_11;   // word select
    let mut sd  = peripherals.PIN_12;   // data
    
    {
    let clk = Input::new(&mut sck, Pull::None);
    let ws = Input::new(&mut ws, Pull::None);
    let sd = Input::new(&mut sd, Pull::None);
    defmt::info!("CLK={}, WS={}, SD={}", clk.is_high(), ws.is_high(), sd.is_high());
    } // Input pins are dropped here

    let pio_sck = common.make_pio_pin(sck);
    let pio_ws  = common.make_pio_pin(ws);
    let pio_sd  = common.make_pio_pin(sd);

    let prg_sck = pio_asm!(
        ".wrap_target",
            "set    pindirs, 1", // ASTA ERA BAGA MI AS PL
            "set pins, 1 [0]",   //  ─┐ HIGH 1 cycle
            "set pins, 0 [0]",   //  ─┘ LOW  1 cycle   → 50 % duty-cycle clock
        ".wrap"
    );

        /* 64 SCKs per audio frame: 32 high (right), 32 low (left) */
    let prg_ws  = pio_asm!(
        ".wrap_target",
            "set    pindirs, 1", // ASTA ERA BAGA MI AS PL
            "set pins, 0",       // Left channel
            "set y, 31",         // 32 SCK pulses
        "rloop:",
            "nop     [0]",
            "jmp y-- rloop",

            "set pins, 1",       // Right channel
            "set y, 31",
        "lloop:",
            "nop     [0]",
            "jmp y-- lloop",
        ".wrap"
    );
        
    let prg_sd  = pio_asm!(
        ".wrap_target",
            "wait 0 pin 0",      // wait for SCK ↓ edge  (pin 0 is the JMP-pin we’ll set)
            "in   pins, 1",      // sample SD
        ".wrap"
    );

    // ── Host-side setup ─────────────────────────────────────────────────────────────
    let loaded_sck = common.load_program(&prg_sck.program);
    let loaded_ws  = common.load_program(&prg_ws .program);
    let loaded_sd  = common.load_program(&prg_sd .program);

    // ❶ SCK  ────────────────────────────────────────────────────────────────────────
    let mut cfg = embassy_rp::pio::Config::default();
    cfg.use_program(&loaded_sck, &[]);
    cfg.set_set_pins (&[&pio_sck]);
    cfg.set_out_pins(&[&pio_sck]);       // keep OUT + SET same base pin
    cfg.clock_divider = U56F8!(61.035).to_fixed(); // 1 MHz ≈ 16 kHz × 64
    sm0.set_config(&cfg);
    sm0.set_enable(true);

    // ❷ WS   ────────────────────────────────────────────────────────────────────────
    let mut cfg = embassy_rp::pio::Config::default();
    cfg.use_program(&loaded_ws, &[]);
    cfg.set_set_pins (&[&pio_ws]);
    cfg.set_out_pins(&[&pio_ws]);
    cfg.clock_divider = U56F8!(61.035).to_fixed();
    sm1.set_config(&cfg);
    sm1.set_enable(true);

    // ❸ SD   ────────────────────────────────────────────────────────────────────────
    let mut cfg = embassy_rp::pio::Config::default();
    cfg.use_program(&loaded_sd, &[]);
    cfg.set_in_pins (&[&pio_sd]);
    cfg.set_jmp_pin (&pio_sck);       // pin 0 inside the program
    cfg.shift_in.auto_fill = true;
    cfg.shift_in.direction = ShiftDirection::Left;
    cfg.shift_in.threshold = 32;
    cfg.clock_divider = U56F8!(61.035).to_fixed(); // sample on every SCK
    sm2.set_config(&cfg);
    sm2.set_enable(true);

    Timer::after(Duration::from_millis(200)).await;
    // spawner.spawn(i2s_input_task_test(sm2)).unwrap();
    defmt::info!("Start recording...");
    let core1 = peripherals.CORE1;  
    let core1_stack = CORE1_STACK.init(CoreStack::new());
    // ---- launch core-1 ----
    let sm2_for_core1 = sm2; 
    unsafe {
        spawn_core1(core1,
            core1_stack,
            move || {
                defmt::info!("🟢 Core 1 booted");
                let exec = EXECUTOR1.init(Executor::new());
                exec.run(|sp| {
                    defmt::info!("🟢 Core 1 executor ready");
                    // use the moved handle here
                    sp.spawn(record_5s(sm2_for_core1)).unwrap();
                });
            });
    }
    
    RECORD_DONE.wait().await;

    // --------------------------------- DEBUG ---------------------------------

    // 2) prepare your USB descriptors and state
    static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESCRIPTOR: StaticCell<[u8; 64]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    static CDC_STATE: StaticCell<State> = StaticCell::new();

    // 3) set up the device‐level config
    let mut config = embassy_usb::Config::new(
        0xc0de,      // VID (vendor ID)
        0xcafe,      // PID (product ID)
    );
    config.manufacturer   = Some("Embassy");
    config.product        = Some("CDC-ACM Example");
    config.serial_number = Some("DEADBEEF");

    // 4) create the Builder
    let mut builder = Builder::new(
        usb_driver,
        config,
        CONFIG_DESCRIPTOR.init([0; 256]),
        BOS_DESCRIPTOR.init([0; 64]),
        &mut [],                       // no MSOS in this example
        CONTROL_BUF.init([0; 64]),
    );
    // 5) add the CDC-ACM class
    let mut cdc = CdcAcmClass::new(&mut builder, CDC_STATE.init(State::new()), 64);

    // 6) build and run
    let mut usb = builder.build();
    #[embassy_executor::task]
    async fn usb_task(mut usb: embassy_usb::UsbDevice<'static, Driver<'static, USB>>) {
        usb.run().await;
    }

    spawner.spawn(usb_task(usb)).unwrap();

        // block until the host opens the COM port
    while !cdc.dtr() {
        Timer::after(Duration::from_millis(10)).await;
    }

    let wav: &[u8] = unsafe { &*WAV_BUF.0.get() };
    let mut offset = 0;
    while offset < wav.len() {
        if let Ok(_) = cdc.write_packet(&wav[offset..]).await {
            offset += 64; // Use the packet size (64 bytes) as the increment
        } else {
            break;
        }
    }


    // --------------------------------- DEBUG ---------------------------------
    
    // spawner.spawn(upload_to_whisper(stack)).unwrap();

    let mut uart_config = embassy_rp::uart::Config::default();
    uart_config.baudrate = 115200;

    let mut uart = Uart::new(
        peripherals.UART0,
        peripherals.PIN_16,     // TX
        peripherals.PIN_17,     // RX
        Irqs,
        peripherals.DMA_CH0,   // TX DMA channel
        peripherals.DMA_CH1,   // RX DMA channel
        uart_config            
    );

    send_chatgpt_request(stack, &mut uart).await;

    let i2c = I2c::new_async(
        peripherals.I2C0,     // I2C0 
        peripherals.PIN_5,    // SCL = GP5
        peripherals.PIN_4,    // SDA = GP4
        Irqs,                 // <- this is important for async!
        I2cConfig::default(),
    );

    // DEBUG
    info!("Hello world!");

    // -- I2C setup FOR OLED
    let interface = I2CDisplayInterface::new(i2c);
    let mut display: Ssd1306<_, _, BufferedGraphicsMode<_>> = Ssd1306::new(
        interface,
        DisplaySize128x64,
        DisplayRotation::Rotate0,
    )
    .into_buffered_graphics_mode();

    display.init().unwrap();
    display.flush().unwrap();

    let text_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    Text::new("Hello :)", Point::new(32, 32), text_style)
        .draw(&mut display)
        .unwrap();
    display.flush().unwrap();
    // -- I2C setup FOR OLED

    loop {
        // blink it on
        display.clear(BinaryColor::Off).unwrap();
    
        // Show smiley as an emoji
        let text_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
        Text::new("MOOD: :)", Point::new(10, 50), text_style)
            .draw(&mut display)
            .unwrap();

        // let raw_image = ImageRaw::<BinaryColor>::new(SMILEY, 16);
        // let image = Image::new(&raw_image, Point::new(60, 24));
        // image.draw(&mut display).unwrap();
    
        display.flush().unwrap();
        Timer::after(Duration::from_millis(500)).await;
    
        // blink it off
        display.clear(BinaryColor::Off).unwrap();
        display.flush().unwrap();
        Timer::after(Duration::from_millis(200)).await;
    }
}
