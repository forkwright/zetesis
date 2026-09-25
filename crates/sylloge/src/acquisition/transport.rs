//! Private one-hop HTTP/1.1 exchange over a stream the acquirer connected.
//!
//! One request per connection (`Connection: close`), no pool, no automatic
//! redirects. The body streams through a [`BodyDecoder`], which enforces
//! the wire and decoded ceilings as bytes arrive. The hyper connection future is driven
//! inline alongside the request and body futures instead of being spawned,
//! so dropping the exchange closes the socket and no task outlives the
//! call.

use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};

use http_body_util::{BodyExt, Empty};
use hyper::body::{Body, Bytes, Incoming};
use hyper::client::conn::http1::{self, Connection, SendRequest};
use hyper::header::{
    ACCEPT, ACCEPT_ENCODING, CONNECTION, HOST, HeaderMap, HeaderValue, USER_AGENT,
};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use url::Url;

use super::connector::ConnectedStream;
use super::limits::AcquisitionLimits;
use super::record::AcquisitionFailure;
use crate::evidence::decode::{self, BodyDecoder, DecodeError, DecodedBody};

type Io = TokioIo<Box<dyn ConnectedStream>>;
type Conn = Connection<Io, Empty<Bytes>>;

/// Keeps the hyper connection future polled while the caller awaits the
/// response head or body frames.
struct Driver {
    conn: Pin<Box<Conn>>,
    finished: bool,
}

impl Driver {
    /// Await `fut` while polling the connection. Once the connection
    /// future completes it is not polled again; hyper has by then delivered
    /// the response, an error, or end-of-body to `fut`.
    async fn run<F: Future>(&mut self, fut: F) -> F::Output {
        let mut fut = pin!(fut);
        poll_fn(|cx| {
            if !self.finished && self.conn.as_mut().poll(cx).is_ready() {
                self.finished = true;
            }
            fut.as_mut().poll(cx)
        })
        .await
    }
}

/// A response head with its connection still open for the body.
pub(crate) struct Exchange {
    driver: Driver,
    response: Response<Incoming>,
}

impl Exchange {
    pub(crate) fn status(&self) -> StatusCode {
        self.response.status()
    }

    pub(crate) fn headers(&self) -> &HeaderMap {
        self.response.headers()
    }

    /// The body length the head declares (`Content-Length`, or zero for a
    /// status that carries no body), when it declares one.
    pub(crate) fn declared_length(&self) -> Option<u64> {
        self.response.body().size_hint().exact()
    }

    /// Stream the body through `decoder`, stopping as soon as a chunk would
    /// cross the wire or decoded ceiling. Nothing past a ceiling is kept.
    pub(crate) async fn read_body(
        self,
        mut decoder: BodyDecoder,
    ) -> Result<DecodedBody, AcquisitionFailure> {
        let Self {
            mut driver,
            response,
        } = self;
        let mut body = response.into_body();
        while let Some(frame) = driver.run(body.frame()).await {
            let frame = frame.map_err(|source| exchange_failure(&source, 0))?;
            let Ok(data) = frame.into_data() else {
                // NOTE: trailers carry no body bytes and are not recorded.
                continue;
            };
            decoder.push(&data).map_err(decode_failure)?;
        }
        decoder.finish().map_err(decode_failure)
    }
}

/// Map a body-decoding refusal onto the failure taxonomy.
pub(crate) fn decode_failure(error: DecodeError) -> AcquisitionFailure {
    match error {
        DecodeError::WireLimit { max } => AcquisitionFailure::WireLimit {
            max_body_bytes: max,
        },
        DecodeError::DecodedLimit { max } => AcquisitionFailure::DecodedLimit {
            max_decoded_bytes: max,
        },
        DecodeError::UnsupportedCoding { coding } => {
            AcquisitionFailure::UnsupportedContentEncoding { coding }
        }
        DecodeError::StackedCodings { codings } => AcquisitionFailure::UnsupportedContentEncoding {
            coding: codings.join(", "),
        },
        DecodeError::Corrupt { detail } => {
            AcquisitionFailure::CorruptContentEncoding { reason: detail }
        }
    }
}

/// Send one anonymous `GET` for `url` over `stream` and return the response
/// head.
///
/// The request carries exactly `Host` (the URL host, with the port when it
/// is not the scheme default), `User-Agent`, `Accept` (`*/*` for a document
/// fetch, the provider's media type for a data fetch),
/// `Accept-Encoding: gzip, deflate` (exactly the codings [`BodyDecoder`]
/// removes), and `Connection: close`; no cookies,
/// credentials, `Referer`, or body.
pub(crate) async fn send_get(
    stream: Box<dyn ConnectedStream>,
    url: &Url,
    user_agent: &HeaderValue,
    accept: &HeaderValue,
    limits: &AcquisitionLimits,
) -> Result<Exchange, AcquisitionFailure> {
    let request = build_request(url, user_agent, accept)?;
    let header_limit = limits.max_header_bytes();
    let (sender, conn) = http1::Builder::new()
        // WHY: title-case header names are what older origins expect; the
        // names are case-insensitive for every conforming server.
        .title_case_headers(true)
        // INVARIANT: `AcquisitionLimits` keeps this at or above hyper's
        // minimum, so `max_buf_size` cannot panic.
        .max_buf_size(header_limit)
        .handshake::<Io, Empty<Bytes>>(TokioIo::new(stream))
        .await
        .map_err(|source| exchange_failure(&source, header_limit))?;
    let mut driver = Driver {
        conn: Box::pin(conn),
        finished: false,
    };
    let response = send(&mut driver, sender, request)
        .await
        .map_err(|source| exchange_failure(&source, header_limit))?;
    Ok(Exchange { driver, response })
}

async fn send(
    driver: &mut Driver,
    mut sender: SendRequest<Empty<Bytes>>,
    request: Request<Empty<Bytes>>,
) -> hyper::Result<Response<Incoming>> {
    driver.run(sender.ready()).await?;
    driver.run(sender.send_request(request)).await
}

fn build_request(
    url: &Url,
    user_agent: &HeaderValue,
    accept: &HeaderValue,
) -> Result<Request<Empty<Bytes>>, AcquisitionFailure> {
    let target = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    };
    let host = match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => {
            return Err(AcquisitionFailure::HttpProtocol {
                reason: "URL has no host".to_owned(),
            });
        }
    };
    Request::builder()
        .method(Method::GET)
        .uri(target)
        .header(HOST, host)
        .header(USER_AGENT, user_agent)
        .header(ACCEPT, accept)
        .header(
            ACCEPT_ENCODING,
            HeaderValue::from_static(decode::ACCEPT_ENCODING),
        )
        .header(CONNECTION, HeaderValue::from_static("close"))
        .body(Empty::new())
        .map_err(|source| AcquisitionFailure::HttpProtocol {
            reason: format!("request target not representable: {source}"),
        })
}

/// hyper's message for a response head that overflows `max_buf_size`.
///
/// WHY: `hyper::Error::is_parse_too_large` exists only with hyper's
/// `server` feature, which would compile the server half of hyper (and add
/// `httpdate`) into a client-only library. The message is matched only on
/// errors hyper already classifies as parse errors, and
/// `oversized_response_head_is_header_limit` in the integration tests fails
/// if a hyper upgrade changes it.
const HEAD_TOO_LARGE: &str = "message head is too large";

/// Classify a hyper error from the handshake, request, or body.
fn exchange_failure(source: &hyper::Error, header_limit: usize) -> AcquisitionFailure {
    if source.is_parse() && source.to_string().contains(HEAD_TOO_LARGE) {
        AcquisitionFailure::HeaderLimit {
            max_header_bytes: header_limit,
        }
    } else if source.is_parse() || source.is_parse_status() || source.is_parse_version_h2() {
        AcquisitionFailure::HttpProtocol {
            reason: error_chain(source),
        }
    } else {
        AcquisitionFailure::InterruptedStream {
            reason: error_chain(source),
        }
    }
}

/// Render an error and its source chain; hyper's own `Display` omits the
/// underlying I/O cause.
fn error_chain(source: &hyper::Error) -> String {
    use std::error::Error as _;

    let mut rendered = source.to_string();
    let mut cause = source.source();
    while let Some(inner) = cause {
        rendered.push_str(": ");
        rendered.push_str(&inner.to_string());
        cause = inner.source();
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_carries_only_the_fixed_headers() {
        let request = build_request(
            &Url::parse("http://Public.Example:8080/a/b?q=1#frag").unwrap(),
            &HeaderValue::from_static("zetesis-test"),
            &HeaderValue::from_static("*/*"),
        )
        .unwrap();
        assert_eq!(request.uri(), "/a/b?q=1", "origin-form target, no fragment");
        let names: Vec<&str> = request
            .headers()
            .keys()
            .map(hyper::header::HeaderName::as_str)
            .collect();
        assert_eq!(
            names,
            [
                "host",
                "user-agent",
                "accept",
                "accept-encoding",
                "connection"
            ],
            "no cookie, credential, referer, or body header"
        );
        assert_eq!(
            request.headers()[HOST],
            "public.example:8080",
            "Host keeps a non-default port"
        );
    }

    #[test]
    fn build_request_sends_the_profiles_accept_and_user_agent() {
        let request = build_request(
            &Url::parse("https://api.example/search?q=1").unwrap(),
            &HeaderValue::from_static("client/1.0 (https://example.org/contact)"),
            &HeaderValue::from_static("application/json"),
        )
        .unwrap();
        assert_eq!(
            request.headers()[ACCEPT],
            "application/json",
            "a data fetch asks for its media type"
        );
        assert_eq!(
            request.headers()[USER_AGENT],
            "client/1.0 (https://example.org/contact)",
            "and sends the User-Agent it was given"
        );
    }

    #[test]
    fn build_request_omits_default_port_and_brackets_ipv6() {
        let request = build_request(
            &Url::parse("https://[2001:4860:4860::8888]:443/").unwrap(),
            &HeaderValue::from_static("zetesis-test"),
            &HeaderValue::from_static("*/*"),
        )
        .unwrap();
        assert_eq!(
            request.headers()[HOST],
            "[2001:4860:4860::8888]",
            "default port omitted, IPv6 literal bracketed"
        );
    }
}
