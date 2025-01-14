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

use stacks_common::codec::StacksMessageCodec;
use stacks_common::types::net::{PeerAddress, PeerHost};

use crate::net::http::common::{HTTP_PREAMBLE_MAX_ENCODED_SIZE, HTTP_PREAMBLE_MAX_NUM_HEADERS};
use crate::net::http::{
    HttpContentType, HttpRequestPreamble, HttpReservedHeader, HttpResponsePreamble, HttpVersion,
};

#[test]
fn test_parse_reserved_header() {
    let tests = vec![
        (
            "Content-Length",
            "123",
            Some(HttpReservedHeader::ContentLength(123)),
        ),
        (
            "Content-Type",
            "text/plain",
            Some(HttpReservedHeader::ContentType(HttpContentType::Text)),
        ),
        (
            "Content-Type",
            "application/octet-stream",
            Some(HttpReservedHeader::ContentType(HttpContentType::Bytes)),
        ),
        (
            "Content-Type",
            "application/json",
            Some(HttpReservedHeader::ContentType(HttpContentType::JSON)),
        ),
        (
            "Host",
            "foo:123",
            Some(HttpReservedHeader::Host(PeerHost::DNS(
                "foo".to_string(),
                123,
            ))),
        ),
        (
            "Host",
            "1.2.3.4:123",
            Some(HttpReservedHeader::Host(PeerHost::IP(
                PeerAddress([
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0x01,
                    0x02, 0x03, 0x04,
                ]),
                123,
            ))),
        ),
        // errors
        ("Content-Length", "-1", None),
        ("Content-Length", "asdf", None),
        ("Content-Length", "4294967296", None),
        ("Content-Type", "blargh", None),
        ("Unrecognized", "header", None),
    ];

    for (key, value, expected_result) in tests {
        let result = HttpReservedHeader::try_from_str(key, value);
        assert_eq!(result, expected_result);
    }
}

#[test]
fn test_parse_http_request_preamble_ok() {
    let tests = vec![
        ("GET /foo HTTP/1.1\r\nHost: localhost:6270\r\n\r\n",
         HttpRequestPreamble::from_headers(HttpVersion::Http11, "GET".to_string(), "/foo".to_string(), "localhost".to_string(), 6270, true, vec![], vec![])),
        ("POST asdf HTTP/1.1\r\nHost: core.blockstack.org\r\nFoo: Bar\r\n\r\n",
         HttpRequestPreamble::from_headers(HttpVersion::Http11, "POST".to_string(), "asdf".to_string(), "core.blockstack.org".to_string(), 80, true, vec!["foo".to_string()], vec!["Bar".to_string()])),
        ("POST asdf HTTP/1.1\r\nHost: core.blockstack.org\r\nFoo: Bar\r\n\r\n",
         HttpRequestPreamble::from_headers(HttpVersion::Http11, "POST".to_string(), "asdf".to_string(), "core.blockstack.org".to_string(), 80, true, vec!["foo".to_string()], vec!["Bar".to_string()])),
        ("GET /foo HTTP/1.1\r\nConnection: close\r\nHost: localhost:6270\r\n\r\n",
         HttpRequestPreamble::from_headers(HttpVersion::Http11, "GET".to_string(), "/foo".to_string(), "localhost".to_string(), 6270, false, vec![], vec![])),
        ("POST asdf HTTP/1.1\r\nHost: core.blockstack.org\r\nConnection: close\r\nFoo: Bar\r\n\r\n",
         HttpRequestPreamble::from_headers(HttpVersion::Http11, "POST".to_string(), "asdf".to_string(), "core.blockstack.org".to_string(), 80, false, vec!["foo".to_string()], vec!["Bar".to_string()])),
        ("POST asdf HTTP/1.1\r\nHost: core.blockstack.org\r\nFoo: Bar\r\nConnection: close\r\n\r\n",
         HttpRequestPreamble::from_headers(HttpVersion::Http11, "POST".to_string(), "asdf".to_string(), "core.blockstack.org".to_string(), 80, false, vec!["foo".to_string()], vec!["Bar".to_string()])) 
    ];

    for (data, request) in tests.iter() {
        let req = HttpRequestPreamble::consensus_deserialize(&mut data.as_bytes());
        assert!(req.is_ok(), "{:?}", &req);
        assert_eq!(req.unwrap(), *request);
    }
}

#[test]
fn test_parse_http_request_options() {
    let data = "OPTIONS /foo HTTP/1.1\r\nHost: localhost:6270\r\n\r\n";
    let req = HttpRequestPreamble::consensus_deserialize(&mut data.as_bytes());
    let preamble = HttpRequestPreamble::from_headers(
        HttpVersion::Http11,
        "OPTIONS".to_string(),
        "/foo".to_string(),
        "localhost".to_string(),
        6270,
        true,
        vec![],
        vec![],
    );
    assert_eq!(req.unwrap(), preamble);
}

#[test]
fn test_parse_http_request_preamble_case_ok() {
    let tests = vec![
        ("GET /foo HTTP/1.1\r\nhOsT: localhost:6270\r\n\r\n",
         HttpRequestPreamble::from_headers(HttpVersion::Http11, "GET".to_string(), "/foo".to_string(), "localhost".to_string(), 6270, true, vec![], vec![])),
        ("GET /foo HTTP/1.1\r\ncOnNeCtIoN: cLoSe\r\nhOsT: localhost:6270\r\n\r\n",
         HttpRequestPreamble::from_headers(HttpVersion::Http11, "GET".to_string(), "/foo".to_string(), "localhost".to_string(), 6270, false, vec![], vec![])),
        ("POST asdf HTTP/1.1\r\nhOsT: core.blockstack.org\r\nCOnNeCtIoN: kEeP-aLiVE\r\nFoo: Bar\r\n\r\n",
         HttpRequestPreamble::from_headers(HttpVersion::Http11, "POST".to_string(), "asdf".to_string(), "core.blockstack.org".to_string(), 80, true, vec!["foo".to_string()], vec!["Bar".to_string()])),
    ];

    for (data, request) in tests.iter() {
        let req = HttpRequestPreamble::consensus_deserialize(&mut data.as_bytes());
        assert!(req.is_ok(), "{:?}", &req);
        assert_eq!(req.unwrap(), *request);
    }
}

#[test]
fn test_parse_http_request_preamble_err() {
    let tests = vec![
        ("GET /foo HTTP/1.1\r\n", "failed to fill whole buffer"),
        ("GET /foo HTTP/1.1\r\n\r\n", "Missing Host header"),
        (
            "GET /foo HTTP/1.1\r\nFoo: Bar\r\n\r\n",
            "Missing Host header",
        ),
        ("GET /foo HTTP/\r\n\r\n", "Failed to parse HTTP request"),
        ("GET /foo HTTP/1.1\r\nHost:", "failed to fill whole buffer"),
        (
            "GET /foo HTTP/1.1\r\nHost: foo:80\r\nHost: bar:80\r\n\r\n",
            "duplicate header",
        ),
        (
            "GET /foo HTTP/1.1\r\nHost: localhost:6270\r\nfoo: \u{2764}\r\n\r\n",
            "header value is not ASCII-US",
        ),
        (
            "Get /foo HTTP/1.1\r\nHost: localhost:666666\r\n\r\n",
            "Missing Host header",
        ),
        (
            "GET /foo HTTP/1.1\r\nHost: localhost:8080\r\nConnection: foo\r\n\r\n",
            "invalid Connection: header",
        ),
    ];

    for (data, errstr) in tests.iter() {
        let res = HttpRequestPreamble::consensus_deserialize(&mut data.as_bytes());
        test_debug!("Expect '{}'", errstr);
        assert!(res.is_err(), "{:?}", &res);
        assert!(
            res.as_ref().unwrap_err().to_string().find(errstr).is_some(),
            "{:?}",
            &res
        );
    }
}

#[test]
fn test_http_request_preamble_headers() {
    let mut req = HttpRequestPreamble::new(
        HttpVersion::Http11,
        "GET".to_string(),
        "/foo".to_string(),
        "localhost".to_string(),
        6270,
        true,
    );
    let req_11 = HttpRequestPreamble::new(
        HttpVersion::Http11,
        "GET".to_string(),
        "/foo".to_string(),
        "localhost".to_string(),
        6270,
        false,
    );
    let req_10 = HttpRequestPreamble::new(
        HttpVersion::Http10,
        "GET".to_string(),
        "/foo".to_string(),
        "localhost".to_string(),
        6270,
        false,
    );

    req.add_header("foo".to_string(), "bar".to_string());

    assert_eq!(req.content_type, None);
    req.set_content_type(HttpContentType::JSON);
    assert_eq!(req.content_type, Some(HttpContentType::JSON));

    req.add_header(
        "content-type".to_string(),
        "application/octet-stream".to_string(),
    );
    assert_eq!(req.content_type, Some(HttpContentType::Bytes));

    let mut bytes = vec![];
    req.consensus_serialize(&mut bytes).unwrap();
    let txt = String::from_utf8(bytes).unwrap();

    test_debug!("headers:\n{}", txt);

    assert!(txt.find("HTTP/1.1").is_some(), "HTTP version is missing");
    assert!(
        txt.find("User-Agent: stacks/3.0\r\n").is_some(),
        "User-Agnet header is missing"
    );
    assert!(
        txt.find("Host: localhost:6270\r\n").is_some(),
        "Host header is missing"
    );
    assert!(txt.find("foo: bar\r\n").is_some(), "foo header is missing");
    assert!(
        txt.find("Content-Type: application/octet-stream\r\n")
            .is_some(),
        "content-type is missing"
    );
    assert!(txt.find("Connection: ").is_none()); // not sent if keep_alive is true (for HTTP/1.1)

    let mut bytes_10 = vec![];
    req_10.consensus_serialize(&mut bytes_10).unwrap();
    let txt_10 = String::from_utf8(bytes_10).unwrap();

    assert!(txt_10.find("HTTP/1.0").is_some(), "HTTP version is missing");

    let mut bytes_11 = vec![];
    req_11.consensus_serialize(&mut bytes_11).unwrap();
    let txt_11 = String::from_utf8(bytes_11).unwrap();

    assert!(txt_11.find("HTTP/1.1").is_some(), "HTTP version is wrong");
    assert!(
        txt_11.find("Connection: close").is_some(),
        "Explicit Connection: close is missing"
    );
}

#[test]
fn test_parse_http_response_preamble_ok() {
    let tests = vec![
        ("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 123\r\n\r\n",
         HttpResponsePreamble::from_headers(200, "OK".to_string(), true, Some(123), HttpContentType::Bytes, vec![], vec![])),
        ("HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: 456\r\nFoo: Bar\r\n\r\n",
         HttpResponsePreamble::from_headers(400, "Bad Request".to_string(), true, Some(456), HttpContentType::JSON,vec!["foo".to_string()], vec!["Bar".to_string()])),
        ("HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: 456\r\nFoo: Bar\r\n\r\n",
         HttpResponsePreamble::from_headers(400, "Bad Request".to_string(), true, Some(456), HttpContentType::JSON, vec!["foo".to_string()], vec!["Bar".to_string()])),
        ("HTTP/1.1 200 Ok\r\nContent-Type: application/octet-stream\r\nTransfer-encoding: chunked\r\n\r\n",
         HttpResponsePreamble::from_headers(200, "Ok".to_string(), true, None, HttpContentType::Bytes, vec![], vec![])),
        ("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 123\r\nConnection: close\r\n\r\n",
         HttpResponsePreamble::from_headers(200, "OK".to_string(), false, Some(123), HttpContentType::Bytes, vec![], vec![])),
        ("HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: 456\r\nConnection: close\r\nFoo: Bar\r\n\r\n",
         HttpResponsePreamble::from_headers(400, "Bad Request".to_string(), false, Some(456), HttpContentType::JSON, vec!["foo".to_string()], vec!["Bar".to_string()])),
        ("HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 456\r\nFoo: Bar\r\n\r\n",
         HttpResponsePreamble::from_headers(400, "Bad Request".to_string(), false, Some(456), HttpContentType::JSON, vec!["foo".to_string()], vec!["Bar".to_string()])),
        ("HTTP/1.1 200 Ok\r\nConnection: close\r\nContent-Type: application/octet-stream\r\nTransfer-encoding: chunked\r\n\r\n",
         HttpResponsePreamble::from_headers(200, "Ok".to_string(), false, None, HttpContentType::Bytes, vec![], vec![])),
    ];

    for (data, response) in tests.iter() {
        test_debug!("Try parsing:\n{}\n", data);
        let res = HttpResponsePreamble::consensus_deserialize(&mut data.as_bytes());
        assert!(res.is_ok(), "{:?}", &res);
        assert_eq!(res.unwrap(), *response);
    }
}

#[test]
fn test_parse_http_response_case_ok() {
    let tests = vec![
        ("HTTP/1.1 200 OK\r\ncOnTeNt-TyPe: aPpLiCaTiOn/oCtEt-StReAm\r\ncOnTeNt-LeNgTh: 123\r\n\r\n",
         HttpResponsePreamble::from_headers(200, "OK".to_string(), true, Some(123), HttpContentType::Bytes, vec![], vec![])),
        ("HTTP/1.1 200 Ok\r\ncOnTeNt-tYpE: aPpLiCaTiOn/OcTeT-sTrEaM\r\ntRaNsFeR-eNcOdInG: cHuNkEd\r\n\r\n",
         HttpResponsePreamble::from_headers(200, "Ok".to_string(), true, None, HttpContentType::Bytes, vec![], vec![])),
        ("HTTP/1.1 200 Ok\r\ncOnNeCtIoN: cLoSe\r\nContent-Type: application/octet-stream\r\nTransfer-encoding: chunked\r\n\r\n",
         HttpResponsePreamble::from_headers(200, "Ok".to_string(), false, None, HttpContentType::Bytes, vec![], vec![])),
        ("HTTP/1.1 200 Ok\r\ncOnNeCtIoN: kEeP-AlIvE\r\nContent-Type: application/octet-stream\r\nTransfer-encoding: chunked\r\n\r\n",
         HttpResponsePreamble::from_headers(200, "Ok".to_string(), true, None, HttpContentType::Bytes, vec![], vec![])),
    ];

    for (data, response) in tests.iter() {
        test_debug!("Try parsing:\n{}\n", data);
        let res = HttpResponsePreamble::consensus_deserialize(&mut data.as_bytes());
        assert!(res.is_ok(), "{:?}", &res);
        assert_eq!(res.unwrap(), *response);
    }
}

#[test]
fn test_http_response_preamble_headers() {
    let mut res = HttpResponsePreamble::new(
        HttpVersion::Http11,
        200,
        "OK".to_string(),
        Some(123),
        HttpContentType::JSON,
        true,
    );

    res.add_header("foo".to_string(), "bar".to_string());
    res.add_CORS_headers();

    let mut bytes = vec![];
    res.consensus_serialize(&mut bytes).unwrap();
    let txt = String::from_utf8(bytes).unwrap();
    assert!(
        txt.find("Server: stacks/2.0\r\n").is_some(),
        "Server header is missing"
    );
    assert!(
        txt.find("Content-Length: 123\r\n").is_some(),
        "Content-Length is missing"
    );
    assert!(
        txt.find("Content-Type: application/json\r\n").is_some(),
        "Content-Type is missing"
    );
    assert!(txt.find("Date: ").is_some(), "Date header is missing");
    assert!(txt.find("foo: bar\r\n").is_some(), "foo header is missing");
    assert!(
        txt.find("Access-Control-Allow-Origin: *\r\n").is_some(),
        "CORS header is missing"
    );
    assert!(
        txt.find("Access-Control-Allow-Headers: origin, content-type\r\n")
            .is_some(),
        "CORS header is missing"
    );
    assert!(
        txt.find("Access-Control-Allow-Methods: POST, GET, OPTIONS\r\n")
            .is_some(),
        "CORS header is missing"
    );
    assert!(txt.find("Connection: ").is_none()); // not sent if keep_alive is true
}

#[test]
fn test_parse_http_response_preamble_err() {
    let tests = vec![
        ("HTTP/1.1 200",
        "failed to fill whole buffer"),
        ("HTTP/1.1 200 OK\r\nfoo: \u{2764}\r\n\r\n",
        "header value is not ASCII-US"),
        ("HTTP/1.1 200 OK\r\nfoo: bar\r\nfoo: bar\r\n\r\n",
         "duplicate header"),
        ("HTTP/1.1 200 OK\r\nContent-Type: image/png\r\n\r\n",
         "Unsupported HTTP content type"),
        ("HTTP/1.1 200 OK\r\nContent-Length: foo\r\n\r\n",
         "Invalid Content-Length"),
        ("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\n",
         "missing Content-Type, Content-Length"),
        ("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 123\r\nTransfer-Encoding: chunked\r\n\r\n",
         "incompatible transfer-encoding and content-length"),
        ("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 123\r\nConnection: foo\r\n\r\n",
         "invalid Connection: header"),
    ];

    for (data, errstr) in tests.iter() {
        let res = HttpResponsePreamble::consensus_deserialize(&mut data.as_bytes());
        test_debug!("Expect '{}', got: {:?}", errstr, &res);
        assert!(res.is_err(), "{:?}", &res);
        assert!(res.unwrap_err().to_string().find(errstr).is_some());
    }
}

#[test]
fn test_http_parse_host_header_value() {
    let hosts = vec![
        "1.2.3.4",
        "1.2.3.4:5678",
        "[1:203:405:607:809:a0b:c0d:e0f]",
        "[1:203:405:607:809:a0b:c0d:e0f]:12345",
        "www.foo.com",
        "www.foo.com:12345",
        // invalid IP addresses will be parsed to DNS names
        "1.2.3.4.5",
        "[1:203:405:607:809:a0b:c0d:e0f:1011]",
        // these won't parse at all, since the port is invalid
        "1.2.3.4:1234567",
        "1.2.3.4.5:1234567",
        "[1:203:405:607:809:a0b:c0d:e0f]:1234567",
        "[1:203:405:607:809:a0b:c0d:e0f:1011]:1234567",
        "www.foo.com:1234567",
        ":",
        ":123",
    ];

    let peerhosts = vec![
        Some(PeerHost::IP(
            PeerAddress([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 1, 2, 3, 4]),
            80,
        )),
        Some(PeerHost::IP(
            PeerAddress([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 1, 2, 3, 4]),
            5678,
        )),
        Some(PeerHost::IP(
            PeerAddress([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]),
            80,
        )),
        Some(PeerHost::IP(
            PeerAddress([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]),
            12345,
        )),
        Some(PeerHost::DNS("www.foo.com".to_string(), 80)),
        Some(PeerHost::DNS("www.foo.com".to_string(), 12345)),
        Some(PeerHost::DNS("1.2.3.4.5".to_string(), 80)),
        Some(PeerHost::DNS(
            "[1:203:405:607:809:a0b:c0d:e0f:1011]".to_string(),
            80,
        )),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    ];

    for (host, expected_host) in hosts.iter().zip(peerhosts.iter()) {
        let peerhost = match host.parse::<PeerHost>() {
            Ok(ph) => Some(ph),
            Err(_) => None,
        };

        match (peerhost, expected_host) {
            (Some(ref ph), Some(ref expected_ph)) => assert_eq!(*ph, *expected_ph),
            (None, None) => {}
            (Some(ph), None) => {
                eprintln!(
                    "Parsed {} successfully to {:?}, but expected error",
                    host, ph
                );
                assert!(false);
            }
            (None, Some(expected_ph)) => {
                eprintln!("Failed to parse {} successfully", host);
                assert!(false);
            }
        }
    }
}

#[test]
fn test_http_headers_too_big() {
    let bad_header_value = std::iter::repeat("A")
        .take(HTTP_PREAMBLE_MAX_ENCODED_SIZE as usize)
        .collect::<String>();
    let bad_request_preamble = format!(
        "GET /v2/neighbors HTTP/1.1\r\nHost: localhost:1234\r\nBad-Header: {}\r\n\r\n",
        &bad_header_value
    );
    let bad_response_preamble = format!("HTTP/1.1 200 OK\r\nServer: stacks/v2.0\r\nX-Request-ID: 123\r\nContent-Type: text/plain\r\nContent-Length: 64\r\nBad-Header: {}\r\n\r\n", &bad_header_value);

    let request_err =
        HttpRequestPreamble::consensus_deserialize(&mut bad_request_preamble.as_bytes())
            .unwrap_err();
    let response_err =
        HttpResponsePreamble::consensus_deserialize(&mut bad_response_preamble.as_bytes())
            .unwrap_err();

    eprintln!("request_err: {:?}", &request_err);
    eprintln!("response_err: {:?}", &response_err);

    assert!(request_err
        .to_string()
        .find("Not enough bytes to form a HTTP request preamble")
        .is_some());
    assert!(response_err
        .to_string()
        .find("Not enough bytes to form a HTTP response preamble")
        .is_some());
}

#[test]
fn test_http_headers_too_many() {
    let mut too_many_headers_list = vec![];
    for i in 0..HTTP_PREAMBLE_MAX_NUM_HEADERS {
        too_many_headers_list.push(format!("H{}: {}\r\n", i + 1, i + 1));
    }
    let too_many_headers = too_many_headers_list.join("");
    let bad_request_preamble = format!(
        "GET /v2/neighbors HTTP/1.1\r\nHost: localhost:1234\r\n{}\r\n",
        &too_many_headers
    );
    let bad_response_preamble = format!("HTTP/1.1 200 OK\r\nServer: stacks/v2.0\r\nX-Request-ID: 123\r\nContent-Type: text/plain\r\nContent-Length: 64\r\n{}\r\n", &too_many_headers);

    let request_err =
        HttpRequestPreamble::consensus_deserialize(&mut bad_request_preamble.as_bytes())
            .unwrap_err();
    let response_err =
        HttpResponsePreamble::consensus_deserialize(&mut bad_response_preamble.as_bytes())
            .unwrap_err();

    eprintln!("request_err: {:?}", &request_err);
    eprintln!("response_err: {:?}", &response_err);

    assert!(request_err
        .to_string()
        .find("Failed to parse HTTP request: TooManyHeaders")
        .is_some());
    assert!(response_err
        .to_string()
        .find("Failed to parse HTTP response: TooManyHeaders")
        .is_some());
}

#[test]
fn test_http_request_version_keep_alive() {
    let requests = vec![
        HttpRequestPreamble::new(
            HttpVersion::Http10,
            "GET".to_string(),
            "/v2/info".to_string(),
            "localhost".to_string(),
            8080,
            true,
        ),
        HttpRequestPreamble::new(
            HttpVersion::Http10,
            "GET".to_string(),
            "/v2/info".to_string(),
            "localhost".to_string(),
            8080,
            false,
        ),
        HttpRequestPreamble::new(
            HttpVersion::Http11,
            "GET".to_string(),
            "/v2/info".to_string(),
            "localhost".to_string(),
            8080,
            true,
        ),
        HttpRequestPreamble::new(
            HttpVersion::Http11,
            "GET".to_string(),
            "/v2/info".to_string(),
            "localhost".to_string(),
            8080,
            false,
        ),
    ];

    // (have 'connection' header?, have 'keep-alive' value?)
    let requests_connection_expected =
        [(true, true), (false, false), (false, false), (true, false)];

    for (r, (has_connection, is_keep_alive)) in
        requests.iter().zip(requests_connection_expected.iter())
    {
        let mut bytes = vec![];
        r.consensus_serialize(&mut bytes).unwrap();
        let txt = String::from_utf8(bytes).unwrap();

        eprintln!(
            "has_connection: {}, is_keep_alive: {}\n{}",
            *has_connection, *is_keep_alive, &txt
        );
        if *has_connection {
            if *is_keep_alive {
                assert!(txt.find("Connection: keep-alive\r\n").is_some());
            } else {
                assert!(txt.find("Connection: close\r\n").is_some());
            }
        } else {
            assert!(txt.find("Connection: ").is_none());
        }
    }
}

#[test]
fn test_http_response_version_keep_alive() {
    // (version, explicit keep-alive?)
    let responses_args = [
        (HttpVersion::Http10, true),
        (HttpVersion::Http10, false),
        (HttpVersion::Http11, true),
        (HttpVersion::Http11, false),
    ];

    let mut responses = vec![];
    for res in responses_args.iter() {
        let mut bytes = vec![];
        let preamble =
            HttpResponsePreamble::new(res.0, 200, "OK".into(), None, HttpContentType::JSON, res.1);
        preamble.consensus_serialize(&mut bytes).unwrap();
        responses.push(String::from_utf8(bytes).unwrap());
    }

    for (response, (version, sent_keep_alive)) in responses.iter().zip(responses_args.iter()) {
        test_debug!(
            "version: {:?}, sent keep-alive: {}, response:\n{}",
            version,
            sent_keep_alive,
            response
        );
        match version {
            HttpVersion::Http10 => {
                // be explicit about Connection: with http/1.0 clients
                if *sent_keep_alive {
                    assert!(response.find("Connection: keep-alive\r\n").is_some());
                } else {
                    assert!(response.find("Connection: close\r\n").is_some());
                }
            }
            HttpVersion::Http11 => {
                if *sent_keep_alive {
                    // we don't send connection: keep-alive if the client is 1.1 and it didn't
                    // send its own connection: <option>
                    assert!(response.find("Connection:").is_none());
                } else {
                    assert!(response.find("Connection: close\r\n").is_some());
                }
            }
        }
    }
}

#[test]
fn test_http_live_headers() {
    // headers pulled from prod
    let live_headers = &[
        "GET /v2/info HTTP/1.1\r\naccept-language: en-US,en;q=0.9\r\naccept-encoding: gzip, deflate, br\r\nsec-fetch-dest: document\r\nsec-fetch-user: ?1\r\nsec-fetch-mode: navigate\r\nsec-fetch-site: none\r\naccept: text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.9\r\nuser-agent: Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_3) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/81.0.4044.113 Safari/537.36\r\nupgrade-insecure-requests: 1\r\ncache-control: max-age=0\r\nconnection: close\r\nx-forwarded-port: 443\r\nx-forwarded-host: crashy-stacky.zone117x.com\r\nx-forwarded-proto: https\r\nx-forwarded-for: 213.127.17.55\r\nx-real-ip: 213.127.17.55\r\nhost: stacks-blockchain:20443\r\n\r\n"
    ];

    let bad_live_headers = &[
        "GET /favicon.ico HTTP/1.1\r\nConnection: upgrade\r\nHost: crashy-stacky.zone117x.com\r\nX-Real-IP: 213.127.17.55\r\nX-Forwarded-For: 213.127.17.55\r\nX-Forwarded-Proto: http\r\nX-Forwarded-Host: crashy-stacky.zone117x.com\r\nX-Forwarded-Port: 9001\r\nUser-Agent: Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_3) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/81.0.4044.113 Safari/537.36\r\nAccept: image/webp,image/apng,image/*,*/*;q=0.8\r\nReferer: http://crashy-stacky.zone117x.com:9001/v2/info\r\nAccept-Encoding: gzip, deflate\r\nAccept-Language: en-US,en;q=0.9\r\n\r\n",
    ];

    for live_header in live_headers {
        let res = HttpRequestPreamble::consensus_deserialize(&mut live_header.as_bytes());
        assert!(res.is_ok(), "headers: {}\nerror: {:?}", live_header, &res);
    }

    for bad_live_header in bad_live_headers {
        let res = HttpRequestPreamble::consensus_deserialize(&mut bad_live_header.as_bytes());
        assert!(
            res.is_err(),
            "headers: {}\nshould not have parsed",
            bad_live_header
        );
    }
}
