use redis::AsyncCommands;
use uuid::Uuid;

use crate::models;

pub const FUZZYSEARCH_OWO_QUEUE: &str = "fuzzysearch-owo";

pub fn add_account_job(user_id: Uuid, account_id: Uuid) -> faktory::Job {
    let args = vec![
        serde_json::to_value(user_id).unwrap(),
        serde_json::to_value(account_id).unwrap(),
    ];

    faktory::Job::new("add_account", args).on_queue(FUZZYSEARCH_OWO_QUEUE)
}

pub fn add_furaffinity_submission(
    user_id: Uuid,
    account_id: Uuid,
    submission_id: i32,
    import: bool,
) -> faktory::Job {
    let args = vec![
        serde_json::to_value(user_id).unwrap(),
        serde_json::to_value(account_id).unwrap(),
        serde_json::to_value(submission_id).unwrap(),
        serde_json::to_value(import).unwrap(),
    ];

    faktory::Job::new("add_submission_furaffinity", args).on_queue(FUZZYSEARCH_OWO_QUEUE)
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

    let handle = tokio::runtime::Handle::current();

    let ctx_clone = ctx.clone();
    let handle_clone = handle.clone();
    client.register(
        "add_submission_furaffinity",
        move |job| -> std::io::Result<()> {
            let ctx = ctx_clone.clone();

            handle_clone.block_on(async move {
                tracing::info!("wanting to load fa submission: {:?}", job.args());

                let mut args = job.args().iter();
                let user_id: Uuid = args.next().unwrap().as_str().unwrap().parse().unwrap();
                let account_id: Uuid = args.next().unwrap().as_str().unwrap().parse().unwrap();
                let sub_id = args.next().unwrap().as_i64().unwrap() as i32;
                let was_import = args.next().and_then(|val| val.as_bool()).unwrap_or(false);

                let fa = furaffinity_rs::FurAffinity::new(
                    ctx.config.furaffinity_cookie_a.clone(),
                    ctx.config.furaffinity_cookie_b.clone(),
                    ctx.config.user_agent.clone(),
                    None,
                );

                let sub = fa.get_submission(sub_id).await.unwrap().unwrap();
                let sub = fa.calc_image_hash(sub).await.unwrap();

                let sha256_hash: [u8; 32] = sub.file_sha256.unwrap().try_into().unwrap();

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
                .await
                .unwrap();

                let im = image::load_from_memory(&sub.file.unwrap()).unwrap();
                models::OwnedMediaItem::update_media(&ctx.conn, &ctx.s3, &ctx.config, item_id, im)
                    .await
                    .unwrap();

                if was_import {
                    let mut redis = ctx.redis.clone();
                    let key = format!("account-import-ids:{}", account_id);

                    redis.srem::<_, _, ()>(&key, sub_id).await.unwrap();

                    let remaining: i64 = redis.scard(key).await.unwrap();
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
                        .await
                        .unwrap();
                    }
                }

                tracing::debug!("enqueuing check for previous items");
                ctx.faktory
                    .enqueue(
                        faktory::Job::new(
                            "search_existing_submissions",
                            vec![
                                serde_json::to_value(user_id).unwrap(),
                                serde_json::to_value(item_id).unwrap(),
                            ],
                        )
                        .on_queue(FUZZYSEARCH_OWO_QUEUE),
                    )
                    .await
                    .unwrap();
            });

            Ok(())
        },
    );

    let ctx_clone = ctx.clone();
    let handle_clone = handle.clone();
    client.register(
        "search_existing_submissions",
        move |job| -> std::io::Result<()> {
            let ctx = ctx_clone.clone();

            handle_clone.block_on(async move {
                let mut args = job.args().iter();
                let user_id: Uuid = args.next().unwrap().as_str().unwrap().parse().unwrap();
                let media_id: Uuid = args.next().unwrap().as_str().unwrap().parse().unwrap();

                tracing::info!("performing lookup for previous matches");

                let media = models::OwnedMediaItem::get_by_id(&ctx.conn, media_id, user_id)
                    .await
                    .unwrap()
                    .unwrap();

                let perceptual_hash = match media.perceptual_hash {
                    Some(hash) => hash,
                    None => {
                        tracing::warn!("got media item with no perceptual hash");
                        return;
                    }
                };

                let similar_items = ctx
                    .fuzzysearch
                    .lookup_hashes(&[perceptual_hash], Some(3))
                    .await
                    .unwrap();

                for similar_item in similar_items {
                    let similar_image = models::SimilarImage {
                        site: models::Site::from(
                            similar_item.site_info.as_ref().unwrap().to_owned(),
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
                    .await
                    .unwrap();
                }
            });

            Ok(())
        },
    );

    let ctx_clone = ctx.clone();
    let handle_clone = handle.clone();
    client.register("add_account", move |job| -> std::io::Result<()> {
        let ctx = ctx_clone.clone();

        handle_clone.block_on(async move {
            let args = job.args();
            let mut ids = args
                .iter()
                .filter_map(|arg| arg.as_str())
                .filter_map(|arg| arg.parse::<Uuid>().ok());
            let (user_id, account_id) = (ids.next().unwrap(), ids.next().unwrap());

            tracing::info!("updating loading state of account {:?}", account_id);

            let account = models::LinkedAccount::lookup_by_id(&ctx.conn, account_id, user_id)
                .await
                .unwrap()
                .unwrap();

            models::LinkedAccount::update_loading_state(
                &ctx.conn,
                &ctx.redis,
                user_id,
                account_id,
                models::LoadingState::DiscoveringItems,
            )
            .await
            .unwrap();

            match account.source_site {
                models::SourceSite::FurAffinity => {
                    let ids = discover_furaffinity_submissions(&account.username, &ctx.config)
                        .await
                        .unwrap();
                    let known = ids.len() as i32;

                    let mut redis = ctx.redis.clone();
                    let key = format!("account-import-ids:{}", account_id);
                    redis.sadd::<_, _, ()>(&key, &ids).await.unwrap();
                    redis.expire::<_, ()>(key, 60 * 60 * 24 * 7).await.unwrap();

                    for id in ids {
                        ctx.faktory
                            .enqueue(add_furaffinity_submission(user_id, account_id, id, true))
                            .await
                            .unwrap();
                    }

                    models::LinkedAccount::update_loading_state(
                        &ctx.conn,
                        &ctx.redis,
                        user_id,
                        account_id,
                        models::LoadingState::LoadingItems { known },
                    )
                    .await
                    .unwrap();
                }
            }
        });

        Ok(())
    });

    let ctx_clone = ctx.clone();
    let handle_clone = handle.clone();
    client.register("new_submission", move |job| -> std::io::Result<()> {
        let ctx = ctx_clone.clone();

        handle_clone.block_on(async move {
            let data: fuzzysearch_common::faktory::WebHookData =
                serde_json::from_value(job.args().iter().next().unwrap().to_owned()).unwrap();

            let hash = match data.hash {
                Some(hash) => i64::from_be_bytes(hash),
                None => {
                    tracing::info!("webhook data had no hash");
                    return;
                }
            };

            let similar_items = models::OwnedMediaItem::find_similar(&ctx.conn, hash)
                .await
                .unwrap();

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

            let similar_image = models::SimilarImage {
                site: models::Site::from(data.site),
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
                .await
                .unwrap();
            }
        });

        Ok(())
    });

    tokio::task::spawn_blocking(move || {
        let mut client = client.connect(Some(&ctx.config.faktory_host)).unwrap();

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
    let id_selector = scraper::Selector::parse(".submission-list u a").unwrap();

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
