use crate::Error;

trait FListAuth {
    fn inject_auth(self, auth: &str) -> Self;
}

impl FListAuth for reqwest::RequestBuilder {
    fn inject_auth(self, auth: &str) -> Self {
        self.header("Cookie", format!("FLS={}", auth))
    }
}

pub struct FList {
    client: reqwest::Client,
    csrf_token: scraper::Selector,
    gallery_item: scraper::Selector,
    image_url: regex::Regex,
    character_url: regex::Regex,

    fls_cookie: Option<String>,
}

#[derive(Debug)]
pub struct FListGalleryItem {
    pub id: i32,
    pub ext: String,
    pub character_name: String,
}

impl FList {
    pub fn new(user_agent: &str) -> Self {
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

    pub async fn sign_in(&mut self, username: &str, password: &str) -> Result<(), Error> {
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
            .ok_or(Error::Missing)?
            .value()
            .attr("value")
            .ok_or(Error::Missing)?;

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
            .ok_or(Error::Missing)?;

        self.fls_cookie = Some(cookie.value().to_string());

        Ok(())
    }

    pub async fn get_latest_gallery_items(
        &self,
        offset: Option<i32>,
    ) -> Result<Vec<FListGalleryItem>, Error> {
        // F-List only returns first 10,000 items
        if offset.unwrap_or(0) > 10_000 {
            return Ok(Vec::new());
        }

        let auth = self.fls_cookie.as_deref().ok_or(Error::Missing)?;
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
