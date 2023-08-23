# Kiomet+ by Sheumais

[![Build](https://github.com/sheumais/kiomet-plus/actions/workflows/build.yml/badge.svg)](https://github.com/sheumais/kiomet-plus/actions/workflows/build.yml)
<a href='https://discord.gg/YMheuFQWTX'>
  <img src='https://img.shields.io/badge/Kiomet.com-%23announcements-blue.svg' alt='Kiomet.com Discord' />
</a>

![Logo](/assets/branding/512x340.jpg)

[Kiomet.com](https://kiomet.com) is an online multiplayer real-time strategy game. Command your forces wisely and prepare for intense battles!

## Build Instructions

0. Install `rustup` ([see instructions here](https://rustup.rs/))
1. Install Rust Nightly and the WebAssembly target

```console
rustup install nightly-2023-04-25
rustup default nightly-2023-04-25
rustup target add wasm32-unknown-unknown
```

2. Install `trunk` (`cargo install --locked trunk --version 0.15.0`, install `gcc` first if it complains about missing `cc`)
3. `trunk build --release` in `/client`
4. `cargo run --release` in `/server`

## HTTPS

If you build the server in `--release` mode, it will force HTTPS using a self-signed certificate.

Optionally, specify `--certificate-path` and `--private-key-path` to use a trusted CA certificate (e.g. acquired via [Let's Encrypt](https://letsencrypt.org/)). The server will periodically check for and load renewed certificates.

## Official Server(s)

To avoid potential visibility-cheating, you are prohibited from using the open-source
client to play on official Kiomet server(s).