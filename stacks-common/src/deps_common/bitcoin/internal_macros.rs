// Rust Bitcoin Library
// Written in 2014 by
//     Andrew Poelstra <apoelstra@wpsoftware.net>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the CC0 Public Domain Dedication
// along with this software.
// If not, see <http://creativecommons.org/publicdomain/zero/1.0/>.
//

macro_rules! impl_consensus_encoding {
    ($thing:ident, $($field:ident),+) => (
        impl<S: crate::deps_common::bitcoin::network::serialize::SimpleEncoder> crate::deps_common::bitcoin::network::encodable::ConsensusEncodable<S> for $thing {
            #[inline]
            fn consensus_encode(&self, s: &mut S) -> Result<(), crate::deps_common::bitcoin::network::serialize::Error> {
                $( self.$field.consensus_encode(s)?; )+
                Ok(())
            }
        }

        impl<D: crate::deps_common::bitcoin::network::serialize::SimpleDecoder> crate::deps_common::bitcoin::network::encodable::ConsensusDecodable<D> for $thing {
            #[inline]
            fn consensus_decode(d: &mut D) -> Result<$thing, crate::deps_common::bitcoin::network::serialize::Error> {
                use crate::deps_common::bitcoin::network::encodable::ConsensusDecodable;
                Ok($thing {
                    $( $field: ConsensusDecodable::consensus_decode(d)?, )+
                })
            }
        }
    );
}

macro_rules! impl_newtype_consensus_encoding {
    ($thing:ident) => {
        impl<S: crate::deps_common::bitcoin::network::serialize::SimpleEncoder>
            crate::deps_common::bitcoin::network::encodable::ConsensusEncodable<S> for $thing
        {
            #[inline]
            fn consensus_encode(
                &self,
                s: &mut S,
            ) -> Result<(), crate::deps_common::bitcoin::network::serialize::Error> {
                let &$thing(ref data) = self;
                data.consensus_encode(s)
            }
        }

        impl<D: crate::deps_common::bitcoin::network::serialize::SimpleDecoder>
            crate::deps_common::bitcoin::network::encodable::ConsensusDecodable<D> for $thing
        {
            #[inline]
            fn consensus_decode(
                d: &mut D,
            ) -> Result<$thing, crate::deps_common::bitcoin::network::serialize::Error> {
                Ok($thing(ConsensusDecodable::consensus_decode(d)?))
            }
        }
    };
}

macro_rules! user_enum {
    (
        $(#[$attr:meta])*
        pub enum $name:ident {
            $(#[$doc:meta]
              $elem:ident <-> $txt:expr),*
        }
    ) => (
        $(#[$attr])*
        pub enum $name {
            $(#[$doc] $elem),*
        }

        impl ::std::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
                f.pad(match *self {
                    $($name::$elem => $txt),*
                })
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
                f.pad(match *self {
                    $($name::$elem => $txt),*
                })
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = ::std::io::Error;
            #[inline]
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($txt => Ok($name::$elem)),*,
                    _ => Err(::std::io::Error::new(
                        ::std::io::ErrorKind::InvalidInput,
                        format!("Unknown network (type {})", s),
                    )),
                }
            }
        }

        #[cfg(feature = "serde")]
        impl<'de> serde::Deserialize<'de> for $name {
            #[inline]
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                use std::fmt::{self, Formatter};

                struct Visitor;
                impl<'de> serde::de::Visitor<'de> for Visitor {
                    type Value = $name;

                    fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
                        formatter.write_str("an enum value")
                    }

                    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
                    where
                        E: serde::de::Error,
                    {
                        static FIELDS: &'static [&'static str] = &[$(stringify!($txt)),*];

                        $( if v == $txt { Ok($name::$elem) } )else*
                        else {
                            Err(E::unknown_variant(v, FIELDS))
                        }
                    }

                    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E>
                    where
                        E: serde::de::Error,
                    {
                        self.visit_str(v)
                    }

                    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
                    where
                        E: serde::de::Error,
                    {
                        self.visit_str(&v)
                    }

                }

                deserializer.deserialize_str(Visitor)
            }
        }

        #[cfg(feature = "serde")]
        impl ::serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: ::serde::Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }
    );
}

macro_rules! impl_index_newtype {
    ($thing:ident, $ty:ty) => {
        impl ::std::ops::Index<::std::ops::Range<usize>> for $thing {
            type Output = [$ty];

            #[inline]
            fn index(&self, index: ::std::ops::Range<usize>) -> &[$ty] {
                &self.0[index]
            }
        }

        impl ::std::ops::Index<::std::ops::RangeTo<usize>> for $thing {
            type Output = [$ty];

            #[inline]
            fn index(&self, index: ::std::ops::RangeTo<usize>) -> &[$ty] {
                &self.0[index]
            }
        }

        impl ::std::ops::Index<::std::ops::RangeFrom<usize>> for $thing {
            type Output = [$ty];

            #[inline]
            fn index(&self, index: ::std::ops::RangeFrom<usize>) -> &[$ty] {
                &self.0[index]
            }
        }

        impl ::std::ops::Index<::std::ops::RangeFull> for $thing {
            type Output = [$ty];

            #[inline]
            fn index(&self, _: ::std::ops::RangeFull) -> &[$ty] {
                &self.0[..]
            }
        }
    };
}

macro_rules! display_from_debug {
    ($thing:ident) => {
        impl ::std::fmt::Display for $thing {
            fn fmt(&self, f: &mut ::std::fmt::Formatter) -> Result<(), ::std::fmt::Error> {
                ::std::fmt::Debug::fmt(self, f)
            }
        }
    };
}

#[cfg(test)]
macro_rules! hex_script (($s:expr) => (crate::deps_common::bitcoin::blockdata::script::Script::from(crate::util::hash::hex_bytes($s).unwrap())));

macro_rules! serde_struct_impl {
    ($name:ident, $($fe:ident),*) => (
        #[cfg(feature = "serde")]
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<$name, D::Error>
            where
                D: serde::de::Deserializer<'de>,
            {
                use std::fmt::{self, Formatter};
                use serde::de::IgnoredAny;

                #[allow(non_camel_case_types)]
                enum Enum { Unknown__Field, $($fe),* }

                struct EnumVisitor;
                impl<'de> serde::de::Visitor<'de> for EnumVisitor {
                    type Value = Enum;

                    fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
                        formatter.write_str("a field name")
                    }

                    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
                    where
                        E: serde::de::Error,
                    {
                        match v {
                            $(
                            stringify!($fe) => Ok(Enum::$fe)
                            ),*,
                            _ => Ok(Enum::Unknown__Field)
                        }
                    }
                }

                impl<'de> serde::Deserialize<'de> for Enum {
                    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
                    where
                        D: ::serde::de::Deserializer<'de>,
                    {
                        deserializer.deserialize_str(EnumVisitor)
                    }
                }

                struct Visitor;

                impl<'de> serde::de::Visitor<'de> for Visitor {
                    type Value = $name;

                    fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
                        formatter.write_str("a struct")
                    }

                    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
                    where
                        A: serde::de::MapAccess<'de>,
                    {
                        use serde::de::Error;

                        $(let mut $fe = None;)*

                        loop {
                            match map.next_key::<Enum>()? {
                                Some(Enum::Unknown__Field) => {
                                    map.next_value::<IgnoredAny>()?;
                                }
                                $(
                                    Some(Enum::$fe) => {
                                        $fe = Some(map.next_value()?);
                                    }
                                )*
                                None => { break; }
                            }
                        }

                        $(
                            let $fe = match $fe {
                                Some(x) => x,
                                None => return Err(A::Error::missing_field(stringify!($fe))),
                            };
                        )*

                        let ret = $name {
                            $($fe: $fe),*
                        };

                        Ok(ret)
                    }
                }
                // end type defs

                static FIELDS: &'static [&'static str] = &[$(stringify!($fe)),*];

                deserializer.deserialize_struct(stringify!($name), FIELDS, Visitor)
            }
        }

        #[cfg(feature = "serde")]
        impl<'de> serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                use serde::ser::SerializeStruct;

                // Only used to get the struct length.
                static FIELDS: &'static [&'static str] = &[$(stringify!($fe)),*];

                let mut st = serializer.serialize_struct(stringify!($name), FIELDS.len())?;

                $(
                    st.serialize_field(stringify!($fe), &self.$fe)?;
                )*

                st.end()
            }
        }
    )
}
