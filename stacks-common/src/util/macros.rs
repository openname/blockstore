// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020 Stacks Open Internet Foundation
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

// is this machine big-endian?
pub fn is_big_endian() -> bool {
    u32::from_be(0x1Au32) == 0x1Au32
}

/// Define an iterable enum: an enum where each variant is an atomic
/// type (i.e., has no paramters), and the variants can be iterated over
/// with an Enum::ALL const
#[macro_export]
macro_rules! iterable_enum {
    ($Name:ident { $($Variant:ident,)* }) =>
    {
        pub enum $Name {
            $($Variant),*,
        }
        impl $Name {
            pub const ALL: &'static [$Name] = &[$($Name::$Variant),*];
        }
    }
}

/// Define a "named" enum, i.e., each variant corresponds
///  to a string literal, with a 1-1 mapping. You get EnumType::lookup_by_name
///  and EnumType.get_name() for free.
#[macro_export]
macro_rules! define_named_enum {
    ($Name:ident { $($Variant:ident($VarName:literal),)* }) =>
    {
        #[derive(::serde::Serialize, ::serde::Deserialize, Debug, Hash, PartialEq, Eq, Copy, Clone)]
        pub enum $Name {
            $($Variant),*,
        }
        impl $Name {
            pub const ALL: &[$Name] = &[$($Name::$Variant),*];
            pub const ALL_NAMES: &[&str] = &[$($VarName),*];

            pub fn lookup_by_name(name: &str) -> Option<Self> {
                match name {
                    $(
                        $VarName => Some($Name::$Variant),
                    )*
                    _ => None
                }
            }

            pub fn get_name(&self) -> String {
                match self {
                    $(
                        $Name::$Variant => $VarName.to_string(),
                    )*
                }
            }

            pub fn get_name_str(&self) -> &'static str {
                match self {
                    $(
                        $Name::$Variant => $VarName,
                    )*
                }
            }
        }
        impl ::std::fmt::Display for $Name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                write!(f, "{}", self.get_name_str())
            }
        }
    }
}

/// Define a "named" enum, i.e., each variant corresponds
///  to a string literal, with a 1-1 mapping. You get EnumType::lookup_by_name
///  and EnumType.get_name() for free.
#[macro_export]
macro_rules! define_versioned_named_enum {
    ($Name:ident($VerType:ty) { $($Variant:ident($VarName:literal, $MinVersion:expr)),* $(,)* }) => {
        $crate::define_versioned_named_enum_internal!($Name($VerType) {
            $($Variant($VarName, $MinVersion, None)),*
        });
    };
}
#[macro_export]
macro_rules! define_versioned_named_enum_with_max {
    ($Name:ident($VerType:ty) { $($Variant:ident($VarName:literal, $MinVersion:expr, $MaxVersion:expr)),* $(,)* }) => {
        $crate::define_versioned_named_enum_internal!($Name($VerType) {
            $($Variant($VarName, $MinVersion, $MaxVersion)),*
        });
    };
}

// An internal macro that does the actual enum definition
#[macro_export]
macro_rules! define_versioned_named_enum_internal {
    ($Name:ident($VerType:ty) { $($Variant:ident($VarName:literal, $MinVersion:expr, $MaxVersion:expr)),* $(,)* }) => {
        #[derive(::serde::Serialize, ::serde::Deserialize, Debug, Hash, PartialEq, Eq, Copy, Clone)]
        pub enum $Name {
            $($Variant),*,
        }

        impl $Name {
            pub const ALL: &[$Name] = &[$($Name::$Variant),*];
            pub const ALL_NAMES: &[&str] = &[$($VarName),*];

            pub fn lookup_by_name(name: &str) -> Option<Self> {
                match name {
                    $($VarName => Some($Name::$Variant),)*
                    _ => None,
                }
            }

            pub fn lookup_by_name_at_version(name: &str, version: &ClarityVersion) -> Option<Self> {
                Self::lookup_by_name(name).and_then(|variant| {
                    let is_active = match (
                        variant.get_min_version(),
                        variant.get_max_version(),
                    ) {
                        (ref min_version, Some(ref max_version)) => {
                            min_version <= version && version <= max_version
                        }
                        // No max version is set, so the function is active for all versions greater than min
                        (ref min_version, None) => min_version <= version,
                    };
                    if is_active {
                        Some(variant)
                    } else {
                        None
                    }
                })
            }

            /// Returns the first Clarity version in which `self` is defined.
            pub fn get_min_version(&self) -> $VerType {
                match self {
                    $(Self::$Variant => $MinVersion,)*
                }
            }

            /// Returns `Some` for the last Clarity version in which `self` is
            /// defined, or `None` if `self` is defined for all versions after
            /// `get_min_version()`.
            pub fn get_max_version(&self) -> Option<$VerType> {
                match self {
                    $(Self::$Variant => $MaxVersion,)*
                }
            }

            pub fn get_name(&self) -> String {
                match self {
                    $(
                        $Name::$Variant => $VarName.to_string(),
                    )*
                }
            }

            pub fn get_name_str(&self) -> &'static str {
                match self {
                    $(Self::$Variant => $VarName,)*
                }
            }
        }

        impl ::std::fmt::Display for $Name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                write!(f, "{}", self.get_name_str())
            }
        }
    };
}

#[allow(clippy::crate_in_macro_def)]
#[macro_export]
macro_rules! guarded_string {
    ($Name:ident, $Label:literal, $Regex:expr, $MaxStringLength:expr, $ErrorType:ty, $ErrorVariant:path) => {
        #[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $Name(String);
        impl TryFrom<String> for $Name {
            type Error = $ErrorType;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                if value.len() > ($MaxStringLength as usize) {
                    return Err($ErrorVariant($Label, value));
                }
                if $Regex.is_match(&value) {
                    Ok(Self(value))
                } else {
                    Err($ErrorVariant($Label, value))
                }
            }
        }

        impl $Name {
            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn len(&self) -> u8 {
                u8::try_from(self.as_str().len()).unwrap()
            }

            pub fn is_empty(&self) -> bool {
                self.len() == 0
            }
        }

        impl Deref for $Name {
            type Target = str;
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl Borrow<str> for $Name {
            fn borrow(&self) -> &str {
                self.as_str()
            }
        }

        impl Into<String> for $Name {
            fn into(self) -> String {
                self.0
            }
        }

        impl From<&'_ str> for $Name {
            fn from(value: &str) -> Self {
                Self::try_from(value.to_string()).unwrap()
            }
        }

        impl fmt::Display for $Name {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

/// Define a "u8" enum
///  gives you a try_from(u8) -> Option<Self> function
#[macro_export]
macro_rules! define_u8_enum {
    ($(#[$outer:meta])*
     $Name:ident {
         $(
             $(#[$inner:meta])*
             $Variant:ident = $Val:literal),+
     }) =>
    {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[repr(u8)]
        $(#[$outer])*
        pub enum $Name {
            $(  $(#[$inner])*
                $Variant = $Val),*,
        }
        impl $Name {
            /// All members of the enum
            pub const ALL: &'static [$Name] = &[$($Name::$Variant),*];

            /// Return the u8 representation of the variant
            pub fn to_u8(&self) -> u8 {
                match self {
                    $(
                        $Name::$Variant => $Val,
                    )*
                }
            }

            /// Returns Some and the variant if `v` is a u8 corresponding to a variant in this enum.
            /// Returns None otherwise
            pub fn from_u8(v: u8) -> Option<Self> {
                match v {
                    $(
                        v if v == $Name::$Variant as u8 => Some($Name::$Variant),
                    )*
                    _ => None
                }
            }
        }
    }
}

/// Borrowed from Andrew Poelstra's rust-bitcoin
#[macro_export]
macro_rules! impl_array_newtype {
    ($thing:ident, $ty:ty, $len:expr) => {
        impl $thing {
            #[inline]
            #[allow(dead_code)]
            /// Converts the object to a raw pointer
            pub fn as_ptr(&self) -> *const $ty {
                let &$thing(ref dat) = self;
                dat.as_ptr()
            }

            #[inline]
            #[allow(dead_code)]
            /// Converts the object to a mutable raw pointer
            pub fn as_mut_ptr(&mut self) -> *mut $ty {
                let &mut $thing(ref mut dat) = self;
                dat.as_mut_ptr()
            }

            #[inline]
            #[allow(dead_code)]
            /// Returns the length of the object as an array
            pub fn len(&self) -> usize {
                $len
            }

            #[inline]
            #[allow(dead_code)]
            /// Returns whether the object, as an array, is empty. Always false.
            pub fn is_empty(&self) -> bool {
                false
            }

            #[inline]
            #[allow(dead_code)]
            /// Returns the underlying bytes.
            pub fn as_bytes(&self) -> &[$ty; $len] {
                &self.0
            }

            #[inline]
            #[allow(dead_code)]
            /// Returns the underlying bytes.
            pub fn to_bytes(&self) -> [$ty; $len] {
                self.0.clone()
            }

            #[inline]
            #[allow(dead_code)]
            /// Returns the underlying bytes.
            pub fn into_bytes(self) -> [$ty; $len] {
                self.0
            }
        }

        impl<'a> From<&'a [$ty]> for $thing {
            fn from(data: &'a [$ty]) -> $thing {
                assert_eq!(data.len(), $len);
                let mut ret = [0; $len];
                ret.copy_from_slice(&data[..]);
                $thing(ret)
            }
        }

        impl ::std::ops::Index<usize> for $thing {
            type Output = $ty;

            #[inline]
            fn index(&self, index: usize) -> &$ty {
                let &$thing(ref dat) = self;
                &dat[index]
            }
        }

        impl_index_newtype!($thing, $ty);

        impl PartialEq for $thing {
            #[inline]
            fn eq(&self, other: &$thing) -> bool {
                &self[..] == &other[..]
            }
        }

        impl Eq for $thing {}

        impl PartialOrd for $thing {
            #[inline]
            fn partial_cmp(&self, other: &$thing) -> Option<::std::cmp::Ordering> {
                Some(self.cmp(&other))
            }
        }

        impl Ord for $thing {
            #[inline]
            fn cmp(&self, other: &$thing) -> ::std::cmp::Ordering {
                // manually implement comparison to get little-endian ordering
                // (we need this for our numeric types; non-numeric ones shouldn't
                // be ordered anyway except to put them in BTrees or whatever, and
                // they don't care how we order as long as we're consisistent).
                for i in 0..$len {
                    if self[$len - 1 - i] < other[$len - 1 - i] {
                        return ::std::cmp::Ordering::Less;
                    }
                    if self[$len - 1 - i] > other[$len - 1 - i] {
                        return ::std::cmp::Ordering::Greater;
                    }
                }
                ::std::cmp::Ordering::Equal
            }
        }

        impl Clone for $thing {
            #[inline]
            fn clone(&self) -> $thing {
                *self
            }
        }

        impl Copy for $thing {}

        impl ::std::hash::Hash for $thing {
            #[inline]
            fn hash<H>(&self, state: &mut H)
            where
                H: ::std::hash::Hasher,
            {
                (&self[..]).hash(state);
            }

            fn hash_slice<H>(data: &[$thing], state: &mut H)
            where
                H: ::std::hash::Hasher,
            {
                for d in data.iter() {
                    (&d[..]).hash(state);
                }
            }
        }
    };
}

#[macro_export]
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

#[macro_export]
macro_rules! impl_array_hexstring_fmt {
    ($thing:ident) => {
        impl ::std::fmt::Debug for $thing {
            fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
                let &$thing(data) = self;
                for ch in data.iter() {
                    write!(f, "{:02x}", ch)?;
                }
                Ok(())
            }
        }
    };
}

#[allow(unused_macros)]
#[macro_export]
macro_rules! impl_byte_array_newtype {
    ($thing:ident, $ty:ty, $len:expr) => {
        impl $thing {
            /// Instantiates from a hex string
            #[allow(dead_code)]
            pub fn from_hex(hex_str: &str) -> Result<$thing, $crate::util::HexError> {
                use $crate::util::hash::hex_bytes;
                let _hex_len = $len * 2;
                match (hex_str.len(), hex_bytes(hex_str)) {
                    (_hex_len, Ok(bytes)) => {
                        if bytes.len() != $len {
                            return Err($crate::util::HexError::BadLength(hex_str.len()));
                        }
                        let mut ret = [0; $len];
                        ret.copy_from_slice(&bytes);
                        Ok($thing(ret))
                    }
                    (_, Err(e)) => Err(e),
                }
            }

            /// Instantiates from a slice of bytes
            /// Note: if this type is a hashing type, this sets the hash result to `inp` exactly: this method does **not** perform the hash.
            #[allow(dead_code)]
            pub fn from_bytes(inp: &[u8]) -> Option<$thing> {
                match inp.len() {
                    $len => {
                        let mut ret = [0; $len];
                        ret.copy_from_slice(inp);
                        Some($thing(ret))
                    }
                    _ => None,
                }
            }

            /// Instantiates from a slice of bytes, converting to host byte order
            #[allow(dead_code)]
            pub fn from_bytes_be(inp: &[u8]) -> Option<$thing> {
                $thing::from_vec_be(&inp.to_vec())
            }

            /// Instantiates from a vector of bytes
            #[allow(dead_code)]
            pub fn from_vec(inp: &[u8]) -> Option<$thing> {
                match inp.len() {
                    $len => {
                        let mut ret = [0; $len];
                        let bytes = &inp[..inp.len()];
                        ret.copy_from_slice(&bytes);
                        Some($thing(ret))
                    }
                    _ => None,
                }
            }

            /// Instantiates from a big-endian vector of bytes, converting to host byte order
            #[allow(dead_code)]
            pub fn from_vec_be(b: &[u8]) -> Option<$thing> {
                match b.len() {
                    $len => {
                        let mut ret = [0; $len];
                        let bytes = &b[0..b.len()];
                        // flip endian to le if we are le
                        for i in 0..$len {
                            ret[$len - 1 - i] = bytes[i];
                        }
                        Some($thing(ret))
                    }
                    _ => None,
                }
            }

            /// Convert to a hex string
            #[allow(dead_code)]
            pub fn to_hex(&self) -> String {
                use $crate::util::hash::to_hex;
                to_hex(&self.0)
            }
        }
        impl std::fmt::LowerHex for $thing {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "{}", self.to_hex())
            }
        }
        impl std::fmt::Display for $thing {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "{}", self.to_hex())
            }
        }
        impl std::convert::AsRef<[u8]> for $thing {
            fn as_ref(&self) -> &[u8] {
                &self.0
            }
        }
        impl std::convert::From<[u8; $len]> for $thing {
            fn from(o: [u8; $len]) -> Self {
                Self(o)
            }
        }
    };
}

#[allow(unused_macros)]
#[macro_export]
macro_rules! impl_byte_array_serde {
    ($thing:ident) => {
        impl serde::Serialize for $thing {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                let inst = self.to_hex();
                s.serialize_str(inst.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $thing {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<$thing, D::Error> {
                let inst_str = String::deserialize(d)?;
                $thing::from_hex(&inst_str).map_err(serde::de::Error::custom)
            }
        }
    };
}

#[allow(unused_macros)]
#[macro_export]
macro_rules! impl_file_io_serde_json {
    ($thing:ident) => {
        impl $thing {
            pub fn serialize_to_file<P>(&self, path: P) -> Result<(), std::io::Error>
            where
                P: AsRef<std::path::Path>,
            {
                $crate::util::serialize_json_to_file(self, path)
            }

            pub fn deserialize_from_file<P>(path: P) -> Result<Self, std::io::Error>
            where
                P: AsRef<std::path::Path>,
            {
                $crate::util::deserialize_json_from_file(path)
            }
        }
    };
}

// print debug statements while testing
#[allow(unused_macros)]
#[macro_export]
macro_rules! test_debug {
    ($($arg:tt)*) => (
        #[cfg(any(test, feature = "testing"))]
        {
            use std::env;
            if env::var("BLOCKSTACK_DEBUG") == Ok("1".to_string()) {
                debug!($($arg)*);
            }
        }
    )
}

#[cfg(test)]
pub const TRACE_ENABLED: bool = true;

#[cfg(test)]
pub fn is_trace() -> bool {
    use std::env;
    TRACE_ENABLED && env::var("BLOCKSTACK_TRACE") == Ok("1".to_string())
}

#[cfg(not(test))]
#[inline]
pub fn is_trace() -> bool {
    false
}

#[allow(unused_macros)]
macro_rules! trace {
    ($($arg:tt)*) => (
        #[cfg(any(test, feature = "testing"))]
        {
            if $crate::util::macros::is_trace() {
                debug!($($arg)*);
            }
        }
    )
}

#[macro_export]
macro_rules! fmin {
    ($x: expr) => ($x);
    ($x: expr, $($z: expr),+) => {{
        let y = fmin!($($z),*);
        if $x < y {
            $x
        } else {
            y
        }
    }}
}

#[macro_export]
macro_rules! fmax {
    ($x: expr) => ($x);
    ($x: expr, $($z: expr),+) => {{
        let y = fmax!($($z),*);
        if $x > y {
            $x
        } else {
            y
        }
    }}
}

#[cfg(feature = "canonical")]
macro_rules! impl_byte_array_rusqlite_only {
    ($thing:ident) => {
        impl rusqlite::types::FromSql for $thing {
            fn column_result(
                value: rusqlite::types::ValueRef,
            ) -> rusqlite::types::FromSqlResult<Self> {
                let hex_str = value.as_str()?;
                let byte_str = $crate::util::hash::hex_bytes(hex_str)
                    .map_err(|_e| rusqlite::types::FromSqlError::InvalidType)?;
                let inst = $thing::from_bytes(&byte_str)
                    .ok_or(rusqlite::types::FromSqlError::InvalidType)?;
                Ok(inst)
            }
        }

        impl rusqlite::types::ToSql for $thing {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput> {
                let hex_str = self.to_hex();
                Ok(hex_str.into())
            }
        }
    };
}

// Test hepler to get the name of the current function.
#[macro_export]
macro_rules! function_name {
    () => {
        stdext::function_name!()
    };
}
