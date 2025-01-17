mod call;
use cfg_if::cfg_if;

cfg_if! {
    if #[cfg(feature = "web-worker")] {
        mod worker;
        use crate::worker as request;
    } else {
        mod browser;
        use crate::browser as request;
    }
}

use bytes::Bytes;
use call::{Encoding, GrpcWebCall};
use core::{
    fmt,
    task::{Context, Poll},
};
use futures::{Future, Stream, TryStreamExt};
use http::{
    header::{HeaderName, InvalidHeaderName, InvalidHeaderValue, ToStrError},
    request::Request,
    response::Response,
    HeaderMap, HeaderValue,
};
use http_body::Body;
use js_sys::{Array, Uint8Array};
use std::{error::Error, pin::Pin};
use tonic::{body::BoxBody, client::GrpcService, Status};
use wasm_bindgen::{JsCast, JsValue};
use wasm_streams::ReadableStream;
use web_sys::Headers;

#[derive(Debug, Clone, PartialEq)]
pub enum ClientError {
    Err,
    FetchFailed(JsValue),
    Other(String),
}

impl From<ToStrError> for ClientError {
    fn from(_: ToStrError) -> Self {
        Self::Other("Header value contained invalid ASCII".into())
    }
}

impl From<InvalidHeaderName> for ClientError {
    fn from(_: InvalidHeaderName) -> Self {
        Self::Other("Header name was invalid".into())
    }
}

impl From<InvalidHeaderValue> for ClientError {
    fn from(_: InvalidHeaderValue) -> Self {
        Self::Other("Header value was invalid".into())
    }
}

impl From<JsValue> for ClientError {
    fn from(val: JsValue) -> Self {
        Self::FetchFailed(val)
    }
}

impl Error for ClientError {}
impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

pub type CredentialsMode = web_sys::RequestCredentials;

pub type RequestMode = web_sys::RequestMode;

#[derive(Clone)]
pub struct Client {
    base_uri: String,
    credentials: CredentialsMode,
    mode: RequestMode,
    encoding: Encoding,
}

impl Client {
    pub fn new(base_uri: String) -> Self {
        Client {
            base_uri,
            credentials: CredentialsMode::SameOrigin,
            mode: RequestMode::Cors,
            encoding: Encoding::None,
        }
    }

    async fn request(self, rpc: Request<BoxBody>) -> Result<Response<BoxBody>, ClientError> {
        let mut uri = rpc.uri().to_string();
        uri.insert_str(0, &self.base_uri);

        let headers =
            Headers::new().map_err(|_| ClientError::Other("Failed to create headers".into()))?;

        for (k, v) in rpc.headers().iter() {
            headers.set(k.as_str(), v.to_str()?)?;
        }
        headers.set("x-user-agent", "grpc-web-rust/0.1")?;
        headers.set("content-type", self.encoding.to_content_type())?;

        let body_bytes = hyper::body::to_bytes(rpc.into_body())
            .await
            .map_err(|_| ClientError::Other("Failed to convert RPC body to bytes".into()))?;

        let body_array: Uint8Array = body_bytes.as_ref().into();
        let body_js: &JsValue = body_array.as_ref();

        let mut init = request::post_init(self);
        init.body(Some(body_js)).headers(headers.as_ref());

        let request = web_sys::Request::new_with_str_and_init(&uri, &init)?;
        let fetch_res = request::fetch_with_request(request).await?;

        let mut res = Response::builder().status(fetch_res.status());
        let headers = res
            .headers_mut()
            .ok_or_else(|| ClientError::Other("Could not get response headers".into()))?;

        for kv in js_sys::try_iter(fetch_res.headers().as_ref())?
            .ok_or_else(|| ClientError::Other("Response headers iterator was empty".into()))?
        {
            let pair: Array = kv?.into();
            headers.append(
                HeaderName::from_bytes(
                    pair.get(0)
                        .as_string()
                        .ok_or_else(|| ClientError::Other("Header pair had no name".into()))?
                        .as_bytes(),
                )?,
                HeaderValue::from_str(
                    &pair
                        .get(1)
                        .as_string()
                        .ok_or_else(|| ClientError::Other("Header pair had no value".into()))?,
                )?,
            );
        }

        let body_stream = ReadableStream::from_raw(
            fetch_res
                .body()
                .ok_or_else(|| ClientError::Other("Response body was empty".into()))?
                .unchecked_into(),
        );
        let body = GrpcWebCall::client_response(
            ReadableStreamBody::new(body_stream),
            Encoding::from_content_type(headers),
        );

        Ok(res
            .body(BoxBody::new(body))
            .map_err(|e| ClientError::Other(format!("An HTTP error ocurred: {}", e)))?)
    }
}

impl GrpcService<BoxBody> for Client {
    type ResponseBody = BoxBody;
    type Error = ClientError;
    type Future = Pin<Box<dyn Future<Output = Result<Response<BoxBody>, ClientError>>>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, rpc: Request<BoxBody>) -> Self::Future {
        Box::pin(self.clone().request(rpc))
    }
}

struct ReadableStreamBody {
    stream: Pin<Box<dyn Stream<Item = Result<Bytes, Status>>>>,
}

impl ReadableStreamBody {
    fn new(inner: ReadableStream) -> Self {
        ReadableStreamBody {
            stream: Box::pin(
                inner
                    .into_stream()
                    .map_ok(|buf_js| {
                        let buffer = Uint8Array::new(&buf_js);
                        let mut bytes_vec = vec![0; buffer.length() as usize];
                        buffer.copy_to(&mut bytes_vec);
                        let bytes: Bytes = bytes_vec.into();
                        bytes
                    })
                    .map_err(|_| Status::unknown("readablestream error")),
            ),
        }
    }
}

impl Body for ReadableStreamBody {
    type Data = Bytes;
    type Error = Status;

    fn poll_data(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        self.stream.as_mut().poll_next(cx)
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Result<Option<HeaderMap>, Self::Error>> {
        Poll::Ready(Ok(None))
    }

    fn is_end_stream(&self) -> bool {
        false
    }
}

// WARNING: these are required to satisfy the Body and Error traits, but JsValue is not thread-safe.
// This shouldn't be an issue because wasm doesn't have threads currently.

unsafe impl Sync for ReadableStreamBody {}
unsafe impl Send for ReadableStreamBody {}

unsafe impl Sync for ClientError {}
unsafe impl Send for ClientError {}
