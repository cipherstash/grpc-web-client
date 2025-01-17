use web_sys::WorkerGlobalScope;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen::JsCast;
use web_sys::RequestInit;
use crate::{ClientError, Client};

pub(crate) async fn fetch_with_request(request: web_sys::Request) -> Result<web_sys::Response, ClientError> {
    let worker: WorkerGlobalScope = js_sys::global().unchecked_into();

    let fetch = JsFuture::from(worker.fetch_with_request(&request))
    .await
    .map_err(ClientError::FetchFailed)?;

    fetch.dyn_into().map_err(ClientError::FetchFailed)
}

pub(crate) fn post_init(_client: Client) -> RequestInit {
    let mut init = RequestInit::new();
    init.method("POST");

    init
}