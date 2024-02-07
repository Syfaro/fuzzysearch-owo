use std::collections::HashSet;

use async_trait::async_trait;
use foxlib::jobs::{FaktoryForge, FaktoryJob, Job, JobExtra};
use sha2::Digest;

use crate::jobs::{
    self, JobContext, JobInitiator, JobInitiatorExt, NatsNewImage, NewSubmissionJob, Queue,
};
use crate::site::{SiteFromConfig, WatchedSite};
use crate::{models, Config, Error};

pub struct FList;

impl FList {
    pub async fn new() -> Result<Self, Error> {
        Ok(FList)
    }
}

#[async_trait(?Send)]
impl SiteFromConfig for FList {
    async fn site_from_config(_config: &Config) -> Result<Self, Error> {
        FList::new().await
    }
}

#[async_trait(?Send)]
impl WatchedSite for FList {
    fn register_jobs(&self, forge: &mut FaktoryForge<jobs::JobContext, Error>) {
        CollectGalleryImagesJob::register(forge, collect_gallery_images);
        HashImageJob::register(forge, hash_image);
    }
}

pub struct CollectGalleryImagesJob;

impl Job for CollectGalleryImagesJob {
    const NAME: &'static str = "flist_gallery";
    type Data = ();
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Outgoing
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

pub struct HashImageJob(i32);

impl Job for HashImageJob {
    const NAME: &'static str = "flist_hash";
    type Data = i32;
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

trait FListAuth {
    fn inject_auth(self, auth: &str) -> Self;
}

impl FListAuth for reqwest::RequestBuilder {
    fn inject_auth(self, auth: &str) -> Self {
        self.header("Cookie", format!("FLS={auth}"))
    }
}

#[derive(Debug)]
struct FListGalleryItem {
    id: i32,
    ext: String,
    character_name: String,
}

struct FListClient {
    client: reqwest::Client,
    csrf_token: scraper::Selector,
    gallery_item: scraper::Selector,
    image_url: regex::Regex,
    character_url: regex::Regex,

    fls_cookie: Option<String>,
}

impl FListClient {
    fn new(user_agent: &str) -> Self {
        let client = reqwest::ClientBuilder::default()
            .user_agent(user_agent)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("could not create flist client");

        let gallery_image = scraper::Selector::parse("#Content > div a").unwrap();
        let csrf_token = scraper::Selector::parse("input[name=csrf_token]").unwrap();
        let image_url =
            regex::Regex::new(r"https://static.f-list.net/images/charthumb/(\d+).(.{3})").unwrap();
        let character_url = regex::Regex::new(r"https://www.f-list.net/c/(.+)/").unwrap();

        Self {
            client,
            csrf_token,
            gallery_item: gallery_image,
            image_url,
            character_url,

            fls_cookie: None,
        }
    }

    async fn sign_in(&mut self, username: &str, password: &str) -> Result<(), Error> {
        let body = self
            .client
            .get("https://www.f-list.net/index.php")
            .send()
            .await?
            .text()
            .await?;
        let document = scraper::Html::parse_document(&body);

        let token = document
            .select(&self.csrf_token)
            .next()
            .ok_or_else(|| Error::unknown_message("missing csrf token element"))?
            .value()
            .attr("value")
            .ok_or_else(|| Error::unknown_message("missing csrf token value"))?;

        let resp = self
            .client
            .post("https://www.f-list.net/action/script_login.php")
            .form(&[
                ("csrf_token", token),
                ("username", username),
                ("password", password),
            ])
            .send()
            .await?;

        let cookie = resp
            .cookies()
            .find(|cookie| cookie.name() == "FLS")
            .ok_or_else(|| Error::unknown_message("missing fls cookie"))?;

        self.fls_cookie = Some(cookie.value().to_string());

        Ok(())
    }

    async fn get_latest_gallery_items(
        &self,
        offset: Option<i32>,
    ) -> Result<Vec<FListGalleryItem>, Error> {
        // F-List only returns first 10,000 items
        if offset.unwrap_or(0) > 10_000 {
            return Ok(Vec::new());
        }

        let auth = self
            .fls_cookie
            .as_deref()
            .ok_or_else(|| Error::unknown_message("getting gallery without login"))?;
        let body = self
            .client
            .get("https://www.f-list.net/experimental/gallery.php")
            .query(&[("offset", offset.unwrap_or(0).to_string())])
            .inject_auth(auth)
            .send()
            .await?
            .text()
            .await?;

        let document = scraper::Html::parse_document(&body);

        let items = document
            .select(&self.gallery_item)
            .flat_map(|elem| self.extract_gallery_item(elem))
            .collect();

        Ok(items)
    }

    fn extract_gallery_item(&self, elem: scraper::ElementRef) -> Option<FListGalleryItem> {
        let link = self.character_url.captures(elem.value().attr("href")?)?;
        let character_name = link[1].to_string();

        let src = elem.children().next()?.value().as_element()?.attr("src")?;
        let img = self.image_url.captures(src)?;
        let id: i32 = img[1].parse().ok()?;
        let ext = img[2].to_string();

        Some(FListGalleryItem {
            id,
            ext,
            character_name,
        })
    }
}

async fn collect_gallery_images(ctx: JobContext, _job: FaktoryJob, _args: ()) -> Result<(), Error> {
    let previous_run = models::FListImportRun::previous_run(&ctx.conn).await?;
    let (previous_max, resume_run_id) = match previous_run {
        Some(run)
            if run.finished_at.is_none()
                && chrono::Utc::now().signed_duration_since(run.started_at)
                    >= chrono::Duration::minutes(30) =>
        {
            tracing::warn!("previous run never finished, restarting");
            (run.starting_id, Some(run.id))
        }
        Some(run) if run.finished_at.is_none() => {
            tracing::info!("previous run has not yet finished, skipping");
            return Ok(());
        }
        Some(run) => (run.max_id.unwrap_or(0), None),
        None => (0, None),
    };

    let id = match resume_run_id {
        Some(resumed_id) => resumed_id,
        None => models::FListImportRun::start(&ctx.conn, previous_max + 1).await?,
    };

    let mut flist = FListClient::new(&ctx.config.user_agent);
    flist
        .sign_in(&ctx.config.flist_username, &ctx.config.flist_password)
        .await?;

    let mut offset = None;
    let mut max_id = previous_max;
    loop {
        tracing::info!("loading flist gallery with offset {:?}", offset);

        let items = flist.get_latest_gallery_items(offset).await?;
        if items.is_empty() {
            tracing::info!("found no new items, ending");
            break;
        }

        let mut tx = ctx.conn.begin().await?;

        for item in items.iter() {
            models::FListFile::insert_item(&mut tx, item.id, &item.ext, &item.character_name)
                .await?;
        }

        tx.commit().await?;

        let ids: HashSet<_> = items.iter().map(|item| item.id).collect();
        let max = ids.iter().copied().max().unwrap_or(0);

        // Only enqueue jobs after changes have been committed
        for id in ids.iter().copied() {
            ctx.producer
                .enqueue_job(HashImageJob(id).initiated_by(JobInitiator::external("flist")))
                .await?;
        }

        if max_id < max {
            max_id = max;
        }

        if ids.iter().copied().min().unwrap_or(0) < previous_max {
            tracing::info!("found value less than previous max, ending");
            break;
        }

        offset = Some(offset.unwrap_or(0) + items.len() as i32);
    }

    models::FListImportRun::complete(&ctx.conn, id, max_id).await?;

    Ok(())
}

async fn hash_image(ctx: JobContext, _job: FaktoryJob, id: i32) -> Result<(), Error> {
    let file = models::FListFile::get_by_id(&ctx.conn, id)
        .await?
        .ok_or(Error::Missing)?;

    if file.sha256.is_some() {
        tracing::info!("file already had hash, skipping");

        return Ok(());
    }

    let url = format!(
        "https://static.f-list.net/images/charimage/{}.{}",
        file.id, file.ext
    );
    let bytes = ctx.client.get(&url).send().await?.bytes().await?;

    let mut sha256 = sha2::Sha256::new();
    sha256.update(&bytes);
    let sha256: [u8; 32] = sha256.finalize().into();
    let size = bytes.len();

    let perceptual_hash = tokio::task::spawn_blocking(move || -> Option<i64> {
        let im = image::load_from_memory(&bytes).ok()?;

        let hasher = fuzzysearch_common::get_hasher();
        let bytes = hasher.hash_image(&im).as_bytes().try_into().ok()?;
        Some(i64::from_be_bytes(bytes))
    })
    .await
    .map_err(Error::from_displayable)?;

    models::FListFile::update(&ctx.conn, id, size as i32, sha256.to_vec(), perceptual_hash).await?;

    let character_name = percent_encoding::utf8_percent_encode(
        &file.character_name,
        percent_encoding::NON_ALPHANUMERIC,
    );
    let page_url = Some(format!("https://www.f-list.net/c/{character_name}/"));

    let data = jobs::IncomingSubmission {
        site: models::Site::FList,
        site_id: id.to_string(),
        page_url: page_url.clone(),
        posted_by: Some(file.character_name.clone()),
        sha256: Some(sha256),
        perceptual_hash: perceptual_hash.map(|hash| hash.to_be_bytes()),
        content_url: url.clone(),
        posted_at: None,
    };

    ctx.nats_new_image(NatsNewImage {
        site: jobs::NatsSite::FList,
        image_url: url,
        page_url,
        posted_by: Some(file.character_name),
        perceptual_hash: perceptual_hash.map(|hash| hash.to_be_bytes()),
        sha256_hash: Some(sha256),
    })
    .await;

    let mut job = NewSubmissionJob(data)
        .initiated_by(JobInitiator::external("flist"))
        .job()?;
    job.retry = Some(0);
    ctx.producer.enqueue_existing_job(job).await?;

    Ok(())
}
