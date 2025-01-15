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

use std::io::{Read, Write};
use std::{error, fmt, io};

use crate::codec::MAX_MESSAGE_LEN;
use crate::deps_common::httparse;

/// NOTE: it is imperative that the given Read and Write impls here _never_ fail with EWOULDBLOCK.

#[derive(Debug)]
pub enum ChunkedError {
    DeserializeError(String),
    OverflowError(String),
}

impl fmt::Display for ChunkedError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            ChunkedError::DeserializeError(ref s) => fmt::Display::fmt(s, f),
            ChunkedError::OverflowError(ref s) => fmt::Display::fmt(s, f),
        }
    }
}

impl error::Error for ChunkedError {
    fn cause(&self) -> Option<&dyn error::Error> {
        match *self {
            ChunkedError::DeserializeError(..) => None,
            ChunkedError::OverflowError(..) => None,
        }
    }
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, PartialEq, Copy)]
enum HttpChunkedTransferParseMode {
    ChunkBoundary,
    Chunk,
    ChunkTrailer,
    EOF,
}

#[derive(Debug, Clone, PartialEq, Copy)]
pub struct HttpChunkedTransferReaderState {
    parse_step: HttpChunkedTransferParseMode,
    chunk_size: u64,
    chunk_read: u64,
    pub max_size: u64,
    total_size: u64,
    last_chunk_size: u64,

    // for parsing a chunk boundary
    // (we don't use extensions, so 16 bytes for size + 2 for \r\n delimiter ought to be enough)
    chunk_buffer: [u8; 18],
    i: usize,
}

impl HttpChunkedTransferReaderState {
    pub fn new(max_size: u64) -> HttpChunkedTransferReaderState {
        HttpChunkedTransferReaderState {
            parse_step: HttpChunkedTransferParseMode::ChunkBoundary,
            chunk_size: 0,
            chunk_read: 0,
            max_size,
            total_size: 0,
            last_chunk_size: u64::MAX, // if this ever becomes 0, then we should expect chunk boundary '0\r\n\r\n' and EOF
            chunk_buffer: [0u8; 18],
            i: 0,
        }
    }

    pub fn is_eof(&self) -> bool {
        self.parse_step == HttpChunkedTransferParseMode::EOF
    }
}

/// read adapter for chunked transfer encoding
pub struct HttpChunkedTransferReader<'a, R: Read> {
    fd: &'a mut R,
    state: HttpChunkedTransferReaderState,
}

impl<'a, R: Read> HttpChunkedTransferReader<'a, R> {
    pub fn from_reader(r: &'a mut R, max_size: u64) -> HttpChunkedTransferReader<'a, R> {
        HttpChunkedTransferReader {
            fd: r,
            state: HttpChunkedTransferReaderState::new(max_size),
        }
    }

    pub fn from_state(
        r: &'a mut R,
        state: HttpChunkedTransferReaderState,
    ) -> HttpChunkedTransferReader<'a, R> {
        HttpChunkedTransferReader { fd: r, state }
    }
}

impl HttpChunkedTransferReaderState {
    /// Read until we have a chunk marker we can parse completely.
    /// Interruptable -- call repeatedly on EINTR.
    /// Reads at most one byte.
    fn read_chunk_boundary<R: Read>(&mut self, fd: &mut R) -> io::Result<usize> {
        assert_eq!(self.parse_step, HttpChunkedTransferParseMode::ChunkBoundary);

        // next byte
        let mut b = [0u8; 1];

        trace!("Read {} bytes", b.len());
        let nr = fd.read(&mut b)?;
        if nr == 0 {
            return Ok(nr);
        }
        trace!("Got {} bytes", nr);

        self.chunk_buffer[self.i] = b[0];
        self.i += 1;

        if self.i >= self.chunk_buffer.len() {
            // don't allow ridiculous extension lengths
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                ChunkedError::DeserializeError("Invalid HTTP chunk boundary: too long".to_string()),
            ));
        }

        let (offset, chunk_len) = match httparse::parse_chunk_size(&self.chunk_buffer[0..self.i]) {
            Ok(httparse::Status::Partial) => {
                return Ok(nr);
            }
            Ok(httparse::Status::Complete((offset, chunk_len))) => (offset, chunk_len),
            Err(_) => {
                test_debug!(
                    "Invalid chunk boundary: {:?}",
                    self.chunk_buffer[0..self.i].to_vec()
                );
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid HTTP chunk boundary: could not parse".to_string(),
                ));
            }
        };

        trace!("chunk offset: {}. chunk len: {}", offset, chunk_len);
        if chunk_len > MAX_MESSAGE_LEN as u64 {
            trace!("chunk buffer: {:?}", &self.chunk_buffer[0..self.i]);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                ChunkedError::DeserializeError("Invalid HTTP chunk: too big".to_string()),
            ));
        }

        // got an offset/len.
        // offset ought to equal the number of bytes taken by the encoded chunk boundary.
        assert_eq!(offset, self.i);

        // reset buffers
        self.i = 0;
        self.chunk_size = chunk_len;
        self.chunk_read = 0;

        // begin reading chunk
        trace!("begin reading chunk");
        self.parse_step = HttpChunkedTransferParseMode::Chunk;
        Ok(nr)
    }

    /// Read a chunk -- read up to self.chunk_size bytes over successive calls.
    /// Reads at most self.chunk_size bytes.
    fn read_chunk_bytes<R: Read>(&mut self, fd: &mut R, buf: &mut [u8]) -> io::Result<usize> {
        assert_eq!(self.parse_step, HttpChunkedTransferParseMode::Chunk);

        if self.total_size >= self.max_size && self.chunk_size > 0 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                ChunkedError::OverflowError(
                    "HTTP body exceeds maximum expected length".to_string(),
                ),
            ));
        }

        let remaining = if self.chunk_size - self.chunk_read <= (self.max_size - self.total_size) {
            self.chunk_size - self.chunk_read
        } else {
            self.max_size - self.total_size
        };

        let nr = if (buf.len() as u64) < remaining {
            // can fill buffer
            trace!("Read {} bytes (fill buffer)", buf.len());
            fd.read(buf)? as u64
        } else {
            // will read up to a chunk boundary
            trace!("Read {} bytes (fill remainder)", remaining);
            fd.read(&mut buf[0..(remaining as usize)])? as u64
        };

        trace!("Got {} bytes", nr);

        self.chunk_read += nr;

        if self.chunk_read >= self.chunk_size {
            // done reading; proceed to consume trailer
            trace!(
                "begin reading trailer ({} >= {})",
                self.chunk_read,
                self.chunk_size
            );
            self.parse_step = HttpChunkedTransferParseMode::ChunkTrailer;
        }

        self.total_size += nr;
        Ok(nr as usize)
    }

    /// Read chunk trailer -- read end-of-chunk \r\n
    /// Returns number of bytes read on success
    /// Reads at most 2 bytes.
    fn read_chunk_trailer<R: Read>(&mut self, fd: &mut R) -> io::Result<usize> {
        assert_eq!(self.parse_step, HttpChunkedTransferParseMode::ChunkTrailer);

        let mut nr = 0;

        // read trailer
        if self.i < 2 {
            let mut trailer_buf = [0u8; 2];

            trace!("Read at most {} bytes", 2 - self.i);
            nr = fd.read(&mut trailer_buf[self.i..2])?;
            if nr == 0 {
                return Ok(nr);
            }

            self.chunk_buffer[self.i..2].copy_from_slice(&trailer_buf[self.i..2]);
            self.i += nr;
        }

        if self.i == 2 {
            // expect '\r\n'
            if self.chunk_buffer[0..2] != [0x0d, 0x0a] {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    ChunkedError::DeserializeError("Invalid chunk trailer".to_string()),
                ));
            }

            // end of chunk
            self.last_chunk_size = self.chunk_size;
            self.i = 0;

            trace!("begin reading boundary");
            self.parse_step = HttpChunkedTransferParseMode::ChunkBoundary;
        }

        trace!("Consumed {} bytes of chunk boundary (i = {})", nr, self.i);
        Ok(nr)
    }

    /// Read from a Read.
    /// Returns (number of bytes decoded, number of bytes consumed from the Read)
    pub fn do_read<R: Read>(&mut self, fd: &mut R, buf: &mut [u8]) -> io::Result<(usize, usize)> {
        let mut decoded = 0;
        let mut consumed = 0;
        while decoded < buf.len() {
            match self.parse_step {
                HttpChunkedTransferParseMode::ChunkBoundary => {
                    let count = self.read_chunk_boundary(fd)?;
                    if count == 0 {
                        break;
                    }
                    consumed += count;
                }
                HttpChunkedTransferParseMode::Chunk => {
                    let nr = self.read_chunk_bytes(fd, &mut buf[decoded..])?;
                    if nr == 0 && self.parse_step == HttpChunkedTransferParseMode::Chunk {
                        // still trying to read the chunk, but got 0 bytes
                        break;
                    }
                    decoded += nr;
                    consumed += nr;
                }
                HttpChunkedTransferParseMode::ChunkTrailer => {
                    let count = self.read_chunk_trailer(fd)?;
                    if count == 0 {
                        break;
                    }
                    consumed += count;
                    if self.last_chunk_size == 0 {
                        // we're done
                        trace!("finished last chunk");
                        self.parse_step = HttpChunkedTransferParseMode::EOF;
                        break;
                    }
                }
                HttpChunkedTransferParseMode::EOF => {
                    break;
                }
            }
        }
        Ok((decoded, consumed))
    }
}

impl<R: Read> Read for HttpChunkedTransferReader<'_, R> {
    /// Read a HTTP chunk-encoded stream.
    /// Returns number of decoded bytes (i.e. number of bytes copied to buf, as expected)
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.state.do_read(self.fd, buf).map(|(decoded, _)| decoded)
    }
}

pub struct HttpChunkedTransferWriterState {
    chunk_size: usize,
    chunk_buf: Vec<u8>,
    corked: bool,
}

impl HttpChunkedTransferWriterState {
    pub fn new(chunk_size: usize) -> HttpChunkedTransferWriterState {
        HttpChunkedTransferWriterState {
            chunk_size,
            chunk_buf: vec![],
            corked: false,
        }
    }

    pub fn get_chunk_size(&self) -> usize {
        self.chunk_size
    }
}

pub struct HttpChunkedTransferWriter<'a, 'state, W: Write> {
    fd: &'a mut W,
    state: &'state mut HttpChunkedTransferWriterState,
}

impl<'a, 'state, W: Write> HttpChunkedTransferWriter<'a, 'state, W> {
    pub fn from_writer_state(
        fd: &'a mut W,
        state: &'state mut HttpChunkedTransferWriterState,
    ) -> HttpChunkedTransferWriter<'a, 'state, W> {
        HttpChunkedTransferWriter { fd, state }
    }

    fn send_chunk(fd: &mut W, chunk_size: usize, bytes: &[u8]) -> io::Result<usize> {
        let to_send = if chunk_size < bytes.len() {
            chunk_size
        } else {
            bytes.len()
        };

        fd.write_all(format!("{:x}\r\n", to_send).as_bytes())?;
        fd.write_all(&bytes[0..to_send])?;
        fd.write_all("\r\n".as_bytes())?;
        Ok(to_send)
    }

    fn flush_chunk(&mut self) -> io::Result<usize> {
        let sent = HttpChunkedTransferWriter::send_chunk(
            &mut self.fd,
            self.state.chunk_size,
            &self.state.chunk_buf,
        )?;
        self.state.chunk_buf.clear();
        Ok(sent)
    }

    fn buf_chunk(&mut self, buf: &[u8]) -> usize {
        let to_copy = if self.state.chunk_size - self.state.chunk_buf.len() < buf.len() {
            self.state.chunk_size - self.state.chunk_buf.len()
        } else {
            buf.len()
        };

        self.state.chunk_buf.extend_from_slice(&buf[0..to_copy]);
        to_copy
    }

    pub fn cork(&mut self) {
        // block future flushes from sending trailing empty chunks -- we're done sending
        self.state.corked = true;
    }

    pub fn corked(&self) -> bool {
        self.state.corked
    }
}

impl<W: Write> Write for HttpChunkedTransferWriter<'_, '_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < buf.len() && !self.state.corked {
            if !self.state.chunk_buf.is_empty() {
                if self.state.chunk_buf.len() < self.state.chunk_size {
                    let nw = self.buf_chunk(&buf[written..]);
                    written += nw;
                }
                if self.state.chunk_buf.len() >= self.state.chunk_size {
                    self.flush_chunk()?;
                }
            } else if written + self.state.chunk_size < buf.len() {
                let nw = HttpChunkedTransferWriter::send_chunk(
                    &mut self.fd,
                    self.state.chunk_size,
                    &buf[written..(written + self.state.chunk_size)],
                )?;
                written += nw;
            } else {
                let nw = self.buf_chunk(&buf[written..]);
                written += nw;
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        // send out any buffered chunk data
        if !self.state.corked {
            self.flush_chunk().and_then(|nw| {
                if nw > 0 {
                    // send empty chunk
                    self.fd.write_all(b"0\r\n\r\n").map(|_nw| ())
                } else {
                    Ok(())
                }
            })
        } else {
            Ok(())
        }
    }
}

mod test {
    use std::io;
    use std::io::{Read, Write};

    use rand::RngCore;

    use super::*;

    /// Simulate reading variable-length segments
    struct SegmentReader {
        segments: Vec<Vec<u8>>,
        i: usize, // which segment
        j: usize, // which offset in segment
    }

    impl SegmentReader {
        pub fn new(segments: Vec<Vec<u8>>) -> SegmentReader {
            SegmentReader {
                segments,
                i: 0,
                j: 0,
            }
        }
    }

    impl Read for SegmentReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.i >= self.segments.len() {
                return Ok(0);
            }
            let mut written = 0;
            while written < buf.len() {
                let to_copy = if self.segments[self.i][self.j..].len() < buf[written..].len() {
                    self.segments[self.i][self.j..].len()
                } else {
                    buf[written..].len()
                };

                buf[written..(written + to_copy)]
                    .copy_from_slice(&self.segments[self.i][self.j..(self.j + to_copy)]);

                self.j += to_copy;
                written += to_copy;

                if self.j >= self.segments[self.i].len() {
                    self.i += 1;
                    self.j = 0;
                }
            }
            Ok(written)
        }
    }

    fn vec_u8(v: Vec<&str>) -> Vec<Vec<u8>> {
        v.into_iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    #[test]
    fn test_segment_reader() {
        let tests = vec![
            (vec_u8(vec!["a", "b"]), "ab"),
            (vec_u8(vec!["aa", "bbb", "cccc"]), "aabbbcccc"),
            (vec_u8(vec!["aaaa", "bbb", "cc", "d", ""]), "aaaabbbccd"),
            (vec_u8(vec!["", "a", "", "b", ""]), "ab"),
            (vec_u8(vec![""]), ""),
        ];
        for (input_vec, expected) in tests.into_iter() {
            let num_segments = input_vec.len();
            let mut segment_io = SegmentReader::new(input_vec);
            let mut output = vec![0u8; expected.len()];
            let mut offset = 0;
            for i in 0..num_segments {
                let nw = segment_io.read(&mut output[offset..]).unwrap();
                offset += nw;
            }
            assert_eq!(output, expected.as_bytes().to_vec());
        }
    }

    #[test]
    fn test_http_chunked_encode() {
        let tests = [
            // (chunk size, byte string, expected encoding)
            (10, "aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd", "a\r\naaaaaaaaaa\r\na\r\nbbbbbbbbbb\r\na\r\ncccccccccc\r\na\r\ndddddddddd\r\n0\r\n\r\n"),
            (10, "aaaaaaaaaabbbbbbbbbbccccccccccdddddddddde", "a\r\naaaaaaaaaa\r\na\r\nbbbbbbbbbb\r\na\r\ncccccccccc\r\na\r\ndddddddddd\r\n1\r\ne\r\n0\r\n\r\n"),
            (10, "aaaaaaaaaabbbbbbbbbbccccccccccddddddddddeeeee", "a\r\naaaaaaaaaa\r\na\r\nbbbbbbbbbb\r\na\r\ncccccccccc\r\na\r\ndddddddddd\r\n5\r\neeeee\r\n0\r\n\r\n"),
            (1, "abcd", "1\r\na\r\n1\r\nb\r\n1\r\nc\r\n1\r\nd\r\n0\r\n\r\n"),
            (3, "abcd", "3\r\nabc\r\n1\r\nd\r\n0\r\n\r\n"),
            (10, "", "0\r\n\r\n")
        ];
        for (chunk_size, input_bytes, encoding) in tests.iter() {
            let mut bytes = vec![];
            {
                let mut write_state = HttpChunkedTransferWriterState::new(*chunk_size as usize);
                let mut encoder =
                    HttpChunkedTransferWriter::from_writer_state(&mut bytes, &mut write_state);
                encoder.write_all(input_bytes.as_bytes()).unwrap();
                encoder.flush().unwrap();
            }

            assert_eq!(bytes, encoding.as_bytes().to_vec());
        }
    }

    #[test]
    fn test_http_chunked_encode_multi() {
        let tests = [
            // chunk size, sequence of writes, expected encoding
            (10, vec!["aaaaaaaaaa", "bbbbb", "bbbbb", "ccc", "ccc", "ccc", "c", "dd", "ddddd", "ddd"], "a\r\naaaaaaaaaa\r\na\r\nbbbbbbbbbb\r\na\r\ncccccccccc\r\na\r\ndddddddddd\r\n0\r\n\r\n"),
            (10, vec!["a", "a", "a", "a", "a", "a", "a", "a", "a", "a"], "a\r\naaaaaaaaaa\r\n0\r\n\r\n"),
            (10, vec!["a", "", "a", "", "a", "", "a", "", "a", "", "a", "", "a", "", "a", "", "a", "", "a", ""], "a\r\naaaaaaaaaa\r\n0\r\n\r\n"),
        ];

        for (chunk_size, input_vec, encoding) in tests.iter() {
            let mut bytes = vec![];
            {
                let mut write_state = HttpChunkedTransferWriterState::new(*chunk_size as usize);
                let mut encoder =
                    HttpChunkedTransferWriter::from_writer_state(&mut bytes, &mut write_state);
                for input in input_vec.iter() {
                    encoder.write_all(input.as_bytes()).unwrap();
                }
                encoder.flush().unwrap();
            }

            assert_eq!(bytes, encoding.as_bytes().to_vec());
        }
    }

    #[test]
    fn test_http_chunked_decode() {
        let tests = [
            ("a\r\naaaaaaaaaa\r\na\r\nbbbbbbbbbb\r\na\r\ncccccccccc\r\na\r\ndddddddddd\r\n0\r\n\r\n", "aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd"),
            ("A\r\naaaaaaaaaa\r\nA\r\nbbbbbbbbbb\r\nA\r\ncccccccccc\r\nA\r\ndddddddddd\r\n0\r\n\r\n", "aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd"),
            ("1\r\na\r\n2\r\nbb\r\n3\r\nccc\r\n4\r\ndddd\r\n0\r\n\r\n", "abbcccdddd"),
            ("1\r\na\r\n0\r\n\r\n", "a"),
            ("1\r\na\r\n0\r\n\r\n1\r\nb\r\n0\r\n\r\n", "a"),     // stop reading after the first 0-length chunk encountered
            ("1; a; b\r\na\r\n0; c\r\n\r\n", "a"),                          // ignore short extensions
            ("1  ; a ; b \r\na\r\n0     ; extension003\r\n\r\n", "a"),      // ignore short extensions
            ("1 \t; a\t;\tb ;\r\na\r\n0\t\t;c\r\n\r\n", "a"),               // ignore short extensions
        ];
        for (encoded, expected) in tests.iter() {
            let mut cursor = io::Cursor::new(encoded.as_bytes());
            let mut decoder = HttpChunkedTransferReader::from_reader(&mut cursor, 50);
            let mut output = vec![0u8; expected.len()];
            decoder.read_exact(&mut output).unwrap();

            assert_eq!(output, expected.as_bytes().to_vec());
        }
    }

    #[test]
    fn test_http_chunked_decode_multi() {
        let tests = [
            (vec_u8(vec!["1\r\na", "\r\n", "0\r\n\r\n"]), "a"),
            (vec_u8(vec!["1\r\na\r", "\n0\r\n\r\n"]), "a"),
            (vec_u8(vec!["1\r\na\r\n", "0\r\n\r", "\n"]), "a"),
            (vec_u8(vec!["1\r\na\r\n0\r\n", "\r\n"]), "a"),
            (vec_u8(vec!["1\r\na\r\n0\r", "\n\r\n"]), "a"),
            (vec_u8(vec!["1\r\na\r\n0\r", "\n", "\r\n"]), "a"),
            (vec_u8(vec!["1\r\na\r\n0\r", "\n\r", "\n"]), "a"),
            (vec_u8(vec!["1\r\na\r\n0\r", "\n", "\r", "\n"]), "a"),
            (
                vec_u8(vec![
                    "1", "\r", "\n", "a", "\r", "\n", "0", "\r", "\n", "\r", "\n",
                ]),
                "a",
            ),
            (
                vec_u8(vec![
                    "a\r",
                    "\n",
                    "aaaa",
                    "aaaaa",
                    "a",
                    "\r\n",
                    "a\r\n",
                    "bbbbbbbbbb\r",
                    "\na\r\nccc",
                    "ccccccc",
                    "\r",
                    "\na\r",
                    "\ndddddd",
                    "dddd",
                    "\r\n0\r",
                    "\n",
                    "\r",
                    "\n",
                ]),
                "aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd",
            ),
            (
                vec_u8(vec![
                    "a\r\naaaaaaaaaa",
                    "\r",
                    "\n",
                    "a\r\nbbbbbbbbbb\r",
                    "\n",
                    "a\r\ncccccccccc\r",
                    "\na\r\nddddd",
                    "ddddd\r",
                    "\n0\r",
                    "\n\r",
                    "\n",
                ]),
                "aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd",
            ),
            (
                vec_u8(vec![
                    "1",
                    "\r",
                    "\n",
                    "",
                    "a",
                    "\r",
                    "\n",
                    "2",
                    "\r\n",
                    "bb",
                    "\r\n",
                    "3\r\n",
                    "ccc\r",
                    "\n4\r\n",
                    "dddd\r\n",
                    "0\r\n\r\n",
                ]),
                "abbcccdddd",
            ),
        ];
        for (encoded_vec, expected) in tests.iter() {
            test_debug!("expect {:?}", &expected);

            let mut output = vec![];
            let mut cursor = SegmentReader::new((*encoded_vec).clone());
            let mut decoder = HttpChunkedTransferReader::from_reader(&mut cursor, 50);

            for encoded in encoded_vec.iter() {
                let mut tmp = vec![0u8; encoded.len()];
                let nr = decoder.read(&mut tmp).unwrap();

                output.extend_from_slice(&tmp[0..nr]);
            }

            assert_eq!(output, expected.as_bytes().to_vec());
        }
    }

    #[test]
    fn test_http_chunked_decode_err() {
        let tests = [
            (
                "1; reallyreallyreallyreallylongextension;\r\na\r\n0\r\n\r\n",
                1,
                "too long",
            ),
            ("ffffffff\r\n", 1, "too big"),
            ("nope\r\n", 1, "could not parse"),
            ("1\na\r\n0\r\n\r\n", 1, "could not parse"),
            ("a\r\naaaaaaaaaa", 11, "failed to fill whole buffer"),
            ("1\r\nab\r\n0\r\n\r\n", 2, "Invalid chunk trailer"),
            (
                "15\r\naaaaaaaaaabbbbbbbbbbb\r\n0\r\n\r\n",
                21,
                "HTTP body exceeds maximum expected length",
            ),
            (
                "7\r\naaaaaaa\r\n8\r\nbbbbbbbb\r\n6\r\ncccccc\r\n0\r\n\r\n",
                21,
                "HTTP body exceeds maximum expected length",
            ),
        ];
        for (encoded, expected_len, expected) in tests.iter() {
            test_debug!("expect '{}'", expected);
            let mut cursor = io::Cursor::new(encoded.as_bytes());
            let mut decoder = HttpChunkedTransferReader::from_reader(&mut cursor, 20);
            let mut output = vec![0u8; *expected_len as usize];

            let err = decoder.read_exact(&mut output).unwrap_err();
            let errstr = format!("{:?}", &err);

            assert!(
                errstr.contains(expected),
                "Expected '{}' in '{:?}'",
                expected,
                errstr
            );
        }
    }

    #[test]
    fn test_http_chunked_encode_decode_roundtrip() {
        let mut rng = rand::thread_rng();
        for i in 0..100 {
            let mut data = vec![0u8; 256];
            rng.fill_bytes(&mut data);

            let mut encoded_data = vec![];
            {
                let mut write_state = HttpChunkedTransferWriterState::new(i + 1);
                let mut encoder = HttpChunkedTransferWriter::from_writer_state(
                    &mut encoded_data,
                    &mut write_state,
                );
                encoder.write_all(&data).unwrap();
                encoder.flush().unwrap();
            }

            let mut decoded_data = vec![0u8; 256];
            {
                let mut cursor = io::Cursor::new(&encoded_data);
                let mut decoder = HttpChunkedTransferReader::from_reader(&mut cursor, 256);
                decoder.read_exact(&mut decoded_data).unwrap();
            }

            assert_eq!(data, decoded_data);
        }
    }
}
