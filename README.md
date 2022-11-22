# LMC
## A Lightweight MQTT Client for Rust

 - **Features:** `tls` for TLS support, `dangerous_tls` to allow dangerous TLS functions
 - **License:** [MIT](LICENSE-MIT) or [APACHE](LICENSE-APACHE)

## Example

```rust
let mut opts = Options::new("client_id")
    .enable_tls()
    .expect("Failed to load native system TLS certificates");
    
opts.set_username("username")
    .set_password(b"password");

let (client, shutdown_handle) = Client::connect("localhost", opts)
    .await
    .expect("Failed to connect to broker!");

let subscription = client.subscribe_unbounded("my_topic", QoS::AtLeastOnce)
    .await
    .expect("Failed to subscribe to 'my_topic'");

client.publish_qos_1("my_topic", b"it works!", false, true)
    .await
    .expect("Failed to publish message in 'my_topic'");

let msg = subscription.recv().await.expect("Failed to await message");
println!("Received {}", msg.payload_as_utf8().unwrap());

shutdown_handle.disconnect().await.expect("Could not disconnect gracefully");
```

## TODOs

 - Enhance README
 - More tests!
 - Test doc examples
 - Release on crates.io
