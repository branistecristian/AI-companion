# Microprocessor architecture (MA/PM) Lab

![PMRust Lab logo](https://gitlab.cs.pub.ro/pmrust/pmrust.pages.upb.ro/-/raw/main/website/static/img/logo.svg?ref_type=heads)

This repository contains the lab skeleton for the **MA/PM courses**.

## Project structure

```txt
.
├── Cargo.toml
├── README.md
├── build.rs
├── cyw43-firmware
│   ├── 43439A0.bin
│   ├── 43439A0_btfw.bin
│   ├── 43439A0_clm.bin
│   ├── LICENSE-permissive-binary-license-1.0.txt
│   └── README.md
├── embassy-lab-utils
│   ├── Cargo.toml
│   └── src
│       ├── lib.rs
│       └── wifi.rs
├── memory.x
├── rust-toolchain.toml
└── src
    ├── irqs.rs
    └── main.rs
```

- `src` contains the source code for your lab.
    - `main.rs` contains the source code for the application. By default, it blinks the Pico's LED.
    - `irqs.rs` contains the definition of the `Irqs` struct that binds the interrupt ids to their handlers.
- `embassy-lab-utils` is a helper crate that contains functions and types that act as wrappers over `embassy`'s
and `embedded-hal` more complex routines.
- `cyw43-firmware` contains the firmware for the WiFi and BLE chip. It is already loaded by the `init_wifi` function
from the `embassy-lab-utils` crate.

To better understand how Rust Embedded projects are set up, please read the [debug lab](https://pmrust.pages.upb.ro/docs/acs_cc/lab/01).

## How to build and run

Make sure you have all prerequisites installed properly, as shown in the [lab tutorial](https://pmrust.pages.upb.ro/docs/acs_cc/tutorials/embassy).

The project can be built for the `thumbv8m.main-none-eabihf` target, by running in the project's root:

```shell
cargo build
```

This project is set up to automatically run `probe-rs` in order to flash the lab's board, you can simply run in the project's root:

```shell
cargo run
```
