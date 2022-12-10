use crate::transport::Transport;

use std::io;
use tokio::select;

pub struct SyncTransport<T>(T);

impl<T: Transport> SyncTransport<T>
{
    pub async fn write_fully(&mut self, mut data: &[u8]) -> io::Result<()>
    {
        while data.len() > 0 {
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

            loop {
                let wants = self.0.wants(false, data.len() > 0);
                if !wants.read && !wants.write {
                    break;
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

        Ok(())
    }

    async fn read_fully(&mut self, dst: &mut [u8]) -> io::Result<()>
    {
        let mut pos = 0;

        while pos < dst.len() {
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

            loop {
                let wants = self.0.wants(pos < dst.len(), false);
                if !wants.read && !wants.write {
                    break;
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

        Ok(())
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
