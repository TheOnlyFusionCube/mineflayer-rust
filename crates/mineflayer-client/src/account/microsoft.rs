use std::path::PathBuf;

use mineflayer_auth::{
    AccessTokenResponse, AuthOpts,
    certs::Certificates,
    sessionserver::{self, ClientSessionServerError, SessionServerJoinOpts},
};
use parking_lot::Mutex;
use uuid::Uuid;

use crate::account::{Account, AccountTrait, BoxFuture};

fn default_cache_file() -> PathBuf {
    let minecraft_dir = minecraft_folder_path::minecraft_dir().unwrap_or_else(|| {
        panic!(
            "No {} environment variable found",
            minecraft_folder_path::home_env_var()
        )
    });
    minecraft_dir.join("mineflayer-auth.json")
}

/// Options for Microsoft authentication in the engine.
///
/// This is used by [`Account::microsoft_with_opts`].
#[derive(Clone, Debug, Default)]
pub struct MicrosoftAccountOpts {
    /// Whether we should check if the user owns the game.
    pub check_ownership: bool,
    /// The cache file to use for the auth cache.
    ///
    /// If this is `None`, the engine will default to its standard cache file
    /// (`~/.minecraft/mineflayer-auth.json`).
    pub cache_file: Option<PathBuf>,
    /// An override for the Microsoft Client ID to authenticate with.
    pub client_id: Option<String>,
    /// An override for the OAuth2 scope to authenticate with.
    pub scope: Option<String>,
}

impl MicrosoftAccountOpts {
    fn to_auth_opts(&self) -> AuthOpts<'_> {
        let cache_file = self
            .cache_file
            .clone()
            .or_else(|| Some(default_cache_file()));

        AuthOpts {
            check_ownership: self.check_ownership,
            cache_file,
            client_id: self.client_id.as_deref(),
            scope: self.scope.as_deref(),
        }
    }
}

fn default_account_opts(client_id: Option<&str>, scope: Option<&str>) -> MicrosoftAccountOpts {
    MicrosoftAccountOpts {
        check_ownership: false,
        cache_file: Some(default_cache_file()),
        client_id: client_id.map(str::to_owned),
        scope: scope.map(str::to_owned),
    }
}

/// A type of account that authenticates with Microsoft using the engine's
/// cache.
///
/// This type is not intended to be used directly by the user. To actually make
/// an account that authenticates with Microsoft, see [`Account::microsoft`] or
/// [`Account::microsoft_with_opts`].
#[derive(Debug)]
pub struct MicrosoftAccount {
    cache_key: String,
    auth_opts: MicrosoftAccountOpts,

    username: String,
    uuid: Uuid,

    access_token: Mutex<String>,
    certs: Mutex<Option<Certificates>>,
}
impl MicrosoftAccount {
    // deliberately private, use `Account::microsoft` or
    // `Account::microsoft_with_opts` instead.
    async fn new(
        cache_key: &str,
        auth_opts: MicrosoftAccountOpts,
    ) -> Result<Self, mineflayer_auth::AuthError> {
        let auth_result = mineflayer_auth::auth(cache_key, auth_opts.to_auth_opts()).await?;

        Ok(Self {
            cache_key: cache_key.to_owned(),
            auth_opts,
            username: auth_result.profile.name,
            uuid: auth_result.profile.id,
            access_token: Mutex::new(auth_result.access_token),
            certs: Mutex::new(None),
        })
    }
}
impl AccountTrait for MicrosoftAccount {
    fn username(&self) -> &str {
        &self.username
    }
    fn uuid(&self) -> Uuid {
        self.uuid
    }
    fn access_token(&self) -> Option<String> {
        Some(self.access_token.lock().to_owned())
    }
    fn certs(&self) -> Option<mineflayer_auth::certs::Certificates> {
        self.certs.lock().as_ref().cloned()
    }
    fn set_certs(&self, certs: mineflayer_auth::certs::Certificates) {
        *self.certs.lock() = Some(certs);
    }
    fn refresh(&self) -> BoxFuture<'_, Result<(), mineflayer_auth::AuthError>> {
        Box::pin(async {
            let new_account =
                MicrosoftAccount::new(&self.cache_key, self.auth_opts.clone()).await?;
            let new_access_token = new_account.access_token().unwrap();
            *self.access_token.lock() = new_access_token;
            Ok(())
        })
    }
    fn join<'a>(
        &'a self,
        public_key: &'a [u8],
        private_key: &'a [u8; 16],
        server_id: &'a str,
        proxy: Option<reqwest::Proxy>,
    ) -> BoxFuture<'a, Result<(), ClientSessionServerError>> {
        Box::pin(async move {
            let access_token = self.access_token.lock().clone();
            sessionserver::join(SessionServerJoinOpts {
                access_token: &access_token,
                public_key,
                private_key,
                uuid: &self.uuid(),
                server_id,
                proxy,
            })
            .await
        })
    }
}

/// A type of account that authenticates using a Microsoft access token that the
/// user directly passes.
///
/// This does not use the engine's account cache.
///
/// This type is not intended to be used directly by the user. To actually make
/// an account that authenticates with Microsoft like this, see
/// [`Account::with_microsoft_access_token`].
#[derive(Debug)]
pub struct MicrosoftWithAccessTokenAccount {
    msa: Mutex<mineflayer_auth::cache::ExpiringValue<AccessTokenResponse>>,

    username: String,
    uuid: Uuid,

    access_token: Mutex<String>,
    certs: Mutex<Option<Certificates>>,
}
impl MicrosoftWithAccessTokenAccount {
    async fn new(
        msa: mineflayer_auth::cache::ExpiringValue<AccessTokenResponse>,
        client_id: Option<&str>,
        scope: Option<&str>,
    ) -> Result<Self, mineflayer_auth::AuthError> {
        let client = reqwest::Client::new();

        let mut msa = msa.clone();

        if msa.is_expired() {
            use tracing::trace;

            trace!("refreshing Microsoft auth token");
            msa = mineflayer_auth::refresh_ms_auth_token(
                &client,
                &msa.data.refresh_token,
                client_id,
                scope,
            )
            .await?;
        }

        let msa_token = &msa.data.access_token;
        let res = mineflayer_auth::get_minecraft_token(&client, msa_token).await?;
        let profile = mineflayer_auth::get_profile(&client, &res.minecraft_access_token).await?;

        Ok(Self {
            username: profile.name,
            access_token: Mutex::new(res.minecraft_access_token),
            uuid: profile.id,
            msa: Mutex::new(msa),
            certs: Mutex::new(None),
        })
    }
}
impl AccountTrait for MicrosoftWithAccessTokenAccount {
    fn username(&self) -> &str {
        &self.username
    }
    fn uuid(&self) -> Uuid {
        self.uuid
    }
    fn access_token(&self) -> Option<String> {
        Some(self.access_token.lock().to_owned())
    }
    fn certs(&self) -> Option<mineflayer_auth::certs::Certificates> {
        self.certs.lock().as_ref().cloned()
    }
    fn set_certs(&self, certs: mineflayer_auth::certs::Certificates) {
        *self.certs.lock() = Some(certs);
    }
    fn refresh(&self) -> BoxFuture<'_, Result<(), mineflayer_auth::AuthError>> {
        Box::pin(async {
            let msa_value = self.msa.lock().clone();
            let new_account = MicrosoftWithAccessTokenAccount::new(msa_value, None, None).await?;

            let new_access_token = new_account.access_token().unwrap();

            *self.access_token.lock() = new_access_token;
            *self.msa.lock() = new_account.msa.lock().clone();

            Ok(())
        })
    }
    fn join<'a>(
        &'a self,
        public_key: &'a [u8],
        private_key: &'a [u8; 16],
        server_id: &'a str,
        proxy: Option<reqwest::Proxy>,
    ) -> BoxFuture<'a, Result<(), ClientSessionServerError>> {
        Box::pin(async move {
            let access_token = self.access_token.lock().clone();
            sessionserver::join(SessionServerJoinOpts {
                access_token: &access_token,
                public_key,
                private_key,
                uuid: &self.uuid(),
                server_id,
                proxy,
            })
            .await
        })
    }
}

impl Account {
    /// This will create an online-mode account by authenticating with
    /// Microsoft's servers.
    ///
    /// The cache key is used for avoiding having to log in every time. This is
    /// typically set to the account email, but it can be any string.
    #[cfg(feature = "online-mode")]
    pub async fn microsoft(cache_key: &str) -> Result<Self, mineflayer_auth::AuthError> {
        MicrosoftAccount::new(cache_key, default_account_opts(None, None))
            .await
            .map(Account::from)
    }

    /// Similar to [`Account::microsoft`] but you can pass custom auth options
    /// (including the cache file location).
    ///
    /// For a custom cache directory, set
    /// `auth_opts.cache_file = Some(custom_dir.join("mineflayer-auth.json"))`.
    ///
    /// If `auth_opts.cache_file` is `None`, it will default to the engine's
    /// standard cache file (`~/.minecraft/mineflayer-auth.json`) to match
    /// [`Account::microsoft`].
    #[cfg(feature = "online-mode")]
    pub async fn microsoft_with_opts(
        cache_key: &str,
        auth_opts: MicrosoftAccountOpts,
    ) -> Result<Self, mineflayer_auth::AuthError> {
        MicrosoftAccount::new(cache_key, auth_opts)
            .await
            .map(Account::from)
    }

    /// Similar to [`Account::microsoft`] but you can use your own `client_id`
    /// and `scope`.
    ///
    /// Pass `None` if you want to use default ones.
    #[cfg(feature = "online-mode")]
    #[deprecated(note = "Use `Account::microsoft_with_opts` instead.")]
    pub async fn microsoft_with_custom_client_id_and_scope(
        cache_key: &str,
        client_id: Option<&str>,
        scope: Option<&str>,
    ) -> Result<Self, mineflayer_auth::AuthError> {
        MicrosoftAccount::new(cache_key, default_account_opts(client_id, scope))
            .await
            .map(Account::from)
    }

    /// This will create an online-mode account through
    /// [`mineflayer_auth::get_minecraft_token`] so you can have more control
    /// over the authentication process (like doing your own caching or
    /// displaying the Microsoft user code to the user in a different way).
    ///
    /// This will refresh the given token if it's expired.
    ///
    /// ```
    /// # use mineflayer_client::Account;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = reqwest::Client::new();
    ///
    /// let res = mineflayer_auth::get_ms_link_code(&client, None, None).await?;
    /// // Or, `mineflayer_auth::get_ms_link_code(&client, Some(client_id), None).await?`
    /// // if you want to use your own client_id
    /// println!(
    ///     "Go to {} and enter the code {}",
    ///     res.verification_uri, res.user_code
    /// );
    /// let msa = mineflayer_auth::get_ms_auth_token(&client, res, None).await?;
    /// Account::with_microsoft_access_token(msa).await?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "online-mode")]
    pub async fn with_microsoft_access_token(
        msa: mineflayer_auth::cache::ExpiringValue<AccessTokenResponse>,
    ) -> Result<Self, mineflayer_auth::AuthError> {
        Self::with_microsoft_access_token_and_custom_client_id_and_scope(msa, None, None).await
    }

    /// Similar to [`Account::with_microsoft_access_token`] but you can use
    /// custom `client_id` and `scope`.
    #[cfg(feature = "online-mode")]
    pub async fn with_microsoft_access_token_and_custom_client_id_and_scope(
        msa: mineflayer_auth::cache::ExpiringValue<AccessTokenResponse>,
        client_id: Option<&str>,
        scope: Option<&str>,
    ) -> Result<Self, mineflayer_auth::AuthError> {
        MicrosoftWithAccessTokenAccount::new(msa, client_id, scope)
            .await
            .map(Account::from)
    }
}
