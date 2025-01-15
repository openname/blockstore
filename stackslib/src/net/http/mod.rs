// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2023 Stacks Open Internet Foundation
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

pub mod common;
pub mod error;
pub mod request;
pub mod response;
pub mod stream;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::io::Write;
use std::str::FromStr;
use std::{fmt, io};

use regex::{Captures, Regex};
use serde_json;
use stacks_common::codec::Error as CodecError;

pub use crate::net::http::common::{
    parse_bytes, parse_bytestream, parse_json, HttpReservedHeader, HttpVersion,
    HTTP_PREAMBLE_MAX_NUM_HEADERS,
};
pub use crate::net::http::error::{
    http_error_from_code_and_text, http_reason, HttpBadRequest, HttpError, HttpErrorResponse,
    HttpForbidden, HttpNotFound, HttpPaymentRequired, HttpServerError, HttpServiceUnavailable,
    HttpUnauthorized,
};
pub use crate::net::http::request::{
    HttpRequest, HttpRequestContents, HttpRequestPayload, HttpRequestPreamble,
};
pub use crate::net::http::response::{
    HttpResponse, HttpResponseContents, HttpResponsePayload, HttpResponsePreamble,
};
pub use crate::net::http::stream::HttpChunkGenerator;

#[derive(Debug)]
pub enum Error {
    /// Serde failed to serialize or deserialize
    JsonError(serde_json::Error),
    /// We failed to decode something
    DecodeError(String),
    /// The underlying StacksMessageCodec failed
    CodecError(CodecError),
    /// Failed to write()
    WriteError(io::Error),
    /// Failed to read()
    ReadError(io::Error),
    /// Not enough bytes to parse
    UnderflowError(String),
    /// Http error response
    Http(u16, String),
    /// Application error
    AppError(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::JsonError(json_error) => fmt::Display::fmt(&json_error, f),
            Error::DecodeError(msg) => write!(f, "{}", &msg),
            Error::CodecError(codec_error) => fmt::Display::fmt(&codec_error, f),
            Error::WriteError(io_error) => fmt::Display::fmt(&io_error, f),
            Error::ReadError(io_error) => fmt::Display::fmt(&io_error, f),
            Error::UnderflowError(msg) => write!(f, "{}", msg),
            Error::Http(code, msg) => write!(f, "code={}, msg={}", code, msg),
            Error::AppError(msg) => write!(f, "{}", &msg),
        }
    }
}

impl std::error::Error for Error {
    fn cause(&self) -> Option<&dyn std::error::Error> {
        match self {
            Error::JsonError(json_error) => Some(json_error),
            Error::DecodeError(_) => None,
            Error::CodecError(codec_error) => Some(codec_error),
            Error::WriteError(io_error) => Some(io_error),
            Error::ReadError(io_error) => Some(io_error),
            Error::UnderflowError(_) => None,
            Error::Http(..) => None,
            Error::AppError(_) => None,
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Error {
        Error::JsonError(e)
    }
}

impl From<CodecError> for Error {
    fn from(e: CodecError) -> Error {
        Error::CodecError(e)
    }
}

impl Error {
    /// Convert to an HTTP error
    pub fn into_http_error(self) -> Box<dyn HttpErrorResponse> {
        match self {
            Error::JsonError(x) => Box::new(HttpBadRequest::new(format!(
                "Failed to encode or decode JSON: {:?}",
                &x
            ))),
            Error::DecodeError(x) => {
                Box::new(HttpBadRequest::new(format!("Failed to decode: {}", &x)))
            }
            Error::CodecError(x) => Box::new(HttpBadRequest::new(format!(
                "Failed to decode due to SIP-003 codec error: {:?}",
                &x
            ))),
            Error::WriteError(x) => Box::new(HttpServerError::new(format!(
                "Failed to write data: {:?}",
                &x
            ))),
            Error::ReadError(x) => Box::new(HttpServerError::new(format!(
                "Failed to read data: {:?}",
                &x
            ))),
            Error::UnderflowError(x) => Box::new(HttpBadRequest::new(format!(
                "Failed to parse data (underflow): {:?}",
                &x
            ))),
            Error::Http(code, msg) => http_error_from_code_and_text(code, msg),
            Error::AppError(x) => Box::new(HttpServerError::new(format!(
                "Unhandled application error: {:?}",
                &x
            ))),
        }
    }
}

/// supported HTTP content types
#[derive(Debug, Clone, PartialEq, Copy)]
pub enum HttpContentType {
    Bytes,
    Text,
    JSON,
}

impl fmt::Display for HttpContentType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl HttpContentType {
    pub fn as_str(&self) -> &'static str {
        match *self {
            HttpContentType::Bytes => "application/octet-stream",
            HttpContentType::Text => "text/plain",
            HttpContentType::JSON => "application/json",
        }
    }
}

impl FromStr for HttpContentType {
    type Err = CodecError;

    fn from_str(header: &str) -> Result<HttpContentType, CodecError> {
        let s = header.to_string().to_lowercase();
        if s == "application/octet-stream" {
            Ok(HttpContentType::Bytes)
        } else if s == "text/plain" || s.starts_with("text/plain;") {
            Ok(HttpContentType::Text)
        } else if s == "application/json" || s.starts_with("application/json;") {
            Ok(HttpContentType::JSON)
        } else {
            Err(CodecError::DeserializeError(format!(
                "Unsupported HTTP content type: {header}"
            )))
        }
    }
}

/// Write out a set of HTTP headers to the given Write implementation
pub fn write_headers<W: Write>(
    fd: &mut W,
    headers: &BTreeMap<String, String>,
) -> Result<(), CodecError> {
    for (key, value) in headers.iter() {
        fd.write_all(key.as_str().as_bytes())
            .map_err(CodecError::WriteError)?;
        fd.write_all(": ".as_bytes())
            .map_err(CodecError::WriteError)?;
        fd.write_all(value.as_str().as_bytes())
            .map_err(CodecError::WriteError)?;
        fd.write_all("\r\n".as_bytes())
            .map_err(CodecError::WriteError)?;
    }
    Ok(())
}

/// Create the default accept header
pub fn default_accept_header() -> String {
    format!(
        "Accept: {}, {}, {}\r\n",
        HttpContentType::Bytes,
        HttpContentType::JSON,
        HttpContentType::Text
    )
}
