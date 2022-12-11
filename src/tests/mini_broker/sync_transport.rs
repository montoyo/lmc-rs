use crate::transport::Transport;

use std::io;
use tokio::select;

pub struct SyncTransport(Box<dyn Transport>);

impl SyncTransport
{
    pub fn new<T: Transport + 'static>(transport: T) -> SyncTransport
    {
        SyncTransport(Box::new(transport))
    }

    pub async fn write_fully(&mut self, mut data: &[u8]) -> io::Result<()>
    {
        loop {
            let written = match self.0.write(data, false) {
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

            data = &data[written..];

            if data.len() <= 0 {
                self.0.flush()?;
            }

            let wants = self.0.wants(false, data.len() > 0);
            if !wants.read && !wants.write {
                assert!(data.len() <= 0, "!wants.read && !wants.write, but there's still some data to be sent");
                return Ok(());
            }

            select! {
                err = self.0.ready_for().read(), if wants.read => {
                    err?;
                    self.0.pre_read()?;
                },
                err = self.0.ready_for().write(), if wants.write => {
                    err?;
                    self.0.pre_write()?;
                }
            }
        }
    }

    async fn read_fully(&mut self, dst: &mut [u8]) -> io::Result<()>
    {
        let mut pos = 0;

        loop {
            let read = match self.0.read(dst) {
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

            let wants = self.0.wants(pos < dst.len(), false);
            if !wants.read && !wants.write {
                assert!(pos >= dst.len(), "!wants.read && !wants.write, but there's still some data to be read");
                return Ok(());
            }

            select! {
                err = self.0.ready_for().read(), if wants.read => {
                    err?;
                    self.0.pre_read()?;
                },
                err = self.0.ready_for().write(), if wants.write => {
                    err?;
                    self.0.pre_write()?;
                }
            }
        }
    }

    pub async fn read_packet(&mut self) -> io::Result<Vec<u8>>
    {
        let mut header = [0u8; 2];
        self.read_fully(&mut header).await?;

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
            self.read_fully(&mut header[1..2]).await?;
        }

        if sz > 128_000_000 {
            return Err(io::Error::new(io::ErrorKind::Other, "Invalid packet size"));
        }

        let mut ret = Vec::<u8>::new();
        ret.resize(sz + 1, 0);
        ret[0] = header[0]; //Control field

        self.read_fully(&mut ret[1..]).await?;
        return Ok(ret);
    }
}
