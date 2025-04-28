use async_trait::async_trait;
use foxlib::jobs::{FaktoryForge, FaktoryJob, Job, JobExtra};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use super::{SiteFromConfig, WatchedSite};
use crate::{
    Error,
    jobs::{
        self, JobContext, JobInitiator, JobInitiatorExt, NatsNewImage, NewSubmissionJob, Queue,
    },
    models,
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
    fn register_jobs(&self, forge: &mut FaktoryForge<jobs::JobContext, Error>) {
        CheckSubredditsJob::register(forge, check_subreddits);
        UpdateSubredditJob::register(forge, update_subreddit);
        LoadRedditPostJob::register(forge, load_post);
    }
}

pub struct CheckSubredditsJob;

impl Job for CheckSubredditsJob {
    const NAME: &'static str = "reddit_check";
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

pub struct UpdateSubredditJob<'a>(pub &'a str);

impl Job for UpdateSubredditJob<'_> {
    const NAME: &'static str = "reddit_subreddit";
    type Data = String;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::OutgoingBulk
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

#[derive(Deserialize, Serialize)]
pub struct LoadRedditPostJob {
    pub subreddit_name: String,
    pub post: types::RedditPost,
}

impl Job for LoadRedditPostJob {
    const NAME: &'static str = "reddit_post";
    type Data = Self;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::OutgoingBulk
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

async fn check_subreddits(ctx: JobContext, _job: FaktoryJob, _args: ()) -> Result<(), Error> {
    let subs = models::RedditSubreddit::needing_update(&ctx.conn).await?;

    for sub in subs {
        tracing::info!("queueing subreddit {}", sub.name);

        let mut job = UpdateSubredditJob(&sub.name)
            .initiated_by(JobInitiator::Schedule)
            .job()?;
        job.retry = Some(0);
        ctx.producer.enqueue_existing_job(job).await?;
    }

    Ok(())
}

async fn update_subreddit(ctx: JobContext, _job: FaktoryJob, name: String) -> Result<(), Error> {
    let data = models::RedditSubreddit::get_by_name(&ctx.conn, &name)
        .await?
        .ok_or(Error::Missing)?;

    if matches!(data.last_updated, Some(last_updated) if chrono::Utc::now() < last_updated + chrono::Duration::try_minutes(15).unwrap())
    {
        tracing::info!("updated within last 15 minutes, skipping");

        return Ok(());
    }

    let sub = roux::Reddit::new(
        &ctx.config.user_agent,
        &ctx.config.reddit_client_id,
        &ctx.config.reddit_client_secret,
    )
    .username(&ctx.config.reddit_username)
    .password(&ctx.config.reddit_password)
    .subreddit(&name)
    .await?;

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
                    limit: None,
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
            if models::RedditPost::exists(&ctx.conn, &post.data.name).await? {
                tracing::info!("post had already been loaded, skipping");
                continue;
            }

            ctx.producer
                .enqueue_job(
                    LoadRedditPostJob {
                        subreddit_name: data.name.clone(),
                        post: types::RedditPost::from(post.data),
                    }
                    .initiated_by(JobInitiator::external("reddit")),
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

    models::RedditSubreddit::update_position(&ctx.conn, &sub.name, &last_page).await?;

    Ok(())
}

async fn load_post(
    ctx: JobContext,
    _job: FaktoryJob,
    LoadRedditPostJob {
        subreddit_name,
        post,
    }: LoadRedditPostJob,
) -> Result<(), Error> {
    if models::RedditPost::exists(&ctx.conn, &post.id).await? {
        tracing::info!("post had already been loaded, skipping");
        return Ok(());
    }

    tracing::info!("wanting to load post: {:?}", post);

    models::RedditPost::create(
        &ctx.conn,
        models::RedditPost {
            fullname: post.id.clone(),
            subreddit_name,
            posted_at: post.posted_at,
            author: post.author.clone(),
            permalink: post.permalink.clone(),
            content_link: post.url.clone().unwrap_or_else(|| post.permalink.clone()),
        },
    )
    .await?;

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

        Some(hash)
    } else {
        tracing::warn!("could not calculate perceptual hash");

        None
    };

    let new_image_id =
        models::RedditImage::create(&ctx.conn, &post.id, data.len() as i32, sha256, hash).await?;

    if let Some(id) = new_image_id {
        let data = jobs::IncomingSubmission {
            site: models::Site::Reddit,
            site_id: id.to_string(),
            page_url: Some(post.permalink.clone()),
            posted_by: Some(post.author.clone()),
            sha256: Some(sha256),
            perceptual_hash: hash,
            content_url: url.clone(),
            posted_at: None,
        };

        ctx.producer
            .enqueue_job(NewSubmissionJob(data).initiated_by(JobInitiator::external("reddit")))
            .await?;
    }

    ctx.nats_new_image(NatsNewImage {
        site: jobs::NatsSite::Reddit,
        image_url: url,
        page_url: Some(post.permalink),
        posted_by: Some(post.author),
        perceptual_hash: hash,
        sha256_hash: Some(sha256),
    })
    .await;

    Ok(())
}

pub async fn limited_image_download(
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

    Ok((sha.finalize().into(), buf.freeze()))
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

    impl From<roux::submission::SubmissionData> for RedditPost {
        fn from(data: roux::submission::SubmissionData) -> Self {
            Self {
                id: data.name,
                author: data.author,
                url: data.url,
                posted_at: chrono::Utc
                    .timestamp_opt(data.created_utc as i64, 0)
                    .unwrap(),
                permalink: format!("https://www.reddit.com{}", data.permalink),
            }
        }
    }
}
