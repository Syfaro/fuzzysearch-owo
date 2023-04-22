use std::collections::HashSet;

use async_trait::async_trait;
use foxlib::jobs::{FaktoryForge, FaktoryJob, Job, JobExtra};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::jobs::{JobContext, JobInitiator, JobInitiatorExt, Queue, SearchExistingSubmissionsJob};
use crate::models::LinkedAccount;
use crate::site::{CollectedSite, SiteFromConfig};
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
            format!("a={cookie_a}; b={cookie_b}")
                .parse()
                .expect("furaffinity cookies could not be turned into header"),
        );

        let client = reqwest::ClientBuilder::new()
            .user_agent(&user_agent)
            .default_headers(headers)
            .build()?;

        Ok(Self { client })
    }

    async fn discover_submissions(&self, username: &str) -> Result<HashSet<i32>, Error> {
        let (gallery_ids, scrap_ids) = tokio::try_join!(
            self.discover_submissions_page(username, "gallery"),
            self.discover_submissions_page(username, "scraps")
        )?;

        Ok(gallery_ids
            .into_iter()
            .chain(scrap_ids.into_iter())
            .collect())
    }

    async fn discover_submissions_page(
        &self,
        username: &str,
        collection: &str,
    ) -> Result<HashSet<i32>, Error> {
        let id_selector =
            scraper::Selector::parse(".submission-list u a").expect("known good selector failed");

        let mut ids = HashSet::new();

        let mut page = 1;
        loop {
            tracing::info!(collection, page, "loading page");

            let body = self
                .client
                .get(format!(
                    "https://www.furaffinity.net/{collection}/{username}/{page}/",
                ))
                .send()
                .await?
                .text()
                .await?;

            let body = scraper::Html::parse_document(&body);

            let mut new_ids = body
                .select(&id_selector)
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

    fn register_jobs(&self, forge: &mut FaktoryForge<jobs::JobContext, Error>) {
        AddSubmissionFurAffinityJob::register(forge, add_submission_furaffinity);
        RefreshFurAffinityJob::register(forge, refresh_furaffinity);
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
            &ctx.producer,
            account.owner_id,
            account.id,
            ids,
            |user_id, account_id, id| AddSubmissionFurAffinityJob {
                user_id,
                account_id,
                sub_id: id,
                was_import: true,
            },
        )
        .await?;

        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
pub struct AddSubmissionFurAffinityJob {
    pub user_id: Uuid,
    pub account_id: Uuid,
    pub sub_id: i32,
    pub was_import: bool,
}

impl Job for AddSubmissionFurAffinityJob {
    const NAME: &'static str = "add_submission_furaffinity";
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

#[tracing::instrument(skip(ctx, job), fields(job_id = job.id()))]
async fn add_submission_furaffinity(
    ctx: JobContext,
    job: FaktoryJob,
    AddSubmissionFurAffinityJob {
        user_id,
        account_id,
        sub_id,
        was_import,
    }: AddSubmissionFurAffinityJob,
) -> Result<(), Error> {
    let fa = furaffinity_rs::FurAffinity::new(
        ctx.config.furaffinity_cookie_a.clone(),
        ctx.config.furaffinity_cookie_b.clone(),
        ctx.config.user_agent.clone(),
        Some(ctx.client.clone()),
    );

    match process_submission(&ctx, &fa, user_id, account_id, sub_id).await {
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
    fa: &furaffinity_rs::FurAffinity,
    user_id: Uuid,
    account_id: Uuid,
    sub_id: i32,
) -> Result<(), Error> {
    if models::OwnedMediaItem::lookup_by_site_id(
        &ctx.conn,
        user_id,
        models::Site::FurAffinity,
        &sub_id.to_string(),
    )
    .await?
    .is_some()
    {
        tracing::info!("submission was already loaded, skipping");
        return Ok(());
    }

    let sub = fa
        .get_submission(sub_id)
        .await
        .map_err(|_err| {
            Error::LoadingError(format!("Could not load FurAffinity submission {sub_id}"))
        })?
        .ok_or(Error::Missing)?;

    if sub.content.url().contains("/stories/") || sub.content.url().contains("/music/") {
        tracing::debug!("submission was story or music, skipping");
        return Ok(());
    }

    let sub = fa.calc_image_hash(sub).await.map_err(|_err| {
        Error::LoadingError(format!(
            "Could not load FurAffinity submission content {sub_id}",
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
        Some(format!("https://www.furaffinity.net/view/{sub_id}/")),
        Some(sub.title),
        Some(sub.posted_at),
    )
    .await?;

    let im = image::load_from_memory(&sub.file.ok_or(Error::Missing)?)?;
    models::OwnedMediaItem::update_media(&ctx.conn, &ctx.s3, &ctx.config, item_id, im).await?;

    ctx.producer
        .enqueue_job(
            SearchExistingSubmissionsJob {
                user_id,
                media_id: item_id,
            }
            .initiated_by(JobInitiator::user(user_id)),
        )
        .await?;

    Ok(())
}

#[derive(Deserialize, Serialize)]
pub struct RefreshFurAffinityJob {
    pub account_id: Uuid,
}

impl Job for RefreshFurAffinityJob {
    const NAME: &'static str = "refresh_furaffinity";
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

#[tracing::instrument(skip(ctx, job), fields(job_id = job.id()))]
async fn refresh_furaffinity(
    ctx: JobContext,
    job: FaktoryJob,
    RefreshFurAffinityJob { account_id }: RefreshFurAffinityJob,
) -> Result<(), Error> {
    let account = match models::LinkedAccount::lookup_by_id(&ctx.conn, account_id).await? {
        Some(account) => account,
        None => return Err(Error::Missing),
    };

    if account.source_site != models::Site::FurAffinity {
        tracing::warn!("attempted to refresh non-furaffinity account");
        return Ok(());
    }

    let fa = FurAffinity::site_from_config(&ctx.config).await?;
    let submission_ids = fa.discover_submissions(&account.username).await?;

    super::queue_new_submissions(
        &ctx.producer,
        account.owner_id,
        account.id,
        submission_ids,
        |user_id, account_id, id| AddSubmissionFurAffinityJob {
            user_id,
            account_id,
            sub_id: id,
            was_import: false,
        },
    )
    .await?;

    Ok(())
}
