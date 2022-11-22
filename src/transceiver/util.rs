
use super::packets::PacketType;

/// Panics only in tests, logs an error otherwise.
/// This macro should only be used for errors we can recover from.
macro_rules! panic_in_test {
    ($($args:expr),+) => {
        if cfg!(test) {
            panic!($($args),+)
        } else {
            ::log::error!($($args),+);
        }
    };
}

/// Defines an enumeration where variants have either no values or
/// contain a single struct. For each variants with a single struct
/// value, this macro will automatically generate an [`Into`]
/// implementation to easily convert the structure into the defined
/// enum.
/// 
/// # Example
/// 
/// ```
/// struct A;
/// struct B;
/// 
/// def_enum_with_intos! {
///     pub enum MyEnum {
///         A(A),
///         B(B),
///         C
///     }
/// }
/// 
/// fn main() {
///     let val_a: MyEnum = A.into();
///     let val_b: MyEnum = B.into();
/// 
///     assert!(matches!(val_a, MyEnum::A(_)));
///     assert!(matches!(val_b, MyEnum::B(_)));
/// }
/// ```
macro_rules! def_enum_with_intos {
    { $(#[$($attrs:tt)*])* $vis:vis enum $enum:ident { $($(#[$($vattrs:tt)*])* $name:ident$(($type:ty))?),+ } } => {
        $(#[$($attrs)*])*
        $vis enum $enum
        {
            $($(#[$($vattrs)*])* $name$(($type))?),+
        }

        $($(
            impl Into<$enum> for $type
            {
                fn into(self) -> $enum { $enum::$name(self) }
            }
        )?)+
    };
}

pub(crate) use panic_in_test;
pub(crate) use def_enum_with_intos;

/// Combines a packet ID with its type. Used to index hash maps.
/// 
/// The implementation provides convenience functions to create
/// instances with the most common [`PacketType`]s.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct IdType
{
    pub ty: PacketType,
    pub id: u16
}

impl IdType
{
    pub fn publish(id: u16) -> Self { Self { ty: PacketType::Publish, id } }
    pub fn subscribe(id: u16) -> Self { Self { ty: PacketType::Subscribe, id } }
    pub fn pub_rec(id: u16) -> Self { Self { ty: PacketType::PubRec, id } }
    pub fn pub_rel(id: u16) -> Self { Self { ty: PacketType::PubRel, id } }
    pub fn unsubscribe(id: u16) -> Self { Self { ty: PacketType::Unsubscribe, id } }
}

