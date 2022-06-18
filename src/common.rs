use askama::Template;
use futures::{StreamExt, TryFutureExt};
use lettre::AsyncTransport;
use uuid::Uuid;

use crate::{
    jobs::{self, JobContext},
    models::{self, setting, SimilarImage},
    Error,
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
    (
        models::SimilarImage {
            site: models::Site::FList,
            page_url: Some(format!("https://www.f-list.net/c/{}/", file.character_name)),
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

    let similar_items = models::OwnedMediaItem::find_similar(&ctx.conn, perceptual_hash).await?;

    let similar_image = models::SimilarImage {
        site: sub.site,
        posted_by: sub.posted_by.clone(),
        page_url: sub.page_url.clone(),
        content_url: sub.content_url.clone(),
    };

    let futs = similar_items
        .into_iter()
        .map(|item| notify_found(ctx, sub, item, &similar_image));

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
    owned_item: models::OwnedMediaItem,
    similar: &models::SimilarImage,
) -> Result<(), Error> {
    tracing::debug!("found similar item owned by {}", owned_item.owner_id);

    let user = match models::User::lookup_by_id(&ctx.conn, owned_item.owner_id).await? {
        Some(user) => user,
        None => {
            tracing::warn!("could not find referenced user");
            return Ok(());
        }
    };

    models::UserEvent::similar_found(
        &ctx.conn,
        &ctx.redis,
        user.id,
        owned_item.id,
        similar.clone(),
        None,
    )
    .await?;

    if let Some(ref posted_by) = sub.posted_by {
        if models::UserAllowlist::is_allowed(&ctx.conn, user.id, sub.site, posted_by).await? {
            tracing::info!("user allowlisted poster");
            return Ok(());
        }
    }

    let emails_enabled: setting::EmailNotifications = models::UserSetting::get(&ctx.conn, user.id)
        .await?
        .unwrap_or_default();

    let telegram_enabled: setting::TelegramNotifications =
        models::UserSetting::get(&ctx.conn, user.id)
            .await?
            .unwrap_or_default();

    if !emails_enabled.0 && !telegram_enabled.0 {
        tracing::info!("user had all notifications disabled, skipping");
        return Ok(());
    }

    match user.email {
        Some(ref email) if user.email_verifier.is_none() && emails_enabled.0 => {
            if let Err(err) = notify_email(ctx, &user, email, sub, &owned_item).await {
                tracing::error!("could not send notification email: {}", err);
            }
        }
        _ => (),
    }

    match user.telegram_id {
        Some(telegram_id) if telegram_enabled.0 => {
            if let Err(err) = notify_telegram(ctx, &user, telegram_id, sub, &owned_item).await {
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
}

async fn notify_email(
    ctx: &JobContext,
    user: &models::User,
    email: &str,
    sub: &jobs::IncomingSubmission,
    owned_item: &models::OwnedMediaItem,
) -> Result<(), Error> {
    let body = SimilarEmailTemplate {
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
