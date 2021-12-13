use redis::AsyncCommands;
use uuid::Uuid;

use crate::{models, Error};

pub const FUZZYSEARCH_OWO_QUEUE: &str = "fuzzysearch-owo";

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

    Ok(faktory::Job::new("add_account", args).on_queue(FUZZYSEARCH_OWO_QUEUE))
}

pub fn add_furaffinity_submission(
    user_id: Uuid,
    account_id: Uuid,
    submission_id: i32,
    import: bool,
) -> Result<faktory::Job, Error> {
    let args = serialize_args!(user_id, account_id, submission_id, import);

    Ok(faktory::Job::new("add_submission_furaffinity", args).on_queue(FUZZYSEARCH_OWO_QUEUE))
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

#[derive(Clone)]
pub struct JobContext {
    pub faktory: fuzzysearch_common::faktory::FaktoryClient,
    pub conn: sqlx::Pool<sqlx::Postgres>,
    pub redis: redis::aio::ConnectionManager,
    pub s3: rusoto_s3::S3Client,
    pub fuzzysearch: std::sync::Arc<fuzzysearch::FuzzySearch>,
    pub config: std::sync::Arc<crate::Config>,
}

pub async fn start_job_processing(ctx: JobContext) {
    let mut client = faktory::ConsumerBuilder::default();
    client.labels(vec!["rust".to_string(), "fuzzysearch-owo".to_string()]);
    client.workers(ctx.config.worker_threads);

    let handle = tokio::runtime::Handle::current();

    let ctx_clone = ctx.clone();
    let handle_clone = handle.clone();
    client.register(
        "add_submission_furaffinity",
        move |job| -> Result<(), Error> {
            let ctx = ctx_clone.clone();

            handle_clone.block_on(async move {
                tracing::info!("wanting to load fa submission: {:?}", job.args());

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
                        Error::LoadingError(format!(
                            "Could not load FurAffinity submission {}",
                            sub_id
                        ))
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
                models::OwnedMediaItem::update_media(&ctx.conn, &ctx.s3, &ctx.config, item_id, im)
                    .await?;

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

                tracing::debug!("enqueuing check for previous items");
                ctx.faktory
                    .enqueue(
                        faktory::Job::new(
                            "search_existing_submissions",
                            serialize_args!(user_id, item_id),
                        )
                        .on_queue(FUZZYSEARCH_OWO_QUEUE),
                    )
                    .await
                    .map_err(Error::from_displayable)?;

                Ok(())
            })
        },
    );

    let ctx_clone = ctx.clone();
    let handle_clone = handle.clone();
    client.register(
        "search_existing_submissions",
        move |job| -> Result<(), Error> {
            let ctx = ctx_clone.clone();

            handle_clone.block_on(async move {
                let mut args = job.args().iter();

                let (user_id, media_id) = extract_args!(args, Uuid, Uuid);

                tracing::info!("performing lookup for previous matches");

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
            })
        },
    );

    let ctx_clone = ctx.clone();
    let handle_clone = handle.clone();
    client.register("add_account", move |job| -> Result<(), Error> {
        let ctx = ctx_clone.clone();

        handle_clone.block_on(async move {
            let mut args = job.args().iter();
            let (user_id, account_id) = extract_args!(args, Uuid, Uuid);

            tracing::info!("updating loading state of account {:?}", account_id);

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
                            .enqueue(add_furaffinity_submission(user_id, account_id, id, true)?)
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
        })
    });

    let ctx_clone = ctx.clone();
    let handle_clone = handle.clone();
    client.register("new_submission", move |job| -> Result<(), Error> {
        let ctx = ctx_clone.clone();

        handle_clone.block_on(async move {
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

            if let Some((account_id, user_id)) = models::LinkedAccount::search_site_account(
                &ctx.conn,
                &site.to_string(),
                &data.artist,
            )
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

                models::OwnedMediaItem::update_media(&ctx.conn, &ctx.s3, &ctx.config, item, im)
                    .await?;
            }

            let hash = match hash {
                Some(hash) => hash,
                None => {
                    tracing::info!("webhook data had no hash");
                    return Ok(());
                }
            };

            let similar_items = models::OwnedMediaItem::find_similar(&ctx.conn, hash).await?;

            let similar_image = models::SimilarImage {
                site,
                posted_by: Some(data.artist),
                page_url: Some(link),
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
            }

            Ok(())
        })
    });

    tokio::task::spawn_blocking(move || {
        let mut client = client
            .connect(Some(&ctx.config.faktory_host))
            .expect("could not connect to faktory");

        if let Err(err) = client.run(&[FUZZYSEARCH_OWO_QUEUE]) {
            tracing::error!("worker failed: {:?}", err);
        }
    });
}

async fn discover_furaffinity_submissions(
    user: &str,
    config: &crate::Config,
) -> anyhow::Result<Vec<i32>> {
    let client = reqwest::Client::default();
    let id_selector =
        scraper::Selector::parse(".submission-list u a").expect("known good selector failed");

    let mut ids = Vec::new();

    let mut page = 1;
    loop {
        tracing::info!(page, "Loading gallery page");

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
            tracing::debug!("No new IDs found");

            break;
        }

        ids.extend(new_ids);
        page += 1;
    }

    Ok(ids)
}
