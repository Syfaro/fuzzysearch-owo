use std::{borrow::Cow, collections::HashMap, fmt::Display, sync::Arc};

use askama::Template;
use foxlib::jobs::{
    FaktoryForge, FaktoryForgeMiddleware, FaktoryProducer, Job, JobExtra, JobQueue,
};
use futures::{StreamExt, TryStreamExt};
use itertools::Itertools;
use lettre::AsyncTransport;
use redis::AsyncCommands;
use rusoto_s3::S3;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use uuid::Uuid;

use crate::{api, common, models, site, AsUrl, Error};

#[derive(Clone, Debug, clap::ValueEnum)]
pub enum Queue {
    Core,
    Outgoing,
    OutgoingBulk,
}

impl Queue {
    fn label(&self) -> Option<&'static str> {
        match self {
            Self::Core => None,
            Self::Outgoing | Self::OutgoingBulk => Some("outgoing"),
        }
    }
}

impl JobQueue for Queue {
    fn queue_name(&self) -> Cow<'static, str> {
        match self {
            Self::Core => "fuzzysearch_owo_core".into(),
            Self::Outgoing => "fuzzysearch_owo_outgoing".into(),
            Self::OutgoingBulk => "fuzzysearch_owo_bulk".into(),
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

pub trait JobInitiatorExt: Job {
    fn initiated_by(self, initiator: JobInitiator) -> InitiatedJob<Self>;
}

impl<J: Job> JobInitiatorExt for J {
    fn initiated_by(self, initiator: JobInitiator) -> InitiatedJob<Self> {
        InitiatedJob {
            job: self,
            initiator,
        }
    }
}

pub struct InitiatedJob<J: Job> {
    job: J,
    initiator: JobInitiator,
}

impl<J: Job> Job for InitiatedJob<J> {
    const NAME: &'static str = J::NAME;
    type Data = J::Data;
    type Queue = J::Queue;

    fn queue(&self) -> Self::Queue {
        self.job.queue()
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        let mut extra = self.job.extra()?.unwrap_or_default();
        extra.insert(
            "initiator".to_string(),
            serde_json::to_value(&self.initiator)?,
        );

        Ok(Some(extra))
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        self.job.args()
    }

    fn deserialize(args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        J::deserialize(args)
    }
}

#[derive(Debug)]
pub struct EmailVerificationJob {
    pub user_id: Uuid,
}

impl Job for EmailVerificationJob {
    const NAME: &'static str = "email_verification";
    type Data = Uuid;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Outgoing
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![serde_json::to_value(self.user_id)?])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AddAccountJob {
    pub user_id: Uuid,
    pub account_id: Uuid,
}

impl Job for AddAccountJob {
    const NAME: &'static str = "add_account";
    type Data = Self;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Outgoing
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![serde_json::to_value(self)?])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct VerifyAccountJob {
    pub user_id: Uuid,
    pub account_id: Uuid,
}

impl Job for VerifyAccountJob {
    const NAME: &'static str = "verify_account";
    type Data = Self;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Outgoing
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![serde_json::to_value(self)?])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Deserialize, Serialize)]
pub struct SearchExistingSubmissionsJob {
    pub user_id: Uuid,
    pub media_id: Uuid,
}

impl Job for SearchExistingSubmissionsJob {
    const NAME: &'static str = "search_existing_submissions";
    type Data = Self;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Core
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![serde_json::to_value(self)?])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

pub struct NewSubmissionJob(pub IncomingSubmission);

impl Job for NewSubmissionJob {
    const NAME: &'static str = "new_submission";
    type Data = IncomingSubmission;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Core
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![serde_json::to_value(self.0)?])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

pub struct CreateEmailDigestsJob(pub models::setting::Frequency);

impl Job for CreateEmailDigestsJob {
    const NAME: &'static str = "create_email_digests";
    type Data = models::setting::Frequency;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Core
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![serde_json::to_value(self.0)?])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Serialize, Deserialize)]
pub struct SendEmailDigestJob {
    pub user_id: Uuid,
    pub event_ids: Vec<Uuid>,
}

impl Job for SendEmailDigestJob {
    const NAME: &'static str = "send_email_digest";
    type Data = Self;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Outgoing
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![serde_json::to_value(self)?])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

pub struct SendPasswordResetEmailJob {
    pub user_id: Uuid,
}

impl Job for SendPasswordResetEmailJob {
    const NAME: &'static str = "send_email_password_reset";
    type Data = Uuid;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Outgoing
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![serde_json::to_value(self.user_id)?])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

struct PendingDeletionJob;

impl Job for PendingDeletionJob {
    const NAME: &'static str = "pending_deletion";
    type Data = ();
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Core
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![])
    }

    fn deserialize(_args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        Ok(())
    }
}

struct MigrateStorageJob(i64);

impl Job for MigrateStorageJob {
    const NAME: &'static str = "migrate_storage";
    type Data = i64;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Core
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![self.0.into()])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Serialize, Deserialize)]
struct ToggleSiteAccounts {
    site: models::Site,
    disabled: bool,
}

impl Job for ToggleSiteAccounts {
    const NAME: &'static str = "toggle_site_accounts";
    type Data = Self;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Core
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![serde_json::to_value(self)?])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
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

#[derive(Clone)]
pub struct JobContext {
    pub producer: FaktoryProducer,
    pub conn: sqlx::PgPool,
    pub redis: redis::aio::ConnectionManager,
    pub redlock: Arc<redlock::RedLock>,
    pub s3: rusoto_s3::S3Client,
    pub fuzzysearch: Arc<fuzzysearch::FuzzySearch>,
    pub mailer: crate::Mailer,
    pub config: Arc<crate::Config>,
    pub worker_config: Arc<crate::WorkerConfig>,
    pub client: reqwest::Client,
    pub telegram: Arc<tgbotapi::Telegram>,
    pub nats: async_nats::Client,
}

#[derive(askama::Template)]
#[template(path = "email/verify.txt")]
struct EmailVerify<'a> {
    username: &'a str,
    link: &'a str,
}

struct SimilarItem {
    source_link: String,
    site_name: String,
    posted_by: String,
    found_link: String,
}

#[derive(askama::Template)]
#[template(path = "notification/similar_digest_email.txt")]
struct SimilarDigestEmail<'a> {
    username: &'a str,
    items: &'a [SimilarItem],
    unsubscribe_link: String,
}

#[derive(Template)]
#[template(path = "email/password_reset.txt")]
struct PasswordResetEmail<'a> {
    username: &'a str,
    link: String,
}

struct LogInitiatorMiddleware;

impl<C, E> FaktoryForgeMiddleware<C, E> for LogInitiatorMiddleware {
    fn before_request(&self, _context: C, job: faktory::Job) -> Result<faktory::Job, E> {
        if let Some(Ok(initiator)) = job
            .custom
            .get("initiator")
            .map(|initiator| serde_json::from_value::<JobInitiator>(initiator.clone()))
        {
            tracing::info!(%initiator, "found initiator");
        } else {
            tracing::warn!("job was missing initiator");
        }

        Ok(job)
    }

    fn after_request(&self, _context: C, _duration: f64, result: Result<(), E>) -> Result<(), E> {
        result
    }
}

pub async fn start_job_processing(ctx: JobContext) -> Result<(), Error> {
    let queues: Vec<String> = ctx
        .worker_config
        .faktory_queues
        .iter()
        .map(|queue| queue.queue_name().to_string())
        .collect();

    let labels: Vec<String> = ctx
        .worker_config
        .faktory_queues
        .iter()
        .flat_map(|queue| queue.label())
        .chain(["fuzzysearch-owo"].into_iter())
        .unique()
        .map(str::to_string)
        .collect();

    tracing::info!(
        "starting faktory client on queues {} with labels {}",
        queues.join(","),
        labels.join(",")
    );

    let mut forge =
        FaktoryForge::new(ctx.clone(), Some(vec![Box::new(LogInitiatorMiddleware)])).await;
    site::register_jobs(&ctx.config, &mut forge).await?;

    EmailVerificationJob::register(&mut forge, |ctx, _job, user_id| async move {
        let user = models::User::lookup_by_id(&ctx.conn, user_id)
            .await?
            .ok_or(Error::Missing)?;

        let email = user
            .email
            .as_deref()
            .ok_or(Error::Missing)?
            .parse()
            .map_err(Error::from_displayable)?;

        let verifier = user.email_verifier.ok_or(Error::Missing)?;

        let body = EmailVerify {
            username: user.display_name(),
            link: &format!(
                "{}/user/email/verify?u={}&v={}",
                ctx.config.host_url,
                user.id.as_url(),
                verifier.as_url(),
            ),
        }
        .render()?;

        let email = lettre::Message::builder()
            .header(lettre::message::header::ContentType::TEXT_PLAIN)
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

    AddAccountJob::register(
        &mut forge,
        |ctx,
         _job,
         AddAccountJob {
             user_id,
             account_id,
         }| async move {
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
        },
    );

    VerifyAccountJob::register(
        &mut forge,
        |ctx,
         _job,
         VerifyAccountJob {
             user_id,
             account_id,
         }| async move {
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
                    format!("user-events:{user_id}"),
                    serde_json::to_string(&api::EventMessage::AccountVerified {
                        account_id,
                        verified: account_was_verified,
                    })?,
                )
                .await?;

            if account_was_verified {
                ctx.producer
                    .enqueue_job(
                        AddAccountJob {
                            user_id,
                            account_id: account.id,
                        }
                        .initiated_by(JobInitiator::user(user_id)),
                    )
                    .await?;
            }

            Ok(())
        },
    );

    SearchExistingSubmissionsJob::register(
        &mut forge,
        |ctx, _job, SearchExistingSubmissionsJob { user_id, media_id }| async move {
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

            let found_images = common::search_perceptual_hash(&ctx, perceptual_hash).await?;

            for (similar_image, created_at) in found_images {
                if let Some(posted_by) = &similar_image.posted_by {
                    if models::LinkedAccount::search_site_account(
                        &ctx.conn,
                        &similar_image.site.to_string(),
                        posted_by,
                    )
                    .await?
                    .into_iter()
                    .any(|(_account_id, searched_user_id)| searched_user_id == user_id)
                    {
                        tracing::info!("submission belongs to current user, skipping");
                        continue;
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
        },
    );

    CreateEmailDigestsJob::register(&mut forge, |ctx, _job, frequency| async move {
        let mut notifications_by_owner = HashMap::new();

        let pending_notifications =
            models::PendingNotification::ready(&ctx.conn, frequency).await?;

        for notif in pending_notifications {
            notifications_by_owner
                .entry(notif.owner_id)
                .or_insert_with(Vec::new)
                .push((notif.id, notif.user_event_id));
        }

        for (owner_id, ids) in notifications_by_owner {
            tracing::debug!("found {} events for {}", ids.len(), owner_id);

            let (notification_ids, event_ids): (Vec<_>, Vec<_>) = ids.into_iter().unzip();

            let mut tx = ctx.conn.begin().await?;
            models::PendingNotification::remove(&mut tx, &notification_ids).await?;

            if let Err(err) = ctx
                .producer
                .enqueue_job(
                    SendEmailDigestJob {
                        user_id: owner_id,
                        event_ids,
                    }
                    .initiated_by(JobInitiator::Schedule),
                )
                .await
            {
                tracing::error!("could not enqueue digest email job: {}", err);
            }

            tx.commit().await?;
        }

        Ok(())
    });

    SendEmailDigestJob::register(
        &mut forge,
        |ctx, _job, SendEmailDigestJob { user_id, event_ids }| async move {
            let user = models::User::lookup_by_id(&ctx.conn, user_id)
                .await?
                .ok_or(Error::Missing)?;

            let display_name = user.display_name();

            let email = match user.email.as_deref() {
                Some(email) if user.email_verifier.is_none() => email,
                _ => return Err(Error::Missing),
            };

            let events = models::UserEvent::resolve(&ctx.conn, event_ids.iter().copied()).await?;
            let media = models::OwnedMediaItem::resolve(
                &ctx.conn,
                user_id,
                events
                    .iter()
                    .flat_map(|(_event_id, event)| event.related_to_media_item_id),
            )
            .await?;

            let items: Vec<_> = event_ids
                .into_iter()
                .flat_map(|event_id| events.get(&event_id))
                .flat_map(|event| match event.data {
                    Some(models::UserEventData::SimilarImage(ref similar)) => event
                        .related_to_media_item_id
                        .map(|media_id| (media_id, similar)),
                    _ => None,
                })
                .flat_map(|(media_id, event)| media.get(&media_id).map(|media| (media, event)))
                .map(|(media, event)| SimilarItem {
                    source_link: media
                        .best_link()
                        .unwrap_or_else(|| media.content_url.as_deref().unwrap_or("unknown"))
                        .to_owned(),
                    site_name: event.site.to_string(),
                    posted_by: event
                        .posted_by
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                    found_link: event.best_link().to_owned(),
                })
                .collect();

            let body = SimilarDigestEmail {
                username: display_name,
                items: &items,
                unsubscribe_link: format!(
                    "{}/user/unsubscribe?u={}&t={}",
                    ctx.config.host_url,
                    user.id.as_url(),
                    user.unsubscribe_token.as_url()
                ),
            }
            .render()?;

            let email = lettre::Message::builder()
                .header(lettre::message::header::ContentType::TEXT_PLAIN)
                .from(ctx.config.smtp_from.clone())
                .reply_to(ctx.config.smtp_reply_to.clone())
                .to(lettre::message::Mailbox::new(
                    Some(display_name.to_string()),
                    email.parse().map_err(Error::from_displayable)?,
                ))
                .subject("FuzzySearch OwO match digest")
                .body(body)?;

            ctx.mailer.send(email).await?;

            Ok(())
        },
    );

    NewSubmissionJob::register(&mut forge, |ctx, _job, data| async move {
        common::notify_for_incoming(&ctx, &data).await?;
        common::import_from_incoming(&ctx, &data).await?;

        Ok(())
    });

    SendPasswordResetEmailJob::register(&mut forge, |ctx, _job, user_id| async move {
        let user = models::User::lookup_by_id(&ctx.conn, user_id)
            .await?
            .ok_or(Error::Missing)?;

        let email = match user.email.as_deref() {
            Some(email) => email,
            _ => return Err(Error::Missing),
        };

        let token = user.reset_token.as_deref().ok_or(Error::Missing)?;

        let display_name = user.display_name();

        let body = PasswordResetEmail {
            username: display_name,
            link: format!(
                "{}/auth/forgot/email?u={}&t={}",
                ctx.config.host_url,
                user.id.as_url(),
                token
            ),
        }
        .render()?;

        let email = lettre::Message::builder()
            .header(lettre::message::header::ContentType::TEXT_PLAIN)
            .from(ctx.config.smtp_from.clone())
            .reply_to(ctx.config.smtp_reply_to.clone())
            .to(lettre::message::Mailbox::new(
                Some(display_name.to_owned()),
                email.parse().map_err(Error::from_displayable)?,
            ))
            .subject("FuzzySearch OwO password reset")
            .body(body)?;

        ctx.mailer.send(email).await?;

        Ok(())
    });

    PendingDeletionJob::register(&mut forge, |cx, _job, _args| async move {
        let mut pending =
            sqlx::query!("SELECT id, url FROM pending_deletion LIMIT 100").fetch(&cx.conn);

        while let Some(Ok(row)) = pending.next().await {
            match models::delete_prefixed_object(&cx.s3, &cx.config, Some(&row.url)).await {
                Ok(_) => {
                    tracing::info!("deleted item");
                    sqlx::query!("DELETE FROM pending_deletion WHERE id = $1", row.id)
                        .execute(&cx.conn)
                        .await?;
                }
                Err(err) => {
                    tracing::error!("could not delete object: {err}");
                }
            }
        }

        Ok(())
    });

    MigrateStorageJob::register(&mut forge, |cx, _job, batch_size| async move {
        let items = sqlx::query_file!(
            "queries/owned_media/old_storage.sql",
            format!("{}%", cx.config.s3_cdn_prefix),
            batch_size
        )
        .fetch_all(&cx.conn)
        .await?;

        if items.is_empty() {
            tracing::info!("no items to migrate");
            return Ok(());
        }

        async fn migrate_item(cx: &JobContext, old_url: &str) -> Result<String, Error> {
            let buf = cx.client.get(old_url).send().await?.bytes().await?;

            let id = hex::encode(Uuid::new_v4().as_bytes());
            let path = format!("{}/{}/{}.jpg", &id[0..2], &id[2..4], &id);

            let put = rusoto_s3::PutObjectRequest {
                bucket: cx.config.s3_bucket.clone(),
                content_type: Some("image/jpeg".to_string()),
                key: path.clone(),
                content_length: Some(buf.len() as i64),
                body: Some(rusoto_core::ByteStream::from(buf.to_vec())),
                content_disposition: Some("inline".to_string()),
                ..Default::default()
            };

            cx.s3
                .put_object(put)
                .await
                .map_err(|err| Error::S3(err.to_string()))?;

            Ok(format!("{}/{path}", cx.config.s3_cdn_prefix))
        }

        async fn migrate_row(
            cx: JobContext,
            id: Uuid,
            content_url: Option<String>,
            thumb_url: Option<String>,
        ) -> Result<(), Error> {
            tracing::info!(%id, "attempting to migrate item");

            let new_content_url = if let Some(content_url) = content_url {
                Some(migrate_item(&cx, &content_url).await?)
            } else {
                None
            };

            let new_thumb_url = if let Some(thumb_url) = thumb_url {
                Some(migrate_item(&cx, &thumb_url).await?)
            } else {
                None
            };

            sqlx::query!(
                "UPDATE owned_media_item SET content_url = $2, thumb_url = $3 WHERE id = $1",
                id,
                new_content_url,
                new_thumb_url
            )
            .execute(&cx.conn)
            .await?;

            Ok(())
        }

        futures::stream::iter(items)
            .map(Ok)
            .try_for_each_concurrent(4, |item| {
                migrate_row(cx.clone(), item.id, item.content_url, item.thumb_url)
            })
            .await?;

        cx.producer
            .enqueue_job(MigrateStorageJob(batch_size))
            .await?;

        Ok(())
    });

    ToggleSiteAccounts::register(
        &mut forge,
        |cx, _job, ToggleSiteAccounts { site, disabled }| async move {
            let site_name = serde_plain::to_string(&site).unwrap();

            sqlx::query!(
                "UPDATE linked_account SET disabled = $2 WHERE source_site = $1",
                site_name,
                disabled
            )
            .execute(&cx.conn)
            .await?;

            Ok(())
        },
    );

    let mut client = forge.finalize();

    client.labels(labels);
    client.workers(ctx.worker_config.faktory_workers);

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NatsSite {
    #[serde(rename = "flist")]
    FList,
    Reddit,
    Bluesky,
}

#[serde_as]
#[derive(Debug, Serialize)]
pub struct NatsNewImage {
    pub site: NatsSite,
    pub image_url: String,
    pub page_url: Option<String>,
    pub posted_by: Option<String>,
    #[serde_as(as = "Option<serde_with::hex::Hex>")]
    pub perceptual_hash: Option<[u8; 8]>,
    #[serde_as(as = "Option<serde_with::hex::Hex>")]
    pub sha256_hash: Option<[u8; 32]>,
}

impl JobContext {
    pub async fn nats_new_image(&self, image: NatsNewImage) {
        let site_name = serde_plain::to_string(&image.site).unwrap();

        tracing::debug!(
            site = site_name,
            image_url = image.image_url,
            "sending new image to nats"
        );

        let data = match serde_json::to_vec(&image) {
            Ok(data) => data,
            Err(err) => return tracing::error!("could not serialize image for nats: {err}"),
        };

        let subject = format!("fuzzysearch.ingest.{site_name}");
        if let Err(err) = self.nats.publish(subject, data.into()).await {
            tracing::error!("could not publish new image to nats: {err}");
        }
    }
}
