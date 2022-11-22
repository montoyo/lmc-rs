use std::io;

use super::packets::{self, IncomingPacket, ControlField};
use super::byte_io::ByteReader;
use super::util::panic_in_test;
use super::super::transport::Transport;
use super::super::errors::PacketDecodeError;

#[derive(Clone, Copy)]
struct PacketSizeInfo
{
    /// The total size of the packet in bytes, including its fixed header 
    /// (control and remaining size fields).
    size: usize,

    /// The offset, in bytes, at which the variable header starts (= the
    /// length of the control and remaining size fields).
    start_offset: usize
}

/// A buffer that accumulates bytes until an entire full packet has been read.
#[derive(Default)]
pub struct PacketReader
{
    bytes: Vec<u8>,

    /// If this is [`None`], the packet reader is still waiting for bytes that are
    /// part of the packet's fixed header.
    /// 
    /// If this is [`Some`], the packet reader has already read the packet's fixed
    /// header and is now accumulating the packet's variable header and payload.
    size: Option<PacketSizeInfo>
}

/// Represents the [`PacketReader`]'s possible states after a read attempt.
pub enum PacketReadState
{
    /// A new packet has been read successfully and is ready for processing
    Incoming(IncomingPacket),

    /// The connection has been closed
    ConnectionClosed,

    /// Not enough data to complete the next packet, call back later as soon
    /// as more data is available.
    NeedMoreData
}

impl PacketReader
{
    /// An arbitrary upper limit to make sure not to allocate a giant block because of
    /// a malformed packet.
    const MAX_PACKET_SIZE: usize = 512_000_000;

    /// Attempts to read an additional `n` bytes into [`PacketReader::bytes`]. Returns
    /// `Ok(true)` if the connection was closed.
    fn recv_n(&mut self, transport: &mut dyn Transport, n: usize) -> io::Result<bool>
    {
        if n <= 0 {
            return Ok(false);
        }

        self.bytes.reserve(n);

        unsafe {
            let pos = self.bytes.len();
            let bytes = std::slice::from_raw_parts_mut(self.bytes.as_mut_ptr().add(pos), n);
            let read = transport.read(bytes)?;

            if read <= 0 {
                Ok(true)
            } else {
                self.bytes.set_len(pos + read);
                Ok(false)
            }
        }
    }

    /// Attempts to parse the remaining size of the packet (variable header and payload)
    /// from the accumulated bytes.
    /// 
    /// # Return values
    /// 
    /// - `Ok(Some(_))` is returned if the remaining size was parsed successfully
    /// - `Ok(None)` is returned if more data is required
    /// - `Err(_)` is returned if the remaining size was parsed but is larger than
    ///   [`Self::MAX_PACKET_SIZE`] and is likely to be malformed.
    fn parse_packet_size(&self) -> io::Result<Option<PacketSizeInfo>>
    {
        let mut pos = 1;
        let mut ret = 0;
        let mut shift = 0;

        loop {
            if pos >= self.bytes.len() {
                return Ok(None); //Missing bytes
            }

            let b = self.bytes[pos];
            ret |= ((b & 0x7f) as usize) << shift;
            pos += 1;

            if (b & 0x80) == 0 {
                break;
            }

            shift += 7;
        }

        ret += pos;

        if ret > Self::MAX_PACKET_SIZE {
            return Err(PacketDecodeError::ReachedMaxSize.into());
        }

        Ok(Some(PacketSizeInfo { size: ret, start_offset: pos }))
    }
    
    /// The reader's main function. It reads bytes from the specified [`Transport`] implementation
    /// and returns [`PacketReadState::Incoming`] as soon as a packet has been read completely.
    pub fn recv(&mut self, transport: &mut dyn Transport) -> io::Result<PacketReadState>
    {
        if let Some(size_info) = self.size {
            //Currently reading variable header and payload
            if self.bytes.len() < size_info.size {
                if self.recv_n(transport, size_info.size - self.bytes.len())? {
                    return Ok(PacketReadState::ConnectionClosed);
                }
            }

            if self.bytes.len() >= size_info.size {
                //Packet is ready!
                let ctrl_field = ControlField(self.bytes[0]);
                let mut rd = ByteReader::new(&self.bytes[size_info.start_offset..size_info.size]);
                let ret = IncomingPacket::from_bytes(&mut rd, ctrl_field);

                if let Ok(pkt) = &ret {
                    if rd.remaining() > 0 {
                        panic_in_test!("Did not read {:?} packet in its entirety", pkt.packet_type());
                    }
                }

                if self.bytes.len() > size_info.size {
                    //Only happens for very small packets
                    let remaining = self.bytes.len() - size_info.size;

                    self.bytes.copy_within(size_info.size.., 0);
                    self.bytes.truncate(remaining);

                    self.size = match self.parse_packet_size() {
                        Err(err) => {
                            self.bytes.clear();
                            return Err(err);
                        },
                        Ok(x) => x
                    };
                } else {
                    self.bytes.clear();
                    self.size = None;
                }

                return match ret {
                    Ok(x)  => Ok(PacketReadState::Incoming(x)),
                    Err(x) => Err(x.into())
                };
            }
        } else {
            //Currently reading fixed header
            if self.recv_n(transport, packets::MAX_HEADER_SIZE - self.bytes.len())? {
                return Ok(PacketReadState::ConnectionClosed);
            }

            match self.parse_packet_size() {
                Err(err) => {
                    self.bytes.clear();
                    return Err(err);
                },
                Ok(x) => self.size = x
            }
        }

        Ok(PacketReadState::NeedMoreData)
    }
}
