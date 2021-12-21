use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::jobs::{JobContext, JobInitiator};
use crate::models::LinkedAccount;
use crate::site::{CollectedSite, SiteFromConfig, SiteJob};
use crate::{jobs, models, Config, Error};

pub struct FurAffinity {
    client: reqwest::Client,
}

impl FurAffinity {
    pub fn new<S>(cookie_a: S, cookie_b: S, user_agent: S) -> Result<Self, Error>
    where
        S: Into<String>,
    {
        let (cookie_a, cookie_b, user_agent) =
            (cookie_a.into(), cookie_b.into(), user_agent.into());

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::COOKIE,
            format!("a={}; b={}", cookie_a, cookie_b)
                .parse()
                .expect("furaffinity cookies could not be turned into header"),
        );

        let client = reqwest::ClientBuilder::new()
            .user_agent(&user_agent)
            .default_headers(headers)
            .build()?;

        Ok(Self { client })
    }

    async fn discover_submissions(&self, username: &str) -> Result<Vec<i32>, Error> {
        // TODO: handle scraps?

        let id_selector =
            scraper::Selector::parse(".submission-list u a").expect("known good selector failed");

        let mut ids = Vec::new();

        let mut page = 1;
        loop {
            tracing::info!(page, "loading gallery page");

            let body = self
                .client
                .get(format!(
                    "https://www.furaffinity.net/gallery/{}/{}/",
                    username, page
                ))
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
                tracing::debug!("no new ids found");

                break;
            }

            ids.extend(new_ids);
            page += 1;
        }

        Ok(ids)
    }
}

#[async_trait(?Send)]
impl SiteFromConfig for FurAffinity {
    async fn site_from_config(config: &Config) -> Result<Self, Error> {
        FurAffinity::new(
            &config.furaffinity_cookie_a,
            &config.furaffinity_cookie_b,
            &config.user_agent,
        )
    }
}

#[async_trait(?Send)]
impl CollectedSite for FurAffinity {
    fn oauth_page(&self) -> Option<&'static str> {
        None
    }

    fn jobs(&self) -> HashMap<&'static str, SiteJob> {
        [(
            jobs::job::ADD_SUBMISSION_FURAFFINITY,
            Box::new(_add_submission_furaffinity) as SiteJob,
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

        let ids = self.discover_submissions(&account.username).await?;

        let known = ids.len() as i32;
        tracing::info!("discovered {} submissions", known);

        let mut redis = ctx.redis.clone();

        super::set_loading_submissions(
            &ctx.conn,
            &mut redis,
            account.owner_id,
            account.id,
            ids.iter(),
        )
        .await?;

        super::queue_new_submissions(
            &ctx.faktory,
            account.owner_id,
            account.id,
            ids,
            |user_id, account_id, id| {
                jobs::add_furaffinity_submission_job(user_id, account_id, id, true)
            },
        )
        .await?;

        Ok(())
    }
}

fn _add_submission_furaffinity(
    ctx: Arc<JobContext>,
    job: faktory::Job,
) -> Pin<Box<dyn Future<Output = Result<(), Error>>>> {
    Box::pin(add_submission_furaffinity(ctx, job))
}

#[tracing::instrument(skip(ctx, job), fields(job_id = job.id()))]
async fn add_submission_furaffinity(ctx: Arc<JobContext>, job: faktory::Job) -> Result<(), Error> {
    let mut args = job.args().iter();
    let (user_id, account_id, sub_id, was_import) =
        crate::extract_args!(args, Uuid, Uuid, i32, bool);

    let fa = furaffinity_rs::FurAffinity::new(
        ctx.config.furaffinity_cookie_a.clone(),
        ctx.config.furaffinity_cookie_b.clone(),
        ctx.config.user_agent.clone(),
        Some(ctx.client.clone()),
    );

    let sub = fa
        .get_submission(sub_id)
        .await
        .map_err(|_err| {
            Error::LoadingError(format!("Could not load FurAffinity submission {}", sub_id))
        })?
        .ok_or(Error::Missing)?;

    if sub.content.url().contains("/stories/") {
        tracing::debug!("submission was story, skipping");
    } else {
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
        models::OwnedMediaItem::update_media(&ctx.conn, &ctx.s3, &ctx.config, item_id, im).await?;

        ctx.faktory
            .enqueue_job(
                JobInitiator::User { user_id },
                jobs::search_existing_submissions_job(user_id, item_id)?,
            )
            .await?;
    }

    if was_import {
        let mut redis = ctx.redis.clone();
        super::update_import_progress(&ctx.conn, &mut redis, user_id, account_id, sub_id).await?;
    }

    Ok(())
}
