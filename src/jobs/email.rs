use std::collections::HashMap;

use askama::Template;
use foxlib::jobs::{FaktoryForge, Job, JobExtra};
use itertools::Itertools;
use lettre::{
    message::header::{Header, HeaderName, HeaderValue},
    Address, AsyncTransport,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    jobs::{self, JobContext, JobInitiator, JobInitiatorExt, Queue},
    models, AsUrl, Error,
};

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

pub fn register_email_jobs(forge: &mut FaktoryForge<jobs::JobContext, Error>) {
    EmailVerificationJob::register(forge, |ctx, _job, user_id| async move {
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

    CreateEmailDigestsJob::register(forge, |ctx, _job, frequency| async move {
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
        forge,
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

            let unsubscribe_link = format!(
                "{}/user/unsubscribe?u={}&t={}",
                ctx.config.host_url,
                user.id.as_url(),
                user.unsubscribe_token.as_url()
            );

            let list_unsubscribe =
                ListUnsubscribe::parse(&unsubscribe_link).expect("invalid list unsubscribe header");

            let body = SimilarDigestEmail {
                username: display_name,
                items: &items,
                unsubscribe_link,
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
                .header(ListId::new("match-digest.owo", &ctx.config.smtp_from.email))
                .header(
                    ListUnsubscribePost::parse("List-Unsubscribe=One-Click")
                        .expect("invalid list unsubscribe post header"),
                )
                .header(list_unsubscribe)
                .body(body)?;

            ctx.mailer.send(email).await?;

            Ok(())
        },
    );

    SendPasswordResetEmailJob::register(forge, |ctx, _job, user_id| async move {
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
}

pub(crate) async fn notify_email(
    ctx: &JobContext,
    user: &models::User,
    frequency: models::setting::Frequency,
    email: &str,
    event_ids: &[Uuid],
    sub: &jobs::IncomingSubmission,
    owned_item: &models::OwnedMediaItem,
) -> Result<(), Error> {
    if frequency == models::setting::Frequency::Never {
        tracing::info!("user does not want emails, skipping");

        return Ok(());
    }

    if frequency.is_digest() {
        tracing::info!(
            "frequency is digest, inserting {} pending notifications",
            event_ids.len()
        );

        for event_id in event_ids.iter().copied() {
            if let Err(err) =
                models::PendingNotification::create(&ctx.conn, user.id, event_id).await
            {
                tracing::error!("could not create pending notification: {}", err);
            }
        }

        return Ok(());
    }

    let unsubscribe_link = format!(
        "{}/user/unsubscribe?u={}&t={}",
        ctx.config.host_url,
        user.id.as_url(),
        user.unsubscribe_token.as_url()
    );

    let list_unsubscribe =
        ListUnsubscribe::parse(&unsubscribe_link).expect("invalid list unsubscribe header");

    let body = SimilarEmailTemplate {
        username: user.display_name(),
        source_link: owned_item
            .best_link()
            .unwrap_or_else(|| owned_item.content_url.as_deref().unwrap_or("unknown")),
        site_name: &sub.site.to_string(),
        poster_name: sub.posted_by.as_deref().unwrap_or("unknown"),
        similar_link: sub.page_url.as_deref().unwrap_or(&sub.content_url),
        unsubscribe_link,
    }
    .render()?;

    let email = lettre::Message::builder()
        .header(lettre::message::header::ContentType::TEXT_PLAIN)
        .from(ctx.config.smtp_from.clone())
        .reply_to(ctx.config.smtp_reply_to.clone())
        .to(lettre::message::Mailbox::new(
            Some(user.display_name().to_owned()),
            email.parse().map_err(Error::from_displayable)?,
        ))
        .subject(format!("Similar image found on {}", sub.site))
        .header(ListId::new(
            "match-notification.owo",
            &ctx.config.smtp_from.email,
        ))
        .header(
            ListUnsubscribePost::parse("List-Unsubscribe=One-Click")
                .expect("invalid list unsubscribe post header"),
        )
        .header(list_unsubscribe)
        .body(body)?;

    ctx.mailer.send(email).await?;

    Ok(())
}

#[derive(Template)]
#[template(path = "notification/similar_email.txt")]
struct SimilarEmailTemplate<'a> {
    username: &'a str,
    source_link: &'a str,
    site_name: &'a str,
    poster_name: &'a str,
    similar_link: &'a str,
    unsubscribe_link: String,
}

#[derive(Template)]
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

#[derive(Template)]
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

#[derive(Clone)]
struct ListUnsubscribePost {
    value: String,
}

impl Header for ListUnsubscribePost {
    fn name() -> HeaderName {
        HeaderName::new_from_ascii_str("List-Unsubscribe-Post")
    }

    fn parse(s: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self {
            value: s.to_string(),
        })
    }

    fn display(&self) -> HeaderValue {
        HeaderValue::new(Self::name(), self.value.clone())
    }
}

#[derive(Clone)]
struct ListUnsubscribe {
    values: Vec<url::Url>,
}

impl Header for ListUnsubscribe {
    fn name() -> HeaderName {
        HeaderName::new_from_ascii_str("List-Unsubscribe")
    }

    fn parse(s: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let urls = s.split(',').map(|url| url.parse()).try_collect()?;

        Ok(Self { values: urls })
    }

    fn display(&self) -> HeaderValue {
        HeaderValue::new(
            Self::name(),
            self.values
                .iter()
                .map(|value| format!("<{}>", value.as_str()))
                .join(", "),
        )
    }
}

#[derive(Clone)]
struct ListId {
    id: String,
}

impl ListId {
    fn new<S: ToString>(id: S, list_address: &Address) -> Self {
        let id = id.to_string();
        let domain = list_address.domain();

        Self {
            id: format!("<{id}.{domain}>"),
        }
    }
}

impl Header for ListId {
    fn name() -> HeaderName {
        HeaderName::new_from_ascii_str("List-Id")
    }

    fn parse(s: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self { id: s.to_string() })
    }

    fn display(&self) -> HeaderValue {
        HeaderValue::new(Self::name(), self.id.clone())
    }
}
