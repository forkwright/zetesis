//! Private one-hop HTTP/1.1 exchange over a stream the acquirer connected.
//!
//! One request per connection (`Connection: close`), no pool, no automatic
//! redirects, no decompression. The hyper connection future is driven
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

    /// Read the body, stopping as soon as it would exceed `max_body_bytes`.
    pub(crate) async fn read_body(
        self,
        max_body_bytes: u64,
    ) -> Result<Vec<u8>, AcquisitionFailure> {
        let Self {
            mut driver,
            response,
        } = self;
        let mut body = response.into_body();
        let over_limit = AcquisitionFailure::WireLimit { max_body_bytes };
        if body.size_hint().lower() > max_body_bytes {
            return Err(over_limit);
        }
        let capacity = body.size_hint().exact().unwrap_or(0);
        let mut bytes = Vec::with_capacity(usize::try_from(capacity).unwrap_or(0));
        let mut read: u64 = 0;
        while let Some(frame) = driver.run(body.frame()).await {
            let frame = frame.map_err(|source| exchange_failure(&source, 0))?;
            let Ok(data) = frame.into_data() else {
                // NOTE: trailers carry no body bytes and are not recorded.
                continue;
            };
            read = read.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
            if read > max_body_bytes {
                return Err(over_limit);
            }
            bytes.extend_from_slice(&data);
        }
        Ok(bytes)
    }
}

/// Send one anonymous `GET` for `url` over `stream` and return the response
/// head.
///
/// The request carries exactly `Host` (the URL host, with the port when it
/// is not the scheme default), `User-Agent`, `Accept: */*`,
/// `Accept-Encoding: identity`, and `Connection: close`; no cookies,
/// credentials, `Referer`, or body.
pub(crate) async fn send_get(
    stream: Box<dyn ConnectedStream>,
    url: &Url,
    user_agent: &HeaderValue,
    limits: &AcquisitionLimits,
) -> Result<Exchange, AcquisitionFailure> {
    let request = build_request(url, user_agent)?;
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
        .header(ACCEPT, HeaderValue::from_static("*/*"))
        .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
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
    fn build_request_omits_default_port_and_brackets_ipv6() {
        let request = build_request(
            &Url::parse("https://[2001:4860:4860::8888]:443/").unwrap(),
            &HeaderValue::from_static("zetesis-test"),
        )
        .unwrap();
        assert_eq!(
            request.headers()[HOST],
            "[2001:4860:4860::8888]",
            "default port omitted, IPv6 literal bracketed"
        );
    }
}
