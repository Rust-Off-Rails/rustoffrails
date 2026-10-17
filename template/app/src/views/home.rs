use askama::Template;

use super::Ctx;
use crate::routes::paths;

#[derive(Template)]
#[template(path = "home/index.html")]
pub struct HomeIndex {
    pub ctx: Ctx,
}
