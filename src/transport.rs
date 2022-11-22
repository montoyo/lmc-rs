use std::io;
use std::future::Future;
use tokio::net::TcpStream;

/// A convenience structure used to expose only the `readable` and
/// `writable` methods of a [`TcpStream`].
pub struct ReadyFor<'a>(&'a TcpStream);

impl<'a> ReadyFor<'a>
{
    /// Constructs a [`ReadyFor`] object from an existing [`TcpStream`].
    pub fn wrap(tcp: &'a TcpStream) -> Self
    {
        Self(tcp)
    }

    /// Awaits readabiliy on a [`TcpStream`], see [`TcpStream::readable()`].
    pub fn read(self) -> impl Future<Output = io::Result<()>> + 'a
    {
        self.0.readable()
    }

    /// Awaits writability on a [`TcpStream`], see [`TcpStream::writable()`].
    pub fn write(self) -> impl Future<Output = io::Result<()>> + 'a
    {
        self.0.writable()
    }
}

/// Specifies what event(s) to wait for the next IO operation
pub struct WantedFlags
{
    /// Reading should should be considered as the next IO operation
    pub read: bool,

    /// Writing should should be considered as the next IO operation
    pub write: bool
}

pub trait Transport: Send
{
    /// Determines what event(s) should be waited for the next IO
    /// operation. The implementation may use or ignore the hints
    /// passed as parameter when determining the return value.
    fn wants(&self, rd_hint: bool, wr_hint: bool) -> WantedFlags;

    /// Returns a [`ReadyFor`] object that can be used to await
    /// IO readyness.
    fn ready_for(&self) -> ReadyFor;

    /// Should **ALWAYS** be called if readable, before [`Self::read()`].
    /// 
    /// Will **NOT** return `Err(WouldBlock)`, instead, will return
    /// false if not ready to be read.
    fn pre_read(&mut self) -> io::Result<bool>;

    /// May be called right after a call to [`Self::pre_read()`] that
    /// returned `Ok(true)`.
    /// 
    /// Will return `Err(WouldBlock)` if there is actually nothing to read,
    /// and will return `Ok(0)` if the connection has been closed properly
    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize>;

    /// Should **ALWAYS** be called if writable, before [`Self::write()`].
    /// 
    /// Will **NEVER** return `Err(WouldBlock)`.
    fn pre_write(&mut self) -> io::Result<()>;

    /// May be called at any time to initiate or continue a writing operation.
    /// If the return value is `Err(WouldBlock)`, then this method shall not
    /// be called again until this transport is readable and until
    /// [`Self::pre_write`] is called.
    /// 
    /// However, depending on the value of `zero_if_would_block`,
    /// `Err(WouldBlock)`, will be replaced by `Ok(0)`, making it impossible
    /// to distinguish it from a "connection closed" event.
    fn write(&mut self, src: &[u8], zero_if_would_block: bool) -> io::Result<usize>;

    /// Tells the transport to flush its output buffer and transmit its contents
    /// immediately. This may have no effect.
    fn flush(&mut self) -> io::Result<()>;

    /// Tells the transport that the connection is about to be closed. This may
    /// have no effect.
    fn send_close_notify(&mut self);
}

/// A plain implementation of [`Transport`] using [`TcpStream`]. The data
/// will be transported in plaintext using TCP.
pub struct TcpTransport
{
    tcp: TcpStream
}

impl TcpTransport
{
    /// Creates a new [`Transport`] based on a [`TcpStream`].
    pub fn new(stream: TcpStream) -> Self
    {
        Self {
            tcp: stream
        }
    }
}

impl Transport for TcpTransport
{
    fn wants(&self, rd_hint: bool, wr_hint: bool) -> WantedFlags
    {
        WantedFlags { read: rd_hint, write: wr_hint }
    }

    fn ready_for(&self) -> ReadyFor
    {
        ReadyFor::wrap(&self.tcp)
    }

    fn pre_read(&mut self) -> io::Result<bool>
    {
        Ok(true)
    }

    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize>
    {
        self.tcp.try_read(dst)
    }

    fn pre_write(&mut self) -> io::Result<()>
    {
        Ok(())
    }

    fn write(&mut self, src: &[u8], zero_if_would_block: bool) -> io::Result<usize>
    {
        if zero_if_would_block {
            match self.tcp.try_write(src) {
                Ok(x) => Ok(x),
                Err(err) => if err.kind() == io::ErrorKind::WouldBlock { Ok(0) } else { Err(err) }
            }
        } else {
            self.tcp.try_write(src)
        }
    }

    fn flush(&mut self) -> io::Result<()>
    {
        Ok(())
    }

    fn send_close_notify(&mut self)
    {
    }
}
