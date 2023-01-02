<p align="center">
  <img width="300" height="300" src="./logo.svg">
</p>

# LMC - A Lightweight MQTT Client for Rust

LMC is a MQTT client implementation designed to be small (the "active" code fits in less than 1k lines, including documentation), minimize
memory allocations, use as few crates as possible, and be asynchronous (through [tokio](https://crates.io/crates/tokio)). The project
is under the [MIT](LICENSE-MIT) license or the [apache](LICENSE-APACHE) license, whichever you prefer.

There are a few existing MQTT client crates out there but most of them are wrappers for a C/C++ library, and those which are pure Rust have
a basics APIs that don't allow you to, for instance, await the server's response when subscribing to a topic. This crate gives you fine
control over all of its aspects, but also provides _no-headaches_, _easy-to-use_ functions if all you need is an MQTT client with no concerns
for performance.

## Example use

First, add the crate to your dependencies in `Cargo.toml`:

```toml
[dependencies.lmc]
version = "^0.1"
features = ["tls"] # See below for available features
```

Then, you can use the following code to get started. Note that this example assumes the following:

 - Your broker is hosted on your local machine
 - Your broker uses TLS on port 8883
 - Your broker's certificate was signed by one of the Certificate Authorities in your system's CA store
 - You can log-in using the username `username` and password `password`
 - You can subscribe and publish to the `my_topic` topic

```rust
use lmc::{Options, Client, QoS};

#[tokio::main]
async fn main()
{
    //You can disable TLS by skipping `.enable_tls().unwrap()`
    let mut opts = Options::new("client_id").enable_tls().unwrap();
    opts.set_username("username").set_password(b"password");

    //Connect to localhost using the default TLS port (8883)
    let (client, shutdown_handle) = Client::connect("localhost", opts).await.unwrap();

    //Subscribe to 'my_topic' using an unbounded Tokio MPSC channel
    let (subscription, sub_qos) = client.subscribe_unbounded("my_topic", QoS::AtLeastOnce).await.unwrap();
    println!("Subscribed to topic with QoS {:?}", sub_qos);
    client.publish_qos_1("my_topic", b"it works!", false, true).await.unwrap();

    let msg = subscription.recv().await.expect("Failed to await message");
    println!("Received {}", msg.payload_as_utf8().unwrap());

    shutdown_handle.disconnect().await.expect("Could not disconnect gracefully");
}
```

## Feature list

The following (non-default) features can be enabled:

 - `tls` for TLS support
 - `dangerous_tls` to allow dangerous TLS functions, such as the one that enables you to bypass server certificate verification

## Philosophy

 - **Less dependencies:** Avoid using crates, unless it can improve performances. Make dependencies optional through features as often as possible.
 - **Unsafe code:** Use unsafe code to improve performances, provided the unsafe code can be proven to be safe trivially.

## TODOs

 - Optionalize `parking_lot` and `fxhash`
 - Release on crates.io

## Missing features

For now, the following features are not available in LMC. They may be implemented later as required:

 - Websocket transport
 - Multiple subscriptions using a single call/packet
