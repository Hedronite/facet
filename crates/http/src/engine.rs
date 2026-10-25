use std::{borrow::Cow, future::Future, future::pending, path::Path, time::Instant};

use probe_core::{HttpRequest, RequestSettings};
use reqwest::{Client, header::HeaderMap, redirect::Policy};

use crate::{
    ClusterTls, ExecutionOptions, HttpError, HttpResponse, ResponseHeader,
    request::build_request,
    response::{CollectedBody, collect_bounded, map_reqwest_error, stream_to_file},
};

const DEFAULT_MAX_REDIRECTS: usize = 10;

/// Reusable asynchronous HTTP engine shared by every interface.
#[derive(Clone)]
pub struct HttpEngine {
    default_client: Client,
    cluster_tls: Option<ClusterTls>,
}

impl std::fmt::Debug for HttpEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEngine")
            .field("cluster_tls", &self.cluster_tls)
            .finish_non_exhaustive()
    }
}

impl HttpEngine {
    /// Creates an engine with the default redirect policy.
    pub fn new() -> Result<Self, HttpError> {
        Ok(Self {
            default_client: build_client(true, DEFAULT_MAX_REDIRECTS, None, false)?,
            cluster_tls: None,
        })
    }

    /// Creates a verified client-certificate engine restricted to one cluster.
    /// Credential material remains in the transport and is never added to the
    /// request model, persisted history, or presentation output.
    pub fn with_cluster_tls(cluster_tls: ClusterTls) -> Result<Self, HttpError> {
        Ok(Self {
            default_client: build_client(true, DEFAULT_MAX_REDIRECTS, Some(&cluster_tls), false)?,
            cluster_tls: Some(cluster_tls),
        })
    }

    /// Executes a request until completion.
    pub async fn execute(
        &self,
        request: &HttpRequest,
        options: &ExecutionOptions,
    ) -> Result<HttpResponse, HttpError> {
        self.execute_cancellable(request, options, pending::<()>())
            .await
    }

    /// Executes a request while streaming its response body to a file.
    ///
    /// The destination is replaced only after the full response body has been written.
    pub async fn execute_to_file(
        &self,
        request: &HttpRequest,
        options: &ExecutionOptions,
        output: &Path,
    ) -> Result<HttpResponse, HttpError> {
        self.execute_cancellable_to_file(request, options, output, pending::<()>())
            .await
    }

    /// Executes a request, cancelling it when `cancellation` completes.
    ///
    /// Dropping the execution future also cancels the underlying reqwest request.
    pub async fn execute_cancellable<C>(
        &self,
        request: &HttpRequest,
        options: &ExecutionOptions,
        cancellation: C,
    ) -> Result<HttpResponse, HttpError>
    where
        C: Future + Send,
    {
        self.execute_with_cancellation(request, options, None, false, cancellation)
            .await
    }

    /// Executes a cancellable request while streaming its response body to a file.
    pub async fn execute_cancellable_to_file<C>(
        &self,
        request: &HttpRequest,
        options: &ExecutionOptions,
        output: &Path,
        cancellation: C,
    ) -> Result<HttpResponse, HttpError>
    where
        C: Future + Send,
    {
        self.execute_with_cancellation(request, options, Some(output), false, cancellation)
            .await
    }

    /// Secret-request send: no redirects and no proxy environment.
    pub async fn execute_cancellable_locked<C>(
        &self,
        request: &HttpRequest,
        options: &ExecutionOptions,
        cancellation: C,
    ) -> Result<HttpResponse, HttpError>
    where
        C: Future + Send,
    {
        self.execute_with_cancellation(request, options, None, true, cancellation)
            .await
    }

    /// Secret-request send to a file: no redirects and no proxy environment.
    pub async fn execute_cancellable_to_file_locked<C>(
        &self,
        request: &HttpRequest,
        options: &ExecutionOptions,
        output: &Path,
        cancellation: C,
    ) -> Result<HttpResponse, HttpError>
    where
        C: Future + Send,
    {
        self.execute_with_cancellation(request, options, Some(output), true, cancellation)
            .await
    }

    async fn execute_with_cancellation<C>(
        &self,
        request: &HttpRequest,
        options: &ExecutionOptions,
        output: Option<&Path>,
        lock_transport: bool,
        cancellation: C,
    ) -> Result<HttpResponse, HttpError>
    where
        C: Future + Send,
    {
        tokio::pin!(cancellation);
        tokio::select! {
            biased;
            _ = &mut cancellation => Err(HttpError::Cancelled),
            response = self.execute_inner(request, options, output, lock_transport) => response,
        }
    }

    async fn execute_inner(
        &self,
        request: &HttpRequest,
        options: &ExecutionOptions,
        output: Option<&Path>,
        lock_transport: bool,
    ) -> Result<HttpResponse, HttpError> {
        let client = self.client_for(&request.settings, lock_transport)?;
        let builder = build_request(&client, request, options).await?;
        let built = builder.build().map_err(map_reqwest_error)?;
        if let Some(tls) = &self.cluster_tls {
            // Validate the final URL after path/query substitution, not the
            // template URL. Never send certificate credentials to another origin.
            if !tls.permits(built.url()) {
                return Err(crate::cluster_tls::configuration(
                    "request is outside the configured cluster origin",
                ));
            }
            if built.headers().keys().any(|name| {
                matches!(
                    name.as_str(),
                    "authorization" | "proxy-authorization" | "host"
                ) || name.as_str().starts_with("impersonate-")
            }) {
                return Err(crate::cluster_tls::configuration(
                    "cluster certificate profile cannot be combined with authentication, Host or impersonation headers",
                ));
            }
        }
        let started = Instant::now();
        let mut response = client.execute(built).await.map_err(map_reqwest_error)?;
        let expected_size = response.content_length();
        let status = response.status();
        let url = response.url().to_string();
        let headers = response_headers(response.headers());
        let body = match output {
            Some(output) => CollectedBody {
                preview: Vec::new(),
                size: stream_to_file(&mut response, output).await?,
                complete: false,
                file: None,
                retention_error: None,
            },
            None => {
                collect_bounded(
                    &mut response,
                    options.response_cache.as_ref(),
                    expected_size,
                )
                .await?
            }
        };
        Ok(HttpResponse {
            status: status.as_u16(),
            reason: status.canonical_reason().unwrap_or_default().to_owned(),
            url,
            duration: started.elapsed(),
            size: body.size,
            headers,
            body: body.preview,
            body_complete: body.complete,
            body_file: body.file,
            body_retention_error: body.retention_error,
        })
    }

    fn client_for(
        &self,
        settings: &RequestSettings,
        lock_transport: bool,
    ) -> Result<Cow<'_, Client>, HttpError> {
        if lock_transport {
            return build_client(
                false,
                DEFAULT_MAX_REDIRECTS,
                self.cluster_tls.as_ref(),
                true,
            )
            .map(Cow::Owned);
        }
        let follow = settings.follow_redirects.unwrap_or(true);
        let maximum = settings.max_redirects.unwrap_or(DEFAULT_MAX_REDIRECTS);
        if follow && maximum == DEFAULT_MAX_REDIRECTS {
            Ok(Cow::Borrowed(&self.default_client))
        } else {
            build_client(follow, maximum, self.cluster_tls.as_ref(), false).map(Cow::Owned)
        }
    }
}

fn build_client(
    follow_redirects: bool,
    maximum: usize,
    cluster_tls: Option<&ClusterTls>,
    no_proxy: bool,
) -> Result<Client, HttpError> {
    let policy = if !follow_redirects {
        Policy::none()
    } else if let Some(tls) = cluster_tls {
        let origin = tls.origin.origin();
        Policy::custom(move |attempt| {
            if attempt.url().origin() != origin
                || !attempt.url().username().is_empty()
                || attempt.url().password().is_some()
            {
                attempt.error("redirect is outside the configured cluster origin")
            } else if attempt.previous().len() >= maximum {
                attempt.error("too many redirects")
            } else {
                attempt.follow()
            }
        })
    } else {
        Policy::limited(maximum)
    };
    let mut builder = Client::builder().redirect(policy);
    if cluster_tls.is_some() || no_proxy {
        builder = builder.no_proxy();
    }
    if let Some(tls) = cluster_tls {
        builder = builder
            .tls_certs_only(tls.roots.clone())
            .identity(tls.identity.clone())
            .tls_sslkeylogfile(false);
    }
    builder
        .build()
        .map_err(|error| HttpError::ClientConfiguration(error.to_string()))
}

fn response_headers(headers: &HeaderMap) -> Vec<ResponseHeader> {
    let mut headers: Vec<_> = headers
        .iter()
        .map(|(name, value)| ResponseHeader {
            name: name.as_str().to_owned(),
            value: String::from_utf8_lossy(value.as_bytes()).into_owned(),
        })
        .collect();
    headers.sort_by(|left, right| (&left.name, &left.value).cmp(&(&right.name, &right.value)));
    headers
}
