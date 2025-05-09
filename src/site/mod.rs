use std::future::Future;

use async_trait::async_trait;
use foxlib::jobs::{FaktoryForge, FaktoryProducer, Job};
use oauth2::{AccessToken, RefreshToken, TokenResponse};
use uuid::Uuid;

use crate::jobs::JobInitiatorExt;
use crate::{Error, common};
use crate::{jobs, models};

mod bsky;
mod deviantart;
mod flist;
mod furaffinity;
mod patreon;
mod reddit;
mod twitter;
mod weasyl;

pub use bsky::{BSky, ingest_bsky};
pub use deviantart::DeviantArt;
pub use furaffinity::FurAffinity;
pub use patreon::Patreon;
pub use twitter::Twitter;
pub use weasyl::Weasyl;

/// Initialize a site from the global config.
#[async_trait(?Send)]
pub trait SiteFromConfig: Sized {
    /// Initialize a site from the global config.
    async fn site_from_config(config: &crate::Config) -> Result<Self, Error>;
}

/// A site that is watched for owned submissions.
#[async_trait(?Send)]
pub trait WatchedSite {
    /// Jobs to register for this site.
    fn register_jobs(&self, forge: &mut FaktoryForge<jobs::JobContext, Error>);
}

/// A site that can create owned submissions.
#[async_trait(?Send)]
pub trait CollectedSite {
    /// If the site uses OAuth for login, what URL to redirect to for authentication.
    fn oauth_page(&self) -> Option<&'static str>;

    /// Jobs to register for this site.
    fn register_jobs(&self, forge: &mut FaktoryForge<jobs::JobContext, Error>);

    /// Actions to perform when the account is added.
    ///
    /// This is responsible for setting information about the state of submission loading.
    async fn add_account(
        &self,
        ctx: &jobs::JobContext,
        account: models::LinkedAccount,
    ) -> Result<(), Error>;
}

/// The actix-web services needed for this site.
pub trait SiteServices {
    fn services() -> Vec<actix_web::Scope>;
}

/// Get all services for all sites.
pub fn service() -> Vec<actix_web::Scope> {
    deviantart::DeviantArt::services()
        .into_iter()
        .chain(patreon::Patreon::services())
        .chain(twitter::Twitter::services())
        .chain(bsky::BSky::services())
        .collect()
}

/// Get all jobs for all watched and collected sites.
pub async fn register_jobs(
    config: &crate::Config,
    forge: &mut FaktoryForge<jobs::JobContext, Error>,
) -> Result<(), Error> {
    watched_sites(config)
        .await?
        .into_iter()
        .for_each(|site| site.register_jobs(forge));

    collected_sites(config)
        .await?
        .into_iter()
        .for_each(|site| site.register_jobs(forge));

    Ok(())
}

/// Get all watched sites.
pub async fn watched_sites(config: &crate::Config) -> Result<Vec<Box<dyn WatchedSite>>, Error> {
    Ok(vec![
        Box::new(flist::FList::site_from_config(config).await?),
        Box::new(reddit::Reddit::site_from_config(config).await?),
        Box::new(bsky::BSky::site_from_config(config).await?),
    ])
}

/// Get all collected sites.
pub async fn collected_sites(config: &crate::Config) -> Result<Vec<Box<dyn CollectedSite>>, Error> {
    Ok(vec![
        Box::new(deviantart::DeviantArt::site_from_config(config).await?),
        Box::new(furaffinity::FurAffinity::site_from_config(config).await?),
        Box::new(patreon::Patreon::site_from_config(config).await?),
        Box::new(weasyl::Weasyl::site_from_config(config).await?),
        Box::new(twitter::Twitter::site_from_config(config).await?),
        Box::new(bsky::BSky::site_from_config(config).await?),
    ])
}

/// Get a reqwest client that uses a given bearer token with the default user agent.
fn get_authenticated_client(
    config: &crate::Config,
    token: &AccessToken,
) -> Result<reqwest::Client, Error> {
    use reqwest::header;

    let mut headers = header::HeaderMap::new();

    let mut auth_value = header::HeaderValue::from_str(&format!("Bearer {}", token.secret()))
        .map_err(Error::from_displayable)?;
    auth_value.set_sensitive(true);
    headers.insert(header::AUTHORIZATION, auth_value);

    let client = reqwest::ClientBuilder::default()
        .default_headers(headers)
        .user_agent(&config.user_agent)
        .build()?;

    Ok(client)
}

/// Set IDs of submissions that are still loading for a given account. Used to send progress events
/// to the user.
async fn set_loading_submissions<S, I>(
    conn: &sqlx::PgPool,
    nats: &async_nats::Client,
    user_id: Uuid,
    account_id: Uuid,
    ids: I,
) -> Result<usize, Error>
where
    S: ToString,
    I: Iterator<Item = S>,
{
    let ids: Vec<String> = ids.into_iter().map(|item| item.to_string()).collect();
    let len = ids.len();

    if len == 0 {
        tracing::info!("user had no submissions");

        models::LinkedAccount::update_loading_state(
            conn,
            nats,
            user_id,
            account_id,
            models::LoadingState::Complete,
        )
        .await?;
    } else {
        tracing::info!("user has {} submissions to load", len);

        models::LinkedAccountImport::start(conn, account_id, &ids).await?;

        models::LinkedAccount::update_loading_state(
            conn,
            nats,
            user_id,
            account_id,
            models::LoadingState::LoadingItems { known: len as i32 },
        )
        .await?;
    }

    Ok(len)
}

/// Queue newly discovered submissions for evaluation.
async fn queue_new_submissions<S, Sub, J, F>(
    producer: &FaktoryProducer,
    user_id: Uuid,
    account_id: Uuid,
    submissions: S,
    job_fn: F,
) -> Result<(), Error>
where
    S: IntoIterator<Item = Sub>,
    J: Job,
    F: Fn(Uuid, Uuid, Sub) -> J,
{
    for sub in submissions {
        let job = job_fn(user_id, account_id, sub);

        producer
            .enqueue_job(job.initiated_by(jobs::JobInitiator::user(user_id)))
            .await?;
    }

    Ok(())
}

async fn update_import_progress<S: ToString>(
    conn: &sqlx::PgPool,
    nats: &async_nats::Client,
    user_id: Uuid,
    account_id: Uuid,
    site_id: S,
) -> Result<(), Error> {
    let (loaded, expected) =
        models::LinkedAccountImport::loaded(conn, account_id, &site_id.to_string()).await?;

    tracing::debug!("submission was part of import, loaded {loaded} out of {expected} items");

    if expected - loaded == 0 {
        tracing::info!("marking account import complete");

        models::LinkedAccountImport::complete(conn, account_id).await?;

        models::LinkedAccount::update_loading_state(
            conn,
            nats,
            user_id,
            account_id,
            models::LoadingState::Complete,
        )
        .await?;
    }

    common::send_user_event(
        user_id,
        nats,
        crate::api::EventMessage::LoadingProgress {
            account_id,
            loaded,
            total: expected,
        },
    )
    .await?;

    Ok(())
}

type Tx = sqlx::Transaction<'static, sqlx::Postgres>;

#[tracing::instrument(skip(data, client_fn, extract_fn, data_fn, update_fn))]
async fn refresh_credentials<C, E, D, DFut, Item, U, UFut>(
    conn: &sqlx::PgPool,
    account_id: Uuid,
    data: &Item,
    client_fn: C,
    extract_fn: E,
    data_fn: D,
    update_fn: U,
) -> Result<AccessToken, Error>
where
    C: FnOnce() -> oauth2::basic::BasicClient,
    E: Fn(&Item) -> Option<(String, String, chrono::DateTime<chrono::Utc>)>,
    D: FnOnce(Tx) -> DFut,
    DFut: Future<Output = Result<(Item, Tx), Error>>,
    U: FnOnce(Tx, Item, String, String, chrono::DateTime<chrono::Utc>) -> UFut,
    UFut: Future<Output = Result<Tx, Error>>,
{
    tracing::debug!("checking oauth credentials");

    let (initial_access_token, _initial_refresh_token, initial_expires_at) =
        extract_fn(data).ok_or(Error::Missing)?;

    if initial_expires_at >= chrono::Utc::now() {
        tracing::trace!("credentials have not expired");
        return Ok(AccessToken::new(initial_access_token));
    }

    let tx = conn.begin().await?;

    let (current_data, tx) = data_fn(tx).await?;
    let (current_access_token, current_refresh_token, current_expires_at) =
        extract_fn(&current_data).ok_or(Error::Missing)?;

    if current_expires_at >= chrono::Utc::now() {
        tracing::debug!("credentials were already updated");
        return Ok(AccessToken::new(current_access_token));
    }

    let client = client_fn();
    let refresh_token = RefreshToken::new(current_refresh_token);

    tracing::info!("refreshing credentials");

    let refresh = client
        .exchange_refresh_token(&refresh_token)
        .request_async(oauth2::reqwest::async_http_client)
        .await
        .map_err(Error::from_displayable)?;

    let access_token = refresh.access_token().secret().to_string();
    let refresh_token = refresh
        .refresh_token()
        .ok_or(Error::Missing)?
        .secret()
        .to_string();

    let expires_at = chrono::Utc::now()
        + refresh
            .expires_in()
            .and_then(|dur| chrono::Duration::from_std(dur).ok())
            .unwrap_or_else(|| chrono::Duration::try_seconds(3600).unwrap());

    let tx = update_fn(tx, current_data, access_token, refresh_token, expires_at).await?;

    tx.commit().await?;

    tracing::info!("credential refresh complete");

    Ok(refresh.access_token().to_owned())
}
