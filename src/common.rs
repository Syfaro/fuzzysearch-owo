use std::collections::HashMap;

use askama::Template;
use foxlib::jobs::FaktoryProducer;
use futures::{StreamExt, TryFutureExt, TryStreamExt};
use lettre::AsyncTransport;
use sha2::Digest;
use sqlx::PgPool;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use crate::{
    jobs::{self, JobContext, JobInitiatorExt},
    models::{self, setting, SimilarImage},
    AsUrl, Error,
};

/// Maximum permitted download size for an image.
pub const MAX_DOWNLOAD_SIZE: usize = 50_000_000;

pub type SimilarAndPosted = (SimilarImage, Option<chrono::DateTime<chrono::Utc>>);

/// Search a perceptual hash from all known data sources.
///
/// This includes:
/// - FuzzySearch's index of many sites
/// - F-list
/// - Reddit
pub async fn search_perceptual_hash(
    ctx: &JobContext,
    perceptual_hash: i64,
) -> Result<impl Iterator<Item = SimilarAndPosted>, Error> {
    let hashes = [perceptual_hash];

    let fuzzysearch_fut = ctx
        .fuzzysearch
        .lookup_hashes(&hashes, Some(3))
        .map_ok(|results| results.into_iter().flat_map(fuzzysearch_to_similar))
        .map_err(Error::from);

    let flist_fut = models::FListFile::similar_images(&ctx.conn, perceptual_hash)
        .map_ok(|results| results.into_iter().map(flist_to_similar))
        .map_err(Error::from);

    let reddit_fut = models::RedditImage::similar_images(&ctx.conn, perceptual_hash)
        .map_ok(|results| results.into_iter().map(reddit_to_similar))
        .map_err(Error::from);

    let (fuzzysearch_items, flist_items, reddit_items) =
        futures::try_join!(fuzzysearch_fut, flist_fut, reddit_fut)?;

    Ok(fuzzysearch_items.chain(flist_items).chain(reddit_items))
}

fn fuzzysearch_to_similar(file: fuzzysearch::File) -> Option<SimilarAndPosted> {
    Some((
        models::SimilarImage {
            page_url: Some(file.url()),
            site: models::Site::from(file.site_info?),
            posted_by: file.artists.as_ref().map(|artists| artists.join(", ")),
            content_url: file.url,
        },
        file.posted_at,
    ))
}

fn flist_to_similar(file: models::FListFile) -> SimilarAndPosted {
    let character_name = percent_encoding::utf8_percent_encode(
        &file.character_name,
        percent_encoding::NON_ALPHANUMERIC,
    );

    (
        models::SimilarImage {
            site: models::Site::FList,
            page_url: Some(format!("https://www.f-list.net/c/{character_name}/")),
            posted_by: Some(file.character_name),
            content_url: format!(
                "https://static.f-list.net/images/charimage/{}.{}",
                file.id, file.ext
            ),
        },
        None,
    )
}

fn reddit_to_similar(image: models::RedditImage) -> SimilarAndPosted {
    (
        models::SimilarImage {
            site: models::Site::Reddit,
            page_url: Some(image.post.permalink),
            posted_by: Some(image.post.author),
            content_url: image.post.content_link,
        },
        Some(image.post.posted_at),
    )
}

/// Attempt to send notifications to users related to an incoming submission.
pub async fn notify_for_incoming(
    ctx: &JobContext,
    sub: &jobs::IncomingSubmission,
) -> Result<(), Error> {
    let perceptual_hash = match sub.perceptual_hash {
        Some(hash) => i64::from_be_bytes(hash),
        None => {
            tracing::info!("incoming submission had no perceptual hash, skipping");
            return Ok(());
        }
    };

    if perceptual_hash == 0 {
        tracing::warn!("hash was 0, skipping notifications");
        return Ok(());
    }

    let similar_image = models::SimilarImage {
        site: sub.site,
        posted_by: sub.posted_by.clone(),
        page_url: sub.page_url.clone(),
        content_url: sub.content_url.clone(),
    };

    let mut items_by_owner = HashMap::new();

    for item in models::OwnedMediaItem::find_similar(&ctx.conn, perceptual_hash).await? {
        items_by_owner
            .entry(item.owner_id)
            .or_insert_with(Vec::new)
            .push(item);
    }

    let futs = items_by_owner
        .into_iter()
        .map(|(owner_id, items)| notify_found(ctx, sub, owner_id, items, &similar_image));

    let mut buffered = futures::stream::iter(futs).buffer_unordered(2);

    while let Some(result) = buffered.next().await {
        if let Err(err) = result {
            tracing::error!("could not send notification: {}", err);
        }
    }

    Ok(())
}

async fn notify_found(
    ctx: &JobContext,
    sub: &jobs::IncomingSubmission,
    owner_id: Uuid,
    owned_items: Vec<models::OwnedMediaItem>,
    similar: &models::SimilarImage,
) -> Result<(), Error> {
    tracing::debug!(
        "found {} similar items owned by {}",
        owned_items.len(),
        owner_id
    );

    let user = match models::User::lookup_by_id(&ctx.conn, owner_id).await? {
        Some(user) => user,
        None => {
            tracing::warn!("could not find referenced user");
            return Ok(());
        }
    };

    let mut buffered = futures::stream::iter(owned_items.iter().map(|item| {
        models::UserEvent::similar_found(
            &ctx.conn,
            &ctx.redis,
            user.id,
            item.id,
            similar.clone(),
            None,
        )
    }))
    .buffer_unordered(2);

    let mut event_ids = Vec::with_capacity(owned_items.len());

    while let Some(result) = buffered.next().await {
        match result {
            Ok(event_id) => event_ids.push(event_id),
            Err(err) => {
                tracing::error!("could not create event: {}", err)
            }
        }
    }

    if let Some(ref posted_by) = sub.posted_by {
        if models::UserAllowlist::is_allowed(&ctx.conn, user.id, sub.site, posted_by).await? {
            tracing::debug!("user allowlisted poster");
            return Ok(());
        }
    }

    let (email_frequency, telegram_enabled, skipped_sites) = futures::try_join!(
        models::UserSetting::get(&ctx.conn, user.id),
        models::UserSetting::get(&ctx.conn, user.id),
        models::UserSetting::get(&ctx.conn, user.id),
    )?;
    let email_frequency: setting::EmailFrequency = email_frequency.unwrap_or_default();
    let telegram_enabled: setting::TelegramNotifications = telegram_enabled.unwrap_or_default();
    let skipped_sites: setting::SkippedSites = skipped_sites.unwrap_or_default();

    if email_frequency.0 == models::setting::Frequency::Never && !telegram_enabled.0 {
        tracing::info!("user had all notifications disabled, skipping");
        return Ok(());
    }

    if skipped_sites.0.contains(&sub.site) {
        tracing::info!(site = %sub.site, "user skipping site, skipping");
        return Ok(());
    }

    let most_recent_owned_item = owned_items
        .iter()
        .max_by_key(|item| item.last_modified)
        .ok_or(Error::Missing)?;

    match user.email {
        Some(ref email) if user.email_verifier.is_none() => {
            if let Err(err) = notify_email(
                ctx,
                &user,
                email_frequency.0,
                email,
                &event_ids,
                sub,
                most_recent_owned_item,
            )
            .await
            {
                tracing::error!("could not send notification email: {}", err);
            }
        }
        _ => (),
    }

    match user.telegram_id {
        Some(telegram_id) if telegram_enabled.0 => {
            if let Err(err) =
                notify_telegram(ctx, &user, telegram_id, sub, most_recent_owned_item).await
            {
                tracing::error!("could not send telegram notification: {}", err);
            }
        }
        _ => (),
    }

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

async fn notify_email(
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

    let body = SimilarEmailTemplate {
        username: user.display_name(),
        source_link: owned_item
            .link
            .as_deref()
            .unwrap_or_else(|| owned_item.content_url.as_deref().unwrap_or("unknown")),
        site_name: &sub.site.to_string(),
        poster_name: sub.posted_by.as_deref().unwrap_or("unknown"),
        similar_link: sub.page_url.as_deref().unwrap_or(&sub.content_url),
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
            Some(user.display_name().to_owned()),
            email.parse().map_err(Error::from_displayable)?,
        ))
        .subject(format!("Similar image found on {}", sub.site))
        .body(body)?;

    ctx.mailer.send(email).await?;

    Ok(())
}

#[derive(Template)]
#[template(path = "notification/similar_telegram.txt")]
struct SimilarTelegramTemplate<'a> {
    username: &'a str,
    source_link: &'a str,
    site_name: &'a str,
    poster_name: &'a str,
    similar_link: &'a str,
}

async fn notify_telegram(
    ctx: &JobContext,
    user: &models::User,
    telegram_id: i64,
    sub: &jobs::IncomingSubmission,
    owned_item: &models::OwnedMediaItem,
) -> Result<(), Error> {
    let body = SimilarTelegramTemplate {
        username: user.display_name(),
        source_link: owned_item
            .link
            .as_deref()
            .unwrap_or_else(|| owned_item.content_url.as_deref().unwrap_or("unknown")),
        site_name: &sub.site.to_string(),
        poster_name: sub.posted_by.as_deref().unwrap_or("unknown"),
        similar_link: sub.page_url.as_deref().unwrap_or(&sub.content_url),
    }
    .render()?;

    let send_message = tgbotapi::requests::SendMessage {
        chat_id: telegram_id.into(),
        text: body,
        disable_web_page_preview: Some(true),
        ..Default::default()
    };

    ctx.telegram.make_request(&send_message).await?;

    Ok(())
}

pub async fn import_from_incoming(
    ctx: &JobContext,
    sub: &jobs::IncomingSubmission,
) -> Result<(), Error> {
    // FurAffinity has some weird differences between how usernames are
    // displayed and how they're used in URLs.

    let artist = match &sub.posted_by {
        Some(artist) if sub.site == models::Site::FurAffinity => artist.replace('_', ""),
        Some(artist) => artist.to_owned(),
        None => return Ok(()),
    };

    let accounts =
        models::LinkedAccount::search_site_account(&ctx.conn, &sub.site.to_string(), &artist)
            .await?;

    let futs = accounts
        .into_iter()
        .map(|(account_id, user_id)| import_add(ctx, sub, account_id, user_id));

    let mut buffered = futures::stream::iter(futs).buffer_unordered(2);

    while let Some(result) = buffered.next().await {
        if let Err(err) = result {
            tracing::error!("could not import submission: {}", err);
        }
    }

    Ok(())
}

async fn import_add(
    ctx: &JobContext,
    sub: &jobs::IncomingSubmission,
    account_id: Uuid,
    user_id: Uuid,
) -> Result<(), Error> {
    tracing::info!("submission belongs to account {}", account_id);

    let sha256_hash = sub.sha256.ok_or(Error::Missing)?;

    let item = models::OwnedMediaItem::add_item(
        &ctx.conn,
        user_id,
        account_id,
        sub.site_id.clone(),
        sub.perceptual_hash.map(i64::from_be_bytes),
        sha256_hash,
        sub.page_url.clone(),
        None,
        sub.posted_at,
    )
    .await?;

    match download_image(ctx, &sub.content_url).await {
        Ok(im) => {
            models::OwnedMediaItem::update_media(&ctx.conn, &ctx.s3, &ctx.config, item, im).await?
        }
        Err(err) => tracing::warn!("could not attach image: {}", err),
    }

    Ok(())
}

/// Attempt to download an image, ensuring it does not exceed the maximum
/// download size.
async fn download_image(ctx: &JobContext, url: &str) -> Result<image::DynamicImage, Error> {
    let mut data = ctx.client.get(url).send().await?;
    let mut buf = bytes::BytesMut::new();

    while let Some(chunk) = data.chunk().await? {
        if buf.len() + chunk.len() >= MAX_DOWNLOAD_SIZE {
            tracing::warn!("requested download was larger than permitted maximum");
            return Err(Error::TooLarge(buf.len() + chunk.len()));
        }

        buf.extend(chunk);
    }

    let im = image::load_from_memory(&buf)?;
    Ok(im)
}

pub async fn handle_multipart_upload(
    pool: &PgPool,
    redis: &redis::aio::ConnectionManager,
    s3: &rusoto_s3::S3Client,
    faktory: &FaktoryProducer,
    config: &crate::Config,
    user: &models::User,
    mut form: actix_multipart::Multipart,
) -> Result<Vec<Uuid>, Error> {
    let mut ids = Vec::new();

    while let Ok(Some(mut field)) = form.try_next().await {
        tracing::trace!("checking multipart field: {:?}", field);

        if !matches!(field.content_disposition().get_name(), Some("image")) {
            continue;
        }

        let title = field
            .headers()
            .get("x-image-title")
            .map(|val| String::from_utf8_lossy(val.as_bytes()))
            .map(|title| title.to_string());

        let mut file = tokio::task::spawn_blocking(move || -> Result<_, String> {
            let file = tempfile::tempfile().map_err(|err| err.to_string())?;
            Ok(tokio::fs::File::from_std(file))
        })
        .await
        .map_err(Error::from_displayable)?
        .map_err(|err| Error::UnknownMessage(err.into()))?;

        let mut hasher = sha2::Sha256::default();

        let mut size = 0;
        while let Ok(Some(chunk)) = field.try_next().await {
            if size > 25_000_000 {
                return Err(Error::TooLarge(size));
            }

            file.write_all(&chunk).await?;
            hasher.update(&chunk);
            size += chunk.len();
        }

        if size == 0 {
            continue;
        }

        let sha256_hash: [u8; 32] = hasher
            .finalize()
            .try_into()
            .expect("sha256 hash was wrong size");
        let hash_str = hex::encode(sha256_hash);

        tracing::info!(size, hash = ?hash_str, "received complete file from client");

        file.rewind().await?;

        let file = file.into_std().await;
        let (perceptual_hash, im) = tokio::task::spawn_blocking(
            move || -> Result<([u8; 8], image::DynamicImage), image::ImageError> {
                let reader = std::io::BufReader::new(file);
                let reader = image::io::Reader::new(reader).with_guessed_format()?;
                let im = reader.decode()?;

                let hasher = fuzzysearch_common::get_hasher();
                let hash = hasher.hash_image(&im);
                let hash: [u8; 8] = hash
                    .as_bytes()
                    .try_into()
                    .expect("perceptual hash returned wrong bytes");

                Ok((hash, im))
            },
        )
        .await
        .map_err(|_err| Error::unknown_message("join error"))??;

        let id = models::OwnedMediaItem::add_manual_item(
            pool,
            user.id,
            i64::from_be_bytes(perceptual_hash),
            sha256_hash,
            title.as_deref(),
        )
        .await?;

        models::OwnedMediaItem::update_media(pool, s3, config, id, im).await?;

        models::UserEvent::notify(pool, redis, user.id, "Uploaded image.").await?;

        ids.push(id);

        faktory
            .enqueue_job(
                jobs::SearchExistingSubmissionsJob {
                    user_id: user.id,
                    media_id: id,
                }
                .initiated_by(jobs::JobInitiator::user(user.id)),
            )
            .await?;
    }

    Ok(ids)
}
