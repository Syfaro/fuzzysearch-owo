use actix_web::{get, services, web, HttpResponse, Scope};
use async_trait::async_trait;
use oauth2::{
    AccessToken, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, RedirectUrl,
    TokenResponse, TokenUrl,
};
use sha2::Digest;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

use crate::jobs::{self, search_existing_submissions_job, JobContext, JobInitiator};
use crate::models::{LinkedAccount, Site};
use crate::site::{CollectedSite, SiteFromConfig, SiteJob, SiteServices};
use crate::{extract_args, models, Config, Error};

pub struct DeviantArt {
    auth_url: AuthUrl,
    token_url: TokenUrl,

    client_id: ClientId,
    client_secret: ClientSecret,

    redirect_url: RedirectUrl,
}

impl DeviantArt {
    const AUTH_URL: &'static str = "https://www.deviantart.com/oauth2/authorize";
    const TOKEN_URL: &'static str = "https://www.deviantart.com/oauth2/token";

    fn new<S: Into<String>>(client_id: S, client_secret: S, base_url: S) -> Result<Self, Error> {
        let client_id = ClientId::new(client_id.into());
        let client_secret = ClientSecret::new(client_secret.into());

        let auth_url = AuthUrl::new(Self::AUTH_URL.to_string()).map_err(Error::from_displayable)?;
        let token_url = TokenUrl::new(Self::TOKEN_URL.into()).map_err(Error::from_displayable)?;

        let redirect_url = RedirectUrl::new(format!("{}/deviantart/callback", base_url.into()))
            .map_err(Error::from_displayable)?;

        Ok(Self {
            auth_url,
            token_url,
            client_id,
            client_secret,
            redirect_url,
        })
    }

    fn get_oauth_client(&self) -> oauth2::basic::BasicClient {
        oauth2::basic::BasicClient::new(
            self.client_id.clone(),
            Some(self.client_secret.clone()),
            self.auth_url.clone(),
            Some(self.token_url.clone()),
        )
        .set_redirect_uri(self.redirect_url.clone())
    }

    async fn refresh_credentials(
        &self,
        conn: &sqlx::Pool<sqlx::Postgres>,
        redlock: &redlock::RedLock,
        data: &types::DeviantArtData,
        account_id: Uuid,
    ) -> Result<AccessToken, Error> {
        super::refresh_credentials(
            redlock,
            account_id,
            data,
            || self.get_oauth_client(),
            |item| {
                Some((
                    item.access_token.clone(),
                    item.refresh_token.clone(),
                    item.expires_after,
                ))
            },
            || async {
                let account = models::LinkedAccount::lookup_by_id(conn, account_id)
                    .await?
                    .ok_or(Error::Missing)?;

                let data = account.data.ok_or(Error::Missing)?;
                let data: types::DeviantArtData = serde_json::from_value(data)?;

                Ok(data)
            },
            |_data, access_token, refresh_token, expires_after| async move {
                let data = serde_json::to_value(types::DeviantArtData {
                    access_token,
                    refresh_token,
                    expires_after,
                })?;

                models::LinkedAccount::update_data(conn, account_id, Some(data)).await?;

                Ok(())
            },
        )
        .await
    }
}

#[async_trait(?Send)]
impl SiteFromConfig for DeviantArt {
    async fn site_from_config(config: &Config) -> Result<Self, Error> {
        Self::new(
            &config.deviantart_client_id,
            &config.deviantart_client_secret,
            &config.host_url,
        )
    }
}

#[async_trait(?Send)]
impl CollectedSite for DeviantArt {
    fn oauth_page(&self) -> Option<&'static str> {
        Some("/deviantart/auth")
    }

    fn jobs(&self) -> HashMap<&'static str, SiteJob> {
        [
            (
                jobs::job::ADD_SUBMISSION_DEVIANTART,
                super::wrap_job(add_submission_deviantart),
            ),
            (
                jobs::job::DEVIANTART_COLLECT_ACCOUNTS,
                super::wrap_job(collect_accounts),
            ),
            (
                jobs::job::DEVIANTART_UPDATE_ACCOUNT,
                super::wrap_job(update_account),
            ),
        ]
        .into_iter()
        .collect()
    }

    #[tracing::instrument(skip(self, ctx, account), fields(user_id = %account.owner_id, account_id = %account.id))]
    async fn add_account(
        &self,
        ctx: &jobs::JobContext,
        account: LinkedAccount,
    ) -> Result<(), Error> {
        models::LinkedAccount::update_loading_state(
            &ctx.conn,
            &ctx.redis,
            account.owner_id,
            account.id,
            models::LoadingState::DiscoveringItems,
        )
        .await?;

        let data: types::DeviantArtData = serde_json::from_value(
            account
                .data
                .ok_or_else(|| Error::unknown_message("account missing data"))?,
        )?;

        let token = self
            .refresh_credentials(&ctx.conn, &ctx.redlock, &data, account.id)
            .await?;
        let client = super::get_authenticated_client(&ctx.config, &token)?;

        let subs = collect_gallery_items(&client, &account.username).await?;

        let known = subs.len() as i32;
        tracing::info!("discovered {} submissions", known);

        let mut redis = ctx.redis.clone();

        let ids = subs.iter().map(|sub| sub.deviationid);

        super::set_loading_submissions(&ctx.conn, &mut redis, account.owner_id, account.id, ids)
            .await?;

        super::queue_new_submissions(
            &ctx.faktory,
            account.owner_id,
            account.id,
            subs,
            |user_id, account_id, sub| {
                jobs::add_submission_deviantart_job(user_id, account_id, sub, true)
            },
        )
        .await?;

        Ok(())
    }
}

impl SiteServices for DeviantArt {
    fn services() -> Vec<Scope> {
        vec![web::scope("/deviantart").service(services![auth, callback])]
    }
}

#[tracing::instrument(skip(ctx, job), fields(job_id = job.id()))]
async fn add_submission_deviantart(ctx: Arc<JobContext>, job: faktory::Job) -> Result<(), Error> {
    let mut args = job.args().iter();
    let (user_id, account_id, sub, was_import) =
        extract_args!(args, Uuid, Uuid, types::DeviantArtSubmission, bool);

    let image_url = match sub.content {
        Some(content) => content.src,
        None => {
            tracing::info!("submission had no content");

            if was_import {
                let mut redis = ctx.redis.clone();
                super::update_import_progress(
                    &ctx.conn,
                    &mut redis,
                    user_id,
                    account_id,
                    sub.deviationid,
                )
                .await?;
            }

            return Ok(());
        }
    };

    let image_data = ctx.client.get(image_url).send().await?.bytes().await?;

    let mut sha256 = sha2::Sha256::new();
    sha256.update(&image_data);
    let sha256: [u8; 32] = sha256
        .finalize()
        .try_into()
        .expect("sha256 was wrong length");

    let (im, perceptual_hash) = if let Ok(im) = image::load_from_memory(&image_data) {
        let hasher = fuzzysearch_common::get_hasher();
        let hash: [u8; 8] = hasher
            .hash_image(&im)
            .as_bytes()
            .try_into()
            .expect("perceptual hash was wrong length");
        let perceptual_hash = i64::from_be_bytes(hash);

        (Some(im), Some(perceptual_hash))
    } else {
        (None, None)
    };

    let item_id = models::OwnedMediaItem::add_item(
        &ctx.conn,
        user_id,
        account_id,
        sub.deviationid,
        perceptual_hash,
        sha256,
        sub.url,
        sub.title,
        sub.published_time,
    )
    .await?;

    if let Some(im) = im {
        models::OwnedMediaItem::update_media(&ctx.conn, &ctx.s3, &ctx.config, item_id, im).await?;

        ctx.faktory
            .enqueue_job(
                JobInitiator::User { user_id },
                search_existing_submissions_job(user_id, item_id)?,
            )
            .await?;
    }

    if was_import {
        let mut redis = ctx.redis.clone();
        super::update_import_progress(&ctx.conn, &mut redis, user_id, account_id, sub.deviationid)
            .await?;
    }

    Ok(())
}

async fn collect_accounts(ctx: Arc<JobContext>, _job: faktory::Job) -> Result<(), Error> {
    let accounts = models::LinkedAccount::all_site_accounts(&ctx.conn, Site::DeviantArt).await?;

    for account in accounts {
        if let Err(err) = ctx
            .faktory
            .enqueue_job(
                JobInitiator::Schedule,
                jobs::deviantart_update_account_job(account)?,
            )
            .await
        {
            tracing::error!("could not enqueue deviantart account check: {:?}", err);
        }
    }

    Ok(())
}

async fn update_account(ctx: Arc<JobContext>, job: faktory::Job) -> Result<(), Error> {
    let mut args = job.args().iter();
    let (account_id,) = crate::extract_args!(args, Uuid);

    let account = models::LinkedAccount::lookup_by_id(&ctx.conn, account_id)
        .await?
        .ok_or(Error::Missing)?;

    let data: types::DeviantArtData = serde_json::from_value(account.data.ok_or(Error::Missing)?)?;

    let da = DeviantArt::site_from_config(&ctx.config).await?;

    let token = da
        .refresh_credentials(&ctx.conn, &ctx.redlock, &data, account.id)
        .await?;
    let client = super::get_authenticated_client(&ctx.config, &token)?;

    let subs = collect_gallery_items(&client, &account.username).await?;

    for sub in subs {
        if models::OwnedMediaItem::lookup_by_site_id(&ctx.conn, Site::DeviantArt, sub.deviationid)
            .await?
            .is_some()
        {
            tracing::trace!("submission {} already existed", sub.deviationid);
            continue;
        }

        ctx.faktory
            .enqueue_job(
                JobInitiator::Schedule,
                jobs::add_submission_deviantart_job(account.owner_id, account_id, sub, false)?,
            )
            .await?;
    }

    Ok(())
}

#[get("/auth")]
async fn auth(
    config: web::Data<crate::Config>,
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    user: models::User,
) -> Result<HttpResponse, Error> {
    let da = DeviantArt::site_from_config(&config).await?;
    let client = da.get_oauth_client();

    let (authorize_url, csrf_state) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(oauth2::Scope::new("user".to_string()))
        .add_scope(oauth2::Scope::new("browse".to_string()))
        .url();

    models::AuthState::create(&conn, user.id, csrf_state.secret()).await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", authorize_url.as_str()))
        .finish())
}

#[get("/callback")]
async fn callback(
    config: web::Data<crate::Config>,
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    faktory: web::Data<jobs::FaktoryClient>,
    user: models::User,
    query: web::Query<types::DeviantArtOAuthCallback>,
) -> Result<HttpResponse, Error> {
    if !models::AuthState::lookup(&conn, user.id, &query.state).await? {
        return Err(Error::Missing);
    }

    let da = DeviantArt::site_from_config(&config).await?;
    let client = da.get_oauth_client();

    let code = AuthorizationCode::new(query.code.clone());

    let token = client
        .exchange_code(code)
        .request_async(oauth2::reqwest::async_http_client)
        .await
        .map_err(|_err| Error::UserError("Could not authenticate with DeviantArt".into()))?;

    models::AuthState::remove(&conn, user.id, &query.state).await?;

    let client = super::get_authenticated_client(&config, token.access_token())?;

    let da_user: types::DeviantArtUser = client
        .get("https://www.deviantart.com/api/v1/oauth2/user/whoami")
        .send()
        .await?
        .json()
        .await?;

    let da_id = da_user.userid.to_string();

    let saved_data = serde_json::to_value(types::DeviantArtData {
        access_token: token.access_token().secret().to_string(),
        refresh_token: token
            .refresh_token()
            .ok_or(Error::Missing)?
            .secret()
            .to_string(),
        expires_after: chrono::Utc::now()
            + token
                .expires_in()
                .map(|dur| {
                    chrono::Duration::from_std(dur).expect("invalid deviantart expires after")
                })
                .unwrap_or_else(|| chrono::Duration::seconds(3600)),
    })?;

    let account =
        models::LinkedAccount::lookup_by_site_id(&conn, user.id, Site::DeviantArt, &da_id).await?;

    let id = match account {
        Some(account) => {
            tracing::info!("got existing account");
            models::LinkedAccount::update_data(&conn, account.id, Some(saved_data)).await?;

            account.id
        }
        None => {
            tracing::info!("got new account");
            let account = models::LinkedAccount::create(
                &conn,
                user.id,
                Site::DeviantArt,
                &da_user.username,
                Some(saved_data),
            )
            .await?;

            faktory
                .enqueue_job(
                    jobs::JobInitiator::user(user.id),
                    jobs::add_account_job(user.id, account.id)?,
                )
                .await?;

            account.id
        }
    };

    Ok(HttpResponse::Found()
        .insert_header(("Location", format!("/user/account/{}", id)))
        .finish())
}

async fn collect_gallery_items(
    client: &reqwest::Client,
    username: &str,
) -> Result<Vec<types::DeviantArtSubmission>, Error> {
    let mut subs = HashSet::new();

    let mut offset = None;
    loop {
        tracing::debug!("loading gallery with offset {:?}", offset);

        let query: &[(&str, &str)] = &[
            ("username", username),
            ("offset", offset.as_deref().unwrap_or("0")),
            ("mature_content", "true"),
            ("limit", "24"),
        ];

        let resp: types::DeviantArtPaginatedResponse<types::DeviantArtSubmission> = client
            .get("https://www.deviantart.com/api/v1/oauth2/gallery/all")
            .query(query)
            .send()
            .await?
            .json()
            .await?;

        subs.extend(resp.results);

        if !resp.has_more {
            tracing::trace!("gallery has no more pages");
            break;
        }

        offset = resp.next_offset.map(|offset| offset.to_string());
    }

    Ok(subs.into_iter().collect())
}

pub mod types {
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Deserialize)]
    pub struct DeviantArtOAuthCallback {
        pub code: String,
        pub state: String,
    }

    #[derive(Debug, Deserialize)]
    pub struct DeviantArtUser {
        pub userid: Uuid,
        pub username: String,
    }

    #[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
    pub struct DeviantArtSubmissionContent {
        pub src: String,
    }

    #[derive(Debug, Deserialize)]
    pub struct DeviantArtPaginatedResponse<R> {
        pub has_more: bool,
        pub next_offset: Option<i32>,
        pub results: Vec<R>,
    }

    #[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
    pub struct DeviantArtSubmission {
        pub deviationid: Uuid,
        pub url: Option<String>,
        pub title: Option<String>,
        #[serde(with = "opt_timestamp")]
        pub published_time: Option<chrono::DateTime<chrono::Utc>>,
        pub content: Option<DeviantArtSubmissionContent>,
    }

    #[derive(Deserialize, Serialize)]
    pub struct DeviantArtData {
        pub access_token: String,
        pub refresh_token: String,
        pub expires_after: chrono::DateTime<chrono::Utc>,
    }

    mod opt_timestamp {
        use chrono::TimeZone;
        use serde::Deserialize;

        pub fn serialize<S>(
            published_time: &Option<chrono::DateTime<chrono::Utc>>,
            serializer: S,
        ) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            match published_time {
                None => serializer.serialize_none(),
                Some(published_time) => {
                    serializer.serialize_str(&published_time.timestamp().to_string())
                }
            }
        }

        pub fn deserialize<'de, D>(
            deserializer: D,
        ) -> Result<Option<chrono::DateTime<chrono::Utc>>, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            let val = <Option<String>>::deserialize(deserializer)?
                .and_then(|ts| ts.parse().ok())
                .map(|ts| chrono::Utc.timestamp(ts, 0));

            Ok(val)
        }
    }
}
