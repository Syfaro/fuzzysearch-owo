use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    jobs::{self, search_existing_submissions_job, JobContext, JobInitiator},
    models::{self, LinkedAccount},
    site::{CollectedSite, SiteFromConfig, SiteJob},
    Config, Error,
};

#[derive(Debug, Deserialize, Serialize)]
pub struct WeasylMediaEntry {
    #[serde(rename = "mediaid")]
    id: i32,
    url: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WeasylMedia {
    submission: Option<Vec<WeasylMediaEntry>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WeasylSubmission {
    #[serde(rename = "submitid")]
    id: i32,
    title: String,
    posted_at: chrono::DateTime<chrono::Utc>,
    media: WeasylMedia,
    link: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct WeasylSubmissionPage {
    backid: Option<i32>,
    nextid: Option<i32>,
    submissions: Vec<WeasylSubmission>,
}

pub struct Weasyl {
    client: reqwest::Client,
}

impl Weasyl {
    async fn discover_submissions(&self, username: &str) -> Result<Vec<WeasylSubmission>, Error> {
        let url = format!("https://www.weasyl.com/api/users/{}/gallery", username);

        let mut subs = Vec::new();
        let mut nextid = None;

        loop {
            let mut query = Vec::with_capacity(1);
            if let Some(nextid) = nextid {
                query.push(("nextid", nextid));
            }

            let page: WeasylSubmissionPage = self
                .client
                .get(&url)
                .query(&query)
                .send()
                .await?
                .json()
                .await?;

            if page.submissions.is_empty() {
                break;
            }

            subs.extend(page.submissions);

            match page.nextid {
                Some(id) => nextid = Some(id),
                None => break,
            }
        }

        Ok(subs)
    }
}

#[async_trait(?Send)]
impl SiteFromConfig for Weasyl {
    async fn site_from_config(config: &Config) -> Result<Self, Error> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-weasyl-api-key",
            config
                .weasyl_api_token
                .parse()
                .expect("weasyl api token could not be turned into header"),
        );

        let client = reqwest::ClientBuilder::new()
            .default_headers(headers)
            .build()?;

        Ok(Self { client })
    }
}

#[async_trait(?Send)]
impl CollectedSite for Weasyl {
    fn oauth_page(&self) -> Option<&'static str> {
        None
    }

    fn jobs(&self) -> HashMap<&'static str, SiteJob> {
        [(
            jobs::job::ADD_SUBMISSION_WEASYL,
            super::wrap_job(add_submission_weasyl),
        )]
        .into_iter()
        .collect()
    }

    #[tracing::instrument(skip(self, ctx, account), fields(user_id = %account.owner_id, account_id = %account.id))]
    async fn add_account(&self, ctx: &JobContext, account: LinkedAccount) -> Result<(), Error> {
        models::LinkedAccount::update_loading_state(
            &ctx.conn,
            &ctx.redis,
            account.owner_id,
            account.id,
            models::LoadingState::DiscoveringItems,
        )
        .await?;

        let subs = self.discover_submissions(&account.username).await?;

        let known = subs.len() as i32;
        tracing::info!("discovered {} submissions", known);

        let mut redis = ctx.redis.clone();

        super::set_loading_submissions(
            &ctx.conn,
            &mut redis,
            account.owner_id,
            account.id,
            subs.iter().map(|sub| sub.id),
        )
        .await?;

        super::queue_new_submissions(
            &ctx.faktory,
            account.owner_id,
            account.id,
            subs,
            |user_id, account_id, sub| {
                jobs::add_weasyl_submission_job(user_id, account_id, sub, true)
            },
        )
        .await?;

        Ok(())
    }
}

#[tracing::instrument(skip(ctx, job), fields(job_id = job.id()))]
async fn add_submission_weasyl(ctx: Arc<JobContext>, job: faktory::Job) -> Result<(), Error> {
    let mut args = job.args().iter();
    let (user_id, account_id, sub, was_import) =
        crate::extract_args!(args, Uuid, Uuid, WeasylSubmission, bool);

    let sub_id = sub.id;

    match process_submission(&ctx, sub, user_id, account_id).await {
        Ok(()) => (),
        Err(Error::Missing) => tracing::warn!("submission was missing"),
        Err(err) => {
            tracing::warn!("could not load submission: {}", err);
            return Err(err);
        }
    }

    if was_import {
        let mut redis = ctx.redis.clone();
        super::update_import_progress(&ctx.conn, &mut redis, user_id, account_id, sub_id).await?;
    }

    Ok(())
}

async fn process_submission(
    ctx: &JobContext,
    sub: WeasylSubmission,
    user_id: Uuid,
    account_id: Uuid,
) -> Result<(), Error> {
    for media in sub.media.submission.unwrap_or_default() {
        let image_data = ctx.client.get(&media.url).send().await?.bytes().await?;

        let mut sha256 = Sha256::new();
        sha256.update(&image_data);
        let sha256: [u8; 32] = sha256
            .finalize()
            .try_into()
            .expect("sha256 was wrong length");

        let (im, perceptual_hash) = if let Ok(im) = image::load_from_memory(&image_data) {
            let hasher = fuzzysearch_common::get_hasher();
            let hash: [u8; 8] = hasher
                .hash_image(&im)
                .as_bytes()
                .try_into()
                .expect("perceptual hash was wrong length");
            let perceptual_hash = i64::from_be_bytes(hash);

            (Some(im), Some(perceptual_hash))
        } else {
            (None, None)
        };

        let item_id = models::OwnedMediaItem::add_item(
            &ctx.conn,
            user_id,
            account_id,
            format!("{}-{}", sub.id, media.id),
            perceptual_hash,
            sha256,
            Some(sub.link.clone()),
            Some(sub.title.clone()),
            Some(sub.posted_at),
        )
        .await?;

        if let Some(im) = im {
            models::OwnedMediaItem::update_media(&ctx.conn, &ctx.s3, &ctx.config, item_id, im)
                .await?;

            ctx.faktory
                .enqueue_job(
                    JobInitiator::User { user_id },
                    search_existing_submissions_job(user_id, item_id)?,
                )
                .await?;
        }
    }

    Ok(())
}
