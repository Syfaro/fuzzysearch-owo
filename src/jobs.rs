use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fmt::Display,
    net::TcpStream,
    sync::{Arc, Mutex},
};

use futures::Future;
use opentelemetry::propagation::TextMapPropagator;
use prometheus::{register_counter_vec, register_histogram_vec, CounterVec, HistogramVec};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use tracing::Instrument;
use uuid::Uuid;

use crate::{models, Error};

#[derive(Clone, Debug, clap::ArgEnum)]
pub enum FaktoryQueue {
    Core,
    Outgoing,
}

impl FaktoryQueue {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Core => "fuzzysearch_owo_core",
            Self::Outgoing => "fuzzysearch_owo_outgoing",
        }
    }

    fn label(&self) -> Option<&'static str> {
        match self {
            Self::Core => None,
            Self::Outgoing => Some("outgoing"),
        }
    }
}

pub trait JobFuzzyQueue {
    fn fuzzy_queue(self, queue: FaktoryQueue) -> Self;
}

impl JobFuzzyQueue for faktory::Job {
    fn fuzzy_queue(mut self, queue: FaktoryQueue) -> Self {
        self.queue = queue.as_str().to_string();
        self
    }
}

pub mod job {
    pub const ADD_ACCOUNT: &str = "add_account";
    pub const ADD_SUBMISSION_FURAFFINITY: &str = "add_submission_furaffinity";
    pub const NEW_SUBMISSION: &str = "new_submission";
    pub const SEARCH_EXISTING_SUBMISSIONS: &str = "search_existing_submissions";

    pub const FLIST_COLLECT_GALLERY_IMAGES: &str = "flist_gallery";
    pub const FLIST_HASH_IMAGE: &str = "flist_hash";
}

lazy_static::lazy_static! {
    static ref JOB_EXECUTION_TIME: HistogramVec = register_histogram_vec!("fuzzysearch_owo_job_duration_seconds", "Duration to complete a job.", &["job"]).unwrap();
    static ref JOB_FAILURE_COUNT: CounterVec = register_counter_vec!("fuzzysearch_owo_job_failure_total", "Number of job failures.", &["job"]).unwrap();
}

#[macro_export]
macro_rules! extract_args {
    ($args:expr, $($x:ty),*) => {
        {
            (
                $(
                    get_arg::<$x>(&mut $args)?,
                )*
            )
        }
    }
}

#[macro_export]
macro_rules! serialize_args {
    ($($x:expr),*) => {
        {
            vec![
            $(
                serde_json::to_value($x)?,
            )*
            ]
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IncomingSubmission {
    pub site: models::Site,
    pub site_id: String,
    pub posted_by: Option<String>,
    pub sha256: Option<[u8; 32]>,
    pub perceptual_hash: Option<[u8; 8]>,
    pub content_url: String,
    pub page_url: Option<String>,
    pub posted_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<fuzzysearch_common::faktory::WebHookData> for IncomingSubmission {
    fn from(data: fuzzysearch_common::faktory::WebHookData) -> Self {
        IncomingSubmission {
            site: models::Site::from(data.site),
            site_id: data.site_id.to_string(),
            sha256: data
                .file_sha256
                .map(|sha| sha.try_into().expect("sha256 was wrong length")),
            perceptual_hash: data.hash,
            content_url: data.file_url,
            page_url: Some(match &data.site {
                fuzzysearch_common::types::Site::FurAffinity => {
                    format!("https://www.furaffinity.net/view/{}/", data.site_id)
                }
                fuzzysearch_common::types::Site::Twitter => {
                    format!(
                        "https://twitter.com/{}/status/{}",
                        data.artist, data.site_id
                    )
                }
                fuzzysearch_common::types::Site::E621 => {
                    format!("https://e621.net/posts/{}", data.site_id)
                }
                fuzzysearch_common::types::Site::Weasyl => {
                    format!("https://www.weasyl.com/view/{}", data.site_id)
                }
            }),
            posted_by: Some(data.artist),
            posted_at: None,
        }
    }
}

pub fn add_account_job(user_id: Uuid, account_id: Uuid) -> Result<faktory::Job, Error> {
    let args = serialize_args!(user_id, account_id);

    Ok(faktory::Job::new(job::ADD_ACCOUNT, args).fuzzy_queue(FaktoryQueue::Outgoing))
}

pub fn add_furaffinity_submission_job(
    user_id: Uuid,
    account_id: Uuid,
    submission_id: i32,
    import: bool,
) -> Result<faktory::Job, Error> {
    let args = serialize_args!(user_id, account_id, submission_id, import);

    Ok(
        faktory::Job::new(job::ADD_SUBMISSION_FURAFFINITY, args)
            .fuzzy_queue(FaktoryQueue::Outgoing),
    )
}

pub fn new_submission_job<D>(data: D) -> Result<faktory::Job, Error>
where
    D: Into<IncomingSubmission>,
{
    let args = serialize_args!(data.into());

    Ok(faktory::Job::new(job::NEW_SUBMISSION, args).fuzzy_queue(FaktoryQueue::Core))
}

pub fn search_existing_submissions_job(
    user_id: Uuid,
    media_id: Uuid,
) -> Result<faktory::Job, Error> {
    let args = serialize_args!(user_id, media_id);

    Ok(faktory::Job::new(job::SEARCH_EXISTING_SUBMISSIONS, args).fuzzy_queue(FaktoryQueue::Core))
}

pub fn flist_hash_image_job(id: i32) -> faktory::Job {
    let args = vec![serde_json::Value::from(id)];

    faktory::Job::new(job::FLIST_HASH_IMAGE, args).fuzzy_queue(FaktoryQueue::Outgoing)
}

#[derive(Clone)]
pub struct FaktoryClient {
    client: Arc<Mutex<faktory::Producer<TcpStream>>>,
}

impl FaktoryClient {
    pub async fn connect<H: Into<String>>(host: H) -> Result<Self, Error> {
        let host = host.into();

        let producer = tokio::task::spawn_blocking(move || {
            faktory::Producer::connect(Some(&host))
                .map_err(|err| anyhow::format_err!("faktory connection error: {:?}", err))
        })
        .in_current_span()
        .await
        .map_err(Error::from_displayable)? // TODO: this is gross
        .map_err(Error::from_displayable)?;

        let client = Mutex::new(producer);

        Ok(Self {
            client: Arc::new(client),
        })
    }

    pub async fn enqueue_job(
        &self,
        initiator: JobInitiator,
        mut job: faktory::Job,
    ) -> Result<(), Error> {
        job.custom = get_job_custom(initiator)?;

        let client = self.client.clone();
        tokio::task::spawn_blocking(move || {
            let mut client = client.lock().expect("faktory client was poisoned");
            client
                .enqueue(job)
                .map_err(|err| anyhow::format_err!("faktory enqueue error: {:?}", err))
        })
        .in_current_span()
        .await
        .map_err(Error::from_displayable)? // TODO: this is gross
        .map_err(Error::from_displayable)?;

        Ok(())
    }
}

fn get_arg_opt<T: serde::de::DeserializeOwned>(
    args: &mut core::slice::Iter<serde_json::Value>,
) -> Result<Option<T>, Error> {
    let arg = match args.next() {
        Some(arg) => arg,
        None => return Ok(None),
    };

    let data = serde_json::from_value(arg.to_owned())?;
    Ok(Some(data))
}

fn get_arg<T: serde::de::DeserializeOwned>(
    args: &mut core::slice::Iter<serde_json::Value>,
) -> Result<T, Error> {
    get_arg_opt(args)?.ok_or(Error::Missing)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobInitiator {
    User { user_id: Uuid },
    External { name: Cow<'static, str> },
    Schedule,
    Unknown,
}

impl JobInitiator {
    pub fn user(user_id: Uuid) -> Self {
        Self::User { user_id }
    }

    pub fn external<N: Into<Cow<'static, str>>>(name: N) -> Self {
        Self::External { name: name.into() }
    }
}

impl Default for JobInitiator {
    fn default() -> Self {
        Self::Unknown
    }
}

impl Display for JobInitiator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User { .. } => write!(f, "user"),
            Self::External { name } => write!(f, "external_{}", name.replace(' ', "_")),
            Self::Schedule => write!(f, "schedule"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

pub fn get_job_custom(
    initiator: JobInitiator,
) -> Result<HashMap<String, serde_json::Value>, Error> {
    let mut values: HashMap<String, serde_json::Value> = get_tracing_headers()
        .into_iter()
        .map(|(key, value)| (key, serde_json::Value::from(value)))
        .collect();

    values.insert("initiator".to_string(), serde_json::to_value(initiator)?);

    Ok(values)
}

fn get_tracing_headers() -> HashMap<String, String> {
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let mut headers = HashMap::with_capacity(2);
    let propagator = opentelemetry::sdk::propagation::TraceContextPropagator::new();

    let cx = tracing::Span::current().context();
    propagator.inject_context(&cx, &mut headers);

    headers
}

#[derive(Clone)]
pub struct JobContext {
    pub faktory: FaktoryClient,
    pub conn: sqlx::Pool<sqlx::Postgres>,
    pub redis: redis::aio::ConnectionManager,
    pub s3: rusoto_s3::S3Client,
    pub fuzzysearch: std::sync::Arc<fuzzysearch::FuzzySearch>,
    pub config: std::sync::Arc<crate::Config>,
    pub client: reqwest::Client,
}

struct WorkerEnvironment {
    faktory: faktory::ConsumerBuilder<Error>,
    handle: tokio::runtime::Handle,
    ctx: Arc<JobContext>,
}

impl WorkerEnvironment {
    fn register<F, Fut>(&mut self, name: &'static str, f: F)
    where
        F: 'static + Send + Sync + Fn(Arc<JobContext>, faktory::Job) -> Fut,
        Fut: Future<Output = Result<(), Error>>,
    {
        let handle = self.handle.clone();
        let ctx = self.ctx.clone();

        self.faktory
            .register(name, move |mut job| -> Result<(), Error> {
                let initiator: JobInitiator = job
                    .custom
                    .remove("initiator")
                    .and_then(|initiator| serde_json::from_value(initiator).ok())
                    .unwrap_or_default();

                let string_values: HashMap<String, String> = job
                    .custom
                    .iter()
                    .flat_map(|(key, value)| {
                        value
                            .as_str()
                            .map(|value| (key.to_owned(), value.to_owned()))
                    })
                    .collect();

                let propagator = opentelemetry::sdk::propagation::TraceContextPropagator::new();
                let cx = propagator.extract(&string_values);

                let span = tracing::info_span!("faktory_job", name, %initiator, queue = %job.queue);
                tracing_opentelemetry::OpenTelemetrySpanExt::set_parent(&span, cx);

                let _guard = span.entered();

                tracing::info!("running job with args: {:?}", job.args());

                let execution_time = JOB_EXECUTION_TIME.with_label_values(&[name]).start_timer();
                let result = handle.block_on(f(ctx.clone(), job).in_current_span());
                let execution_time = execution_time.stop_and_record();

                match result {
                    Ok(_) => {
                        tracing::info!(execution_time, "job completed");
                        Ok(())
                    }
                    Err(err) => {
                        JOB_FAILURE_COUNT.with_label_values(&[name]).inc();

                        if err.should_retry() {
                            tracing::error!(execution_time, "job failed, will retry: {:?}", err);
                            Err(err)
                        } else {
                            tracing::error!(
                                execution_time,
                                "job failed, will NOT retry: {:?}",
                                err
                            );
                            Ok(())
                        }
                    }
                }
            });
    }

    fn finalize(self) -> faktory::ConsumerBuilder<Error> {
        self.faktory
    }
}

#[derive(askama::Template)]
#[template(path = "email/similar.txt")]
struct SimilarTemplate<'a> {
    username: &'a str,
    source_link: &'a str,
    site_name: &'a str,
    poster_name: &'a str,
    similar_link: &'a str,
}

pub async fn start_job_processing(ctx: JobContext) -> Result<(), tokio::task::JoinError> {
    let queues: Vec<String> = ctx
        .config
        .faktory_queues
        .iter()
        .map(|queue| queue.as_str().to_string())
        .collect();

    let labels: Vec<String> = ctx
        .config
        .faktory_queues
        .iter()
        .flat_map(|queue| queue.label())
        .chain(["fuzzysearch-owo"].into_iter())
        .map(str::to_string)
        .collect();

    tracing::info!(
        "starting faktory client on queues {} with labels {}",
        queues.join(","),
        labels.join(",")
    );

    let mut client = faktory::ConsumerBuilder::default();
    client.labels(labels);
    client.workers(ctx.config.faktory_workers);

    let handle = tokio::runtime::Handle::current();

    let mut environment = WorkerEnvironment {
        faktory: client,
        handle,
        ctx: Arc::new(ctx.clone()),
    };

    environment.register(job::ADD_SUBMISSION_FURAFFINITY, |ctx, job| async move {
        let mut args = job.args().iter();
        let (user_id, account_id, sub_id) = extract_args!(args, Uuid, Uuid, i32);
        let was_import: bool = get_arg_opt(&mut args)?.unwrap_or(false);

        let fa = furaffinity_rs::FurAffinity::new(
            ctx.config.furaffinity_cookie_a.clone(),
            ctx.config.furaffinity_cookie_b.clone(),
            ctx.config.user_agent.clone(),
            None,
        );

        let sub = fa
            .get_submission(sub_id)
            .await
            .map_err(|_err| {
                Error::LoadingError(format!("Could not load FurAffinity submission {}", sub_id))
            })?
            .ok_or(Error::Missing)?;

        if sub.content.url().contains("/stories/") {
            tracing::debug!("submission was story, skipping");
        } else {
            let sub = fa.calc_image_hash(sub).await.map_err(|_err| {
                Error::LoadingError(format!(
                    "Could not load FurAffinity submission content {}",
                    sub_id
                ))
            })?;

            let sha256_hash: [u8; 32] = sub
                .file_sha256
                .ok_or(Error::Missing)?
                .try_into()
                .expect("sha256 hash was wrong length");

            let item_id = models::OwnedMediaItem::add_item(
                &ctx.conn,
                user_id,
                account_id,
                sub_id,
                sub.hash_num,
                sha256_hash,
                Some(format!("https://www.furaffinity.net/view/{}/", sub_id)),
                Some(sub.title),
                Some(sub.posted_at),
            )
            .await?;

            let im = image::load_from_memory(&sub.file.ok_or(Error::Missing)?)?;
            models::OwnedMediaItem::update_media(&ctx.conn, &ctx.s3, &ctx.config, item_id, im)
                .await?;

            if was_import {
                ctx.faktory
                    .enqueue_job(
                        JobInitiator::User { user_id },
                        search_existing_submissions_job(user_id, item_id)?,
                    )
                    .await?;
            }
        }

        if was_import {
            let mut redis = ctx.redis.clone();
            let loading_key = format!("account-import-ids:loading:{}", account_id);
            let completed_key = format!("account-import-ids:completed:{}", account_id);

            redis
                .smove::<_, _, ()>(&loading_key, &completed_key, sub_id)
                .await?;

            redis
                .expire::<_, ()>(&loading_key, 60 * 60 * 24 * 7)
                .await?;
            redis
                .expire::<_, ()>(&completed_key, 60 * 60 * 24 * 7)
                .await?;

            let (remaining, completed): (i32, i32) = redis::pipe()
                .atomic()
                .scard(loading_key)
                .scard(completed_key)
                .query_async(&mut redis)
                .await?;

            tracing::debug!(
                "submission was part of import, {} items remaining",
                remaining
            );

            if remaining == 0 {
                tracing::info!("marking account import complete");

                models::LinkedAccount::update_loading_state(
                    &ctx.conn,
                    &redis,
                    user_id,
                    account_id,
                    models::LoadingState::Complete,
                )
                .await?;
            }

            redis
                .publish(
                    format!("user-events:{}", user_id.to_string()),
                    serde_json::to_string(&crate::api::EventMessage::LoadingProgress {
                        account_id,
                        loaded: completed,
                        total: remaining + completed,
                    })?,
                )
                .await?;
        }

        Ok(())
    });

    environment.register(job::SEARCH_EXISTING_SUBMISSIONS, |ctx, job| async move {
        let mut args = job.args().iter();
        let (user_id, media_id) = extract_args!(args, Uuid, Uuid);

        let media = models::OwnedMediaItem::get_by_id(&ctx.conn, media_id, user_id)
            .await?
            .ok_or(Error::Missing)?;

        let perceptual_hash = match media.perceptual_hash {
            Some(hash) => hash,
            None => {
                tracing::warn!("got media item with no perceptual hash");
                return Ok(());
            }
        };

        let fuzzysearch_items = ctx
            .fuzzysearch
            .lookup_hashes(&[perceptual_hash], Some(3))
            .await?
            .into_iter()
            .flat_map(|file| {
                Some((
                    models::SimilarImage {
                        page_url: Some(file.url()),
                        site: models::Site::from(file.site_info?),
                        posted_by: file.artists.as_ref().map(|artists| artists.join(", ")),
                        content_url: file.url,
                    },
                    file.posted_at,
                ))
            });

        let flist_items = models::FListFile::similar_images(&ctx.conn, perceptual_hash)
            .await?
            .into_iter()
            .flat_map(|file| {
                Some((
                    models::SimilarImage {
                        site: models::Site::FList,
                        page_url: Some(format!(
                            "https://www.f-list.net/c/{}/",
                            file.character_name
                        )),
                        posted_by: Some(file.character_name),
                        content_url: format!(
                            "https://static.f-list.net/images/charimage/{}.{}",
                            file.id, file.ext
                        ),
                    },
                    None,
                ))
            });

        for (similar_image, created_at) in fuzzysearch_items.chain(flist_items) {
            models::UserEvent::similar_found(
                &ctx.conn,
                &ctx.redis,
                user_id,
                media_id,
                similar_image,
                Some(created_at.unwrap_or_else(chrono::Utc::now)),
            )
            .await?;
        }

        Ok(())
    });

    environment.register(job::ADD_ACCOUNT, |ctx, job| async move {
        let mut args = job.args().iter();
        let (user_id, account_id) = extract_args!(args, Uuid, Uuid);

        let account = models::LinkedAccount::lookup_by_id(&ctx.conn, account_id)
            .await?
            .ok_or(Error::Missing)?;

        models::LinkedAccount::update_loading_state(
            &ctx.conn,
            &ctx.redis,
            user_id,
            account_id,
            models::LoadingState::DiscoveringItems,
        )
        .await?;

        match account.source_site {
            models::Site::FurAffinity => {
                let ids = discover_furaffinity_submissions(&account.username, &ctx.config)
                    .await
                    .map_err(Error::from_displayable)?;

                if ids.is_empty() {
                    models::LinkedAccount::update_loading_state(
                        &ctx.conn,
                        &ctx.redis,
                        user_id,
                        account_id,
                        models::LoadingState::Complete,
                    )
                    .await?;
                } else {
                    let known = ids.len() as i32;

                    let mut redis = ctx.redis.clone();
                    let key = format!("account-import-ids:loading:{}", account_id);
                    redis.sadd::<_, _, ()>(&key, &ids).await?;
                    redis.expire::<_, ()>(key, 60 * 60 * 24 * 7).await?;

                    for id in ids {
                        ctx.faktory
                            .enqueue_job(
                                JobInitiator::User { user_id },
                                add_furaffinity_submission_job(user_id, account_id, id, true)?,
                            )
                            .await?;
                    }

                    models::LinkedAccount::update_loading_state(
                        &ctx.conn,
                        &ctx.redis,
                        user_id,
                        account_id,
                        models::LoadingState::LoadingItems { known },
                    )
                    .await?;
                }
            }
            models::Site::Patreon => {
                use crate::patreon::*;

                models::LinkedAccount::update_loading_state(
                    &ctx.conn,
                    &ctx.redis,
                    user_id,
                    account_id,
                    models::LoadingState::DiscoveringItems,
                )
                .await?;

                let data: SavedPatreonData =
                    serde_json::from_value(account.data.ok_or(Error::Missing)?)?;

                if data.credentials.expires_after < chrono::Utc::now() {
                    return Err(Error::UnknownMessage("patreon credentials expired".into()));
                }

                let client = get_authenticated_client(&ctx.config, &data.credentials.access_token)?;

                let posts: PatreonData<Vec<PatreonDataItem<PatreonPostAttributes>>> = client
                    .get(format!(
                        "https://www.patreon.com/api/oauth2/v2/campaigns/{}/posts",
                        data.site_id
                    ))
                    .query(&[(
                        "fields[post]",
                        "embed_data,embed_url,published_at,title,url",
                    )])
                    .send()
                    .await?
                    .json()
                    .await?;

                tracing::info!("got page of posts: {:?}", posts);

                tracing::warn!("setting patreon to complete without loading");

                models::LinkedAccount::update_loading_state(
                    &ctx.conn,
                    &ctx.redis,
                    user_id,
                    account_id,
                    models::LoadingState::Complete,
                )
                .await?;
            }
            _ => unimplemented!(),
        }

        Ok(())
    });

    environment.register(job::NEW_SUBMISSION, |ctx, job| async move {
        let mut args = job.args().iter();
        let (data,) = extract_args!(args, IncomingSubmission);

        // FurAffinity has some weird differences between how usernames are
        // displayed and how they're used in URLs.
        let artist = if matches!(data.site, models::Site::FurAffinity) {
            data.posted_by
                .as_ref()
                .map(|posted_by| posted_by.replace('_', ""))
        } else {
            data.posted_by.clone()
        };

        if let Some(artist) = artist {
            if let Some((account_id, user_id)) = models::LinkedAccount::search_site_account(
                &ctx.conn,
                &data.site.to_string(),
                &artist,
            )
            .await?
            {
                tracing::info!("new submission belongs to known account");

                let sha256_hash: [u8; 32] = data
                    .sha256
                    .ok_or(Error::Missing)?
                    .try_into()
                    .expect("sha256 hash was wrong length");

                let item = models::OwnedMediaItem::add_item(
                    &ctx.conn,
                    user_id,
                    account_id,
                    data.site_id,
                    data.perceptual_hash.map(|hash| i64::from_be_bytes(hash)),
                    sha256_hash,
                    data.page_url.clone(),
                    None, // TODO: collect title
                    data.posted_at,
                )
                .await?;

                let data = reqwest::Client::default()
                    .get(&data.content_url)
                    .send()
                    .await?
                    .bytes()
                    .await?;
                let im = image::load_from_memory(&data)?;

                models::OwnedMediaItem::update_media(&ctx.conn, &ctx.s3, &ctx.config, item, im)
                    .await?;
            }
        }

        let hash = match data.perceptual_hash {
            Some(hash) => i64::from_be_bytes(hash),
            None => {
                tracing::warn!("webhook data had no hash");
                return Ok(());
            }
        };

        let similar_items = models::OwnedMediaItem::find_similar(&ctx.conn, hash).await?;
        if similar_items.is_empty() {
            tracing::info!("found no similar images");

            return Ok(());
        }

        let similar_image = models::SimilarImage {
            site: data.site,
            posted_by: data.posted_by.clone(),
            page_url: data.page_url.clone(),
            content_url: data.content_url.clone(),
        };

        for item in similar_items {
            tracing::debug!("found similar item owned by {}", item.owner_id);

            models::UserEvent::similar_found(
                &ctx.conn,
                &ctx.redis,
                item.owner_id,
                item.id,
                similar_image.clone(),
                None,
            )
            .await?;

            if let Some(models::User {
                username,
                email: Some(email),
                email_verifier: None,
                ..
            }) = models::User::lookup_by_id(&ctx.conn, item.owner_id).await?
            {
                tracing::debug!("user had email, sending notification");

                let body = askama::Template::render(&SimilarTemplate {
                    username: &username,
                    source_link: item
                        .link
                        .as_deref()
                        .unwrap_or_else(|| item.content_url.as_deref().unwrap_or("unknown")),
                    site_name: &data.site.to_string(),
                    poster_name: data.posted_by.as_deref().unwrap_or("unknown"),
                    similar_link: data.page_url.as_deref().unwrap_or(&data.content_url),
                })?;

                let email = lettre::Message::builder()
                    .from(ctx.config.smtp_from.clone())
                    .reply_to(ctx.config.smtp_reply_to.clone())
                    .to(lettre::message::Mailbox::new(
                        Some(username),
                        email.parse().map_err(Error::from_displayable)?,
                    ))
                    .subject(format!("Similar image found on {}", data.site.to_string()))
                    .body(body)?;

                let creds = lettre::transport::smtp::authentication::Credentials::new(
                    ctx.config.smtp_username.clone(),
                    ctx.config.smtp_password.clone(),
                );

                let mailer: lettre::AsyncSmtpTransport<lettre::Tokio1Executor> =
                    lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(
                        &ctx.config.smtp_host,
                    )
                    .unwrap()
                    .credentials(creds)
                    .build();

                lettre::AsyncTransport::send(&mailer, email).await.unwrap();
            }
        }

        Ok(())
    });

    environment.register(job::FLIST_COLLECT_GALLERY_IMAGES, |ctx, _job| async move {
        let previous_run = models::FListImportRun::previous_run(&ctx.conn).await?;
        let previous_max = match previous_run {
            Some(run) if run.finished_at.is_none() => {
                tracing::info!("previous run has not yet finished, skipping");
                return Ok(());
            }
            Some(run) => run.max_id.unwrap_or(0),
            None => 0,
        };

        let id = models::FListImportRun::start(&ctx.conn, previous_max + 1).await?;

        let mut flist = crate::flist::FList::new(&ctx.config.user_agent);
        flist
            .sign_in(&ctx.config.flist_username, &ctx.config.flist_password)
            .await?;

        let mut offset = None;
        let mut max_id = previous_max;
        loop {
            tracing::info!("loading flist gallery with offset {:?}", offset);

            let items = flist.get_latest_gallery_items(offset).await?;
            if items.is_empty() {
                tracing::info!("found no new items, ending");
                break;
            }

            let mut tx = ctx.conn.begin().await?;

            for item in items.iter() {
                models::FListFile::insert_item(&mut tx, item.id, &item.ext, &item.character_name)
                    .await?;
            }

            tx.commit().await?;

            let ids: HashSet<_> = items.iter().map(|item| item.id).collect();
            let max = ids.iter().copied().max().unwrap_or(0);

            // Only enqueue jobs after changes have been committed
            for id in ids.iter().copied() {
                ctx.faktory
                    .enqueue_job(JobInitiator::external("flist"), flist_hash_image_job(id))
                    .await?;
            }

            if max_id < max {
                max_id = max;
            }

            if ids.contains(&previous_max) {
                tracing::info!("found previous max, ending");
                break;
            }

            offset = Some(offset.unwrap_or(0) + items.len() as i32);
        }

        models::FListImportRun::complete(&ctx.conn, id, max_id).await?;

        Ok(())
    });

    environment.register(job::FLIST_HASH_IMAGE, |ctx, job| async move {
        let mut args = job.args().iter();
        let (id,) = extract_args!(args, i32);

        let file = models::FListFile::get_by_id(&ctx.conn, id)
            .await?
            .ok_or(Error::Missing)?;

        if file.sha256.is_some() {
            tracing::info!("file already had hash, skipping");

            return Ok(());
        }

        let url = format!(
            "https://static.f-list.net/images/charimage/{}.{}",
            file.id, file.ext
        );
        let bytes = ctx.client.get(&url).send().await?.bytes().await?;

        let mut sha256 = sha2::Sha256::new();
        sha256.update(&bytes);
        let sha256: [u8; 32] = sha256
            .finalize()
            .try_into()
            .expect("sha256 was wrong length");
        let size = bytes.len();

        let perceptual_hash = tokio::task::spawn_blocking(move || -> Option<i64> {
            let im = image::load_from_memory(&bytes).ok()?;

            let hasher = fuzzysearch_common::get_hasher();
            let bytes = hasher.hash_image(&im).as_bytes().try_into().ok()?;
            Some(i64::from_be_bytes(bytes))
        })
        .await
        .map_err(Error::from_displayable)?;

        models::FListFile::update(&ctx.conn, id, size as i32, sha256.to_vec(), perceptual_hash)
            .await?;

        let data = IncomingSubmission {
            site: models::Site::FList,
            site_id: id.to_string(),
            page_url: Some(format!("https://www.f-list.net/c/{}/", file.character_name)),
            posted_by: Some(file.character_name),
            sha256: Some(sha256),
            perceptual_hash: perceptual_hash.map(|hash| hash.to_be_bytes()),
            content_url: url,
            posted_at: None,
        };

        ctx.faktory
            .enqueue_job(JobInitiator::external("flist"), new_submission_job(data)?)
            .await?;

        Ok(())
    });

    let client = environment.finalize();

    tokio::task::spawn_blocking(move || {
        let mut client = client
            .connect(Some(&ctx.config.faktory_host))
            .expect("could not connect to faktory");

        if let Err(err) = client.run(&queues) {
            tracing::error!("worker failed: {:?}", err);
        }
    })
    .await
}

async fn discover_furaffinity_submissions(
    user: &str,
    config: &crate::Config,
) -> anyhow::Result<Vec<i32>> {
    // TODO: handle scraps?

    let client = reqwest::Client::default();
    let id_selector =
        scraper::Selector::parse(".submission-list u a").expect("known good selector failed");

    let mut ids = Vec::new();

    let mut page = 1;
    loop {
        tracing::info!(page, "loading gallery page");

        let body = client
            .get(format!(
                "https://www.furaffinity.net/gallery/{}/{}/",
                user, page
            ))
            .header(
                reqwest::header::COOKIE,
                format!(
                    "a={}; b={}",
                    config.furaffinity_cookie_a, config.furaffinity_cookie_b
                ),
            )
            .send()
            .await?
            .text()
            .await?;

        let body = scraper::Html::parse_document(&body);

        let mut new_ids = body
            .select(&id_selector)
            .into_iter()
            .filter_map(|element| element.value().attr("href"))
            .filter_map(|href| href.split('/').nth(2))
            .filter_map(|id| id.parse::<i32>().ok())
            .peekable();

        if new_ids.peek().is_none() {
            tracing::debug!("no new ids found");

            break;
        }

        ids.extend(new_ids);
        page += 1;
    }

    Ok(ids)
}
