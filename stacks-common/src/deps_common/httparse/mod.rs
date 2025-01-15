/*
 copyright: (c) 2013-2020 by Blockstack PBC, a public benefit corporation.

 This file is part of Blockstack.

 Blockstack is free software. You may redistribute or modify
 it under the terms of the GNU General Public License as published by
 the Free Software Foundation, either version 3 of the License or
 (at your option) any later version.

 Blockstack is distributed in the hope that it will be useful,
 but WITHOUT ANY WARRANTY, including without the implied warranty of
 MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 GNU General Public License for more details.

 You should have received a copy of the GNU General Public License
 along with Blockstack. If not, see <http://www.gnu.org/licenses/>.
*/

// Based on code by Sean McArthur (https://github.com/seanmonstar/httparse)

#![cfg_attr(test, deny(warnings))]
// we can't upgrade while supporting Rust 1.3
#![allow(deprecated)]

//! # httparse
//!
//! A push library for parsing HTTP/1.x requests and responses.
//!
//! Originally written by Sean McArthur.
//!
//! Modified by Jude Nelson to remove all unsafe code.
use std::{fmt, mem, result, str};

macro_rules! next {
    ($bytes:ident) => {{
        match $bytes.next() {
            Some(b) => b,
            None => return Ok(Status::Partial),
        }
    }};
}

macro_rules! expect {
    ($bytes:ident.next() == $pat:pat => $ret:expr) => {
        expect!(next!($bytes) => $pat |? $ret)
    };
    ($e:expr => $pat:pat_param |? $ret:expr) => {
        match $e {
            v@$pat => v,
            _ => return $ret
        }
    };
}

macro_rules! complete {
    ($e:expr) => {
        match $e? {
            Status::Complete(v) => v,
            Status::Partial => return Ok(Status::Partial),
        }
    };
}

macro_rules! byte_map {
    ($($flag:expr,)*) => ([
        $($flag != 0,)*
    ])
}

macro_rules! space {
    ($bytes:ident or $err:expr) => ({
        expect!($bytes.next() == b' ' => Err($err));
        $bytes.slice();
    })
}

macro_rules! newline {
    ($bytes:ident) => ({
        match next!($bytes) {
            b'\r' => {
                expect!($bytes.next() == b'\n' => Err(Error::NewLine));
                $bytes.slice();
            },
            b'\n' => {
                $bytes.slice();
            },
            _ => return Err(Error::NewLine)
        }
    })
}

pub struct Bytes<'a> {
    slice: &'a [u8],
    pos: usize,
    skipped_pos: usize,
}

impl<'a> Bytes<'a> {
    #[inline]
    pub fn new(slice: &'a [u8]) -> Bytes<'a> {
        Bytes {
            slice,
            pos: 0,
            skipped_pos: 0,
        }
    }

    #[inline]
    pub fn pos(&self) -> usize {
        self.pos
    }

    #[inline]
    pub fn peek(&self) -> Option<u8> {
        self.slice_peek().get(self.pos).cloned()
    }

    #[inline]
    pub fn bump(&mut self) {
        assert!(self.pos < self.slice_peek().len(), "overflow");
        self.pos += 1;
    }

    #[allow(unused)]
    #[inline]
    pub fn advance(&mut self, n: usize) {
        assert!(self.pos + n <= self.slice_peek().len(), "overflow");
        self.pos += n;
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.slice_peek().len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.slice_peek().is_empty()
    }

    #[inline]
    pub fn slice_peek(&self) -> &'a [u8] {
        &self.slice[self.skipped_pos..]
    }

    #[inline]
    pub fn slice(&mut self) -> &'a [u8] {
        self.slice_skip(0)
    }

    #[inline]
    pub fn slice_skip(&mut self, skip: usize) -> &'a [u8] {
        assert!(self.pos >= skip);
        let head_pos = self.pos - skip;
        let head = &self.slice_peek()[0..head_pos];
        self.skipped_pos += self.pos;
        self.pos = 0;
        head
    }

    #[inline]
    pub fn next_8<'b>(&'b mut self) -> Option<Bytes8<'b, 'a>> {
        if self.slice_peek().len() > self.pos + 8 {
            Some(Bytes8::new(self))
        } else {
            None
        }
    }
}

impl AsRef<[u8]> for Bytes<'_> {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        &self.slice_peek()[self.pos..]
    }
}

impl Iterator for Bytes<'_> {
    type Item = u8;

    #[inline]
    fn next(&mut self) -> Option<u8> {
        if self.slice_peek().len() > self.pos {
            let b = self
                .slice_peek()
                .get(self.pos)
                .expect("BUG: read beyond end of buffer");
            self.pos += 1;
            Some(*b)
        } else {
            None
        }
    }
}

pub struct Bytes8<'a, 'b: 'a> {
    bytes: &'a mut Bytes<'b>,
    pos: usize,
}

macro_rules! bytes8_methods {
    ($f:ident, $pos:expr) => {
        #[inline]
        pub fn $f(&mut self) -> u8 {
            self.assert_pos($pos);
            let b = self
                .bytes
                .slice_peek()
                .get(self.bytes.pos)
                .expect("BUG: read beyond end of buffer");
            self.bytes.pos += 1;
            *b
        }
    };
    () => {
        bytes8_methods!(_0, 0);
        bytes8_methods!(_1, 1);
        bytes8_methods!(_2, 2);
        bytes8_methods!(_3, 3);
        bytes8_methods!(_4, 4);
        bytes8_methods!(_5, 5);
        bytes8_methods!(_6, 6);
        bytes8_methods!(_7, 7);
    };
}

impl<'a, 'b: 'a> Bytes8<'a, 'b> {
    bytes8_methods! {}

    #[inline]
    fn new(bytes: &'a mut Bytes<'b>) -> Bytes8<'a, 'b> {
        Bytes8 { bytes, pos: 0 }
    }

    #[inline]
    fn assert_pos(&mut self, pos: usize) {
        assert!(self.pos == pos);
        self.pos += 1;
    }
}

#[inline]
fn shrink<T>(slice: &mut &mut [T], len: usize) {
    assert!(slice.len() >= len);
    let full = mem::take(slice);
    *slice = &mut full[..len];
}

/// Determines if byte is a token char.
///
/// > ```notrust
/// > token          = 1*tchar
/// >
/// > tchar          = "!" / "#" / "$" / "%" / "&" / "'" / "*"
/// >                / "+" / "-" / "." / "^" / "_" / "`" / "|" / "~"
/// >                / DIGIT / ALPHA
/// >                ; any VCHAR, except delimiters
/// > ```
#[inline]
fn is_token(b: u8) -> bool {
    b > 0x1F && b < 0x7F
}

// ASCII codes to accept URI string.
// i.e. A-Z a-z 0-9 !#$%&'*+-._();:@=,/?[]~^
// TODO: Make a stricter checking for URI string?
static URI_MAP: [bool; 256] = byte_map![
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //  \0                            \n
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //  commands
    0, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    //  \w !  "  #  $  %  &  '  (  )  *  +  ,  -  .  /
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1, 0, 1,
    //  0  1  2  3  4  5  6  7  8  9  :  ;  <  =  >  ?
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    //  @  A  B  C  D  E  F  G  H  I  J  K  L  M  N  O
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    //  P  Q  R  S  T  U  V  W  X  Y  Z  [  \  ]  ^  _
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    //  `  a  b  c  d  e  f  g  h  i  j  k  l  m  n  o
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0,
    //  p  q  r  s  t  u  v  w  x  y  z  {  |  }  ~  del
    //   ====== Extended ASCII (aka. obs-text) ======
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

#[inline]
fn is_uri_token(b: u8) -> bool {
    URI_MAP[b as usize]
}

static HEADER_NAME_MAP: [bool; 256] = byte_map![
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 1, 0, 1, 1, 1, 1, 1, 0, 0, 1, 1, 0, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0,
    0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1, 0, 1, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

#[inline]
fn is_header_name_token(b: u8) -> bool {
    HEADER_NAME_MAP[b as usize]
}

static HEADER_VALUE_MAP: [bool; 256] = byte_map![
    0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
];

#[inline]
fn is_header_value_token(b: u8) -> bool {
    HEADER_VALUE_MAP[b as usize]
}

/// An error in parsing.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Error {
    /// Invalid byte in header name.
    HeaderName,
    /// Invalid byte in header value.
    HeaderValue,
    /// Invalid byte in new line.
    NewLine,
    /// Invalid byte in Response status.
    Status,
    /// Invalid byte where token is required.
    Token,
    /// Parsed more headers than provided buffer can contain.
    TooManyHeaders,
    /// Invalid byte in HTTP version.
    Version,
    /// Invalid chunk size
    ChunkSize,
}

impl Error {
    #[inline]
    fn description_str(&self) -> &'static str {
        match *self {
            Error::HeaderName => "invalid header name",
            Error::HeaderValue => "invalid header value",
            Error::NewLine => "invalid new line",
            Error::Status => "invalid response status",
            Error::Token => "invalid token",
            Error::TooManyHeaders => "too many headers",
            Error::Version => "invalid HTTP version",
            Error::ChunkSize => "invalid chunk size",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.description_str())
    }
}

/*
impl error::Error for Error {
    fn description(&self) -> &str {
        self.description_str()
    }
}
*/

/// A Result of any parsing action.
///
/// If the input is invalid, an `Error` will be returned. Note that incomplete
/// data is not considered invalid, and so will not return an error, but rather
/// a `Ok(Status::Partial)`.
pub type Result<T> = result::Result<Status<T>, Error>;

/// The result of a successful parse pass.
///
/// `Complete` is used when the buffer contained the complete value.
/// `Partial` is used when parsing did not reach the end of the expected value,
/// but no invalid data was found.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Status<T> {
    /// The completed result.
    Complete(T),
    /// A partial result.
    Partial,
}

impl<T> Status<T> {
    /// Convenience method to check if status is complete.
    #[inline]
    pub fn is_complete(&self) -> bool {
        match *self {
            Status::Complete(..) => true,
            Status::Partial => false,
        }
    }

    /// Convenience method to check if status is partial.
    #[inline]
    pub fn is_partial(&self) -> bool {
        match *self {
            Status::Complete(..) => false,
            Status::Partial => true,
        }
    }

    /// Convenience method to unwrap a Complete value. Panics if the status is
    /// `Partial`.
    #[inline]
    pub fn unwrap(self) -> T {
        match self {
            Status::Complete(t) => t,
            Status::Partial => panic!("Tried to unwrap Status::Partial"),
        }
    }
}

/// A parsed Request.
///
/// The optional values will be `None` if a parse was not complete, and did not
/// parse the associated property. This allows you to inspect the parts that
/// could be parsed, before reading more, in case you wish to exit early.
#[derive(Debug, PartialEq)]
pub struct Request<'headers, 'buf: 'headers> {
    /// The request method, such as `GET`.
    pub method: Option<&'buf str>,
    /// The request path, such as `/about-us`.
    pub path: Option<&'buf str>,
    /// The request version, such as `HTTP/1.1`.
    pub version: Option<u8>,
    /// The request headers.
    pub headers: &'headers mut [Header<'buf>],
}

impl<'h, 'b> Request<'h, 'b> {
    /// Creates a new Request, using a slice of headers you allocate.
    #[inline]
    pub fn new(headers: &'h mut [Header<'b>]) -> Request<'h, 'b> {
        Request {
            method: None,
            path: None,
            version: None,
            headers,
        }
    }

    /// Try to parse a buffer of bytes into the Request.
    pub fn parse(&mut self, buf: &'b [u8]) -> Result<usize> {
        let orig_len = buf.len();
        let mut bytes = Bytes::new(buf);
        complete!(skip_empty_lines(&mut bytes));
        self.method = Some(complete!(parse_token(&mut bytes)));
        self.path = Some(complete!(parse_uri(&mut bytes)));
        self.version = Some(complete!(parse_version(&mut bytes)));
        newline!(bytes);

        let len = orig_len - bytes.len();
        let headers_len = complete!(parse_headers_iter(&mut self.headers, &mut bytes));

        Ok(Status::Complete(len + headers_len))
    }
}

#[inline]
fn skip_empty_lines(bytes: &mut Bytes) -> Result<()> {
    loop {
        let b = bytes.peek();
        match b {
            Some(b'\r') => {
                // there's `\r`, so it's safe to bump 1 pos
                bytes.bump();
                expect!(bytes.next() == b'\n' => Err(Error::NewLine));
            }
            Some(b'\n') => {
                // there's `\n`, so it's safe to bump 1 pos
                bytes.bump();
            }
            Some(..) => {
                bytes.slice();
                return Ok(Status::Complete(()));
            }
            None => return Ok(Status::Partial),
        }
    }
}

/// A parsed Response.
///
/// See `Request` docs for explanation of optional values.
#[derive(Debug, PartialEq)]
pub struct Response<'headers, 'buf: 'headers> {
    /// The response version, such as `HTTP/1.1`.
    pub version: Option<u8>,
    /// The response code, such as `200`.
    pub code: Option<u16>,
    /// The response reason-phrase, such as `OK`.
    pub reason: Option<&'buf str>,
    /// The response headers.
    pub headers: &'headers mut [Header<'buf>],
}

impl<'h, 'b> Response<'h, 'b> {
    /// Creates a new `Response` using a slice of `Header`s you have allocated.
    #[inline]
    pub fn new(headers: &'h mut [Header<'b>]) -> Response<'h, 'b> {
        Response {
            version: None,
            code: None,
            reason: None,
            headers,
        }
    }

    /// Try to parse a buffer of bytes into this `Response`.
    pub fn parse(&mut self, buf: &'b [u8]) -> Result<usize> {
        let orig_len = buf.len();
        let mut bytes = Bytes::new(buf);

        complete!(skip_empty_lines(&mut bytes));
        self.version = Some(complete!(parse_version(&mut bytes)));
        space!(bytes or Error::Version);
        self.code = Some(complete!(parse_code(&mut bytes)));

        // RFC7230 says there must be 'SP' and then reason-phrase, but admits
        // its only for legacy reasons. With the reason-phrase completely
        // optional (and preferred to be omitted) in HTTP2, we'll just
        // handle any response that doesn't include a reason-phrase, because
        // it's more lenient, and we don't care anyways.
        //
        // So, a SP means parse a reason-phrase.
        // A newline means go to headers.
        // Anything else we'll say is a malformed status.
        match next!(bytes) {
            b' ' => {
                bytes.slice();
                self.reason = Some(complete!(parse_reason(&mut bytes)));
            }
            b'\r' => {
                expect!(bytes.next() == b'\n' => Err(Error::Status));
                bytes.slice();
                self.reason = Some("");
            }
            b'\n' => self.reason = Some(""),
            _ => return Err(Error::Status),
        }

        let len = orig_len - bytes.len();
        let headers_len = complete!(parse_headers_iter(&mut self.headers, &mut bytes));
        Ok(Status::Complete(len + headers_len))
    }
}

/// Represents a parsed header.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Header<'a> {
    /// The name portion of a header.
    ///
    /// A header name must be valid ASCII-US, so it's safe to store as a `&str`.
    pub name: &'a str,
    /// The value portion of a header.
    ///
    /// While headers **should** be ASCII-US, the specification allows for
    /// values that may not be, and so the value is stored as bytes.
    pub value: &'a [u8],
}

/// An empty header, useful for constructing a `Header` array to pass in for
/// parsing.
pub const EMPTY_HEADER: Header<'static> = Header {
    name: "",
    value: b"",
};

#[inline]
fn parse_version(bytes: &mut Bytes) -> Result<u8> {
    if let Some(mut eight) = bytes.next_8() {
        expect!(eight._0() => b'H' |? Err(Error::Version));
        expect!(eight._1() => b'T' |? Err(Error::Version));
        expect!(eight._2() => b'T' |? Err(Error::Version));
        expect!(eight._3() => b'P' |? Err(Error::Version));
        expect!(eight._4() => b'/' |? Err(Error::Version));
        expect!(eight._5() => b'1' |? Err(Error::Version));
        expect!(eight._6() => b'.' |? Err(Error::Version));
        let v = match eight._7() {
            b'0' => 0,
            b'1' => 1,
            _ => return Err(Error::Version),
        };
        return Ok(Status::Complete(v));
    }

    // else (but not in `else` because of borrow checker)

    // If there aren't at least 8 bytes, we still want to detect early
    // if this is a valid version or not. If it is, we'll return Partial.
    expect!(bytes.next() == b'H' => Err(Error::Version));
    expect!(bytes.next() == b'T' => Err(Error::Version));
    expect!(bytes.next() == b'T' => Err(Error::Version));
    expect!(bytes.next() == b'P' => Err(Error::Version));
    expect!(bytes.next() == b'/' => Err(Error::Version));
    expect!(bytes.next() == b'1' => Err(Error::Version));
    expect!(bytes.next() == b'.' => Err(Error::Version));
    Ok(Status::Partial)
}

/// From [RFC 7230](https://tools.ietf.org/html/rfc7230):
///
/// > ```notrust
/// > reason-phrase  = *( HTAB / SP / VCHAR / obs-text )
/// > HTAB           = %x09        ; horizontal tab
/// > VCHAR          = %x21-7E     ; visible (printing) characters
/// > obs-text       = %x80-FF
/// > ```
///
/// > A.2.  Changes from RFC 2616
/// >
/// > Non-US-ASCII content in header fields and the reason phrase
/// > has been obsoleted and made opaque (the TEXT rule was removed).
///
/// Note that the following implementation deliberately rejects the obsoleted (non-US-ASCII) text range.
///
/// The fully compliant parser should probably just return the reason-phrase as an opaque &[u8] data
/// and leave interpretation to user or specialized helpers (akin to .display() in std::path::Path)
#[inline]
fn parse_reason<'a>(bytes: &mut Bytes<'a>) -> Result<&'a str> {
    loop {
        let b = next!(bytes);
        if b == b'\r' {
            expect!(bytes.next() == b'\n' => Err(Error::Status));
            let res = str::from_utf8(bytes.slice_skip(2)).map_err(|_e| Error::Status)?;
            return Ok(Status::Complete(res));
        } else if b == b'\n' {
            let res = str::from_utf8(bytes.slice_skip(1)).map_err(|_e| Error::Status)?;
            return Ok(Status::Complete(res));
        } else if !((0x20..=0x7E).contains(&b) || b == b'\t') {
            return Err(Error::Status);
        }
    }
}

#[inline]
fn parse_token<'a>(bytes: &mut Bytes<'a>) -> Result<&'a str> {
    loop {
        let b = next!(bytes);
        if b == b' ' {
            let res = str::from_utf8(bytes.slice_skip(1)).map_err(|_e| Error::Token)?;
            return Ok(Status::Complete(res));
        } else if !is_token(b) {
            return Err(Error::Token);
        }
    }
}

#[inline]
fn parse_uri<'a>(bytes: &mut Bytes<'a>) -> Result<&'a str> {
    loop {
        let b = next!(bytes);
        if b == b' ' {
            let res = str::from_utf8(bytes.slice_skip(1)).map_err(|_e| Error::Token)?;
            return Ok(Status::Complete(res));
        } else if !is_uri_token(b) {
            return Err(Error::Token);
        }
    }
}

#[inline]
fn parse_code(bytes: &mut Bytes) -> Result<u16> {
    let hundreds = expect!(bytes.next() == b'0'..=b'9' => Err(Error::Status));
    let tens = expect!(bytes.next() == b'0'..=b'9' => Err(Error::Status));
    let ones = expect!(bytes.next() == b'0'..=b'9' => Err(Error::Status));

    Ok(Status::Complete(
        (hundreds - b'0') as u16 * 100 + (tens - b'0') as u16 * 10 + (ones - b'0') as u16,
    ))
}

/// Parse a buffer of bytes as headers.
///
/// The return value, if complete and successful, includes the index of the
/// buffer that parsing stopped at, and a sliced reference to the parsed
/// headers. The length of the slice will be equal to the number of properly
/// parsed headers.
pub fn parse_headers<'b: 'h, 'h>(
    src: &'b [u8],
    mut dst: &'h mut [Header<'b>],
) -> Result<(usize, &'h [Header<'b>])> {
    let mut iter = Bytes::new(src);
    let pos = complete!(parse_headers_iter(&mut dst, &mut iter));
    Ok(Status::Complete((pos, dst)))
}

#[inline]
#[allow(clippy::never_loop)]
fn parse_headers_iter<'a>(headers: &mut &mut [Header<'a>], bytes: &mut Bytes<'a>) -> Result<usize> {
    let mut num_headers: usize = 0;
    let mut count: usize = 0;
    let mut result = Err(Error::TooManyHeaders);

    {
        let mut iter = headers.iter_mut();

        'headers: loop {
            // a newline here means the head is over!
            let b = next!(bytes);
            if b == b'\r' {
                expect!(bytes.next() == b'\n' => Err(Error::NewLine));
                result = Ok(Status::Complete(count + bytes.pos()));
                break;
            } else if b == b'\n' {
                result = Ok(Status::Complete(count + bytes.pos()));
                break;
            } else if !is_header_name_token(b) {
                return Err(Error::HeaderName);
            }

            let header = match iter.next() {
                Some(header) => header,
                None => break 'headers,
            };

            num_headers += 1;
            // parse header name until colon
            'name: loop {
                let b = next!(bytes);
                if b == b':' {
                    count += bytes.pos();
                    header.name =
                        str::from_utf8(bytes.slice_skip(1)).map_err(|_e| Error::HeaderName)?;
                    break 'name;
                } else if !is_header_name_token(b) {
                    return Err(Error::HeaderName);
                }
            }

            let mut b;

            'value: loop {
                // eat white space between colon and value
                'whitespace: loop {
                    b = next!(bytes);
                    if b == b' ' || b == b'\t' {
                        count += bytes.pos();
                        bytes.slice();
                        continue 'whitespace;
                    } else {
                        if !is_header_value_token(b) {
                            break 'value;
                        }
                        break 'whitespace;
                    }
                }

                // parse value till EOL

                macro_rules! check {
                    ($bytes:ident, $i:ident) => {{
                        b = $bytes.$i();
                        if !is_header_value_token(b) {
                            break 'value;
                        }
                    }};
                    ($bytes:ident) => {{
                        check!($bytes, _0);
                        check!($bytes, _1);
                        check!($bytes, _2);
                        check!($bytes, _3);
                        check!($bytes, _4);
                        check!($bytes, _5);
                        check!($bytes, _6);
                        check!($bytes, _7);
                    }};
                }
                while let Some(mut bytes8) = bytes.next_8() {
                    check!(bytes8);
                }
                loop {
                    b = next!(bytes);
                    if !is_header_value_token(b) {
                        break 'value;
                    }
                }
            }

            //found_ctl
            let value_slice: &[u8] = if b == b'\r' {
                expect!(bytes.next() == b'\n' => Err(Error::HeaderValue));
                count += bytes.pos();
                // having just check that `\r\n` exists, it's safe to skip those 2 bytes
                bytes.slice_skip(2)
            } else if b == b'\n' {
                count += bytes.pos();
                // having just check that `\r\n` exists, it's safe to skip 1 byte
                bytes.slice_skip(1)
            } else {
                return Err(Error::HeaderValue);
            };
            // trim trailing whitespace in the header
            if let Some(last_visible) = value_slice.iter().rposition(|b| *b != b' ' && *b != b'\t')
            {
                // There is at least one non-whitespace character.
                header.value = &value_slice[0..last_visible + 1];
            } else {
                // There is no non-whitespace character. This can only happen when value_slice is
                // empty.
                header.value = value_slice;
            }
        }
    } // drop iter

    shrink(headers, num_headers);
    result
}

/// Parse a buffer of bytes as a chunk size.
///
/// The return value, if complete and successful, includes the index of the
/// buffer that parsing stopped at, and the size of the following chunk.
pub fn parse_chunk_size(buf: &[u8]) -> result::Result<Status<(usize, u64)>, Error> {
    const RADIX: u64 = 16;
    let mut bytes = Bytes::new(buf);
    let mut size = 0u64;
    let mut in_chunk_size = true;
    let mut in_ext = false;
    let mut count = 0;
    loop {
        let b = next!(bytes);
        match b {
            b'0'..=b'9' if in_chunk_size => {
                if count > 15 {
                    return Err(Error::ChunkSize);
                }
                count += 1;
                size = size.checked_mul(RADIX).ok_or(Error::ChunkSize)?;
                size = size
                    .checked_add((b - b'0') as u64)
                    .ok_or(Error::ChunkSize)?;
            }
            b'a'..=b'f' if in_chunk_size => {
                if count > 15 {
                    return Err(Error::ChunkSize);
                }
                count += 1;
                size = size.checked_mul(RADIX).ok_or(Error::ChunkSize)?;
                size = size
                    .checked_add((b + 10 - b'a') as u64)
                    .ok_or(Error::ChunkSize)?;
            }
            b'A'..=b'F' if in_chunk_size => {
                if count > 15 {
                    return Err(Error::ChunkSize);
                }
                count += 1;
                size = size.checked_mul(RADIX).ok_or(Error::ChunkSize)?;
                size = size
                    .checked_add((b + 10 - b'A') as u64)
                    .ok_or(Error::ChunkSize)?;
            }
            b'\r' => match next!(bytes) {
                b'\n' => break,
                _ => return Err(Error::ChunkSize),
            },
            // If we weren't in the extension yet, the ";" signals its start
            b';' if !in_ext => {
                in_ext = true;
                in_chunk_size = false;
            }
            // "Linear white space" is ignored between the chunk size and the
            // extension separator token (";") due to the "implied *LWS rule".
            b'\t' | b' ' if !in_ext & !in_chunk_size => {}
            // LWS can follow the chunk size, but no more digits can come
            b'\t' | b' ' if in_chunk_size => in_chunk_size = false,
            // We allow any arbitrary octet once we are in the extension, since
            // they all get ignored anyway. According to the HTTP spec, valid
            // extensions would have a more strict syntax:
            //     (token ["=" (token | quoted-string)])
            // but we gain nothing by rejecting an otherwise valid chunk size.
            _ if in_ext => {}
            // Finally, if we aren't in the extension and we're reading any
            // other octet, the chunk size line is invalid!
            _ => return Err(Error::ChunkSize),
        }
    }
    Ok(Status::Complete((bytes.pos(), size)))
}

#[cfg(test)]
mod tests {
    use super::{parse_chunk_size, shrink, Error, Request, Response, Status, EMPTY_HEADER};

    const NUM_OF_HEADERS: usize = 4;

    #[test]
    fn test_shrink() {
        let mut arr = [EMPTY_HEADER; 16];
        {
            let slice = &mut &mut arr[..];
            assert_eq!(slice.len(), 16);
            shrink(slice, 4);
            assert_eq!(slice.len(), 4);
        }
        assert_eq!(arr.len(), 16);
    }

    macro_rules! req {
        ($name:ident, $buf:expr, |$arg:ident| $body:expr) => {
            req! {$name, $buf, Ok(Status::Complete($buf.len())), |$arg| $body }
        };
        ($name:ident, $buf:expr, $len:expr, |$arg:ident| $body:expr) => {
            #[test]
            fn $name() {
                let mut headers = [EMPTY_HEADER; NUM_OF_HEADERS];
                let mut req = Request::new(&mut headers[..]);
                let status = req.parse($buf.as_ref());
                assert_eq!(status, $len);
                closure(req);

                fn closure($arg: Request) {
                    $body
                }
            }
        };
    }

    req! {
        test_request_simple,
        b"GET / HTTP/1.1\r\n\r\n",
        |req| {
            assert_eq!(req.method.unwrap(), "GET");
            assert_eq!(req.path.unwrap(), "/");
            assert_eq!(req.version.unwrap(), 1);
            assert_eq!(req.headers.len(), 0);
        }
    }

    req! {
        test_request_simple_with_query_params,
        b"GET /thing?data=a HTTP/1.1\r\n\r\n",
        |req| {
            assert_eq!(req.method.unwrap(), "GET");
            assert_eq!(req.path.unwrap(), "/thing?data=a");
            assert_eq!(req.version.unwrap(), 1);
            assert_eq!(req.headers.len(), 0);
        }
    }

    req! {
        test_request_simple_with_whatwg_query_params,
        b"GET /thing?data=a^ HTTP/1.1\r\n\r\n",
        |req| {
            assert_eq!(req.method.unwrap(), "GET");
            assert_eq!(req.path.unwrap(), "/thing?data=a^");
            assert_eq!(req.version.unwrap(), 1);
            assert_eq!(req.headers.len(), 0);
        }
    }

    req! {
        test_request_headers,
        b"GET / HTTP/1.1\r\nHost: foo.com\r\nCookie: \r\n\r\n",
        |req| {
            assert_eq!(req.method.unwrap(), "GET");
            assert_eq!(req.path.unwrap(), "/");
            assert_eq!(req.version.unwrap(), 1);
            assert_eq!(req.headers.len(), 2);
            assert_eq!(req.headers[0].name, "Host");
            assert_eq!(req.headers[0].value, b"foo.com");
            assert_eq!(req.headers[1].name, "Cookie");
            assert_eq!(req.headers[1].value, b"");
        }
    }

    req! {
        test_request_headers_optional_whitespace,
        b"GET / HTTP/1.1\r\nHost: \tfoo.com\t \r\nCookie: \t \r\n\r\n",
        |req| {
            assert_eq!(req.method.unwrap(), "GET");
            assert_eq!(req.path.unwrap(), "/");
            assert_eq!(req.version.unwrap(), 1);
            assert_eq!(req.headers.len(), 2);
            assert_eq!(req.headers[0].name, "Host");
            assert_eq!(req.headers[0].value, b"foo.com");
            assert_eq!(req.headers[1].name, "Cookie");
            assert_eq!(req.headers[1].value, b"");
        }
    }

    req! {
        // test the scalar parsing
        test_request_header_value_htab_short,
        b"GET / HTTP/1.1\r\nUser-Agent: some\tagent\r\n\r\n",
        |req| {
            assert_eq!(req.method.unwrap(), "GET");
            assert_eq!(req.path.unwrap(), "/");
            assert_eq!(req.version.unwrap(), 1);
            assert_eq!(req.headers.len(), 1);
            assert_eq!(req.headers[0].name, "User-Agent");
            assert_eq!(req.headers[0].value, b"some\tagent");
        }
    }

    req! {
        // test the sse42 parsing
        test_request_header_value_htab_med,
        b"GET / HTTP/1.1\r\nUser-Agent: 1234567890some\tagent\r\n\r\n",
        |req| {
            assert_eq!(req.method.unwrap(), "GET");
            assert_eq!(req.path.unwrap(), "/");
            assert_eq!(req.version.unwrap(), 1);
            assert_eq!(req.headers.len(), 1);
            assert_eq!(req.headers[0].name, "User-Agent");
            assert_eq!(req.headers[0].value, b"1234567890some\tagent");
        }
    }

    req! {
        // test the avx2 parsing
        test_request_header_value_htab_long,
        b"GET / HTTP/1.1\r\nUser-Agent: 1234567890some\t1234567890agent1234567890\r\n\r\n",
        |req| {
            assert_eq!(req.method.unwrap(), "GET");
            assert_eq!(req.path.unwrap(), "/");
            assert_eq!(req.version.unwrap(), 1);
            assert_eq!(req.headers.len(), 1);
            assert_eq!(req.headers[0].name, "User-Agent");
            assert_eq!(req.headers[0].value, &b"1234567890some\t1234567890agent1234567890"[..]);
        }
    }

    req! {
        test_request_headers_max,
        b"GET / HTTP/1.1\r\nA: A\r\nB: B\r\nC: C\r\nD: D\r\n\r\n",
        |req| {
            assert_eq!(req.headers.len(), NUM_OF_HEADERS);
        }
    }

    req! {
        test_request_multibyte,
        b"GET / HTTP/1.1\r\nHost: foo.com\r\nUser-Agent: \xe3\x81\xb2\xe3/1.0\r\n\r\n",
        |req| {
            assert_eq!(req.method.unwrap(), "GET");
            assert_eq!(req.path.unwrap(), "/");
            assert_eq!(req.version.unwrap(), 1);
            assert_eq!(req.headers[0].name, "Host");
            assert_eq!(req.headers[0].value, b"foo.com");
            assert_eq!(req.headers[1].name, "User-Agent");
            assert_eq!(req.headers[1].value, b"\xe3\x81\xb2\xe3/1.0");
        }
    }

    req! {
        test_request_partial,
        b"GET / HTTP/1.1\r\n\r", Ok(Status::Partial),
        |_req| {}
    }

    req! {
        test_request_partial_version,
        b"GET / HTTP/1.", Ok(Status::Partial),
        |_req| {}
    }

    req! {
        test_request_newlines,
        b"GET / HTTP/1.1\nHost: foo.bar\n\n",
        |_r| {}
    }

    req! {
        test_request_empty_lines_prefix,
        b"\r\n\r\nGET / HTTP/1.1\r\n\r\n",
        |req| {
            assert_eq!(req.method.unwrap(), "GET");
            assert_eq!(req.path.unwrap(), "/");
            assert_eq!(req.version.unwrap(), 1);
            assert_eq!(req.headers.len(), 0);
        }
    }

    req! {
        test_request_empty_lines_prefix_lf_only,
        b"\n\nGET / HTTP/1.1\n\n",
        |req| {
            assert_eq!(req.method.unwrap(), "GET");
            assert_eq!(req.path.unwrap(), "/");
            assert_eq!(req.version.unwrap(), 1);
            assert_eq!(req.headers.len(), 0);
        }
    }

    req! {
        test_request_path_backslash,
        b"\n\nGET /\\?wayne\\=5 HTTP/1.1\n\n",
        |req| {
            assert_eq!(req.method.unwrap(), "GET");
            assert_eq!(req.path.unwrap(), "/\\?wayne\\=5");
            assert_eq!(req.version.unwrap(), 1);
            assert_eq!(req.headers.len(), 0);
        }
    }

    req! {
        test_request_with_invalid_token_delimiter,
        b"GET\n/ HTTP/1.1\r\nHost: foo.bar\r\n\r\n",
        Err(Error::Token),
        |_r| {}
    }

    req! {
        test_request_with_invalid_but_short_version,
        b"GET / HTTP/1!",
        Err(Error::Version),
        |_r| {}
    }

    macro_rules! res {
        ($name:ident, $buf:expr, |$arg:ident| $body:expr) => {
            res! {$name, $buf, Ok(Status::Complete($buf.len())), |$arg| $body }
        };
        ($name:ident, $buf:expr, $len:expr, |$arg:ident| $body:expr) => {
            #[test]
            fn $name() {
                let mut headers = [EMPTY_HEADER; NUM_OF_HEADERS];
                let mut res = Response::new(&mut headers[..]);
                let status = res.parse($buf.as_ref());
                assert_eq!(status, $len);
                closure(res);

                fn closure($arg: Response) {
                    $body
                }
            }
        };
    }

    res! {
        test_response_simple,
        b"HTTP/1.1 200 OK\r\n\r\n",
        |res| {
            assert_eq!(res.version.unwrap(), 1);
            assert_eq!(res.code.unwrap(), 200);
            assert_eq!(res.reason.unwrap(), "OK");
        }
    }

    res! {
        test_response_newlines,
        b"HTTP/1.0 403 Forbidden\nServer: foo.bar\n\n",
        |_r| {}
    }

    res! {
        test_response_reason_missing,
        b"HTTP/1.1 200 \r\n\r\n",
        |res| {
            assert_eq!(res.version.unwrap(), 1);
            assert_eq!(res.code.unwrap(), 200);
            assert_eq!(res.reason.unwrap(), "");
        }
    }

    res! {
        test_response_reason_missing_no_space,
        b"HTTP/1.1 200\r\n\r\n",
        |res| {
            assert_eq!(res.version.unwrap(), 1);
            assert_eq!(res.code.unwrap(), 200);
            assert_eq!(res.reason.unwrap(), "");
        }
    }

    res! {
        test_response_reason_missing_no_space_with_headers,
        b"HTTP/1.1 200\r\nFoo: bar\r\n\r\n",
        |res| {
            assert_eq!(res.version.unwrap(), 1);
            assert_eq!(res.code.unwrap(), 200);
            assert_eq!(res.reason.unwrap(), "");
            assert_eq!(res.headers.len(), 1);
            assert_eq!(res.headers[0].name, "Foo");
            assert_eq!(res.headers[0].value, b"bar");
        }
    }

    res! {
        test_response_reason_with_space_and_tab,
        b"HTTP/1.1 101 Switching Protocols\t\r\n\r\n",
        |res| {
            assert_eq!(res.version.unwrap(), 1);
            assert_eq!(res.code.unwrap(), 101);
            assert_eq!(res.reason.unwrap(), "Switching Protocols\t");
        }
    }

    static RESPONSE_REASON_WITH_OBS_TEXT_BYTE: &[u8] = b"HTTP/1.1 200 X\xFFZ\r\n\r\n";
    res! {
        test_response_reason_with_obsolete_text_byte,
        RESPONSE_REASON_WITH_OBS_TEXT_BYTE,
        Err(Error::Status),
        |_res| {}
    }

    res! {
        test_response_reason_with_nul_byte,
        b"HTTP/1.1 200 \x00\r\n\r\n",
        Err(Error::Status),
        |_res| {}
    }

    res! {
        test_response_version_missing_space,
        b"HTTP/1.1",
        Ok(Status::Partial),
        |_res| {}
    }

    res! {
        test_response_code_missing_space,
        b"HTTP/1.1 200",
        Ok(Status::Partial),
        |_res| {}
    }

    res! {
        test_response_empty_lines_prefix_lf_only,
        b"\n\nHTTP/1.1 200 OK\n\n",
        |_res| {}
    }

    #[test]
    fn test_chunk_size() {
        assert_eq!(parse_chunk_size(b"0\r\n"), Ok(Status::Complete((3, 0))));
        assert_eq!(
            parse_chunk_size(b"12\r\nchunk"),
            Ok(Status::Complete((4, 18)))
        );
        assert_eq!(
            parse_chunk_size(b"3086d\r\n"),
            Ok(Status::Complete((7, 198765)))
        );
        assert_eq!(
            parse_chunk_size(b"3735AB1;foo bar*\r\n"),
            Ok(Status::Complete((18, 57891505)))
        );
        assert_eq!(
            parse_chunk_size(b"3735ab1 ; baz \r\n"),
            Ok(Status::Complete((16, 57891505)))
        );
        assert_eq!(parse_chunk_size(b"77a65\r"), Ok(Status::Partial));
        assert_eq!(parse_chunk_size(b"ab"), Ok(Status::Partial));
        assert_eq!(parse_chunk_size(b"567f8a\rfoo"), Err(Error::ChunkSize));
        assert_eq!(parse_chunk_size(b"567f8a\rfoo"), Err(Error::ChunkSize));
        assert_eq!(parse_chunk_size(b"567xf8a\r\n"), Err(Error::ChunkSize));
        assert_eq!(
            parse_chunk_size(b"ffffffffffffffff\r\n"),
            Ok(Status::Complete((18, u64::MAX)))
        );
        assert_eq!(
            parse_chunk_size(b"1ffffffffffffffff\r\n"),
            Err(Error::ChunkSize)
        );
        assert_eq!(
            parse_chunk_size(b"Affffffffffffffff\r\n"),
            Err(Error::ChunkSize)
        );
        assert_eq!(
            parse_chunk_size(b"fffffffffffffffff\r\n"),
            Err(Error::ChunkSize)
        );
    }

    #[test]
    fn test_std_error() {
        use super::Error;
        let err = Error::HeaderName;
        assert_eq!(err.to_string(), err.description_str());
    }
}
