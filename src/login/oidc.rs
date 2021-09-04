use std::collections::HashMap;

use crate::{
    api::AuthInfo,
    client::Client,
    error::ClientError,
    login::core::{MultiLoginCallback, MultiLoginMethod},
};
use async_trait::async_trait;
use tiny_http::{Response, Server};
use tokio::task::JoinHandle;

/// A login method which uses OIDC credentials for obtaining a new token.
#[derive(Debug)]
pub struct OIDCLogin {
    pub port: Option<u16>,    // Defaults to 8250
    pub role: Option<String>, // Defaults to what's configured in the backend
}

/// The callback for the OIDC login method.
#[derive(Debug)]
pub struct OIDCCallback {
    pub handle: JoinHandle<OIDCCallbackParams>,
    pub url: String,
}

// The parameters returned by the OAuth authorization server after successful
// authentication.
#[derive(Debug, Default)]
pub struct OIDCCallbackParams {
    pub code: String,
    pub nonce: String,
    pub state: String,
}

#[async_trait]
impl MultiLoginMethod for OIDCLogin {
    type Callback = OIDCCallback;

    /// Runs a standalone HTTP server which listens for the OIDC callback.
    ///
    /// This method performs several things. It firsts constructs a redirect URL
    /// which points back to the HTTP address of the web server it starts. It
    /// then asks Vault for an authroization URL using the constructed redirect.
    /// Finally, it starts a small HTTP server that listens for the redirect
    /// from the OAuth authorization server, capturing the various parameters
    /// and returning them as a [OIDCCallbackParams].
    ///
    /// The function returns an [OIDCCallback] which contains the authorization
    /// URL generated by Vault which an end-user must visit to complete the
    /// authorization flow. It also returns a handle to the task running the
    /// HTTP server. The `callback` method can be awaited on and will only
    /// return once the redirect has been received.
    async fn login<C: MultiLoginCallback>(
        &self,
        client: &impl Client,
        mount: &str,
    ) -> Result<Self::Callback, ClientError> {
        // The Vault CLI uses http://localhost:8250/oidc/callback by default, so
        // we match that here to try and remain consistent
        let port = self.port.unwrap_or(8250);
        let ip = "127.0.0.1";
        let hostname = "localhost";

        let base = url::Url::parse(format!("http://{}:{}", hostname, port).as_str()).unwrap();
        let redirect = base.join("oidc/callback").unwrap().to_string();
        let response =
            crate::auth::oidc::auth(client, mount, redirect.as_str(), self.role.clone()).await?;
        let server = Server::http(format!("{}:{}", ip, port)).unwrap();

        let handle = tokio::task::spawn_blocking(move || {
            let mut result = OIDCCallbackParams::default();
            for request in server.incoming_requests() {
                let url = base.join(request.url()).unwrap();
                let query: HashMap<_, _> = url.query_pairs().into_owned().collect();

                result.code = query
                    .get("code")
                    .cloned()
                    .or_else(|| Some("".to_string()))
                    .unwrap();
                result.nonce = query
                    .get("nonce")
                    .cloned()
                    .or_else(|| Some("".to_string()))
                    .unwrap();
                result.state = query
                    .get("state")
                    .cloned()
                    .or_else(|| Some("".to_string()))
                    .unwrap();

                request
                    .respond(Response::from_string("Success!"))
                    .expect("Error responding!");
                server.unblock();
            }
            result
        });

        Ok(OIDCCallback {
            handle,
            url: response.auth_url,
        })
    }
}

#[async_trait]
impl MultiLoginCallback for OIDCCallback {
    /// Exchanges OIDC callback parameters for a Vault token.
    ///
    /// This method will block until the underlying HTTP server recieves a
    /// request from the OAuth authorization server at the redirect URL. It uses
    /// the resulting state, code, and nonce to retrieve a token from Vault.
    async fn callback(self, client: &impl Client, mount: &str) -> Result<AuthInfo, ClientError> {
        let result = self.handle.await.unwrap();
        crate::auth::oidc::callback(
            client,
            mount,
            result.state.as_str(),
            result.nonce.as_str(),
            result.code.as_str(),
        )
        .await
    }
}
