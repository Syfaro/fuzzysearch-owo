use std::{
    borrow::Cow,
    collections::HashMap,
    fmt::Display,
    net::TcpStream,
    sync::{Arc, Mutex},
};

use futures::Future;
use opentelemetry::propagation::TextMapPropagator;
use prometheus::{register_counter_vec, register_histogram_vec, CounterVec, HistogramVec};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
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

pub fn new_submission_job(
    data: fuzzysearch_common::faktory::WebHookData,
) -> Result<faktory::Job, Error> {
    let args = serialize_args!(data);

    Ok(faktory::Job::new(job::NEW_SUBMISSION, args).fuzzy_queue(FaktoryQueue::Core))
}

pub fn search_existing_submissions_job(
    user_id: Uuid,
    media_id: Uuid,
) -> Result<faktory::Job, Error> {
    let args = serialize_args!(user_id, media_id);

    Ok(faktory::Job::new(job::SEARCH_EXISTING_SUBMISSIONS, args).fuzzy_queue(FaktoryQueue::Core))
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
        models::OwnedMediaItem::update_media(&ctx.conn, &ctx.s3, &ctx.config, item_id, im).await?;

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

        ctx.faktory
            .enqueue_job(
                JobInitiator::User { user_id },
                search_existing_submissions_job(user_id, item_id)?,
            )
            .await?;

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

        let similar_items = ctx
            .fuzzysearch
            .lookup_hashes(&[perceptual_hash], Some(3))
            .await?;

        for similar_item in similar_items {
            let similar_image = models::SimilarImage {
                site: models::Site::from(
                    similar_item
                        .site_info
                        .as_ref()
                        .ok_or(Error::Missing)?
                        .to_owned(),
                ),
                posted_by: similar_item
                    .artists
                    .as_ref()
                    .map(|artists| artists.join(", ")),
                page_url: Some(similar_item.url()),
                content_url: similar_item.url,
            };

            models::UserEvent::similar_found(
                &ctx.conn,
                &ctx.redis,
                user_id,
                media_id,
                similar_image,
                Some(similar_item.posted_at.unwrap_or_else(chrono::Utc::now)),
            )
            .await?;
        }

        Ok(())
    });

    environment.register(job::ADD_ACCOUNT, |ctx, job| async move {
        let mut args = job.args().iter();
        let (user_id, account_id) = extract_args!(args, Uuid, Uuid);

        let account = models::LinkedAccount::lookup_by_id(&ctx.conn, account_id, user_id)
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
                        .await
                        .map_err(Error::from_displayable)?;
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
            _ => unimplemented!(),
        }

        Ok(())
    });

    environment.register(job::NEW_SUBMISSION, |ctx, job| async move {
        let mut args = job.args().iter();
        let (data,) = extract_args!(args, fuzzysearch_common::faktory::WebHookData);

        let site = models::Site::from(data.site);

        let link = match &data.site {
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
        };

        let hash = data.hash.map(i64::from_be_bytes);

        // FurAffinity has some weird differences between how usernames are
        // displayed and how they're used in URLs.
        let artist = if matches!(data.site, fuzzysearch_common::types::Site::FurAffinity) {
            data.artist.replace('_', "")
        } else {
            data.artist.clone()
        };

        if let Some((account_id, user_id)) =
            models::LinkedAccount::search_site_account(&ctx.conn, &site.to_string(), &artist)
                .await?
        {
            tracing::info!("new submission belongs to known account");

            let sha256_hash: [u8; 32] = data
                .file_sha256
                .ok_or(Error::Missing)?
                .try_into()
                .expect("sha256 hash was wrong length");

            let item = models::OwnedMediaItem::add_item(
                &ctx.conn,
                user_id,
                account_id,
                data.site_id,
                hash,
                sha256_hash,
                Some(link.clone()),
                None, // TODO: collect title
                None, // TODO: collect posted_at
            )
            .await?;

            let data = reqwest::Client::default()
                .get(&data.file_url)
                .send()
                .await?
                .bytes()
                .await?;
            let im = image::load_from_memory(&data)?;

            models::OwnedMediaItem::update_media(&ctx.conn, &ctx.s3, &ctx.config, item, im).await?;
        }

        let hash = match hash {
            Some(hash) => hash,
            None => {
                tracing::warn!("webhook data had no hash");
                return Ok(());
            }
        };

        let similar_items = models::OwnedMediaItem::find_similar(&ctx.conn, hash).await?;

        let similar_image = models::SimilarImage {
            site,
            posted_by: Some(data.artist.clone()),
            page_url: Some(link.clone()),
            content_url: data.file_url,
        };

        for item in similar_items {
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
                ..
            }) = models::User::lookup_by_id(&ctx.conn, item.owner_id).await?
            {
                let body = askama::Template::render(&SimilarTemplate {
                    username: &username,
                    source_link: item
                        .link
                        .as_deref()
                        .unwrap_or_else(|| item.content_url.as_deref().unwrap_or("unknown")),
                    site_name: &data.site.to_string(),
                    poster_name: &data.artist,
                    similar_link: &link,
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
