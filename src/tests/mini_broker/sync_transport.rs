use crate::transport::Transport;
use super::tracing::TracingUtility;

use std::io;
use std::time::Duration;

use tokio::select;
use tokio::time;

pub struct SyncTransport
{
    t: Box<dyn Transport>,
    tracing_utility: TracingUtility
}

const K_BYTES: &'static str = "transport_bytes";
const K_PACKET: &'static str = "transport_packets";
const LOOP_COUNT: &'static [&'static str] = &["loop 0", "loop 1", "loop 2", "loop 3", "loop 4", "loop 5", "loop 6", "loop 7", "loop 8", "loop 9"];

impl SyncTransport
{
    pub fn new<T: Transport + 'static>(transport: T, tracing_utility: TracingUtility) -> Self
    {
        Self {
            t: Box::new(transport),
            tracing_utility
        }
    }

    pub async fn write_fully(&mut self, mut data: &[u8]) -> io::Result<()>
    {
        loop {
            let written = if data.len() <= 0 {
                0
            } else {
                match self.t.write(data, false) {
                    Ok(x) => if x <= 0 {
                        return Err(io::ErrorKind::ConnectionReset.into());
                    } else {
                        x
                    },
                    Err(err) => if err.kind() == io::ErrorKind::WouldBlock {
                        0
                    } else {
                        return Err(err);
                    }
                }
            };

            data = &data[written..];

            if data.len() <= 0 {
                self.t.flush()?;
            }

            let wants = self.t.wants(false, data.len() > 0);
            if !wants.read && !wants.write {
                assert!(data.len() <= 0, "!wants.read && !wants.write, but there's still some data to be sent");
                return Ok(());
            }

            if data.len() <= 0 && !wants.write {
                return Ok(());
            }

            let timeout = time::sleep(Duration::from_millis(100));

            select! {
                err = self.t.ready_for().read(), if wants.read => {
                    err?;
                    self.t.pre_read()?;
                },
                err = self.t.ready_for().write(), if wants.write => {
                    err?;
                    self.t.pre_write()?;
                },
                _ = timeout => {}
            }
        }
    }

    async fn read_fully(&mut self, dst: &mut [u8]) -> io::Result<()>
    {
        if dst.len() <= 0 {
            return Ok(());
        }

        let mut pos = 0;
        let mut counter = 0;

        loop {
            self.tracing_utility.update_state(K_BYTES, LOOP_COUNT[counter]);
            counter = (counter + 1) % LOOP_COUNT.len();

            let read = match self.t.read(&mut dst[pos..]) {
                Ok(x) => if x <= 0 {
                    return Err(io::ErrorKind::ConnectionReset.into());
                } else {
                    x
                },
                Err(err) => if err.kind() == io::ErrorKind::WouldBlock {
                    0
                } else {
                    return Err(err);
                }
            };

            pos += read;

            if pos >= dst.len() {
                return Ok(());
            }

            let wants = self.t.wants(true, false);
            let timeout = time::sleep(Duration::from_millis(100));
            assert!(wants.read || wants.write, "!wants.read && !wants.write, but there's still some data to be read");

            select! {
                err = self.t.ready_for().read(), if wants.read => {
                    err?;
                    self.t.pre_read()?;
                },
                err = self.t.ready_for().write(), if wants.write => {
                    err?;
                    self.t.pre_write()?;
                },
                _ = timeout => {}
            }
        }
    }

    pub async fn drop_byte(&mut self) -> io::Result<bool>
    {
        let mut trash = [0u8];

        loop {
            match self.t.read(&mut trash) {
                Ok(x) => return Ok(x <= 0),
                Err(err) if err.kind() != io::ErrorKind::WouldBlock => return Err(err),
                Err(_would_block) => {}
            }

            let wants = self.t.wants(true, false);
            let timeout = time::sleep(Duration::from_millis(100));
            assert!(wants.read || wants.write, "!wants.read && !wants.write, but there's still some data to be read");

            select! {
                err = self.t.ready_for().read(), if wants.read => {
                    err?;
                    self.t.pre_read()?;
                },
                err = self.t.ready_for().write(), if wants.write => {
                    err?;
                    self.t.pre_write()?;
                },
                _ = timeout => {}
            }
        }
    }

    pub async fn read_packet(&mut self) -> io::Result<Vec<u8>>
    {
        let mut header = [0u8; 2];
        self.tracing_utility.update_state(K_PACKET, "awaiting packet header");
        self.read_fully(&mut header).await?;
        self.tracing_utility.update_state(K_PACKET, "got header");

        let mut sz = 0usize;
        let mut shift = 0;

        for i in 1.. {
            sz |= ((header[1] & 0x7f) as usize) << shift;

            if (header[1] & 0x80) == 0 {
                break;
            }

            if i >= 4 {
                return Err(io::Error::new(io::ErrorKind::Other, "Invalid packet size"));
            }

            shift += 7;
            self.tracing_utility.update_state(K_PACKET, "awaiting size byte");
            self.read_fully(&mut header[1..2]).await?;
        }

        if sz > 128_000_000 {
            return Err(io::Error::new(io::ErrorKind::Other, "Invalid packet size"));
        }

        let mut ret = Vec::<u8>::new();
        ret.resize(sz + 1, 0);
        ret[0] = header[0]; //Control field

        self.tracing_utility.update_state(K_PACKET, "reading remaining packet bytes");
        self.read_fully(&mut ret[1..]).await?;
        self.tracing_utility.update_state(K_PACKET, "packet ready");
        return Ok(ret);
    }

    pub async fn send_close_notify(&mut self) -> io::Result<()>
    {
        self.t.send_close_notify();

        loop {
            let wants = self.t.wants(false, false);
            if !wants.read && !wants.write {
                return Ok(());
            }

            let timeout = time::sleep(Duration::from_millis(100));

            select! {
                err = self.t.ready_for().read(), if wants.read => {
                    err?;
                    self.t.pre_read()?;
                },
                err = self.t.ready_for().write(), if wants.write => {
                    err?;
                    self.t.pre_write()?;
                },
                _ = timeout => {}
            }
        }
    }
}
