use std::{
    borrow::Cow,
    collections::HashMap,
    fmt::Display,
    net::TcpStream,
    sync::{Arc, Mutex},
};

use askama::Template;
use futures::Future;
use lettre::AsyncTransport;
use opentelemetry::propagation::TextMapPropagator;
use prometheus::{register_counter_vec, register_histogram_vec, CounterVec, HistogramVec};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tracing::Instrument;
use uuid::Uuid;

use crate::{
    api,
    models::{self, setting},
    site, Error,
};

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
    pub const EMAIL_VERIFICATION: &str = "email_verification";

    pub const ADD_ACCOUNT: &str = "add_account";
    pub const VERIFY_ACCOUNT: &str = "verify_account";

    pub const NEW_SUBMISSION: &str = "new_submission";

    pub const ADD_SUBMISSION_FURAFFINITY: &str = "add_submission_furaffinity";
    pub const ADD_SUBMISSION_DEVIANTART: &str = "add_submission_deviantart";
    pub const ADD_SUBMISSION_WEASYL: &str = "add_submission_weasyl";

    pub const SEARCH_EXISTING_SUBMISSIONS: &str = "search_existing_submissions";

    pub const FLIST_COLLECT_GALLERY_IMAGES: &str = "flist_gallery";
    pub const FLIST_HASH_IMAGE: &str = "flist_hash";

    pub const DEVIANTART_COLLECT_ACCOUNTS: &str = "deviantart_accounts";
    pub const DEVIANTART_UPDATE_ACCOUNT: &str = "deviantart_account_update";

    pub const REDDIT_CHECK_SUBREDDITS: &str = "reddit_check";
    pub const REDDIT_UPDATE_SUBREDDIT: &str = "reddit_subreddit";
    pub const REDDIT_LOAD_POST: &str = "reddit_post";
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
                    crate::jobs::get_arg::<$x>(&mut $args)?,
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

pub fn email_verification_job(user_id: Uuid) -> Result<faktory::Job, Error> {
    let args = serialize_args!(user_id);

    Ok(faktory::Job::new(job::EMAIL_VERIFICATION, args).fuzzy_queue(FaktoryQueue::Outgoing))
}

pub fn add_account_job(user_id: Uuid, account_id: Uuid) -> Result<faktory::Job, Error> {
    let args = serialize_args!(user_id, account_id);

    Ok(faktory::Job::new(job::ADD_ACCOUNT, args).fuzzy_queue(FaktoryQueue::Outgoing))
}

pub fn verify_account_job(user_id: Uuid, account_id: Uuid) -> Result<faktory::Job, Error> {
    let args = serialize_args!(user_id, account_id);

    Ok(faktory::Job::new(job::VERIFY_ACCOUNT, args).fuzzy_queue(FaktoryQueue::Outgoing))
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

pub fn add_weasyl_submission_job(
    user_id: Uuid,
    account_id: Uuid,
    submission: site::WeasylSubmission,
    import: bool,
) -> Result<faktory::Job, Error> {
    let args = serialize_args!(user_id, account_id, submission, import);

    Ok(faktory::Job::new(job::ADD_SUBMISSION_WEASYL, args).fuzzy_queue(FaktoryQueue::Outgoing))
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

pub fn add_submission_deviantart_job(
    user_id: Uuid,
    account_id: Uuid,
    sub: site::DeviantArtSubmission,
    import: bool,
) -> Result<faktory::Job, Error> {
    let args = serialize_args!(user_id, account_id, sub, import);

    Ok(faktory::Job::new(job::ADD_SUBMISSION_DEVIANTART, args).fuzzy_queue(FaktoryQueue::Outgoing))
}

pub fn deviantart_update_account_job(account_id: Uuid) -> Result<faktory::Job, Error> {
    let args = serialize_args!(account_id);

    Ok(faktory::Job::new(job::DEVIANTART_UPDATE_ACCOUNT, args).fuzzy_queue(FaktoryQueue::Outgoing))
}

pub fn reddit_update_subreddit_job(name: &str) -> faktory::Job {
    let args = vec![serde_json::Value::from(name)];

    faktory::Job::new(job::REDDIT_UPDATE_SUBREDDIT, args).fuzzy_queue(FaktoryQueue::Outgoing)
}

pub fn reddit_post_job(subreddit: &str, post: site::RedditPost) -> Result<faktory::Job, Error> {
    let args = serialize_args!(subreddit, post);

    Ok(faktory::Job::new(job::REDDIT_LOAD_POST, args).fuzzy_queue(FaktoryQueue::Outgoing))
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

pub fn get_arg_opt<T: serde::de::DeserializeOwned>(
    args: &mut core::slice::Iter<serde_json::Value>,
) -> Result<Option<T>, Error> {
    let arg = match args.next() {
        Some(arg) => arg,
        None => return Ok(None),
    };

    let data = serde_json::from_value(arg.to_owned())?;
    Ok(Some(data))
}

pub fn get_arg<T: serde::de::DeserializeOwned>(
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
    pub redlock: Arc<redlock::RedLock>,
    pub s3: rusoto_s3::S3Client,
    pub fuzzysearch: Arc<fuzzysearch::FuzzySearch>,
    pub mailer: crate::Mailer,
    pub config: Arc<crate::Config>,
    pub worker_config: Arc<crate::WorkerConfig>,
    pub client: reqwest::Client,
    pub telegram: Arc<tgbotapi::Telegram>,
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
#[template(path = "email/verify.txt")]
struct EmailVerify<'a> {
    username: &'a str,
    link: &'a str,
}

#[derive(askama::Template)]
#[template(path = "notification/similar_email.txt")]
struct SimilarEmailTemplate<'a> {
    username: &'a str,
    source_link: &'a str,
    site_name: &'a str,
    poster_name: &'a str,
    similar_link: &'a str,
}

#[derive(askama::Template)]
#[template(path = "notification/similar_telegram.txt")]
struct SimilarTelegramTemplate<'a> {
    username: &'a str,
    source_link: &'a str,
    site_name: &'a str,
    poster_name: &'a str,
    similar_link: &'a str,
}

pub async fn start_job_processing(ctx: JobContext) -> Result<(), Error> {
    let queues: Vec<String> = ctx
        .worker_config
        .faktory_queues
        .iter()
        .map(|queue| queue.as_str().to_string())
        .collect();

    let labels: Vec<String> = ctx
        .worker_config
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
    client.workers(ctx.worker_config.faktory_workers);

    let handle = tokio::runtime::Handle::current();

    let mut environment = WorkerEnvironment {
        faktory: client,
        handle,
        ctx: Arc::new(ctx.clone()),
    };

    for (name, job_fn) in site::jobs(&ctx.config).await? {
        environment.register(name, job_fn);
    }

    environment.register(job::EMAIL_VERIFICATION, |ctx, job| async move {
        let mut args = job.args().iter();
        let (user_id,) = extract_args!(args, Uuid);

        let user = models::User::lookup_by_id(&ctx.conn, user_id)
            .await?
            .ok_or(Error::Missing)?;

        let email = user
            .email
            .as_deref()
            .ok_or(Error::Missing)?
            .parse()
            .map_err(Error::from_displayable)?;

        let body = EmailVerify {
            username: user.display_name(),
            link: &format!(
                "{}/user/email/verify?u={}&v={}",
                ctx.config.host_url,
                user.id,
                user.email_verifier.unwrap_or_else(Uuid::new_v4)
            ),
        }
        .render()?;

        let email = lettre::Message::builder()
            .from(ctx.config.smtp_from.clone())
            .reply_to(ctx.config.smtp_reply_to.clone())
            .to(lettre::message::Mailbox::new(
                Some(user.display_name().to_owned()),
                email,
            ))
            .subject("Verify your FuzzySearch OwO account email address")
            .body(body)?;

        ctx.mailer.send(email).await.unwrap();

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
            .map(|file| {
                (
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
                )
            });

        let reddit_items = models::RedditImage::similar_images(&ctx.conn, perceptual_hash)
            .await?
            .into_iter()
            .map(|image| {
                (
                    models::SimilarImage {
                        site: models::Site::Reddit,
                        page_url: Some(image.post.permalink),
                        posted_by: Some(image.post.author),
                        content_url: image.post.content_link,
                    },
                    Some(image.post.posted_at),
                )
            });

        let found_images = fuzzysearch_items.chain(flist_items).chain(reddit_items);

        for (similar_image, created_at) in found_images {
            if let Some(posted_by) = &similar_image.posted_by {
                if let Some((_account_id, searched_user_id)) =
                    models::LinkedAccount::search_site_account(
                        &ctx.conn,
                        &similar_image.site.to_string(),
                        posted_by,
                    )
                    .await?
                {
                    tracing::debug!("existing submission belongs to known account");

                    if searched_user_id == user_id {
                        tracing::info!("submission belongs to current user, skipping");
                        continue;
                    }
                }
            }

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

        match account.source_site.collected_site(&ctx.config).await {
            Ok(Some(collected_site)) => collected_site.add_account(&ctx, account).await?,
            Ok(None) => return Err(Error::user_error("Account cannot be added on this site.")),
            Err(err) => return Err(err),
        }

        Ok(())
    });

    environment.register(job::VERIFY_ACCOUNT, |ctx, job| async move {
        let mut args = job.args().iter();
        let (user_id, account_id) = extract_args!(args, Uuid, Uuid);

        let account = models::LinkedAccount::lookup_by_id(&ctx.conn, account_id)
            .await?
            .ok_or(Error::Missing)?;

        let key = match account.verification_key() {
            Some(key) => key,
            None => return Err(Error::Missing),
        };

        let account_was_verified = match account.source_site {
            models::Site::FurAffinity => {
                let body = ctx
                    .client
                    .get(format!(
                        "https://www.furaffinity.net/user/{}/",
                        account.username,
                    ))
                    .header(
                        reqwest::header::COOKIE,
                        format!(
                            "a={}; b={}",
                            ctx.config.furaffinity_cookie_a, ctx.config.furaffinity_cookie_b
                        ),
                    )
                    .send()
                    .await?
                    .text()
                    .await?;

                let page = scraper::Html::parse_document(&body);
                let profile = scraper::Selector::parse(".userpage-layout-profile").unwrap();

                let text = page
                    .select(&profile)
                    .next()
                    .map(|elem| elem.text().collect::<String>())
                    .unwrap_or_default();

                let verifier_found = text.contains(key);

                if verifier_found {
                    models::LinkedAccount::update_data(&ctx.conn, account.id, None).await?;
                }

                verifier_found
            }
            models::Site::Weasyl => {
                let profile: serde_json::Value = ctx
                    .client
                    .get(format!(
                        "https://www.weasyl.com/api/users/{}/view",
                        account.username
                    ))
                    .header("x-weasyl-api-key", &ctx.config.weasyl_api_token)
                    .send()
                    .await?
                    .json()
                    .await?;

                let profile_text = profile
                    .as_object()
                    .and_then(|profile| profile.get("profile_text"))
                    .and_then(|profile_text| profile_text.as_str())
                    .ok_or_else(|| Error::unknown_message("Weasyl was missing profile text"))?;

                let verifier_found = profile_text.contains(key);

                if verifier_found {
                    models::LinkedAccount::update_data(&ctx.conn, account.id, None).await?;
                }

                verifier_found
            }
            _ => {
                return Err(Error::unknown_message(
                    "attempted to verify unsupported account",
                ))
            }
        };

        let account_was_verified = if ctx.config.skip_verifications {
            tracing::warn!(
                "skipping verifications, found state was: {}",
                account_was_verified
            );

            true
        } else {
            account_was_verified
        };

        tracing::info!(account_was_verified, "checked verification");

        let mut redis = ctx.redis.clone();
        redis
            .publish(
                format!("user-events:{}", user_id),
                serde_json::to_string(&api::EventMessage::AccountVerified {
                    account_id,
                    verified: account_was_verified,
                })?,
            )
            .await?;

        if account_was_verified {
            ctx.faktory
                .enqueue_job(
                    JobInitiator::user(user_id),
                    add_account_job(user_id, account.id)?,
                )
                .await?;
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

                let sha256_hash = data.sha256.ok_or(Error::Missing)?;

                let item = models::OwnedMediaItem::add_item(
                    &ctx.conn,
                    user_id,
                    account_id,
                    data.site_id,
                    data.perceptual_hash.map(i64::from_be_bytes),
                    sha256_hash,
                    data.page_url.clone(),
                    None, // TODO: collect title
                    data.posted_at,
                )
                .await?;

                let data = ctx
                    .client
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

        // TODO: this loop should split each item into a new job
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

            if let Some(user) = models::User::lookup_by_id(&ctx.conn, item.owner_id).await? {
                tracing::debug!("sending notifications to user");

                let emails_enabled: setting::EmailNotifications =
                    models::UserSetting::get(&ctx.conn, user.id)
                        .await?
                        .unwrap_or_default();

                let telegram_enabled: setting::TelegramNotifications =
                    models::UserSetting::get(&ctx.conn, user.id)
                        .await?
                        .unwrap_or_default();

                if !emails_enabled.0 && !telegram_enabled.0 {
                    tracing::debug!("all notifications disabled, skipping");
                    continue;
                }

                match &user.email {
                    Some(email) if user.email_verifier.is_none() && emails_enabled.0 => {
                        let body = SimilarEmailTemplate {
                            username: user.display_name(),
                            source_link: item.link.as_deref().unwrap_or_else(|| {
                                item.content_url.as_deref().unwrap_or("unknown")
                            }),
                            site_name: &data.site.to_string(),
                            poster_name: data.posted_by.as_deref().unwrap_or("unknown"),
                            similar_link: data.page_url.as_deref().unwrap_or(&data.content_url),
                        }
                        .render()?;

                        let email = lettre::Message::builder()
                            .from(ctx.config.smtp_from.clone())
                            .reply_to(ctx.config.smtp_reply_to.clone())
                            .to(lettre::message::Mailbox::new(
                                Some(user.display_name().to_owned()),
                                email.parse().map_err(Error::from_displayable)?,
                            ))
                            .subject(format!("Similar image found on {}", data.site))
                            .body(body)?;

                        if let Err(err) = ctx.mailer.send(email).await {
                            tracing::error!("could not send email: {:?}", err);
                        };
                    }
                    _ => (),
                }

                match user.telegram_id {
                    Some(telegram_id) if telegram_enabled.0 => {
                        let body = SimilarTelegramTemplate {
                            username: user.display_name(),
                            source_link: item.link.as_deref().unwrap_or_else(|| {
                                item.content_url.as_deref().unwrap_or("unknown")
                            }),
                            site_name: &data.site.to_string(),
                            poster_name: data.posted_by.as_deref().unwrap_or("unknown"),
                            similar_link: data.page_url.as_deref().unwrap_or(&data.content_url),
                        }
                        .render()?;

                        let send_message = tgbotapi::requests::SendMessage {
                            chat_id: telegram_id.into(),
                            text: body,
                            disable_web_page_preview: Some(true),
                            ..Default::default()
                        };

                        if let Err(err) = ctx.telegram.make_request(&send_message).await {
                            tracing::error!("could not send telegram message: {:?}", err);
                        }
                    }
                    _ => (),
                }
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
    .map_err(Error::from_displayable)
}
