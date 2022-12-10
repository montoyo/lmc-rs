use std::{str, mem, ptr};
use mem::MaybeUninit;

use super::super::errors::PacketDecodeError;

/// A marker for big endian values. These values must be converted into native byte order
/// before processing them.
#[derive(Copy)]
#[repr(C, packed)]
pub struct BigEndian<T: Copy>(pub T);

impl BigEndian<u16>
{
    /// Converts a u16 integer from native byte order into big endian
    pub const fn from_native(val: u16) -> Self
    {
        Self(val.to_be())
    }

    /// Converts a u16 integer from big endian to native byte order
    pub const fn to_native(self) -> u16
    {
        u16::from_be(self.0)
    }
}

//We have to implement this manually otherwise the compiler cries because the struct is packed
impl<T: Copy> Clone for BigEndian<T>
{
    fn clone(&self) -> Self
    {
        Self(self.0)
    }
}

/// A utility struct to help with reading MQTT v3 packet data types from byte slices.
/// Every time a read occurs, the slice reference is offseted.
pub struct ByteReader<'a>(&'a [u8]);

impl<'a> ByteReader<'a>
{
    /// Instantiates a new [`ByteReader`] using the provided slice
    pub fn new(bytes: &'a [u8]) -> Self
    {
        Self(bytes)
    }

    /// Reads a single byte and returns it.
    /// 
    /// A [`PacketDecodeError`] is returned if the slice was empty.
    pub fn read_u8(&mut self) -> Result<u8, PacketDecodeError>
    {
        if self.0.len() < 1 {
            return Err(PacketDecodeError::ReachedEndUnexpectedly);
        }

        let ret = self.0[0];
        self.0 = &self.0[1..];

        return Ok(ret);
    }

    /// Reads a big endian u16. This value needs to by converted into native byte
    /// order before being processed. See [`BigEndian`] for more information.
    /// 
    /// A [`PacketDecodeError`] will be returned if not enough bytes are left in
    /// the slice.
    pub fn read_u16(&mut self) -> Result<BigEndian<u16>, PacketDecodeError>
    {
        self.read_trivial::<u16>().map(BigEndian)
    }

    /// Reads a byte array prefixed with its length as a big endian u16. See MQTT
    /// v3 data types specification for more information.
    /// 
    /// A [`PacketDecodeError`] will be returned if not enough bytes are left in
    /// the slice.
    pub fn read_byte_array(&mut self) -> Result<&'a [u8], PacketDecodeError>
    {
        let len = u16::from_be(self.read_trivial::<u16>()?) as usize;
        
        if self.0.len() < len {
            Err(PacketDecodeError::ReachedEndUnexpectedly)
        } else {
            let ret = &self.0[..len];
            self.0 = &self.0[len..];

            Ok(ret)
        }
    }

    /// Reads a UTF-8 string prefixed with its length as a big endian u16. See MQTT
    /// v3 data types specification for more information.
    /// 
    /// A [`PacketDecodeError`] will be returned if not enough bytes are left in
    /// the slice, or if the string contains malformed UTF-8.
    pub fn read_utf8(&mut self) -> Result<&'a str, PacketDecodeError>
    {
        let slice = self.read_byte_array()?;
        str::from_utf8(slice).map_err(PacketDecodeError::Utf8Error)
    }

    /// Reads a value that can be trivially copied (that implements the `Copy` trait).
    /// The developer must make sure that the passed type matches with the data inside
    /// the packet.
    /// 
    /// A [`PacketDecodeError`] will be returned if not enough bytes are left in
    /// the slice.
    pub fn read_trivial<T: Copy>(&mut self) -> Result<T, PacketDecodeError>
    {
        if self.0.len() < mem::size_of::<T>() {
            return Err(PacketDecodeError::ReachedEndUnexpectedly);
        }

        let ret = unsafe {
            //SAFETY:
            // - 'T' can be copied trivially because it is `Copy`
            // - We have checked the length of `self.0` to ensure that `ptr::read_unaligned` won't read outside the buffer
            // - Whether to use `read` or `read_unaligned`, I do not know. Change this if I'm wrong.

            ptr::read_unaligned(self.0.as_ptr().cast())
        };

        self.0 = &self.0[mem::size_of::<T>()..];
        return Ok(ret);
    }

    /// Returns the remaining amount of bytes left in the buffer.
    pub fn remaining(&self) -> usize
    {
        self.0.len()
    }

    /// Returns all the bytes left in this buffer as a u8 slice. After this,
    /// any attempt at reading from this [`ByteReader`] will fail with
    /// [`PacketDecodeError::ReachedEndUnexpectedly`].
    pub fn read_remaining(&mut self) -> &'a [u8]
    {
        mem::replace(&mut self.0, &[])
    }
}

/// A convenience trait used to add an offset to a mutable slice reference.
/// Rust makes it unecessarily hard to do that, so we're using this trait
/// to perform this operation in a single line of code.
/// 
/// This is used by [`ByteWriter`].
trait AddOffset
{
    /// Modifies this slice reference so that it starts as the specified offset.
    /// 
    /// # Example
    /// 
    /// ```ignore
    /// let mut slice = [0, 1, 2, 3];
    /// let mut slice_ref = &mut slice;
    /// 
    /// assert_eq!(slice_ref[0], slice[0]);
    /// slice_ref.add_offset(2);
    /// assert_eq!(slice_ref[0], slice[2]);
    /// ```
    fn add_offset(&mut self, offset: usize);
}

impl<T> AddOffset for &mut [T]
{
    #[inline]
    fn add_offset(&mut self, offset: usize)
    {
        //Turns out you can't just `self.slice = &mut self.slice[offset..]`, no no no... that'd be too simple!
        //This little piece of shit of a work around took me hours to find... thanks Rust

        let old_ref = mem::replace(self, &mut []);
        *self = &mut old_ref[offset..];
    }
}

/// A utility struct to help with writing MQTT v3 packet data types into uninitialized
/// byte slices. Every time a write occurs, the slice reference is offseted.
/// 
/// [`ByteWriter::finish_and_check()`] can be used to ensure that the entire range of
/// the initial slice reference has been written. After that call, the initial slice
/// can be assumed to be initialized safely.
pub struct ByteWriter<'a>(&'a mut [MaybeUninit<u8>]);

impl<'a> ByteWriter<'a>
{
    /// Instantiates a new [`ByteWriter`] using the provided slice
    pub fn new(dst: &'a mut [MaybeUninit<u8>]) -> Self
    {
        Self(dst)
    }

    /// Writes a value that can be trivially copied (that implements the `Copy` trait).
    /// If not enough space is left, this function will panic.
    pub fn write_trivial<T: Copy>(&mut self, data: &T) -> &mut Self
    {
        let len = mem::size_of::<T>();
        assert!(self.0.len() >= len, "not enough space left");

        unsafe {
            //SAFETY:
            // - Everything in `self.0` has not been initialized yet, thus does not need to be dropped
            // - `data` can be copied trivially because it implements `Copy`
            // - We just checked that `self.0` is large enough to handle `len` bytes
            // - After `copy_nonoverlapping`, `self.0[0..len]` will be initialized, but we're changing `self.0` so that it only contains uninitialized memory again

            ptr::copy_nonoverlapping((data as *const T).cast(), self.0.as_mut_ptr(), len);
            self.0.add_offset(len);
        }

        self
    }

    /// Writes a single byte into the slice. If no space is left, this function
    /// will panic.
    pub fn write_u8(&mut self, byte: u8) -> &mut Self
    {
        assert!(self.0.len() >= 1, "not enough space left");

        self.0[0].write(byte);
        self.0.add_offset(1);

        self
    }

    /// Writes a [`BigEndian`] u16 into the slice.
    /// This function will panic if the remaining space is not enough to fit a u16.
    pub fn write_u16(&mut self, data: BigEndian<u16>) -> &mut Self
    {
        self.write_trivial(&data)
    }

    /// Writes a byte slice (or str) prefixed with its length as a big endian u16.
    /// See MQTT v3 data types specification for more information.
    /// 
    /// If the slice is not large enough to handle the provided bytes (and the length
    /// field), this function will panic.
    pub fn write_bytes<T: AsRef<[u8]>>(&mut self, bytes: T) -> &mut Self
    {
        let bytes = bytes.as_ref();
        let array_len = bytes.len();
        let array_len_be = u16::try_from(array_len).expect("array too wide").to_be();
        let total_len = mem::size_of::<u16>() + array_len;

        assert!(self.0.len() >= total_len, "not enough space left");

        unsafe {
            //SAFETY:
            // - Everything in `self.0` has not been initialized yet, thus does not need to be dropped
            // - `u16` and `[u8]` can be copied trivially
            // - We just checked that `self.0` is large enough to handle an extra u16 as well as the whole `bytes` array
            // - At the end, `self.0[0..len]` will be initialized, but we're changing `self.0` so that it only contains uninitialized memory again

            let ptr = self.0.as_mut_ptr();
            ptr::write_unaligned(ptr.cast(), array_len_be);
            ptr::copy_nonoverlapping(bytes.as_ptr().cast(), ptr.add(mem::size_of::<u16>()), array_len);

            self.0.add_offset(total_len);
        }

        self
    }

    /// Writes a raw byte slice (or str), **without** any length field. This is
    /// generally used for the last field in the packet as the length information
    /// can be recovered using packet size.
    /// 
    /// See MQTT v3 data types specification for more information.
    /// 
    /// If the slice is not large enough to accomodate the provided bytes, this
    /// function will panic.
    pub fn write_bytes_no_len<T: AsRef<[u8]>>(&mut self, bytes: T) -> &mut Self
    {
        let bytes = bytes.as_ref();
        assert!(self.0.len() >= bytes.len(), "not enough space left");

        unsafe {
            //SAFETY:
            // - Everything in `self.0` has not been initialized yet, thus does not need to be dropped
            // - `[u8]` can be copied trivially
            // - We just checked that `self.0` is large enough to handle `bytes`
            // - After `copy_nonoverlapping`, `self.0[0..len]` will be initialized, but we're changing `self.0` so that it only contains uninitialized memory again

            ptr::copy_nonoverlapping(bytes.as_ptr().cast(), self.0.as_mut_ptr(), bytes.len());
            self.0.add_offset(bytes.len());
        }

        self
    }

    /// Utility function used to assert that the initial slice has been written in
    /// its entirety, and that no more space is left.
    /// 
    /// This function will panic if some space is left. After calling this function,
    /// the initial slice can be assumed to be initialized safely.
    pub fn finish_and_check(self)
    {
        assert!(self.0.len() <= 0, "uninitialized bytes left");
    }
}
