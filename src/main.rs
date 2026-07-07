use ::config::Config;
use ::config::File;
use ::config::Environment;
use app::App;
use crate::config::programmer_config::ProgrammerConfig;

mod ui;
pub mod config;
pub mod app;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let terminal = ratatui::init();
    let programmer_config = Config::builder()
        .add_source(File::with_name("config"))
        .add_source(Environment::with_prefix("Programmer"))
        .build()?;

    let programmer_config: ProgrammerConfig  = programmer_config.try_deserialize()?;

    let result = App::new(programmer_config).await.run(terminal).await;
    ratatui::restore();
    result
}
