use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use futures::TryStreamExt;
use sha2::Digest;

use super::{SiteFromConfig, SiteJob, WatchedSite};
use crate::{
    jobs::{self, JobContext},
    models, Error,
};

pub struct Reddit {}

impl Reddit {
    fn new() -> Self {
        Self {}
    }
}

#[async_trait(?Send)]
impl SiteFromConfig for Reddit {
    async fn site_from_config(_config: &crate::Config) -> Result<Self, Error> {
        Ok(Reddit::new())
    }
}

#[async_trait(?Send)]
impl WatchedSite for Reddit {
    fn jobs(&self) -> HashMap<&'static str, SiteJob> {
        [
            (
                jobs::job::REDDIT_CHECK_SUBREDDITS,
                super::wrap_job(check_subreddits),
            ),
            (
                jobs::job::REDDIT_UPDATE_SUBREDDIT,
                super::wrap_job(update_subreddit),
            ),
            (jobs::job::REDDIT_LOAD_POST, super::wrap_job(load_post)),
        ]
        .into_iter()
        .collect()
    }
}

async fn check_subreddits(ctx: Arc<JobContext>, _job: faktory::Job) -> Result<(), Error> {
    let mut stream = sqlx::query!("SELECT name FROM reddit_subreddit WHERE last_updated IS NULL OR last_updated < now() + interval '15 minutes'").fetch(&ctx.conn);

    while let Some(sub) = stream.try_next().await? {
        tracing::info!("queueing subreddit {}", sub.name);

        ctx.faktory
            .enqueue_job(
                jobs::JobInitiator::Schedule,
                jobs::reddit_update_subreddit_job(&sub.name),
            )
            .await?;
    }

    Ok(())
}

async fn update_subreddit(ctx: Arc<JobContext>, job: faktory::Job) -> Result<(), Error> {
    let mut args = job.args().iter();
    let (name,) = crate::extract_args!(args, String);

    let data = sqlx::query!("SELECT * FROM reddit_subreddit WHERE name = $1", name)
        .fetch_one(&ctx.conn)
        .await?;

    if matches!(data.last_updated, Some(last_updated) if chrono::Utc::now() < last_updated + chrono::Duration::minutes(15))
    {
        tracing::info!("updated within last 15 minutes, skipping");

        return Ok(());
    }

    let sub = roux::Subreddit::new(&name);

    let previous_max = data
        .last_page
        .and_then(|prev| i64::from_str_radix(&prev, 36).ok())
        .unwrap_or_default();

    let mut max_id = 0;
    let mut after = None;

    loop {
        tracing::debug!("loading subreddit with after {:?}", after);

        let posts = sub
            .latest(
                100,
                Some(roux::util::FeedOption {
                    after: after.clone(),
                    before: None,
                    count: None,
                    period: None,
                }),
            )
            .await
            .map_err(Error::from_displayable)?;

        if posts.data.children.is_empty() {
            tracing::info!("loaded all new posts, ending");
            break;
        }

        let ids = posts
            .data
            .children
            .iter()
            .map(|post| &post.data.id)
            .flat_map(|id| i64::from_str_radix(id, 36).ok());

        let min_id = ids.clone().min().unwrap_or_default();

        let new_max_id = ids.max().unwrap_or_default();
        if new_max_id > max_id {
            max_id = new_max_id;
        }

        for post in posts.data.children {
            ctx.faktory
                .enqueue_job(
                    jobs::JobInitiator::external("reddit"),
                    jobs::reddit_post_job(&data.name, types::RedditPost::from(post.data))?,
                )
                .await?;
        }

        if posts.data.after.is_none() || min_id < previous_max {
            tracing::info!("no more posts or min id is greater than previous run, ending");
            break;
        }

        after = posts.data.after;
    }

    tracing::debug!("setting max_id to {}", max_id);
    let last_page = radix_fmt::radix(max_id, 36).to_string();

    sqlx::query!("UPDATE reddit_subreddit SET last_updated = current_timestamp, last_page = $2 WHERE name = $1", sub.name, last_page).execute(&ctx.conn).await?;

    Ok(())
}

async fn load_post(ctx: Arc<JobContext>, job: faktory::Job) -> Result<(), Error> {
    let mut args = job.args().iter();
    let (subreddit_name, post) = crate::extract_args!(args, String, types::RedditPost);

    tracing::info!("wanting to load post: {:?}", post);

    let url = match post.url {
        Some(url) => url,
        None => {
            tracing::warn!("post had no url, skipping");
            return Ok(());
        }
    };

    let (sha256, data) = match limited_image_download(&ctx.client, &url, 50 * 1024 * 1024).await {
        Ok(data) => data,
        Err(err) => {
            tracing::warn!("post did not appear to be image: {:?}", err);
            return Ok(());
        }
    };

    let hash = if let Ok(im) = image::load_from_memory(&data) {
        let hasher = fuzzysearch_common::get_hasher();
        let hash: [u8; 8] = hasher
            .hash_image(&im)
            .as_bytes()
            .try_into()
            .expect("perceptual hash was wrong length");

        Some(i64::from_be_bytes(hash))
    } else {
        tracing::warn!("could not calculate perceptual hash");

        None
    };

    sqlx::query!("INSERT INTO reddit_post (fullname, subreddit_name, permalink, author, content_link, posted_at) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING", post.id, subreddit_name, post.permalink, post.author, url, post.posted_at).execute(&ctx.conn).await?;

    let id = sqlx::query_scalar!("INSERT INTO reddit_image (post_fullname, size, sha256, perceptual_hash) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING RETURNING id", post.id, data.len() as i32, sha256.to_vec(), hash).fetch_optional(&ctx.conn).await?;

    if let Some(id) = id {
        let data = jobs::IncomingSubmission {
            site: models::Site::Reddit,
            site_id: id.to_string(),
            page_url: Some(post.permalink),
            posted_by: Some(post.author),
            sha256: Some(sha256),
            perceptual_hash: hash.map(|hash| hash.to_be_bytes()),
            content_url: url,
            posted_at: None,
        };

        ctx.faktory
            .enqueue_job(
                jobs::JobInitiator::external("reddit"),
                jobs::new_submission_job(data)?,
            )
            .await?;
    }

    Ok(())
}

async fn limited_image_download(
    client: &reqwest::Client,
    url: &str,
    max_download: usize,
) -> Result<([u8; 32], bytes::Bytes), Error> {
    let mut data = client.get(url).send().await?;

    let mut buf = bytes::BytesMut::new();
    let mut checked_image = false;

    let mut sha = sha2::Sha256::new();

    while let Some(chunk) = data.chunk().await? {
        if buf.len() + chunk.len() > max_download {
            return Err(Error::unknown_message("content too large"));
        }

        sha.update(&chunk);
        buf.extend(chunk);

        if !checked_image && buf.len() > 8_192 {
            if !infer::is_image(&buf) {
                return Err(Error::unknown_message("content not image"));
            }

            checked_image = true;
        }
    }

    if !checked_image {
        return Err(Error::unknown_message("content too short to check type"));
    }

    Ok((
        sha.finalize().try_into().expect("sha256 was wrong length"),
        buf.freeze(),
    ))
}

pub mod types {
    use chrono::TimeZone;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, Serialize)]
    pub struct RedditPost {
        pub id: String,
        pub author: String,
        pub url: Option<String>,
        pub posted_at: chrono::DateTime<chrono::Utc>,
        pub permalink: String,
    }

    impl From<roux::subreddit::responses::SubmissionsData> for RedditPost {
        fn from(data: roux::subreddit::responses::SubmissionsData) -> Self {
            Self {
                id: data.name,
                author: data.author,
                url: data.url,
                posted_at: chrono::Utc.timestamp(data.created_utc as i64, 0),
                permalink: format!("https://www.reddit.com{}", data.permalink),
            }
        }
    }
}
